use super::*;

impl<'a> InvestigationsRepository<'a> {
    /// Save one immutable plan revision. A retry with the same id is idempotent;
    /// saving a new id supersedes the previous draft/active revision for the task.
    pub fn save_plan(&self, plan: &InvestigationPlan) -> Result<()> {
        let steps = serde_json::to_string(&plan.steps)?;
        let approval = plan
            .approval
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let observation_window = plan
            .observation_window
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let existing_task = tx
                .query_row(
                    "SELECT task_id FROM investigation_plans WHERE id = ?1",
                    params![plan.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(task_id) = existing_task {
                if task_id != plan.task_id {
                    return Err(DatabaseError::Validation(
                        "investigation plan id belongs to another task".into(),
                    ));
                }
                tx.execute(
                    "UPDATE investigation_plans
                        SET revision = ?2, status = ?3, created_at = ?4, updated_at = ?5,
                            current_step_id = ?6, steps = ?7, approval = ?8, observation_window = ?9
                      WHERE id = ?1",
                    params![
                        plan.id,
                        i64::from(plan.revision),
                        plan.status.as_str(),
                        plan.created_at,
                        plan.updated_at,
                        plan.current_step_id,
                        steps,
                        approval,
                        observation_window,
                    ],
                )?;
            } else {
                tx.execute(
                    "UPDATE investigation_plans SET status = 'superseded', updated_at = ?2
                      WHERE task_id = ?1 AND status IN ('draft','active')",
                    params![plan.task_id, plan.updated_at],
                )?;
                tx.execute(
                    "INSERT INTO investigation_plans
                        (id, task_id, revision, status, created_at, updated_at, current_step_id, steps, approval, observation_window)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        plan.id,
                        plan.task_id,
                        i64::from(plan.revision),
                        plan.status.as_str(),
                        plan.created_at,
                        plan.updated_at,
                        plan.current_step_id,
                        steps,
                        approval,
                        observation_window,
                    ],
                )?;
            }
            tx.commit()?;
            Ok(())
        })
    }

    pub fn get_plan(&self, id: &str) -> Result<InvestigationPlan> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, revision, status, created_at, updated_at,
                            current_step_id, steps, approval, observation_window
                     FROM investigation_plans WHERE id = ?1",
                    params![id],
                    row_to_plan,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn latest_plan(&self, task_id: &str) -> Result<Option<InvestigationPlan>> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, revision, status, created_at, updated_at,
                            current_step_id, steps, approval, observation_window
                     FROM investigation_plans
                     WHERE task_id = ?1 AND status IN ('draft','active','completed')
                     ORDER BY revision DESC, id DESC LIMIT 1",
                    params![task_id],
                    row_to_plan,
                )
                .optional()
                .map_err(DatabaseError::from)
        })
    }

    /// Insert or update one bounded phase artifact. Replaying the same id is idempotent;
    /// a newer artifact id remains a separate immutable observation in the task timeline.
    pub fn upsert_artifact(&self, artifact: &InvestigationArtifact) -> Result<()> {
        let content = serde_json::to_string(&artifact.content)?;
        if content.len() > MAX_ARTIFACT_SERIALIZED_BYTES {
            return Err(DatabaseError::Validation(
                "investigation artifact content exceeds the 1 MiB limit".into(),
            ));
        }
        let evidence_ids = serde_json::to_string(&artifact.evidence_ids)?;
        self.db.with(|connection| {
            let existing_task = connection
                .query_row(
                    "SELECT task_id FROM investigation_artifacts WHERE id = ?1",
                    params![artifact.id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?;
            if let Some(task_id) = existing_task {
                if task_id != artifact.task_id {
                    return Err(DatabaseError::Validation(
                        "investigation artifact id belongs to another task".into(),
                    ));
                }
            }
            connection.execute(
                "INSERT INTO investigation_artifacts (
                    id, task_id, run_id, plan_id, plan_step_id, phase, kind, status, title, summary, content,
                    evidence_ids, created_at, updated_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
                 ON CONFLICT(id) DO UPDATE SET
                    run_id = excluded.run_id,
                    plan_id = excluded.plan_id,
                    plan_step_id = excluded.plan_step_id,
                    phase = excluded.phase,
                    kind = excluded.kind,
                    status = excluded.status,
                    title = excluded.title,
                    summary = excluded.summary,
                    content = excluded.content,
                    evidence_ids = excluded.evidence_ids,
                    updated_at = excluded.updated_at",
                params![
                    artifact.id,
                    artifact.task_id,
                    artifact.run_id,
                    artifact.plan_id,
                    artifact.plan_step_id,
                    artifact.phase.as_str(),
                    artifact.kind.as_str(),
                    artifact.status.as_str(),
                    artifact.title,
                    artifact.summary,
                    content,
                    evidence_ids,
                    artifact.created_at,
                    artifact.updated_at,
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_artifacts(
        &self,
        task_id: &str,
        limit: usize,
    ) -> Result<Vec<InvestigationArtifact>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, plan_id, plan_step_id, phase, kind, status, title, summary, content,
                        evidence_ids, created_at, updated_at
                 FROM investigation_artifacts
                 WHERE task_id = ?1 ORDER BY updated_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_artifact)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DatabaseError::from)
        })
    }
}
