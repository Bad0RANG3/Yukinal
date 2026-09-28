use super::*;

impl<'a> InvestigationsRepository<'a> {
    /// Create a durable read-only trigger. The task's safety envelope is checked at
    /// creation time and again by every tick; changing a task to executable never
    /// silently widens an existing schedule.
    pub fn create_schedule(&self, schedule: &InvestigationSchedule) -> Result<()> {
        validate_schedule_shape(schedule)?;
        let budget = serde_json::to_string(&schedule.budget)?;
        self.db.with(|connection| {
            let task = connection
                .query_row(
                    "SELECT mode, automation_level FROM investigation_tasks WHERE id = ?1",
                    params![schedule.task_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()?;
            let Some((mode, automation)) = task else {
                return Err(DatabaseError::NotFound);
            };
            if mode != "readonly" || automation != "readonly" {
                return Err(DatabaseError::Validation(
                    "scheduler triggers may only target read-only investigation tasks".into(),
                ));
            }
            connection.execute(
                "INSERT INTO investigation_schedules (
                    id, task_id, status, interval_seconds, cooldown_seconds, dedupe_window_seconds,
                    max_concurrent_runs, budget, notification_policy, next_run_at, last_run_at,
                    last_outcome, last_error, created_at, updated_at, baseline_run_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    schedule.id,
                    schedule.task_id,
                    schedule.status.as_str(),
                    i64::try_from(schedule.interval_seconds)
                        .map_err(|_| DatabaseError::Validation("interval is too large".into()))?,
                    i64::try_from(schedule.cooldown_seconds)
                        .map_err(|_| DatabaseError::Validation("cooldown is too large".into()))?,
                    i64::try_from(schedule.dedupe_window_seconds).map_err(|_| {
                        DatabaseError::Validation("dedupe window is too large".into())
                    })?,
                    i64::from(schedule.max_concurrent_runs),
                    budget,
                    schedule.notification_policy.as_str(),
                    schedule.next_run_at,
                    schedule.last_run_at,
                    schedule.last_outcome,
                    schedule.last_error,
                    schedule.created_at,
                    schedule.updated_at,
                    schedule.baseline_run_id,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_schedule(&self, schedule_id: &str) -> Result<InvestigationSchedule> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, status, interval_seconds, cooldown_seconds,
                            dedupe_window_seconds, max_concurrent_runs, budget,
                            notification_policy, next_run_at, last_run_at, last_outcome,
                            last_error, created_at, updated_at, baseline_run_id
                     FROM investigation_schedules WHERE id = ?1",
                    params![schedule_id],
                    row_to_schedule,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn get_schedule_for_run(&self, run_id: &str) -> Result<InvestigationSchedule> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT s.id, s.task_id, s.status, s.interval_seconds, s.cooldown_seconds,
                            s.dedupe_window_seconds, s.max_concurrent_runs, s.budget,
                            s.notification_policy, s.next_run_at, s.last_run_at, s.last_outcome,
                            s.last_error, s.created_at, s.updated_at, s.baseline_run_id
                     FROM investigation_schedules s
                     JOIN investigation_schedule_runs r ON r.schedule_id = s.id
                     WHERE r.id = ?1",
                    params![run_id],
                    row_to_schedule,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn list_schedules(&self, limit: usize) -> Result<Vec<InvestigationSchedule>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, status, interval_seconds, cooldown_seconds,
                        dedupe_window_seconds, max_concurrent_runs, budget,
                        notification_policy, next_run_at, last_run_at, last_outcome,
                        last_error, created_at, updated_at, baseline_run_id
                 FROM investigation_schedules ORDER BY next_run_at ASC, id ASC LIMIT ?1",
            )?;
            let rows = statement.query_map(params![limit as i64], row_to_schedule)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn update_schedule(&self, schedule: &InvestigationSchedule) -> Result<()> {
        validate_schedule_shape(schedule)?;
        let budget = serde_json::to_string(&schedule.budget)?;
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_schedules SET status = ?2, interval_seconds = ?3,
                        cooldown_seconds = ?4, dedupe_window_seconds = ?5,
                        max_concurrent_runs = ?6, budget = ?7, notification_policy = ?8,
                        next_run_at = ?9, last_run_at = ?10, last_outcome = ?11,
                        last_error = ?12, updated_at = ?13, baseline_run_id = ?14 WHERE id = ?1",
                params![
                    schedule.id,
                    schedule.status.as_str(),
                    i64::try_from(schedule.interval_seconds)
                        .map_err(|_| DatabaseError::Validation("interval is too large".into()))?,
                    i64::try_from(schedule.cooldown_seconds)
                        .map_err(|_| DatabaseError::Validation("cooldown is too large".into()))?,
                    i64::try_from(schedule.dedupe_window_seconds).map_err(|_| {
                        DatabaseError::Validation("dedupe window is too large".into())
                    })?,
                    i64::from(schedule.max_concurrent_runs),
                    budget,
                    schedule.notification_policy.as_str(),
                    schedule.next_run_at,
                    schedule.last_run_at,
                    schedule.last_outcome,
                    schedule.last_error,
                    schedule.updated_at,
                    schedule.baseline_run_id,
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            Ok(())
        })
    }

    pub fn list_schedule_runs(
        &self,
        schedule_id: &str,
        limit: usize,
    ) -> Result<Vec<InvestigationScheduleRun>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                        started_at, finished_at, dedupe_key, outcome, error
                 FROM investigation_schedule_runs
                 WHERE schedule_id = ?1 ORDER BY scheduled_at DESC, id DESC LIMIT ?2",
            )?;
            let rows =
                statement.query_map(params![schedule_id, limit as i64], row_to_schedule_run)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()
                .map_err(DatabaseError::from)
        })
    }

