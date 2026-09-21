use super::*;

impl<'a> InvestigationsRepository<'a> {
    pub fn create_task(&self, task: &InvestigationTask) -> Result<()> {
        let success_criteria = serde_json::to_string(&task.success_criteria)?;
        let scope = serde_json::to_string(&task.scope)?;
        let guardrails = serde_json::to_string(&task.guardrails)?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_tasks (
                    id, workspace_id, server_id, objective, success_criteria, scope, mode,
                    permission_mode, automation_level, created_by, phase, status, max_steps,
                    max_run_ms, max_attempts, created_at, updated_at, completed_at,
                    active_run_id, last_failure, guardrails
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                params![
                    task.id,
                    task.workspace_id,
                    task.server_id,
                    task.objective,
                    success_criteria,
                    scope,
                    task.mode.as_str(),
                    task.permission_mode.as_str(),
                    task.automation_level.as_str(),
                    task.created_by,
                    task.phase.as_str(),
                    task.status.as_str(),
                    i64::from(task.budget.max_steps),
                    i64::try_from(task.budget.max_run_ms).map_err(|_| {
                        DatabaseError::Validation("max_run_ms exceeds SQLite integer range".into())
                    })?,
                    i64::from(task.budget.max_attempts),
                    task.created_at,
                    task.updated_at,
                    task.completed_at,
                    task.active_run_id,
                    task.last_failure.as_ref().map(serde_json::to_string).transpose()?,
                    guardrails,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_task(&self, id: &str) -> Result<InvestigationTask> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![id],
                    row_to_task,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn list_tasks(
        &self,
        status: Option<TaskStatus>,
        limit: usize,
    ) -> Result<Vec<InvestigationTask>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                        permission_mode, automation_level, created_by, phase, status, max_steps,
                        max_run_ms, max_attempts, created_at, updated_at, completed_at,
                        active_run_id, last_failure, guardrails
                 FROM investigation_tasks
                 WHERE (?1 IS NULL OR status = ?1)
                 ORDER BY updated_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(
                params![status.map(|value| value.as_str()), limit as i64],
                row_to_task,
            )?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn update_task_status(
        &self,
        id: &str,
        status: TaskStatus,
        updated_at: &str,
        completed_at: Option<&str>,
    ) -> Result<InvestigationTask> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_tasks
                    SET status = ?2,
                        phase = CASE ?2
                            WHEN 'waiting_user' THEN 'decision'
                            WHEN 'executing' THEN 'execution'
                            WHEN 'verifying' THEN 'verification'
                            WHEN 'completed' THEN 'completed'
                            WHEN 'failed' THEN 'recovery'
                            WHEN 'stopped' THEN 'recovery'
                            WHEN 'expired' THEN 'recovery'
                            ELSE 'investigating'
                        END,
                        updated_at = ?3, completed_at = ?4
                  WHERE id = ?1",
                params![id, status.as_str(), updated_at, completed_at],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                        completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![id],
                    row_to_task,
                )
                .map_err(DatabaseError::from)
        })
    }

    /// Advance a task only if the event still belongs to the run that owns it.
    ///
    /// The caller may have read `active_run_id` immediately before this update, but a
    /// retry/recovery command can replace it in between those two operations.  Keeping the
    /// expected run in the SQL predicate makes the event fence atomic instead of relying on a
    /// best-effort read/check/write sequence.
    pub fn update_task_status_if_active(
        &self,
        id: &str,
        status: TaskStatus,
        updated_at: &str,
        completed_at: Option<&str>,
        expected_run_id: &str,
    ) -> Result<Option<InvestigationTask>> {
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_tasks
                    SET status = ?2,
                        phase = CASE ?2
                            WHEN 'waiting_user' THEN 'decision'
                            WHEN 'executing' THEN 'execution'
                            WHEN 'verifying' THEN 'verification'
                            WHEN 'completed' THEN 'completed'
                            WHEN 'failed' THEN 'recovery'
                            WHEN 'stopped' THEN 'recovery'
                            WHEN 'expired' THEN 'recovery'
                            ELSE 'investigating'
                        END,
                        updated_at = ?3, completed_at = ?4
                  WHERE id = ?1 AND active_run_id = ?5",
                params![
                    id,
                    status.as_str(),
                    updated_at,
                    completed_at,
                    expected_run_id
                ],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![id],
                    row_to_task,
                )
                .map(Some)
                .map_err(DatabaseError::from)
        })
    }

    pub fn update_task_progress(
        &self,
        update: &TaskProgressUpdate<'_>,
    ) -> Result<InvestigationTask> {
        let failure = update.last_failure.map(serde_json::to_string).transpose()?;
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_tasks
                    SET status = ?2, phase = ?3, active_run_id = ?4, last_failure = ?5,
                        updated_at = ?6, completed_at = ?7
                  WHERE id = ?1",
                params![
                    update.id,
                    update.status.as_str(),
                    update.phase.as_str(),
                    update.active_run_id,
                    failure,
                    update.updated_at,
                    update.completed_at
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![update.id],
                    row_to_task,
                )
                .map_err(DatabaseError::from)
        })
    }

    /// Persist run finalisation only while the task still points at that run.
    ///
    /// Returning `None` is an expected stale-event outcome, not a missing task: a newer run or
    /// recovery transaction has already taken ownership of the task.
    pub fn update_task_progress_if_active(
        &self,
        update: &TaskProgressUpdate<'_>,
        expected_run_id: &str,
    ) -> Result<Option<InvestigationTask>> {
        let failure = update.last_failure.map(serde_json::to_string).transpose()?;
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_tasks
                    SET status = ?2, phase = ?3, active_run_id = ?4, last_failure = ?5,
                        updated_at = ?6, completed_at = ?7
                  WHERE id = ?1 AND active_run_id = ?8",
                params![
                    update.id,
                    update.status.as_str(),
                    update.phase.as_str(),
                    update.active_run_id,
                    failure,
                    update.updated_at,
                    update.completed_at,
                    expected_run_id,
                ],
            )?;
            if changed == 0 {
                return Ok(None);
            }
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![update.id],
                    row_to_task,
                )
                .map(Some)
                .map_err(DatabaseError::from)
        })
    }

    pub fn create_run(&self, run: &InvestigationRun) -> Result<()> {
        let checkpoint = run
            .checkpoint
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let failure = run
            .failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_runs (
                    id, task_id, session_id, message_id, trace_id, attempt, phase, status,
                    started_at, updated_at, ended_at, checkpoint, failure
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
                params![
                    run.id,
                    run.task_id,
                    run.session_id,
                    run.message_id,
                    run.trace_id,
                    i64::from(run.attempt),
                    run.phase.as_str(),
                    run.status.as_str(),
                    run.started_at,
                    run.updated_at,
                    run.ended_at,
                    checkpoint,
                    failure,
                ],
            )?;
            Ok(())
        })
    }

    pub fn get_run(&self, id: &str) -> Result<InvestigationRun> {
        self.db.with(|connection| {
            connection
                .query_row(
                    "SELECT id, task_id, session_id, message_id, trace_id, attempt, phase, status,
                            started_at, updated_at, ended_at, checkpoint, failure
                     FROM investigation_runs WHERE id = ?1",
                    params![id],
                    row_to_run,
                )
                .optional()
                .map_err(DatabaseError::from)?
                .ok_or(DatabaseError::NotFound)
        })
    }

    pub fn list_runs(&self, task_id: &str, limit: usize) -> Result<Vec<InvestigationRun>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, session_id, message_id, trace_id, attempt, phase, status,
                        started_at, updated_at, ended_at, checkpoint, failure
                 FROM investigation_runs
                 WHERE task_id = ?1 ORDER BY updated_at DESC, id DESC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_run)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn update_run(&self, run: &InvestigationRun) -> Result<InvestigationRun> {
        let checkpoint = run
            .checkpoint
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let failure = run
            .failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            let changed = connection.execute(
                "UPDATE investigation_runs
                    SET session_id = ?2, message_id = ?3, trace_id = ?4, attempt = ?5,
                        phase = ?6, status = ?7, started_at = ?8, updated_at = ?9,
                        ended_at = ?10, checkpoint = ?11, failure = ?12
                  WHERE id = ?1",
                params![
                    run.id,
                    run.session_id,
                    run.message_id,
                    run.trace_id,
                    i64::from(run.attempt),
                    run.phase.as_str(),
                    run.status.as_str(),
                    run.started_at,
                    run.updated_at,
                    run.ended_at,
                    checkpoint,
                    failure,
                ],
            )?;
            if changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            connection
                .query_row(
                    "SELECT id, task_id, session_id, message_id, trace_id, attempt, phase, status,
                            started_at, updated_at, ended_at, checkpoint, failure
                     FROM investigation_runs WHERE id = ?1",
                    params![run.id],
                    row_to_run,
                )
                .map_err(DatabaseError::from)
        })
    }

    pub fn upsert_step(&self, step: &InvestigationStep) -> Result<()> {
        let target = step
            .target
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let failure = step
            .failure
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        self.db.with(|connection| {
            connection.execute(
                "INSERT INTO investigation_steps (
                    id, task_id, run_id, ordinal, kind, title, status, attempt, tool_name,
                    plan_id, plan_step_id, target, input_summary, output_summary, evidence_ids,
                    started_at, ended_at, failure
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)
                 ON CONFLICT(id) DO UPDATE SET
                    task_id = excluded.task_id, run_id = excluded.run_id, ordinal = excluded.ordinal,
                    kind = excluded.kind, title = excluded.title, status = excluded.status,
                    attempt = excluded.attempt, tool_name = excluded.tool_name,
                    plan_id = excluded.plan_id, plan_step_id = excluded.plan_step_id,
                    target = excluded.target, input_summary = excluded.input_summary,
                    output_summary = excluded.output_summary, evidence_ids = excluded.evidence_ids,
                    started_at = excluded.started_at, ended_at = excluded.ended_at,
                    failure = excluded.failure",
                params![
                    step.id,
                    step.task_id,
                    step.run_id,
                    i64::from(step.ordinal),
                    step.kind.as_str(),
                    step.title,
                    step.status.as_str(),
                    i64::from(step.attempt),
                    step.tool_name,
                    step.plan_id,
                    step.plan_step_id,
                    target,
                    step.input_summary,
                    step.output_summary,
                    serde_json::to_string(&step.evidence_ids)?,
                    step.started_at,
                    step.ended_at,
                    failure,
                ],
            )?;
            Ok(())
        })
    }

    pub fn list_steps(&self, task_id: &str, limit: usize) -> Result<Vec<InvestigationStep>> {
        self.db.with(|connection| {
            let mut statement = connection.prepare(
                "SELECT id, task_id, run_id, ordinal, kind, title, status, attempt, tool_name,
                        plan_id, plan_step_id, target, input_summary, output_summary, evidence_ids,
                        started_at, ended_at, failure
                 FROM investigation_steps
                 WHERE task_id = ?1 ORDER BY ordinal ASC, id ASC LIMIT ?2",
            )?;
            let rows = statement.query_map(params![task_id, limit as i64], row_to_step)?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .map_err(DatabaseError::from)
        })
    }

    pub fn recover_task(
        &self,
        task_id: &str,
        updated_at: &str,
        failure: &InvestigationFailure,
    ) -> Result<InvestigationTask> {
        let failure_json = serde_json::to_string(failure)?;
        self.db.with(|connection| {
            let tx = connection.unchecked_transaction()?;
            // Close every step that was still open when the run was interrupted. A
            // recovered task must not leave a phantom in-flight step in its timeline:
            // active/waiting steps are failed with the durable interruption reason,
            // while steps that had not started are explicitly skipped. Completed and
            // previously failed steps remain immutable audit history.
            tx.execute(
                "UPDATE investigation_steps
                    SET status = CASE status
                                   WHEN 'pending' THEN 'skipped'
                                   ELSE 'failed'
                                 END,
                        ended_at = ?2,
                        failure = CASE
                                    WHEN status = 'pending' THEN NULL
                                    ELSE ?3
                                  END
                  WHERE task_id = ?1
                    AND status IN ('pending','running','waiting_user')
                    AND run_id IN (
                        SELECT id FROM investigation_runs
                         WHERE task_id = ?1 AND status IN ('admitted','running','waiting_user')
                    )",
                params![task_id, updated_at, failure_json],
            )?;
            let changed = tx.execute(
                "UPDATE investigation_runs
                    SET status = 'interrupted', updated_at = ?2, ended_at = ?2, failure = ?3
                  WHERE task_id = ?1 AND status IN ('admitted','running','waiting_user')",
                params![task_id, updated_at, failure_json],
            )?;
            let _ = changed;
            let task_changed = tx.execute(
                "UPDATE investigation_tasks
                    SET status = 'investigating', phase = 'recovery', active_run_id = NULL,
                        last_failure = ?2, updated_at = ?3, completed_at = NULL
                  WHERE id = ?1 AND status NOT IN ('completed','expired')",
                params![task_id, failure_json, updated_at],
            )?;
            if task_changed == 0 {
                return Err(DatabaseError::NotFound);
            }
            tx.commit()?;
            connection
                .query_row(
                    "SELECT id, workspace_id, server_id, objective, success_criteria, scope, mode,
                            permission_mode, automation_level, created_by, phase, status,
                            max_steps, max_run_ms, max_attempts, created_at, updated_at,
                            completed_at, active_run_id, last_failure, guardrails
                     FROM investigation_tasks WHERE id = ?1",
                    params![task_id],
                    row_to_task,
                )
                .map_err(DatabaseError::from)
        })
    }
}
