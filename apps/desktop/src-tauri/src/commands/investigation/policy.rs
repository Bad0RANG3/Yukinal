//! Investigation task policy, validation and prompt projection.
//!
//! Commands use this module as a deep policy seam: input normalization, state transitions,
//! guardrails and bounded prompt instructions live here, while IPC handlers only orchestrate
//! persistence and sidecar calls.

use super::*;

pub(super) fn continuation_for_option(
    brief: &DecisionBrief,
    option_id: &str,
) -> DecisionOptionContinuation {
    brief
        .options
        .iter()
        .find(|option| option.id == option_id)
        .and_then(|option| option.continuation)
        .unwrap_or(DecisionOptionContinuation::WaitUser)
}

pub(super) fn should_approve_selected_plan(continuation: DecisionOptionContinuation) -> bool {
    continuation == DecisionOptionContinuation::StartPlan
}

pub(super) fn validate_create_input(
    state: &AppState,
    input: InvestigationTaskCreateInput,
) -> Result<InvestigationTask, String> {
    let objective = input.objective.trim().to_string();
    if objective.is_empty() || objective.chars().count() > MAX_OBJECTIVE_CHARS {
        return Err(format!(
            "objective must be between 1 and {MAX_OBJECTIVE_CHARS} characters"
        ));
    }
    if input.success_criteria.is_empty() || input.success_criteria.len() > MAX_SUCCESS_CRITERIA {
        return Err(format!(
            "successCriteria must contain 1 to {MAX_SUCCESS_CRITERIA} items"
        ));
    }
    let success_criteria = input
        .success_criteria
        .into_iter()
        .map(|criterion| {
            let criterion = criterion.trim().to_string();
            if criterion.is_empty() || criterion.chars().count() > MAX_SUCCESS_CRITERION_CHARS {
                Err(format!(
                    "each success criterion must be between 1 and {MAX_SUCCESS_CRITERION_CHARS} characters"
                ))
            } else {
                Ok(criterion)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;

    let id = input
        .id
        .map(|value| validate_id(&value, "task id"))
        .transpose()?
        .unwrap_or_else(|| crate::commands::server::next_id("task"));
    let workspace_id = reconcile_reference(
        input.workspace_id,
        input.scope.workspace_id.clone(),
        "workspace",
    )?;
    let server_id = reconcile_reference(input.server_id, input.scope.server_id.clone(), "server")?;

    if input.scope.host == InvestigationTargetHost::Remote && server_id.is_none() {
        return Err("remote investigation tasks require a server scope".into());
    }
    if input.scope.host == InvestigationTargetHost::Local && server_id.is_some() {
        return Err("local investigation tasks cannot carry a server scope".into());
    }
    if let Some(server_id) = server_id.as_deref() {
        if !yukinal_core::ids::is_stable_server_id(server_id) {
            return Err("server scope must be an opaque srv_ id".into());
        }
        state
            .database
            .servers()
            .get(server_id)
            .map_err(|error| format!("server scope is not available: {error}"))?;
    }
    if let Some(workspace_id) = workspace_id.as_deref() {
        state
            .database
            .workspaces()
            .get(workspace_id)
            .map_err(|error| format!("workspace scope is not available: {error}"))?;
    }

    let scope = InvestigationTarget {
        host: input.scope.host,
        server_id,
        workspace_id,
        environment: input.scope.environment,
    };
    let guardrails = validate_guardrails(input.guardrails)?;
    if input.mode == InvestigationRunMode::Readonly
        && input.automation_level != TaskAutomationLevel::Readonly
    {
        return Err("readonly run mode cannot create an executable task".into());
    }
    if input.mode == InvestigationRunMode::Plan
        && input.automation_level == TaskAutomationLevel::Execute
    {
        return Err("plan mode can propose a change but cannot authorize execution".into());
    }
    let budget = input.budget.unwrap_or(TaskBudget {
        max_steps: DEFAULT_MAX_STEPS,
        max_run_ms: DEFAULT_MAX_RUN_MS,
        max_attempts: DEFAULT_MAX_ATTEMPTS,
    });
    if budget.max_steps == 0 || budget.max_steps > 10_000 {
        return Err("budget.maxSteps must be between 1 and 10000".into());
    }
    if budget.max_run_ms == 0 || budget.max_run_ms > 86_400_000 {
        return Err("budget.maxRunMs must be between 1 and 86400000".into());
    }
    if budget.max_attempts == 0 || budget.max_attempts > MAX_ATTEMPTS {
        return Err(format!(
            "budget.maxAttempts must be between 1 and {MAX_ATTEMPTS}"
        ));
    }
    let now = yukinal_core::sidecar::iso8601_now();
    Ok(InvestigationTask {
        id,
        workspace_id: scope.workspace_id.clone(),
        server_id: scope.server_id.clone(),
        objective,
        success_criteria,
        scope,
        guardrails,
        mode: input.mode,
        permission_mode: input.permission_mode,
        automation_level: input.automation_level,
        created_by: "user".into(),
        phase: TaskPhase::Investigating,
        status: TaskStatus::Pending,
        budget,
        created_at: now.clone(),
        updated_at: now,
        completed_at: None,
        active_run_id: None,
        last_failure: None,
    })
}

pub(super) fn autonomous_task_prompt(task: &InvestigationTask) -> String {
    let criteria = task
        .success_criteria
        .iter()
        .enumerate()
        .map(|(index, criterion)| format!("{}. {}", index + 1, criterion))
        .collect::<Vec<_>>()
        .join("\n");
    let mode_instruction = match task.mode {
        InvestigationRunMode::Readonly => {
            "只调用任务范围内已允许的只读工具；不要写入、重启、删除或改变目标状态。"
        }
        InvestigationRunMode::Plan => {
            "先调查并生成宿主可验证的 dry-run 计划；没有用户选择和审批时不要执行行动步骤。"
        }
        InvestigationRunMode::Goal if task.permission_mode == InvestigationPermissionMode::Auto
            && task.automation_level == TaskAutomationLevel::Execute
            && task.scope.host == InvestigationTargetHost::Remote
            && matches!(
                task.scope.environment,
                yukinal_database::models::Environment::Development
                    | yukinal_database::models::Environment::Staging
            ) => {
            "按宿主保存的计划逐步推进；本任务已明确启用受限 auto 委托，medium 风险的配置备份/编辑可在宿主复核后自动推进；备份清理、恢复、重启、包安装及其他高风险或未明确授权的动作必须停下等待用户审批。"
        }
        InvestigationRunMode::Goal => {
            "按宿主保存的计划逐步推进；高风险或未明确授权的动作必须停下等待用户审批。"
        }
    };
    let recovery_instruction = if rollback_requested(task) {
        "\n\n用户选择了“先规划回退”。这不是对旧回退文本或旧工具调用的授权：请只生成一份新的、独立的回退 dry-run 计划，重新校验当前基线、目标范围和精确参数绑定；涉及写入时仍须取得本轮审批，禁止直接重放旧动作。"
    } else if fresh_baseline_required(task) {
        "\n\n这是一次中断后的恢复运行：旧计划审批、旧变更前基线和正在进行的观察窗口已经失效。请先重新校验目标身份、收集新的只读证据并生成/确认新的计划；在新基线和本轮审批完成前不要重放旧行动。"
    } else {
        ""
    };
    let guardrail_instruction = format_guardrails_for_prompt(&task.guardrails);
    format!(
        "你正在自主推进一项由用户创建的可恢复运维任务。\n\n目标：\n{}\n\n完成标准：\n{}\n\n边界：\n{}{}{}\n\n先调用 investigation.plan 声明有序步骤；如果目标属于只读健康检查、受保护配置编辑、容器重启、systemd 服务重启、明确的 apt/dnf 包安装或受限 deploy_sequence，可以调用 investigation.playbook 生成宿主拥有的 dry-run 计划。playbook 只记录计划，不直接执行写入。之后按计划收集证据、记录 Finding 和决策摘要。每个事实都要引用证据；证据不足时明确停在等待用户，而不是猜测已经完成。",
        task.objective, criteria, mode_instruction, recovery_instruction, guardrail_instruction
    )
}

pub(super) fn validate_guardrails(
    raw: Option<InvestigationTaskGuardrails>,
) -> Result<InvestigationTaskGuardrails, String> {
    let mut guardrails = raw.unwrap_or_default();
    guardrails.not_before_at =
        normalize_guardrail_timestamp(guardrails.not_before_at, "notBeforeAt")?;
    guardrails.expires_at = normalize_guardrail_timestamp(guardrails.expires_at, "expiresAt")?;
    if guardrails.forbidden_tools.len() > MAX_GUARDRAIL_TOOLS {
        return Err(format!(
            "guardrails.forbiddenTools must contain at most {MAX_GUARDRAIL_TOOLS} items"
        ));
    }
    let mut tools = Vec::with_capacity(guardrails.forbidden_tools.len());
    for tool in guardrails.forbidden_tools {
        let tool = tool.trim().to_string();
        if tool.is_empty()
            || tool.chars().count() > MAX_GUARDRAIL_NAME_CHARS
            || tool.chars().any(char::is_whitespace)
        {
            return Err(format!(
                "each guardrails.forbiddenTools item must be a non-empty internal tool name of at most {MAX_GUARDRAIL_NAME_CHARS} characters"
            ));
        }
        if !tools.iter().any(|candidate: &String| candidate == &tool) {
            tools.push(tool);
        }
    }
    guardrails.forbidden_tools = tools;

    if guardrails.forbidden_path_prefixes.len() > MAX_GUARDRAIL_PATH_PREFIXES {
        return Err(format!(
            "guardrails.forbiddenPathPrefixes must contain at most {MAX_GUARDRAIL_PATH_PREFIXES} items"
        ));
    }
    let mut prefixes = Vec::with_capacity(guardrails.forbidden_path_prefixes.len());
    for prefix in guardrails.forbidden_path_prefixes {
        let prefix = normalize_guardrail_path_prefix(&prefix)?;
        if !prefixes
            .iter()
            .any(|candidate: &String| candidate == &prefix)
        {
            prefixes.push(prefix);
        }
    }
    guardrails.forbidden_path_prefixes = prefixes;

    if let (Some(not_before), Some(expires)) = (
        guardrails.not_before_at.as_deref(),
        guardrails.expires_at.as_deref(),
    ) {
        let start = yukinal_time::parse_iso8601_utc(not_before)
            .ok_or_else(|| "guardrails.notBeforeAt must be a UTC ISO-8601 timestamp".to_string())?;
        let end = yukinal_time::parse_iso8601_utc(expires)
            .ok_or_else(|| "guardrails.expiresAt must be a UTC ISO-8601 timestamp".to_string())?;
        if end <= start {
            return Err("guardrails.expiresAt must be later than guardrails.notBeforeAt".into());
        }
        if end.saturating_sub(start) > MAX_GUARDRAIL_WINDOW_SECONDS {
            return Err("guardrails time window cannot exceed 365 days".into());
        }
    }
    Ok(guardrails)
}

pub(super) fn normalize_guardrail_timestamp(
    value: Option<String>,
    field: &str,
) -> Result<Option<String>, String> {
    let Some(value) = value else { return Ok(None) };
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err(format!("guardrails.{field} cannot be empty"));
    }
    if yukinal_time::parse_iso8601_utc(&value).is_none() {
        return Err(format!(
            "guardrails.{field} must be a UTC ISO-8601 timestamp"
        ));
    }
    Ok(Some(value))
}

pub(super) fn normalize_guardrail_path_prefix(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > MAX_GUARDRAIL_PATH_CHARS {
        return Err(format!(
            "each guardrails.forbiddenPathPrefixes item must be 1-{MAX_GUARDRAIL_PATH_CHARS} characters"
        ));
    }
    if !value.starts_with('/') {
        return Err("guardrails.forbiddenPathPrefixes items must be absolute paths".into());
    }
    if value
        .bytes()
        .any(|byte| byte == 0 || byte == b'\r' || byte == b'\n')
    {
        return Err("guardrails.forbiddenPathPrefixes cannot contain control characters".into());
    }
    let mut parts = Vec::new();
    for part in value.split('/') {
        match part {
            "" | "." => {}
            ".." => return Err("guardrails.forbiddenPathPrefixes cannot contain `..`".into()),
            value => parts.push(value),
        }
    }
    if parts.is_empty() {
        Ok("/".into())
    } else {
        Ok(format!("/{}", parts.join("/")))
    }
}

pub(crate) fn task_time_window_error(task: &InvestigationTask) -> Result<(), String> {
    let now = yukinal_time::now_epoch_seconds();
    if let Some(not_before) = task.guardrails.not_before_at.as_deref() {
        let Some(not_before) = yukinal_time::parse_iso8601_utc(not_before) else {
            return Err(
                "task guardrail notBeforeAt is invalid; task is blocked until it is repaired"
                    .into(),
            );
        };
        if now < not_before {
            return Err(format!(
                "task execution is not allowed before {}",
                task.guardrails.not_before_at.as_deref().unwrap_or_default()
            ));
        }
    }
    if let Some(expires) = task.guardrails.expires_at.as_deref() {
        let Some(expires) = yukinal_time::parse_iso8601_utc(expires) else {
            return Err(
                "task guardrail expiresAt is invalid; task is blocked until it is repaired".into(),
            );
        };
        if now >= expires {
            return Err(format!(
                "task execution time window expired at {}",
                task.guardrails.expires_at.as_deref().unwrap_or_default()
            ));
        }
    }
    Ok(())
}

pub(super) fn format_guardrails_for_prompt(guardrails: &InvestigationTaskGuardrails) -> String {
    let mut lines = Vec::new();
    if let Some(not_before) = guardrails.not_before_at.as_deref() {
        lines.push(format!("不得早于 {not_before}"));
    }
    if let Some(expires) = guardrails.expires_at.as_deref() {
        lines.push(format!("不得晚于 {expires}"));
    }
    if !guardrails.forbidden_tools.is_empty() {
        lines.push(format!(
            "禁止工具：{}",
            guardrails.forbidden_tools.join(", ")
        ));
    }
    if !guardrails.forbidden_path_prefixes.is_empty() {
        lines.push(format!(
            "禁止路径前缀：{}",
            guardrails.forbidden_path_prefixes.join(", ")
        ));
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!(
            "\n\n宿主硬边界（不能通过提示词或重试绕过）：{}。",
            lines.join("；")
        )
    }
}

pub(super) fn rollback_requested(task: &InvestigationTask) -> bool {
    task.last_failure
        .as_ref()
        .and_then(|failure| failure.detail.as_ref())
        .and_then(Value::as_object)
        .and_then(|detail| detail.get("rollbackRequested"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(super) fn fresh_baseline_required(task: &InvestigationTask) -> bool {
    task.last_failure
        .as_ref()
        .and_then(|failure| failure.detail.as_ref())
        .and_then(Value::as_object)
        .and_then(|detail| detail.get("requiresFreshBaseline"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(super) fn validate_scheduler_task(task: &InvestigationTask) -> Result<(), String> {
    if task.mode != InvestigationRunMode::Readonly
        || task.automation_level != TaskAutomationLevel::Readonly
    {
        return Err("scheduler triggers can only target read-only tasks".into());
    }
    if matches!(
        task.status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired
    ) {
        return Err("terminal investigation tasks cannot be scheduled".into());
    }
    Ok(())
}

pub(super) fn schedule_update_requires_live_task(
    status: Option<InvestigationScheduleStatus>,
) -> bool {
    !matches!(status, Some(InvestigationScheduleStatus::Revoked))
}

pub(super) fn validate_schedule_input(schedule: &InvestigationSchedule) -> Result<(), String> {
    if schedule.interval_seconds == 0 || schedule.interval_seconds > 86_400 {
        return Err("intervalSeconds must be between 1 and 86400".into());
    }
    if schedule.cooldown_seconds > 86_400 {
        return Err("cooldownSeconds must be at most 86400".into());
    }
    if schedule.dedupe_window_seconds == 0 || schedule.dedupe_window_seconds > 86_400 {
        return Err("dedupeWindowSeconds must be between 1 and 86400".into());
    }
    if schedule.max_concurrent_runs == 0 || schedule.max_concurrent_runs > 16 {
        return Err("maxConcurrentRuns must be between 1 and 16".into());
    }
    if schedule.budget.max_steps == 0
        || schedule.budget.max_steps > 10_000
        || schedule.budget.max_run_ms == 0
        || schedule.budget.max_run_ms > 86_400_000
        || schedule.budget.max_attempts == 0
        || schedule.budget.max_attempts > MAX_ATTEMPTS
    {
        return Err("schedule budget is outside the supported bounds".into());
    }
    if yukinal_time::parse_iso8601_utc(&schedule.next_run_at).is_none() {
        return Err("nextRunAt must be a UTC timestamp".into());
    }
    Ok(())
}

pub(super) fn reconcile_reference(
    explicit: Option<String>,
    scoped: Option<String>,
    label: &str,
) -> Result<Option<String>, String> {
    match (explicit, scoped) {
        (Some(explicit), Some(scoped)) if explicit != scoped => {
            Err(format!("{label} id disagrees with the task scope"))
        }
        (Some(value), _) | (_, Some(value)) => Ok(Some(validate_id(&value, label)?)),
        (None, None) => Ok(None),
    }
}

pub(super) fn validate_id(value: &str, label: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 256 || value.chars().any(char::is_control) {
        return Err(format!(
            "{label} must be between 1 and 256 visible characters"
        ));
    }
    Ok(value.to_string())
}

pub(super) fn validate_timestamp(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > 80 || value.chars().any(char::is_control) {
        return Err("completedAt must be a bounded timestamp".into());
    }
    Ok(value.to_string())
}

pub(super) fn retention_cutoff(value: Option<String>) -> Result<String, String> {
    let cutoff = value.unwrap_or_else(|| {
        let now = yukinal_time::now_epoch_seconds();
        yukinal_time::iso8601_utc(now.saturating_sub(DEFAULT_RETENTION_DAYS * 86_400))
    });
    let cutoff = validate_timestamp(&cutoff)?;
    if yukinal_time::parse_iso8601_utc(&cutoff).is_none() {
        return Err("retention cutoff must be a UTC ISO-8601 timestamp".into());
    }
    Ok(cutoff)
}

pub(super) fn parse_retention_kind(value: &str) -> Result<InvestigationRetentionKind, String> {
    match value.trim() {
        "evidence" => Ok(InvestigationRetentionKind::Evidence),
        "artifact" => Ok(InvestigationRetentionKind::Artifact),
        _ => Err("retention item kind must be evidence or artifact".into()),
    }
}

pub(super) fn retention_item_response(
    item: yukinal_database::repositories::InvestigationRetentionItem,
) -> InvestigationRetentionItemResponse {
    InvestigationRetentionItemResponse {
        id: item.id,
        task_id: item.task_id,
        kind: item.kind.as_str().into(),
        created_at: item.created_at,
        bytes: item.bytes,
        reason: item.reason,
    }
}

pub(super) fn retention_skip_response(
    item: yukinal_database::repositories::InvestigationRetentionSkip,
) -> InvestigationRetentionSkipResponse {
    InvestigationRetentionSkipResponse {
        id: item.id,
        kind: item.kind.as_str().into(),
        reason: item.reason,
    }
}

pub(super) fn is_terminal(status: TaskStatus) -> bool {
    matches!(
        status,
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired
    )
}

pub(crate) fn can_transition(from: TaskStatus, to: TaskStatus) -> bool {
    if from == to {
        return true;
    }
    match from {
        TaskStatus::Pending => matches!(
            to,
            TaskStatus::Investigating | TaskStatus::Stopped | TaskStatus::Expired
        ),
        TaskStatus::Investigating => matches!(
            to,
            TaskStatus::WaitingUser
                | TaskStatus::Executing
                | TaskStatus::Verifying
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Stopped
                | TaskStatus::Expired
        ),
        TaskStatus::WaitingUser => matches!(
            to,
            TaskStatus::Investigating
                | TaskStatus::Executing
                | TaskStatus::Stopped
                | TaskStatus::Expired
        ),
        TaskStatus::Executing => matches!(
            to,
            TaskStatus::Verifying | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired
        ),
        TaskStatus::Verifying => matches!(
            to,
            TaskStatus::WaitingUser
                | TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Stopped
                | TaskStatus::Expired
        ),
        TaskStatus::Completed | TaskStatus::Failed | TaskStatus::Stopped | TaskStatus::Expired => {
            false
        }
    }
}