    /// Read one durable schedule run for host-side baseline validation. The
    /// schedule update command uses this rather than trusting a UI-provided run
    /// from a different trigger or an unbounded history page.
    pub fn get_schedule_run(&self, run_id: &str) -> Result<InvestigationScheduleRun> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                            started_at, finished_at, dedupe_key, outcome, error
                     FROM investigation_schedule_runs WHERE id = ?1",
                    params![run_id],
                    row_to_schedule_run,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    /// Atomically claim due schedules. The transaction makes a UI refresh, a second
    /// scheduler tick and startup recovery observe one run at most. It returns skipped
    /// rows as first-class records so the user can see why a trigger did not run.
    pub fn claim_due_schedule_runs(
        &self,
        now: &str,
        limit: usize,
    ) -> Result<(Vec<InvestigationScheduleRun>, Vec<InvestigationScheduleRun>)> {
        let now_epoch = yukinal_time::parse_iso8601_utc(now).ok_or_else(|| {
            DatabaseError::Validation("scheduler now must be a UTC timestamp".into())
        })?;
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let mut statement = tx.prepare(
                "SELECT id, task_id, status, interval_seconds, cooldown_seconds,
                        dedupe_window_seconds, max_concurrent_runs, budget,
                        notification_policy, next_run_at, last_run_at, last_outcome,
                        last_error, created_at, updated_at, baseline_run_id
                 FROM investigation_schedules
                 WHERE status = 'active' AND next_run_at <= ?1
                 ORDER BY next_run_at ASC, id ASC LIMIT ?2",
            )?;
            let schedules = statement
                .query_map(params![now, limit as i64], row_to_schedule)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            drop(statement);
            let mut claimed = Vec::new();
            let mut skipped = Vec::new();
            for schedule in schedules {
                let task = tx
                    .query_row(
                        "SELECT status, mode, automation_level FROM investigation_tasks WHERE id = ?1",
                        params![schedule.task_id],
                        |row| {
                            Ok((
                                row.get::<_, String>(0)?,
                                row.get::<_, String>(1)?,
                                row.get::<_, String>(2)?,
                            ))
                        },
                    )
                    .optional()?;
                let due_at = schedule.next_run_at.clone();
                let due_epoch = yukinal_time::parse_iso8601_utc(&due_at).unwrap_or(now_epoch);
                let dedupe_bucket = due_epoch / schedule.dedupe_window_seconds.max(1);
                let dedupe_key = format!("bucket:{dedupe_bucket}");
                let next_run_at = yukinal_time::iso8601_utc(
                    now_epoch.saturating_add(schedule.interval_seconds),
                );
                let existing = tx
                    .query_row(
                        "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                                started_at, finished_at, dedupe_key, outcome, error
                         FROM investigation_schedule_runs
                         WHERE schedule_id = ?1 AND dedupe_key = ?2",
                        params![schedule.id, dedupe_key],
                        row_to_schedule_run,
                    )
                    .optional()?;
                if let Some(existing) = existing {
                    tx.execute(
                        "UPDATE investigation_schedules SET next_run_at = ?2, updated_at = ?3 WHERE id = ?1",
                        params![schedule.id, next_run_at, now],
                    )?;
                    let task_terminal = task.as_ref().is_some_and(|(status, _, _)| {
                        matches!(status.as_str(), "completed" | "failed" | "stopped" | "expired")
                    });
                    if task_terminal || existing.outcome.as_deref() == Some("task_terminal") {
                        // Keep upgrades and manually repaired rows quiet too: an older
                        // terminal skip may already occupy this dedupe bucket before the
                        // current claim reaches the reason-mapping branch below.
                        tx.execute(
                            "UPDATE investigation_schedules SET status = 'revoked' WHERE id = ?1",
                            params![schedule.id],
                        )?;
                    }
                    if matches!(
                        existing.status,
                        InvestigationScheduleRunStatus::Claimed | InvestigationScheduleRunStatus::Running
                    ) {
                        claimed.push(existing);
                    } else {
                        skipped.push(existing);
                    }
                    continue;
                }
                let reason = match task {
                    Some((status, mode, automation))
                        if mode == "readonly"
                            && automation == "readonly"
                            && matches!(status.as_str(), "pending" | "investigating" | "waiting_user") => None,
                    Some((status, _, _)) if matches!(status.as_str(), "completed" | "failed" | "stopped" | "expired") => {
                        Some("task_terminal")
                    }
                    Some(_) => Some("task_not_readonly_or_waiting_user"),
                    None => Some("task_missing"),
                };
                let active_count: u32 = tx.query_row(
                    "SELECT COUNT(*) FROM investigation_schedule_runs
                     WHERE schedule_id = ?1 AND status IN ('queued','claimed','running')",
                    params![schedule.id],
                    |row| row.get::<_, i64>(0),
                )?.try_into().unwrap_or(u32::MAX);
                let cooldown_active = schedule
                    .last_run_at
                    .as_deref()
                    .and_then(yukinal_time::parse_iso8601_utc)
                    .is_some_and(|last| now_epoch.saturating_sub(last) < schedule.cooldown_seconds);
                let concurrency_reason = if reason.is_some() {
                    reason
                } else if cooldown_active {
                    Some("cooldown")
                } else if active_count >= schedule.max_concurrent_runs {
                    Some("concurrency_limit")
                } else {
                    None
                };
                let run_id = schedule_run_id(&schedule.id, &dedupe_key);
                let run = if let Some(reason) = concurrency_reason {
                    let run = InvestigationScheduleRun {
                        id: run_id,
                        schedule_id: schedule.id.clone(),
                        task_id: schedule.task_id.clone(),
                        status: InvestigationScheduleRunStatus::Skipped,
                        scheduled_at: due_at,
                        claimed_at: None,
                        started_at: None,
                        finished_at: Some(now.to_string()),
                        dedupe_key,
                        outcome: Some(reason.to_string()),
                        error: None,
                    };
                    insert_schedule_run(&tx, &run)?;
                    skipped.push(run.clone());
                    run
                } else {
                    let run = InvestigationScheduleRun {
                        id: run_id,
                        schedule_id: schedule.id.clone(),
                        task_id: schedule.task_id.clone(),
                        status: InvestigationScheduleRunStatus::Claimed,
                        scheduled_at: due_at,
                        claimed_at: Some(now.to_string()),
                        started_at: None,
                        finished_at: None,
                        dedupe_key,
                        outcome: None,
                        error: None,
                    };
                    insert_schedule_run(&tx, &run)?;
                    claimed.push(run.clone());
                    run
                };
                let last_run_at = if run.status == InvestigationScheduleRunStatus::Skipped {
                    schedule.last_run_at
                } else {
                    Some(now.to_string())
                };
                if run.status == InvestigationScheduleRunStatus::Skipped {
                    // A skipped trigger is itself a terminal schedule outcome.  A claimed
                    // run, however, is only an in-flight marker: keep the previous terminal
                    // outcome so the scheduler can give the sidecar a meaningful changed /
                    // no_change / baseline follow-up prompt.
                    tx.execute(
                        "UPDATE investigation_schedules SET next_run_at = ?2, last_run_at = ?3,
                                last_outcome = ?4, last_error = ?5, updated_at = ?6 WHERE id = ?1",
                        params![
                            schedule.id,
                            next_run_at,
                            last_run_at,
                            run.outcome,
                            run.error,
                            now
                        ],
                    )?;
                    if run.outcome.as_deref() == Some("task_terminal") {
                        // A terminal task can no longer be scheduled. Revoke the
                        // trigger in the same transaction as the skipped run so
                        // every later tick stays quiet and auditable.
                        tx.execute(
                            "UPDATE investigation_schedules SET status = 'revoked' WHERE id = ?1",
                            params![schedule.id],
                        )?;
                    }
                } else {
                    tx.execute(
                        "UPDATE investigation_schedules SET next_run_at = ?2, last_run_at = ?3,
                                updated_at = ?3 WHERE id = ?1",
                        params![schedule.id, next_run_at, last_run_at],
                    )?;
                }
            }
            tx.commit()?;
            Ok((claimed, skipped))
        })
    }

    /// Close a scheduled run that failed before the sidecar could start.
    ///
    /// A schedule claim creates both an investigation run and a schedule run.
    /// The launch failure therefore has to clear the task fence, terminate the
    /// investigation row, close open steps, and finish the schedule row in one
    /// transaction. Repeating the call is safe once the combined terminal state
    /// has already been persisted.
    #[allow(clippy::too_many_arguments)]
    pub fn fail_schedule_run_launch(
        &self,
        task_id: &str,
        run_id: &str,
        message: &str,
        code: TaskFailureCode,
        retryable: bool,
        options: Vec<InvestigationFailureOption>,
        outcome: &str,
        finished_at: &str,
    ) -> Result<InvestigationScheduleRun> {
        let message: String = message.chars().take(4_096).collect();
        let outcome: String = outcome.chars().take(256).collect();
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let schedule_id = tx
                .query_row(
                    "SELECT schedule_id FROM investigation_schedule_runs
                     WHERE id = ?1 AND task_id = ?2",
                    params![run_id, task_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?;
            let active_run_id = tx
                .query_row(
                    "SELECT active_run_id FROM investigation_tasks WHERE id = ?1",
                    params![task_id],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?;
            if active_run_id
                .as_deref()
                .is_some_and(|active_run_id| active_run_id != run_id)
            {
                return Err(DatabaseError::Validation(
                    "a newer investigation run owns the task".into(),
                ));
            }

            let attempt = tx
                .query_row(
                    "SELECT attempt FROM investigation_runs
                     WHERE id = ?1 AND task_id = ?2",
                    params![run_id, task_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?
                .try_into()
                .unwrap_or(u32::MAX);
            let run_status = tx
                .query_row(
                    "SELECT status FROM investigation_runs WHERE id = ?1 AND task_id = ?2",
                    params![run_id, task_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or(DatabaseError::NotFound)?;
            if !matches!(
                run_status.as_str(),
                "admitted" | "running" | "waiting_user" | "interrupted" | "failed"
            ) {
                return Err(DatabaseError::Validation(
                    "investigation run is already terminal and cannot be failed".into(),
                ));
            }

            let failure = InvestigationFailure {
                code,
                message,
                retryable,
                attempt,
                at: finished_at.to_string(),
                detail: None,
                options: Some(options),
            };
            let failure_json = serde_json::to_string(&failure)?;

            tx.execute(
                "UPDATE investigation_steps
                    SET status = CASE status WHEN 'pending' THEN 'skipped' ELSE 'failed' END,
                        ended_at = ?3,
                        failure = CASE WHEN status = 'pending' THEN NULL ELSE ?4 END
                  WHERE task_id = ?1 AND run_id = ?2
                    AND status IN ('pending','running','waiting_user')",
                params![task_id, run_id, finished_at, failure_json],
            )?;
            tx.execute(
                "UPDATE investigation_runs
                    SET status = 'failed', updated_at = ?3,
                        ended_at = ?3, failure = ?4
                  WHERE id = ?1 AND task_id = ?2
                    AND status IN ('admitted','running','waiting_user','interrupted')",
                params![run_id, task_id, finished_at, failure_json],
            )?;
            tx.execute(
                "UPDATE investigation_tasks
                    SET status = 'failed', phase = 'recovery', active_run_id = NULL,
                        last_failure = ?3, updated_at = ?4, completed_at = ?4
                  WHERE id = ?1 AND active_run_id = ?2",
                params![task_id, run_id, failure_json, finished_at],
            )?;

            let schedule_changed = tx.execute(
                "UPDATE investigation_schedule_runs
                    SET status = 'failed', finished_at = ?2,
                        outcome = ?3, error = ?4
                  WHERE id = ?1 AND status IN ('queued','claimed','running','interrupted')",
                params![run_id, finished_at, outcome, failure.message],
            )?;
            let schedule_run_status = tx.query_row(
                "SELECT status FROM investigation_schedule_runs WHERE id = ?1",
                params![run_id],
                |row| row.get::<_, String>(0),
            )?;
            if schedule_changed == 0 && schedule_run_status != "failed" {
                return Err(DatabaseError::Validation(
                    "schedule run is already terminal and cannot be failed".into(),
                ));
            }
            tx.execute(
                "UPDATE investigation_schedules
                    SET last_outcome = ?2, last_error = ?3, updated_at = ?4
                  WHERE id = ?1",
                params![schedule_id, outcome, failure.message, finished_at],
            )?;
            tx.commit()?;

            connection
                .query_row(
                    "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                            started_at, finished_at, dedupe_key, outcome, error
                     FROM investigation_schedule_runs WHERE id = ?1",
                    params![run_id],
                    row_to_schedule_run,
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn finish_schedule_run(
        &self,
        run_id: &str,
        status: InvestigationScheduleRunStatus,
        outcome: Option<&str>,
        error: Option<&str>,
        finished_at: &str,
    ) -> Result<InvestigationScheduleRun> {
        self.db.with(|connection| {
            let schedule_id = connection.query_row(
                "SELECT schedule_id FROM investigation_schedule_runs WHERE id = ?1",
                params![run_id],
                |row| row.get::<_, String>(0),
            )?;
            let changed = connection.execute(
                "UPDATE investigation_schedule_runs SET status = ?2, finished_at = ?3,
                        outcome = ?4, error = ?5 WHERE id = ?1
                 AND status IN ('claimed','running','queued')",
                params![run_id, status.as_str(), finished_at, outcome, error],
            )?;
            if changed == 0 {
                return Err(DatabaseError::Validation(
                    "schedule run is missing or already terminal".into(),
                ));
            }
            connection.execute(
                "UPDATE investigation_schedules
                    SET last_outcome = COALESCE(?2, ?3), last_error = ?4, updated_at = ?5
                  WHERE id = ?1",
                params![schedule_id, outcome, status.as_str(), error, finished_at],
            )?;
            connection
                .query_row(
                    "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                            started_at, finished_at, dedupe_key, outcome, error
                     FROM investigation_schedule_runs WHERE id = ?1",
                    params![run_id],
                    row_to_schedule_run,
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn start_schedule_run(
        &self,
        run_id: &str,
        started_at: &str,
    ) -> Result<InvestigationScheduleRun> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_schedule_runs SET status = 'running', started_at = ?2
                 WHERE id = ?1 AND status = 'claimed'",
                params![run_id, started_at],
            )?;
            if changed == 0 {
                return Err(DatabaseError::Validation(
                    "schedule run is missing or not claimable".into(),
                ));
            }
            connection
                .query_row(
                    "SELECT id, schedule_id, task_id, status, scheduled_at, claimed_at,
                            started_at, finished_at, dedupe_key, outcome, error
                     FROM investigation_schedule_runs WHERE id = ?1",
                    params![run_id],
                    row_to_schedule_run,
                )
                .map_err(DatabaseError::from)
        })
    }

    /// Close every task whose active run can no longer be owned after an
    /// application restart or sidecar exit. The task fence is part of the
    /// transaction so a stale worker cannot reopen a run after recovery.
    pub fn interrupt_active_investigation_runs(&self, now: &str, reason: &str) -> Result<usize> {
        let reason: String = reason.chars().take(256).collect();
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let active = {
                let mut statement = tx.prepare(
                    "SELECT id, active_run_id FROM investigation_tasks
                     WHERE active_run_id IS NOT NULL
                     ORDER BY id ASC",
                )?;
                let rows = statement.query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };

            for (task_id, run_id) in &active {
                let attempt = tx
                    .query_row(
                        "SELECT attempt FROM investigation_runs
                         WHERE id = ?1 AND task_id = ?2",
                        params![run_id, task_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .unwrap_or(0)
                    .try_into()
                    .unwrap_or(u32::MAX);
                let failure = InvestigationFailure {
                    code: TaskFailureCode::Transport,
                    message: format!("active investigation run was interrupted: {reason}"),
                    retryable: TaskFailureCode::Transport.retryable(),
                    attempt,
                    at: now.to_string(),
                    detail: Some(serde_json::json!({ "reason": reason })),
                    options: None,
                };
                let failure_json = serde_json::to_string(&failure)?;

                tx.execute(
                    "UPDATE investigation_steps
                        SET status = CASE status WHEN 'pending' THEN 'skipped' ELSE 'failed' END,
                            ended_at = ?3,
                            failure = CASE WHEN status = 'pending' THEN NULL ELSE ?4 END
                      WHERE task_id = ?1 AND run_id = ?2
                        AND status IN ('pending','running','waiting_user')",
                    params![task_id, run_id, now, failure_json],
                )?;
                tx.execute(
                    "UPDATE investigation_runs
                        SET status = 'interrupted', updated_at = ?3,
                            ended_at = ?3, failure = ?4
                      WHERE id = ?1 AND task_id = ?2
                        AND status IN ('admitted','running','waiting_user')",
                    params![run_id, task_id, now, failure_json],
                )?;
                tx.execute(
                    "UPDATE investigation_tasks
                        SET status = CASE
                                WHEN status IN ('completed','expired','stopped') THEN status
                                ELSE 'investigating'
                            END,
                            phase = CASE
                                WHEN status IN ('completed','expired','stopped') THEN phase
                                ELSE 'recovery'
                            END,
                            active_run_id = NULL,
                            last_failure = CASE
                                WHEN status IN ('completed','expired') THEN last_failure
                                ELSE ?3
                            END,
                            updated_at = ?4,
                            completed_at = CASE
                                WHEN status IN ('completed','expired','stopped') THEN completed_at
                                ELSE NULL
                            END
                      WHERE id = ?1 AND active_run_id = ?2",
                    params![task_id, run_id, failure_json, now],
                )?;

                // A schedule run owns the same id as its durable investigation
                // run. Keep the scheduler state in sync even when the failure is
                // observed by the general sidecar-exit path.
                tx.execute(
                    "UPDATE investigation_schedule_runs
                        SET status = 'interrupted', finished_at = ?2,
                            outcome = 'interrupted', error = ?3
                      WHERE id = ?1 AND status IN ('claimed','running')",
                    params![run_id, now, reason],
                )?;
                tx.execute(
                    "UPDATE investigation_schedules
                        SET last_outcome = 'interrupted', last_error = ?2, updated_at = ?3
                      WHERE id = (
                          SELECT schedule_id FROM investigation_schedule_runs WHERE id = ?1
                      )",
                    params![run_id, reason, now],
                )?;
            }

            tx.commit()?;
            Ok(active.len())
        })
    }

    /// On application restart, an in-flight scheduler claim cannot be assumed to
    /// have reached the sidecar. Mark the schedule run and its matching durable
    /// investigation run interrupted, close open steps, and clear the task's
    /// active-run fence atomically. The next due tick can then make a deliberate,
    /// deduplicated decision instead of getting stuck behind a dead run id.
    pub fn interrupt_schedule_runs(&self, now: &str) -> Result<usize> {
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            let in_flight = {
                let mut statement = tx.prepare(
                    "SELECT id, task_id FROM investigation_schedule_runs
                     WHERE status IN ('claimed','running')
                     ORDER BY id ASC",
                )?;
                let rows = statement
                    .query_map([], |row| {
                        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                    })?;
                rows.collect::<rusqlite::Result<Vec<_>>>()?
            };
            for (run_id, task_id) in &in_flight {
                let attempt = tx
                    .query_row(
                        "SELECT attempt FROM investigation_runs
                         WHERE id = ?1 AND task_id = ?2",
                        params![run_id, task_id],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()?
                    .unwrap_or(0)
                    .try_into()
                    .unwrap_or(u32::MAX);
                let failure = InvestigationFailure {
                    code: TaskFailureCode::Transport,
                    message: "应用重启时持续巡检运行被标记为中断".into(),
                    retryable: TaskFailureCode::Transport.retryable(),
                    attempt,
                    at: now.to_string(),
                    detail: Some(serde_json::json!({
                        "source": "scheduler",
                        "reason": "application_restarted",
                    })),
                    options: None,
                };
                let failure_json = serde_json::to_string(&failure)?;

                // Do not leave a phantom running/waiting step in the timeline.  The
                // schedule run owns the same id as its durable investigation run.
                tx.execute(
                    "UPDATE investigation_steps
                        SET status = CASE status WHEN 'pending' THEN 'skipped' ELSE 'failed' END,
                            ended_at = ?3,
                            failure = CASE WHEN status = 'pending' THEN NULL ELSE ?4 END
                      WHERE task_id = ?1 AND run_id = ?2
                        AND status IN ('pending','running','waiting_user')",
                    params![task_id, run_id, now, failure_json],
                )?;
                tx.execute(
                    "UPDATE investigation_runs
                        SET status = 'interrupted', updated_at = ?3,
                            ended_at = ?3, failure = ?4
                      WHERE id = ?1 AND task_id = ?2
                        AND status IN ('admitted','running','waiting_user')",
                    params![run_id, task_id, now, failure_json],
                )?;
                // Only clear the fence if this task still points at the interrupted
                // scheduler run. A newer explicit run must retain ownership.
                tx.execute(
                    "UPDATE investigation_tasks
                        SET status = CASE WHEN status = 'stopped' THEN status ELSE 'investigating' END,
                            phase = CASE WHEN status = 'stopped' THEN phase ELSE 'recovery' END,
                            active_run_id = NULL, last_failure = ?3,
                            updated_at = ?4,
                            completed_at = CASE WHEN status = 'stopped' THEN completed_at ELSE NULL END
                      WHERE id = ?1 AND active_run_id = ?2
                        AND status NOT IN ('completed','expired')",
                    params![task_id, run_id, failure_json, now],
                )?;
                tx.execute(
                    "UPDATE investigation_schedule_runs
                        SET status = 'interrupted', finished_at = ?2,
                            outcome = 'interrupted', error = 'application_restarted'
                      WHERE id = ?1 AND status IN ('claimed','running')",
                    params![run_id, now],
                )?;
                tx.execute(
                    "UPDATE investigation_schedules
                        SET last_outcome = 'interrupted', last_error = 'application_restarted',
                            updated_at = ?2
                      WHERE id = (SELECT schedule_id FROM investigation_schedule_runs WHERE id = ?1)",
                    params![run_id, now],
                )?;
            }
            tx.commit()?;
            Ok(in_flight.len())
        })
    }
}
