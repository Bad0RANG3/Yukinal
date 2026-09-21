use super::*;

impl<'a> InvestigationsRepository<'a> {
    pub fn add_evidence(&self, evidence: &Evidence) -> Result<()> {
        self.validate_evidence(evidence)?;
        let content = serde_json::to_string(&evidence.content)?;
        let scope = serde_json::to_string(&evidence.scope)?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_evidence (
                    id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                    content_type, content, content_hash, truncated, redaction_status
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    evidence.id,
                    evidence.task_id,
                    evidence.run_id,
                    scope,
                    evidence.kind.as_str(),
                    evidence.source_tool,
                    evidence.collected_at,
                    evidence.input_summary,
                    evidence.content_type.as_str(),
                    content,
                    evidence.content_hash,
                    if evidence.truncated { 1_i64 } else { 0_i64 },
                    evidence.redaction_status.as_str(),
                ],
            )?;
            Ok(())
        })
    }

    /// Validate the bounded, redacted evidence envelope without mutating the database.
    ///
    /// The host uses this before same-run deduplication so an invalid retry cannot
    /// reuse a previously persisted row merely because its metadata happens to match.
    pub fn validate_evidence(&self, evidence: &Evidence) -> Result<()> {
        let content = serde_json::to_string(&evidence.content)?;
        if content.len() > MAX_EVIDENCE_SERIALIZED_BYTES {
            return Err(DatabaseError::Validation(format!(
                "evidence content exceeds {MAX_EVIDENCE_SERIALIZED_BYTES} bytes"
            )));
        }
        if evidence.redaction_status == EvidenceRedactionStatus::Unknown {
            return Err(DatabaseError::Validation(
                "evidence must be redacted before persistence".into(),
            ));
        }
        let actual_hash = content_hash(&evidence.content);
        if actual_hash != evidence.content_hash {
            return Err(DatabaseError::Validation(
                "evidence contentHash does not match content".into(),
            ));
        }
        Ok(())
    }

    /// Find an identical observation already persisted for one durable run.
    ///
    /// The run id is deliberately part of the key: two scheduled samples with
    /// identical content are still distinct observations that must remain
    /// comparable across runs.  This lookup only collapses a repeated record
    /// inside the same run, after the host has already assigned the run id.
    pub fn find_evidence_in_run(&self, evidence: &Evidence) -> Result<Option<Evidence>> {
        let Some(run_id) = evidence.run_id.as_deref() else {
            return Ok(None);
        };
        let scope = serde_json::to_string(&evidence.scope)?;
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                            content_type, content, content_hash, truncated, redaction_status
                     FROM investigation_evidence
                     WHERE task_id = ?1 AND run_id = ?2 AND scope = ?3
                       AND kind = ?4 AND source_tool = ?5 AND input_summary = ?6
                       AND content_hash = ?7 AND truncated = ?8 AND content_type = ?9
                     ORDER BY collected_at DESC, id DESC LIMIT 1",
                    params![
                        evidence.task_id,
                        run_id,
                        scope,
                        evidence.kind.as_str(),
                        evidence.source_tool,
                        evidence.input_summary,
                        evidence.content_hash,
                        if evidence.truncated { 1_i64 } else { 0_i64 },
                        evidence.content_type.as_str(),
                    ],
                    row_to_evidence,
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }

    pub fn list_evidence(&self, task_id: &str, limit: usize) -> Result<Vec<Evidence>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                        content_type, content, content_hash, truncated, redaction_status
                 FROM investigation_evidence
                 WHERE task_id = ?1 ORDER BY collected_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_evidence)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn get_evidence(&self, id: &str) -> Result<Evidence> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                            content_type, content, content_hash, truncated, redaction_status
                     FROM investigation_evidence WHERE id = ?1",
                    params![id],
                    row_to_evidence,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    /// Search one task's evidence by bounded metadata only. The body is returned
    /// to the host for summary projection, but the sidecar search contract never
    /// includes it; callers must fetch one id explicitly to read content.
    pub fn search_evidence(
        &self,
        task_id: &str,
        query: &EvidenceSearchQuery,
    ) -> Result<Vec<Evidence>> {
        if !(1..=64).contains(&query.limit) {
            return Err(DatabaseError::Validation(
                "evidence search limit must be between 1 and 64".into(),
            ));
        }
        if query
            .source_tool
            .as_deref()
            .is_some_and(|value| value.trim().is_empty() || value.chars().count() > 256)
        {
            return Err(DatabaseError::Validation(
                "evidence source tool must be between 1 and 256 characters".into(),
            ));
        }
        for timestamp in [query.from.as_deref(), query.to.as_deref()]
            .into_iter()
            .flatten()
        {
            if yukinal_time::parse_iso8601_utc(timestamp).is_none() {
                return Err(DatabaseError::Validation(
                    "evidence search timestamps must be UTC ISO-8601 values".into(),
                ));
            }
        }
        if let (Some(from), Some(to)) = (query.from.as_deref(), query.to.as_deref()) {
            if yukinal_time::parse_iso8601_utc(from) > yukinal_time::parse_iso8601_utc(to) {
                return Err(DatabaseError::Validation(
                    "evidence search from must not be after to".into(),
                ));
            }
        }
        let scope = query
            .scope
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                        content_type, content, content_hash, truncated, redaction_status
                 FROM investigation_evidence
                 WHERE task_id = ?1
                   AND (?2 IS NULL OR source_tool = ?2)
                   AND (?3 IS NULL OR kind = ?3)
                   AND (?4 IS NULL OR collected_at >= ?4)
                   AND (?5 IS NULL OR collected_at <= ?5)
                   AND (?6 IS NULL OR scope = ?6)
                 ORDER BY collected_at DESC, id DESC LIMIT ?7",
            )?;
            let rows = statement.query_map(
                params![
                    task_id,
                    query.source_tool.as_deref(),
                    query.kind.map(|kind| kind.as_str()),
                    query.from.as_deref(),
                    query.to.as_deref(),
                    scope,
                    query.limit as i64,
                ],
                row_to_evidence,
            )?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Return the evidence collected by one host-owned run. This is intentionally a
    /// separate query from the task-wide timeline: scheduled comparisons must never
    /// accidentally include a previous run or a user-started run.
    pub fn list_evidence_for_run(&self, run_id: &str, limit: usize) -> Result<Vec<Evidence>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, scope, kind, source_tool, collected_at, input_summary,
                        content_type, content, content_hash, truncated, redaction_status
                 FROM investigation_evidence
                 WHERE run_id = ?1 ORDER BY collected_at ASC, id ASC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![run_id, limit as i64], row_to_evidence)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Compare one scheduled run with its explicit baseline, or the latest successful run of
    /// the same schedule when no baseline was selected. The comparison is intentionally
    /// content-fingerprint based: collection timestamps and row ids are excluded, while
    /// source, input summary, kind, truncation and content hash remain part of the signal.
    /// Empty samples are reported as insufficient evidence rather than a false "no change".
    pub fn compare_schedule_run(&self, run_id: &str) -> Result<InvestigationScheduleComparison> {
        self.db.with(|connection| {
            let (schedule_id, baseline_run_id): (String, Option<String>) = connection
                .query_row(
                    "SELECT r.schedule_id, s.baseline_run_id
                     FROM investigation_schedule_runs r
                     JOIN investigation_schedules s ON s.id = r.schedule_id
                     WHERE r.id = ?1",
                    params![run_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?;
            let previous_run_id: Option<String> = if let Some(baseline_run_id) = baseline_run_id {
                // An explicit baseline is deliberately fail-closed when its evidence has
                // been pruned or the run was otherwise corrupted. Falling back to the
                // latest sample would silently change the user's comparison contract.
                connection
                    .query_row(
                        "SELECT id FROM investigation_schedule_runs
                         WHERE id = ?1 AND schedule_id = ?2 AND id <> ?3
                           AND status = 'succeeded' AND finished_at IS NOT NULL
                           AND EXISTS (
                               SELECT 1 FROM investigation_evidence e WHERE e.run_id = investigation_schedule_runs.id
                           )",
                        params![baseline_run_id, schedule_id, run_id],
                        |row| row.get(0),
                    )
                    .optional()?
            } else {
                connection
                    .query_row(
                        "SELECT id FROM investigation_schedule_runs
                         WHERE schedule_id = ?1 AND id <> ?2 AND status = 'succeeded'
                           AND finished_at IS NOT NULL
                           AND EXISTS (
                               SELECT 1 FROM investigation_evidence e WHERE e.run_id = investigation_schedule_runs.id
                           )
                         ORDER BY finished_at DESC, id DESC LIMIT 1",
                        params![schedule_id, run_id],
                        |row| row.get(0),
                    )
                    .optional()?
            };
            let current = query_evidence_for_run(connection, run_id)?;
            let previous = previous_run_id
                .as_deref()
                .map(|id| query_evidence_for_run(connection, id))
                .transpose()?
                .unwrap_or_default();
            let current_signature = evidence_signature(&current);
            let previous_signature = evidence_signature(&previous);
            let status = match (&current_signature, &previous_signature) {
                (None, _) => InvestigationScheduleComparisonStatus::InsufficientEvidence,
                (Some(_), None) => InvestigationScheduleComparisonStatus::Baseline,
                (Some(current), Some(previous)) if current == previous => {
                    InvestigationScheduleComparisonStatus::NoChange
                }
                (Some(_), Some(_)) => InvestigationScheduleComparisonStatus::Changed,
            };
            Ok(InvestigationScheduleComparison {
                status,
                current_signature,
                previous_signature,
                current_evidence_ids: current.into_iter().map(|evidence| evidence.id).collect(),
                previous_evidence_ids: previous.into_iter().map(|evidence| evidence.id).collect(),
            })
        })
    }

    pub fn add_finding(&self, finding: &Finding) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_findings (
                    id, task_id, title, kind, statement, evidence_ids, confidence,
                    next_verification, created_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    finding.id,
                    finding.task_id,
                    finding.title,
                    finding.kind.as_str(),
                    finding.statement,
                    serde_json::to_string(&finding.evidence_ids)?,
                    finding.confidence.as_str(),
                    finding.next_verification,
                    finding.created_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_findings(&self, task_id: &str, limit: usize) -> Result<Vec<Finding>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, title, kind, statement, evidence_ids, confidence,
                        next_verification, created_at
                 FROM investigation_findings
                 WHERE task_id = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_finding)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn get_finding(&self, id: &str) -> Result<Finding> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, title, kind, statement, evidence_ids, confidence,
                            next_verification, created_at
                     FROM investigation_findings WHERE id = ?1",
                    params![id],
                    row_to_finding,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn save_decision_brief(&self, brief: &DecisionBrief) -> Result<()> {
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_decision_briefs (
                    id, task_id, plan_id, generated_at, status, finding_ids, options, selected_option_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    brief.id,
                    brief.task_id,
                    brief.plan_id,
                    brief.generated_at,
                    brief.status.as_str(),
                    serde_json::to_string(&brief.finding_ids)?,
                    serde_json::to_string(&brief.options)?,
                    brief.selected_option_id,
                ],
            )?;
            Ok(())
        })
    }

    pub fn select_decision_brief_option(
        &self,
        task_id: &str,
        brief_id: &str,
        option_id: &str,
    ) -> Result<DecisionBrief> {
        self.db.with(|connection| {
            let brief = connection
                .query_row(
                    "SELECT id, task_id, plan_id, generated_at, status, finding_ids, options, selected_option_id
                     FROM investigation_decision_briefs WHERE id = ?1",
                    params![brief_id],
                    row_to_brief,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)?;
            if brief.task_id != task_id {
                return Err(DatabaseError::Validation(
                    "decision brief does not belong to the current investigation task".into(),
                ));
            }
            if brief.status == DecisionBriefStatus::Dismissed {
                return Err(DatabaseError::Validation(
                    "dismissed decision briefs cannot be selected".into(),
                ));
            }
            if !brief.options.iter().any(|option| option.id == option_id) {
                return Err(DatabaseError::Validation(
                    "decision option was not found in the selected brief".into(),
                ));
            }
            let mut options = brief.options.clone();
            for option in &mut options {
                option.status = if option.id == option_id {
                    DecisionOptionStatus::Selected
                } else {
                    DecisionOptionStatus::Rejected
                };
            }
            let options = serde_json::to_string(&options)?;
            connection.execute(
                "UPDATE investigation_decision_briefs
                    SET status = ?3, selected_option_id = ?2, options = ?5
                  WHERE id = ?1 AND task_id = ?4",
                params![
                    brief_id,
                    option_id,
                    DecisionBriefStatus::Selected.as_str(),
                    task_id,
                    options
                ],
            )?;
            connection
                .query_row(
                    "SELECT id, task_id, plan_id, generated_at, status, finding_ids, options, selected_option_id
                     FROM investigation_decision_briefs WHERE id = ?1",
                    params![brief_id],
                    row_to_brief,
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn latest_decision_brief(&self, task_id: &str) -> Result<Option<DecisionBrief>> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, plan_id, generated_at, status, finding_ids, options, selected_option_id
                     FROM investigation_decision_briefs
                     WHERE task_id = ?1 ORDER BY generated_at DESC, id DESC LIMIT 1",
                    params![task_id],
                    row_to_brief,
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }
}
