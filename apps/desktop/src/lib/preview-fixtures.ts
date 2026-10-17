/**
 * Isolated in-memory data for desktop and browser visual evaluation.
 *
 * It is reachable only from Vite development with `?fixture=1`.  Responses
 * still traverse the regular Zod IPC schema. Infrastructure stays in memory;
 * native preview chat is separately allow-listed in preview-chat.ts and has
 * no tool access. Browser fixtures never invoke native commands. Reloading
 * resets mock infrastructure, while native chat history remains durable.
 */

import {
  IPC_COMMANDS,
  type Activity,
  type AiProviderConfig,
  type InvestigationSchedule,
  type InvestigationScheduleRun,
  type InvestigationTaskDetailResponse,
  type IpcCommandName,
  type McpServerView,
  type ProviderModelOption,
  type Server,
  type TransferSnapshot,
} from "@yukinal/shared";
import { previewMemoryResponse } from "./preview-memory.js";

const AT = new Date().toISOString();
const FINGERPRINT = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";

const SERVERS: Server[] = [
  {
    id: "srv_previewapi",
    name: "Production API",
    connection: { host: "api.example.com", port: 22, username: "deploy" },
    capabilities: { linux: true, docker: true, systemd: true, nginx: true },
    status: "connected",
    metadata: { environment: "production", region: "Singapore", tags: ["critical", "api"] },
    createdAt: AT,
    updatedAt: AT,
  },
  {
    id: "srv_previewdb",
    name: "Staging Database Cluster",
    connection: { host: "staging-db.internal.example.net", port: 2222, username: "ops" },
    capabilities: { linux: true, docker: true, systemd: true, postgres: true },
    status: "connected",
    metadata: { environment: "staging", region: "Tokyo", tags: ["database", "canary"] },
    createdAt: AT,
    updatedAt: AT,
  },
  {
    id: "srv_previewrunner",
    name: "Local Build Runner",
    connection: { host: "runner.dev.example.net", port: 22, username: "builder" },
    capabilities: { linux: true, docker: true },
    status: "disconnected",
    metadata: { environment: "development", region: "Shanghai", tags: ["ci"] },
    createdAt: AT,
    updatedAt: AT,
  },
  {
    id: "srv_previewanalytics",
    name: "Analytics Worker — Long Environment Name",
    connection: { host: "analytics-worker-01.eu.example.net", port: 22, username: "readonly" },
    capabilities: { linux: true, systemd: true, redis: true },
    status: "error",
    metadata: { environment: "unknown", region: "Frankfurt", tags: ["needs-attention"] },
    createdAt: AT,
    updatedAt: AT,
  },
  {
    id: "srv_previewedge",
    name: "边缘代理 · 东京",
    connection: { host: "edge-tokyo-01.example.net", port: 22, username: "ops" },
    capabilities: { linux: true, nginx: true, systemd: true },
    status: "connecting",
    metadata: { environment: "production", region: "Tokyo", tags: ["edge"] },
    createdAt: AT,
    updatedAt: AT,
  },
  {
    id: "srv_previewinference",
    name: "本地模型推理节点",
    connection: { host: "inference.local.example.net", port: 2202, username: "developer" },
    capabilities: { linux: true, docker: true, redis: true },
    status: "connected",
    metadata: { environment: "local", region: "Local", tags: ["gpu", "inference"] },
    createdAt: AT,
    updatedAt: AT,
  },
];

const MODELS: ProviderModelOption[] = [
  { id: "preview-model", label: "运维推理 · 长上下文与工具调用评估模型", contextWindow: 200000, supportsToolCalling: true, supportsStreaming: true },
  { id: "preview-code-reasoning-128k", label: "Code Reasoning — Infrastructure Diagnostics / Extended Context", contextWindow: 131072, supportsToolCalling: true, supportsStreaming: true },
  { id: "preview-text-only", label: "文本摘要模型（不支持工具调用）", contextWindow: 32768, supportsToolCalling: false, supportsStreaming: true },
];

const PROVIDERS: AiProviderConfig[] = [
  {
    id: "prv_preview",
    kind: "openai-compatible",
    label: "Preview Gateway · 平台运维与可观测性共享网关",
    baseUrl: "https://preview.invalid/v1",
    model: "preview-model",
    apiKeyCredentialRef: "keychain://preview/no-secret",
    enabled: true,
    wireApi: "chat",
    models: MODELS,
    createdAt: AT,
    updatedAt: AT,
  },
  {
    id: "prv_preview_local",
    kind: "openai-compatible",
    label: "本机推理 · LM Studio / 开发环境长名称示例",
    baseUrl: "http://127.0.0.1:1234/v1",
    model: "preview-local-reasoning-32b",
    enabled: true,
    wireApi: "chat",
    models: [{ id: "preview-local-reasoning-32b", label: "Local Reasoning 32B · 只读排查与配置分析", contextWindow: 65536, supportsToolCalling: true, supportsStreaming: true }],
    createdAt: AT,
    updatedAt: AT,
  },
  {
    id: "prv_preview_deepseek",
    kind: "openai-compatible",
    label: "DeepSeek · 模拟目录",
    baseUrl: "https://api.deepseek.com",
    model: "deepseek-flash",
    enabled: true,
    wireApi: "chat",
    models: [
      { id: "deepseek-flash", label: "DeepSeek Flash", supportsToolCalling: true, supportsStreaming: true },
      { id: "deepseek-v4-pro", label: "DeepSeek V4 Pro", supportsToolCalling: true, supportsStreaming: true },
    ],
    createdAt: AT,
    updatedAt: AT,
  },
  {
    id: "prv_preview_disabled",
    kind: "anthropic",
    label: "备用分析网关（已停用，仅供配置布局评估）",
    baseUrl: "https://standby.preview.invalid",
    model: "preview-analysis",
    enabled: false,
    apiVersion: "2023-06-01",
    createdAt: AT,
    updatedAt: AT,
  },
];

const TASK: InvestigationTaskDetailResponse["task"] = {
  id: "task_preview",
  workspaceId: "ws_preview",
  serverId: "srv_previewdb",
  objective: "确认 Staging Database Cluster 在批量导入与数据库迁移并行时的 API 延迟升高原因，并整理可复核的只读排查结论",
  successCriteria: ["确认最近的延迟变化与连接池等待时间", "检查服务、迁移任务及应用日志是否有共同异常", "保留证据与观察基线，不修改文件、不重启服务"],
  scope: { host: "remote", serverId: "srv_previewdb", environment: "staging" },
  guardrails: { forbiddenTools: ["filesystem.write", "docker.restart"], forbiddenPathPrefixes: ["/etc/production"] },
  mode: "readonly",
  permissionMode: "ask",
  automationLevel: "readonly",
  createdBy: "browser-fixture",
  phase: "decision",
  status: "waiting_user",
  budget: { maxSteps: 12, maxRunMs: 300000, maxAttempts: 2 },
  createdAt: AT,
  updatedAt: AT,
};

const TASK_DETAIL: InvestigationTaskDetailResponse = {
  task: TASK,
  evidence: [
    {
      id: "evidence_preview_logs", taskId: TASK.id, runId: "run_preview", scope: TASK.scope, kind: "log", sourceTool: "server.logs.read", collectedAt: AT,
      inputSummary: "journalctl · staging-checkout-api-with-background-migration.service · 最近 120 行，已移除敏感字段",
      contentType: "text", content: "[fixture] request latency p95=1840ms\nconnection_pool.wait_ms=1230 active=48 idle=0 max=48\nmigration-worker: relation orders_2026 already exists\n仅用于布局评估；这些内容并非真实服务器读数。",
      contentHash: "a".repeat(64), truncated: false, redactionStatus: "clean",
      freshness: { status: "fresh", policy: "default-v1", evaluatedAt: AT, ageSeconds: 0, staleAfterSeconds: 300, expiresAfterSeconds: 3600 },
    },
    {
      id: "evidence_preview_snapshot", taskId: TASK.id, runId: "run_preview", scope: TASK.scope, kind: "snapshot", sourceTool: "server.health", collectedAt: AT,
      inputSummary: "Staging 数据库节点 · CPU / 内存 / PostgreSQL 数据盘 / 迁移容器状态",
      contentType: "json", content: { fixture: true, cpuPercent: 67.4, memoryPercent: 82, dataDiskPercent: 92, mountPoint: "/var/lib/postgresql/data/analytics-and-checkout-history", migrationState: "exited" },
      contentHash: "b".repeat(64), truncated: false, redactionStatus: "clean",
    },
  ],
  findings: [
    { id: "finding_preview_pool", taskId: TASK.id, title: "连接池等待与延迟升高发生在同一观察窗口", kind: "fact", statement: "模拟日志显示 p95 为 1840ms，连接池 48 个连接全部占用，同时出现 1230ms 的等待。它证明存在资源等待，尚不足以断言单一根因。", evidenceIds: ["evidence_preview_logs"], confidence: "high", nextVerification: "继续只读比较迁移任务失败前后的连接持有时间与应用请求量。", createdAt: AT },
    { id: "finding_preview_migration", taskId: TASK.id, title: "迁移工作进程反复退出可能延长数据库连接占用", kind: "inference", statement: "迁移容器有重复退出记录，数据盘使用率达到 92%。需先核对事务锁与重试策略，再决定是否处理迁移任务。", evidenceIds: ["evidence_preview_logs", "evidence_preview_snapshot"], confidence: "medium", nextVerification: "核对只读事务视图与最近迁移记录；不尝试删除关系或重启数据库。", createdAt: AT },
  ],
  decisionBrief: {
    id: "brief_preview", taskId: TASK.id, planId: "plan_preview", generatedAt: AT, status: "draft", findingIds: ["finding_preview_pool", "finding_preview_migration"],
    options: [
      { id: "option_preview_observe", title: "保持现状，继续对比连接池与迁移任务的只读证据", summary: "补充 15 分钟观察，比较事务等待、连接持有时间和迁移进程日志。", impact: "仅增加有限的状态读取，不改变服务配置和远端文件。", riskLevel: "read", evidenceIds: ["evidence_preview_logs"], findingIds: ["finding_preview_pool"], verification: "确认新样本能区分请求量增加与长事务占用；保存观察结果供人工复核。", requiresApproval: false, status: "available", continuation: "continue_readonly" },
      { id: "option_preview_plan", title: "先整理迁移恢复与连接池调整的变更计划", summary: "形成包含前置检查、验证标准和回滚步骤的提案，等待单独审批。", impact: "本选项只形成计划；任何数据库或配置变更仍需人工确认。", riskLevel: "medium", evidenceIds: ["evidence_preview_snapshot"], findingIds: ["finding_preview_migration"], preview: "检查迁移重复执行保护 → 确认事务锁来源 → 评估连接池上限 → 记录验证与回滚步骤。", verification: "计划中的每一步都能关联对应证据，并具备明确的成功与停止条件。", rollback: "计划阶段不执行变更；后续变更需使用已批准的回滚方案。", requiresApproval: true, status: "available", continuation: "start_plan" },
    ],
  },
  plan: {
    id: "plan_preview", taskId: TASK.id, revision: 2, status: "active", createdAt: AT, updatedAt: AT, currentStepId: "plan_step_preview_decision", approval: { status: "pending", note: "只读观察完成后等待选择下一步。" },
    observationWindow: { durationSeconds: 900, intervalSeconds: 300, allowedTools: ["server.health", "server.logs.read"], successCriteria: ["完成三个相同范围的采样，保留连接池等待与数据盘读数"], status: "succeeded", sampleCount: 3, startedAt: AT, lastSampleAt: AT },
    steps: [
      { id: "plan_step_preview_evidence", ordinal: 0, kind: "evidence", title: "读取健康快照及 staging-checkout-api-with-background-migration.service 最近日志", purpose: "确认异常对应的目标与采样时间。", allowedTools: ["server.health", "server.logs.read"], evidenceIds: ["evidence_preview_logs", "evidence_preview_snapshot"], successCriteria: ["证据来自同一台 Staging 数据库节点"], requiresApproval: false, maxAttempts: 2, attempts: 1, status: "succeeded" },
      { id: "plan_step_preview_decision", ordinal: 1, kind: "decision", title: "比较观察结果并等待用户选择后续处理范围", purpose: "将已证实事实与尚待验证推测分开。", allowedTools: [], evidenceIds: ["evidence_preview_logs"], successCriteria: ["提供影响、风险与验证方式"], requiresApproval: false, maxAttempts: 1, attempts: 0, status: "pending" },
    ],
  },
  artifacts: [
    { id: "artifact_preview_baseline", taskId: TASK.id, runId: "run_preview", planId: "plan_preview", phase: "investigating", kind: "baseline", status: "ready", title: "批量导入期间的数据库节点资源与服务基线", summary: "模拟基线包含 CPU、内存、PostgreSQL 数据盘和迁移容器状态，用于检查长标题及嵌套内容在窄面板中的表现。", content: { fixture: true, observation: "只读采集", connectionPool: { active: 48, idle: 0 }, dataDisk: "/var/lib/postgresql/data/analytics-and-checkout-history" }, evidenceIds: ["evidence_preview_snapshot"], createdAt: AT, updatedAt: AT },
  ],
  runs: [
    { id: "run_preview_retry", taskId: TASK.id, attempt: 1, phase: "investigating", status: "failed", startedAt: AT, updatedAt: AT, endedAt: AT, failure: { code: "timeout", message: "模拟首次日志读取超过时间预算；没有执行任何远端变更。", retryable: true, attempt: 1, at: AT } },
    { id: "run_preview", taskId: TASK.id, sessionId: "ses_preview_triage", traceId: "trace_preview", attempt: 2, phase: "decision", status: "waiting_user", startedAt: AT, updatedAt: AT },
  ],
  steps: [
    { id: "step_preview_timeout", taskId: TASK.id, runId: "run_preview_retry", ordinal: 0, kind: "evidence", title: "读取 staging-checkout-api-with-background-migration.service 的历史日志超时", status: "failed", attempt: 1, toolName: "server.logs.read", inputSummary: "最近 500 行；只读请求", outputSummary: "模拟请求超时，未改变服务器。", evidenceIds: [], startedAt: AT, endedAt: AT, failure: { code: "timeout", message: "日志范围较大，重试时缩小为最近 120 行。", retryable: true, attempt: 1, at: AT } },
    { id: "step_preview_collect", taskId: TASK.id, runId: "run_preview", ordinal: 1, kind: "evidence", title: "缩小范围后读取服务日志与数据库节点快照", status: "succeeded", attempt: 2, toolName: "server.logs.read", inputSummary: "{\"unit\":\"staging-checkout-api-with-background-migration.service\",\"lines\":120}", outputSummary: "[fixture] p95=1840ms\n连接池 active=48 / idle=0 / max=48\n迁移工作进程退出，数据盘使用率 92%。\n这些数据仅供界面评估。", evidenceIds: ["evidence_preview_logs", "evidence_preview_snapshot"], startedAt: AT, endedAt: AT },
    { id: "step_preview_decision", taskId: TASK.id, runId: "run_preview", ordinal: 2, kind: "decision", title: "整理已有事实和验证缺口，等待选择后续只读观察或计划范围", status: "waiting_user", attempt: 1, evidenceIds: ["evidence_preview_logs", "evidence_preview_snapshot"], startedAt: AT },
  ],
};

const SCHEDULES: InvestigationSchedule[] = [
  { id: "schedule_preview", taskId: TASK.id, status: "active", intervalSeconds: 300, cooldownSeconds: 60, dedupeWindowSeconds: 300, maxConcurrentRuns: 1, budget: TASK.budget, notificationPolicy: "on_change", nextRunAt: AT, baselineRunId: "schedule_run_preview_baseline", lastRunAt: AT, lastOutcome: "连接池等待仍高于基线，资源状态暂未继续恶化；已保留只读观察结果。", createdAt: AT, updatedAt: AT },
  { id: "schedule_preview_paused", taskId: TASK.id, status: "paused", intervalSeconds: 900, cooldownSeconds: 300, dedupeWindowSeconds: 900, maxConcurrentRuns: 1, budget: TASK.budget, notificationPolicy: "failed_runs_only", nextRunAt: AT, lastRunAt: AT, lastError: "模拟历史错误：节点状态读取超时，等待人工恢复巡检；不会自动连接真实服务器。", createdAt: AT, updatedAt: AT },
];

function scheduleRuns(scheduleId: string): InvestigationScheduleRun[] {
  if (!SCHEDULES.some((schedule) => schedule.id === scheduleId)) throw new Error("找不到这项模拟巡检。");
  return [
    { id: "schedule_run_preview_baseline", scheduleId, taskId: TASK.id, status: "succeeded", scheduledAt: AT, startedAt: AT, finishedAt: AT, dedupeKey: "preview-baseline", outcome: "已保存连接池与数据盘观察基线。" },
    { id: "schedule_run_preview_failed", scheduleId, taskId: TASK.id, status: "failed", scheduledAt: AT, startedAt: AT, finishedAt: AT, dedupeKey: "preview-timeout", error: "模拟状态读取超过预算；未执行任何变更，下一次读取仍遵守只读范围。" },
    { id: "schedule_run_preview_latest", scheduleId, taskId: TASK.id, status: "succeeded", scheduledAt: AT, startedAt: AT, finishedAt: AT, dedupeKey: "preview-latest", outcome: "采样已恢复，延迟仍偏高；等待选择下一步范围。" },
  ];
}

const MCP_SERVERS: McpServerView[] = [
  {
    config: { id: "mcp_preview_observability", label: "Observability Toolkit · 日志检索与跨服务关联分析（模拟）", transport: "stdio", command: "preview-observability-toolkit", args: ["--workspace", "platform-observability-readonly"], httpAuthHeaders: [], enabled: true, allowedTools: ["mcp.preview_observability.search_service_logs"], trustLevel: "reviewed", annotationTrust: "trusted" },
    status: { serverId: "mcp_preview_observability", running: true, pid: 4242, program: "preview-observability-toolkit", startedAt: AT, protocolVersion: "2025-03-26", serverName: "Platform Observability Toolkit", serverVersion: "2.4.0-preview", toolCount: 3, lastExit: null, stderrTail: [], diagnostics: ["模拟目录：日志与服务状态仅供界面评估。"] },
    tools: [
      { name: "mcp.preview_observability.search_service_logs", remoteName: "search_service_logs", description: "在指定服务器及时间窗内检索服务日志，返回经过脱敏的匹配片段、时间戳和来源。只读，不修改日志、服务或远端配置。", inputSchema: { type: "object", properties: { service: { type: "string" }, query: { type: "string" }, limit: { type: "integer", maximum: 120 } }, required: ["service", "query"] }, annotations: { readOnlyHint: true, destructiveHint: false, idempotentHint: true, openWorldHint: false } },
      { name: "mcp.preview_observability.correlate_database_connection_pool_with_recent_deployments", remoteName: "correlate_database_connection_pool_with_recent_deployments", description: "把连接池等待、事务锁与最近部署事件放到同一时间线上。工具名与描述故意保留较长内容，用于检查授权选项在当前窗口中的换行。", inputSchema: { type: "object", properties: { serverId: { type: "string" }, windowMinutes: { type: "integer" } }, required: ["serverId"] }, annotations: { readOnlyHint: true, destructiveHint: false } },
      { name: "mcp.preview_observability.export_incident_summary", remoteName: "export_incident_summary", description: "根据已选证据生成事件摘要；目录显示仅用于评估，模拟模式不会创建文件或发送任何请求。", inputSchema: { type: "object", properties: { evidenceIds: { type: "array", items: { type: "string" } } }, required: ["evidenceIds"] }, annotations: { readOnlyHint: false, destructiveHint: false } },
    ],
  },
  {
    config: { id: "mcp_preview_remote", label: "Remote Runbooks · 运维知识库与团队工作手册（待授权）", transport: "http", url: "https://runbooks.preview.invalid/teams/platform-observability/mcp", httpAuthHeaders: [], oauth: { issuer: "https://identity.preview.invalid/organizations/platform-observability", clientId: "preview-desktop-client", flow: "device_code", clientAuth: "none", dpop: true, scopes: ["runbooks.read", "incidents.read", "service-catalog.read"] }, enabled: true, allowedTools: [], trustLevel: "unreviewed", annotationTrust: "none" },
    status: { serverId: "mcp_preview_remote", running: false, pid: null, program: null, toolCount: 0, lastExit: null, stderrTail: [], diagnostics: ["模拟连接状态：需要重新完成知识库授权。"] },
    tools: [], unavailable: { code: "request_failed", message: "模拟端点尚未授权，无法读取团队工作手册目录。此预览不会启动 OAuth 或访问网络。" },
  },
  {
    config: { id: "mcp_preview_stopped", label: "Database Diagnostics · 数据库只读诊断（进程退出示例）", transport: "stdio", command: "preview-database-diagnostics", args: ["--readonly"], httpAuthHeaders: [], enabled: false, allowedTools: [], trustLevel: "unreviewed", annotationTrust: "none" },
    status: { serverId: "mcp_preview_stopped", running: false, pid: null, program: "preview-database-diagnostics", toolCount: 0, lastExit: { code: 1, at: AT, reason: "模拟诊断进程读取配置失败后退出；不会自动重新启动。" }, restart: { attempt: 3, maxAttempts: 3, exhausted: true, at: AT }, stderrTail: ["[fixture] configuration file not found: /srv/platform-observability/database-diagnostics/readonly-profile.json"], diagnostics: ["检查配置后可以手动启动；此按钮在模拟模式中仅显示明确的不可执行反馈。"] },
    tools: [], unavailable: { code: "disabled", message: "此模拟连接已禁用。" },
  },
];

const PREVIEW_CHAT_SESSION = {
  id: "ses_preview_triage",
  workspaceId: "ws_preview",
  serverId: "srv_previewdb",
  title: "排查 Staging API 延迟升高",
  createdAt: "2026-09-21T07:53:00.000Z",
  updatedAt: "2026-09-21T07:56:00.000Z",
  messageCount: 4,
  lastMessagePreview: "建议先核对迁移工作进程失败原因，再评估是否调整连接池。",
};

const PREVIEW_CHAT_MESSAGES = [
  {
    id: "msg_preview_user_1",
    sessionId: PREVIEW_CHAT_SESSION.id,
    role: "user",
    content: "请只读排查 Staging API 的 p95 延迟升高，不执行任何修改。",
    createdAt: "2026-09-21T07:53:10.000Z",
  },
  {
    id: "msg_preview_assistant_1",
    sessionId: PREVIEW_CHAT_SESSION.id,
    role: "assistant",
    participant: { id: "blue-logs", label: "蓝队 · 日志分析", kind: "agent" },
    model: "preview-code-reasoning-128k",
    content: "我会先读取服务状态和最近日志；不会重启容器、修改配置或写入文件。",
    createdAt: "2026-09-21T07:53:14.000Z",
  },
  {
    id: "msg_preview_user_2",
    sessionId: PREVIEW_CHAT_SESSION.id,
    role: "user",
    content: "请总结目前能确认的事实和下一步只读建议。",
    createdAt: "2026-09-21T07:55:10.000Z",
  },
  {
    id: "msg_preview_assistant_2",
    sessionId: PREVIEW_CHAT_SESSION.id,
    role: "assistant",
    participant: { id: "green-observability", label: "绿队 · 可观测性共建", kind: "agent" },
    model: "preview-code-reasoning-128k",
    content: [
      "下面是模拟排查结果，仅供界面评估。连接池等待与 p95 1840ms 同时出现，迁移工作进程在 9 分钟前失败。",
      "",
      "| 观察项 | 结果 | 下一步核对 |",
      "| --- | --- | --- |",
      "| API p95 延迟 | 1840ms | 比较批量导入前后的请求量与连接池等待时间 |",
      "| 数据库连接池 | active=48，idle=0 | 核对长事务与迁移进程持有连接的时间 |",
      "| PostgreSQL 数据盘 | 92% | 检查 /var/lib/postgresql/data/analytics-and-checkout-history 的增长来源 |",
      "",
      "只读检查命令示例：",
      "",
      "```sh",
      "journalctl --unit staging-checkout-api-with-background-migration.service --since '15 minutes ago' --lines 120 --no-pager",
      "systemctl show staging-checkout-api-with-background-migration.service --property ActiveState,SubState,ExecMainStatus",
      "```",
      "",
      "建议先核对迁移工作进程失败日志与连接持有时间。当前证据不足以批准配置变更，也没有执行文件写入、服务重启或数据库迁移。",
    ].join("\n"),
    createdAt: "2026-09-21T07:56:00.000Z",
  },
];

const PREVIEW_TRANSFER: TransferSnapshot = {
  transferId: "transfer_preview",
  serverId: "srv_previewdb",
  direction: "upload",
  status: "completed",
  startedAtEpochMs: Date.parse(AT),
  updatedAtEpochMs: Date.parse(AT),
  totalFiles: 1,
  completedFiles: 1,
  skippedFiles: 0,
  totalBytes: 8192,
  transferredBytes: 8192,
  currentItem: null,
  currentItemBytes: 8192,
  currentItemTotalBytes: 8192,
  activeConflict: null,
  verifiedFiles: 1,
  unverifiedFiles: 0,
  failures: [],
  stagingResidue: [],
};

const TRANSFERS: TransferSnapshot[] = [
  { ...PREVIEW_TRANSFER, totalFiles: 24, completedFiles: 24, totalBytes: 47185920, transferredBytes: 47185920, verifiedFiles: 24 },
  { ...PREVIEW_TRANSFER, transferId: "transfer_preview_failed", direction: "download", status: "failed", totalFiles: 3, completedFiles: 2, currentItem: "/srv/platform-observability/incidents/2026-10-16/database-connection-pool-and-migration-worker-evidence-bundle.json", transferredBytes: 4096, verifiedFiles: 2, unverifiedFiles: 1, failures: [{ item: "database-connection-pool-and-migration-worker-evidence-bundle.json", kind: "verification", message: "模拟校验失败：下载内容与采集时的摘要不同，保留错误记录供人工检查。" }] },
  { ...PREVIEW_TRANSFER, transferId: "transfer_preview_conflict", status: "waitingConflict", completedFiles: 0, currentItem: "/srv/app/config/staging-checkout-api-with-background-migration.production-reference.yml", transferredBytes: 0, currentItemBytes: 0, verifiedFiles: 0, activeConflict: { itemIndex: 0, sourceName: "staging-checkout-api-with-background-migration.production-reference.yml", targetName: "staging-checkout-api-with-background-migration.production-reference.yml", existingSize: 6144, incomingSize: 8192, existingModifiedEpochSeconds: Math.floor(Date.parse(AT) / 1000), allowedActions: ["skip", "rename", "overwrite"] } },
];

function paramString(params: unknown, name: string, fallback: string): string {
  if (typeof params !== "object" || params === null) return fallback;
  const value = Reflect.get(params, name);
  return typeof value === "string" ? value : fallback;
}

function serverFor(params: unknown) {
  const serverId = paramString(params, "serverId", SERVERS[0]?.id ?? "");
  const server = SERVERS.find((candidate) => candidate.id === serverId);
  if (!server) throw new Error("找不到这台模拟服务器，请刷新模拟数据。");
  return server;
}

function snapshot(params: unknown): unknown {
  const server = serverFor(params);
  const collectedAt = new Date(Date.now()).toISOString();
  const warning = server?.id === "srv_previewdb";
  const inference = server.id === "srv_previewinference";
  return {
    snapshot: {
      id: `snap_${server.id}`,
      serverId: server?.id,
      collectedAt,
      health: warning ? "warning" : "healthy",
      os: { distribution: "Ubuntu", version: "24.04", hostname: server?.connection.host, kernel: "6.8.0", arch: "x86_64" },
      cpu: { model: "Intel(R) Xeon(R) Platinum", cores: 8, usagePercent: warning ? 67.4 : inference ? 48.2 : 23.5, loadAverage: [1.2, 0.9, 0.7] },
      memory: { totalBytes: 17179869184, usedBytes: warning ? 14087492730 : inference ? 11338713661 : 6012954214, availableBytes: warning ? 3092371454 : inference ? 5841155523 : 11166914970, usagePercent: warning ? 82 : inference ? 66 : 35 },
      disks: [
        { device: "/dev/sda1", mountPoint: "/", totalBytes: 107374182400, usedBytes: 42949672960, usagePercent: 40 },
        ...(warning ? [{ device: "/dev/sdb1", mountPoint: "/var/lib/postgresql", totalBytes: 214748364800, usedBytes: 197568495616, usagePercent: 92 }] : []),
      ],
      uptimeSeconds: 172800,
      network: [{ name: "eth0", rxBytes: 1099511627776, txBytes: 2199023255552 }],
      networkDetails: {
        interfaces: [{ name: "eth0", state: "UP", addresses: [warning ? "10.24.2.15/24" : inference ? "192.168.50.12/24" : "10.24.1.10/24", "fe80::1/64"] }],
        ...(inference ? { errors: { ipv4Routes: "模拟：目标系统未提供 iproute2 路由信息", ipv6Routes: "模拟：IPv6 路由读取失败" } } : {
          ipv4Routes: [
            ...(!warning ? [{ destination: "default", gateway: "10.24.1.1", interface: "eth0", metric: 100 }] : []),
            { destination: warning ? "10.24.2.0/24" : "10.24.1.0/24", interface: "eth0" },
          ],
          ipv6Routes: [],
        }),
        dnsServers: inference ? ["127.0.0.53"] : ["10.24.0.53", "10.24.0.54"],
      },
      docker: {
        available: true,
        containers: [
          { name: "api", image: "ghcr.io/example/api:2026.09", state: "running", status: "Up 12 hours", restartCount: 0 },
          ...(warning ? [
            { name: "postgres", image: "postgres:16.4", state: "running", status: "Up 3 days", restartCount: 1 },
            { name: "staging-checkout-database-migration-worker", image: "ghcr.io/example/platform-observability/database-migrate:2026.10.16-preview", state: "exited", status: "Exited (1) 9 minutes ago", restartCount: 4 },
          ] : []),
        ],
      },
      capabilities: server?.capabilities,
      collectors: ["os", "cpu", "memory", "disk", "uptime", "network", "docker"].map((id) => ({ collectorId: `collector.${id}`, collectedAt, ok: true })),
    },
  };
}

function hostKey(params: unknown): unknown {
  const server = serverFor(params);
  return {
    host: server?.connection.host ?? "preview.invalid",
    port: server?.connection.port ?? 22,
    pinned: true,
    pinnedFingerprint: FINGERPRINT,
  };
}

export function isPreviewFixture(): boolean {
  return import.meta.env?.DEV === true
    && typeof window !== "undefined"
    && new URLSearchParams(window.location.search).get("fixture") === "1";
}

/** A parsed IPC response for the local visual-audit adapter. */
export function previewResponse(command: IpcCommandName, params: unknown): unknown {
  const memoryResponse = previewMemoryResponse(command, params);
  if (memoryResponse !== undefined) return memoryResponse;
  switch (command) {
    case IPC_COMMANDS.corePing:
      return { version: "1.0.0-preview", os: "browser-fixture" };
    case IPC_COMMANDS.serverList:
      return { servers: SERVERS };
    case IPC_COMMANDS.serverConnect:
      serverFor(params).status = "connected";
      return { status: "connected" };
    case IPC_COMMANDS.serverDisconnect:
      serverFor(params).status = "disconnected";
      return {};
    case IPC_COMMANDS.serverDelete: {
      const server = serverFor(params);
      SERVERS.splice(SERVERS.indexOf(server), 1);
      return { deleted: true };
    }
    case IPC_COMMANDS.workspaceList:
      return {
        workspaces: [
          {
            id: "ws_preview",
            name: "Platform Observability",
            serverIds: ["srv_previewdb", "srv_previewanalytics"],
            repositories: [{ id: "repo_preview", name: "observability", host: "remote", path: "/srv/observability", serverId: "srv_previewdb", gitUrl: "https://example.invalid/observability.git", defaultBranch: "main" }],
            providerIds: ["prv_preview"],
            defaultEnvironment: "staging",
          },
          {
            id: "ws_previewapi",
            name: "Checkout API",
            serverIds: ["srv_previewapi"],
            repositories: [{ id: "repo_api", name: "api", host: "remote", path: "/srv/api", serverId: "srv_previewapi", gitUrl: "https://example.invalid/api.git", defaultBranch: "main" }],
            providerIds: ["prv_preview"],
            defaultEnvironment: "production",
          },
        ],
      };
    case IPC_COMMANDS.serverSnapshot:
      return snapshot(params);
    case IPC_COMMANDS.serverServices:
      return {
        source: "systemd",
        services: [
          { name: "api.service", state: "running", status: "active/running", description: "Staging API service" },
          { name: "postgresql.service", state: "running", status: "active/running", description: "PostgreSQL database" },
          { name: "staging-checkout-api-with-background-migration.service", state: "failed", status: "failed/exit-code · Result=exit-code · Main process exited, code=exited, status=1/FAILURE", description: "数据库迁移工作进程 — 负责订单历史分区初始化与跨服务数据回填（模拟长说明）" },
        ],
      };
    case IPC_COMMANDS.serverLogs:
      return {
        source: "journalctl",
        lines: [
          { text: "2026-09-21T07:54:11+0800 api[42]: pool exhausted; waiting for connection", level: "warning" },
          { text: "2026-09-21T07:54:13+0800 migration[51]: relation orders_2026 already exists", level: "error" },
          { text: "2026-09-21T07:55:00+0800 api[42]: request latency p95=1840ms", level: "warning" },
          { text: "2026-09-21T07:55:08+0800 api[42]: recovered one idle connection", level: "info" },
        ],
      };
    case IPC_COMMANDS.remoteFileList: {
      const path = paramString(params, "path", "/srv/app");
      const directory = path.replace(/\/+$/, "");
      return {
        path,
        entries: [
          { name: "config", path: `${directory}/config`, type: "directory", size: 0 },
          { name: "docker-compose.yml", path: `${directory}/docker-compose.yml`, type: "file", size: 4820 },
          { name: ".env.example", path: `${directory}/.env.example`, type: "file", size: 812 },
          { name: "staging-checkout-api-with-background-migration.production-reference.yml", path: `${directory}/staging-checkout-api-with-background-migration.production-reference.yml`, type: "file", size: 32768 },
          { name: "current-release", path: `${directory}/current-release`, type: "symlink", size: 0 },
        ],
      };
    }
    case IPC_COMMANDS.remoteFileRead:
      return {
        path: paramString(params, "path", "/srv/app/docker-compose.yml"),
        content: "services:\n  api:\n    image: ghcr.io/example/api:2026.09\n    restart: unless-stopped\n  postgres:\n    image: postgres:16.4\n",
        truncated: false,
      };
    case IPC_COMMANDS.remoteFilePreview:
      return { name: paramString(params, "path", "/srv/app/docker-compose.yml").split("/").at(-1) ?? "preview.yml", size: 135, kind: "text", truncated: false, text: "# fixture: 仅用于界面评估\nservices:\n  staging-checkout-api-with-background-migration:\n    image: ghcr.io/example/platform-observability/checkout-api:2026.10.16-preview\n    restart: unless-stopped\n    environment:\n      CONNECTION_POOL_MAX: 48\n      READONLY_DIAGNOSTICS: true\n", dataUrl: null };
    case IPC_COMMANDS.localFilePreview:
      return { name: "local-config.yml", size: 64, kind: "text", truncated: false, text: "service:\n  port: 8080\n  environment: staging\n", dataUrl: null };
    case IPC_COMMANDS.localPathPick:
      return { handles: paramString(params, "kind", "uploadFiles") === "uploadFiles"
        ? [{ handleId: "fixture-local-config", name: "staging-checkout-api-with-background-migration.local-preview.yml", kind: "file", size: 64 }]
        : [{ handleId: "fixture-local-directory", name: "模拟目录 · staging-incident-evidence-and-diagnostics", kind: "directory" }] };
    case IPC_COMMANDS.fileTransferGet: {
      const transfer = TRANSFERS.find((item) => item.transferId === paramString(params, "transferId", ""));
      if (!transfer) throw new Error("找不到这项模拟传输。");
      return { transfer };
    }
    case IPC_COMMANDS.fileTransferList:
      return { transfers: TRANSFERS.filter((item) => !paramString(params, "serverId", "") || item.serverId === paramString(params, "serverId", "")) };
    case IPC_COMMANDS.activityList: {
      const activities: Activity[] = [
        { id: "act_preview_health", serverId: "srv_previewdb", type: "health", title: "内存使用率接近阈值", description: "最近一次采集显示内存使用率 82%，建议检查数据库连接池。", source: "system", actor: "collector", outcome: "failure", traceId: "trace_preview", createdAt: AT },
        { id: "act_preview_agent", serverId: "srv_previewdb", type: "agent_action", title: "Agent 读取了应用日志", description: "只读排查：筛选最近 120 行并发现 3 条数据库连接错误。", source: "agent", actor: "Agent", outcome: "success", traceId: "trace_preview", createdAt: "2026-09-21T07:55:00.000Z" },
        { id: "act_preview_connection", serverId: "srv_previewrunner", type: "connection", title: "连接已断开", description: "本地审计 fixture：服务器等待重新连接。", source: "user", actor: "用户", outcome: "cancelled", createdAt: "2026-09-21T07:40:00.000Z" },
      ];
      const serverId = paramString(params, "serverId", "");
      const requestedLimit = typeof params === "object" && params !== null ? Reflect.get(params, "limit") : undefined;
      const limit = typeof requestedLimit === "number" && requestedLimit > 0 ? requestedLimit : 100;
      return { activities: activities.filter((activity) => !serverId || activity.serverId === serverId).slice(0, limit) };
    }
    case IPC_COMMANDS.toolExecutionList:
      return {
        executions: [
          { traceId: "trace_preview", stepId: "step_preview_logs", callId: "call_preview_logs", toolName: "server.logs.read", serverId: "srv_previewdb", environment: "staging", riskLevel: "read", decision: "auto", approvedBy: "policy", status: "success", input: { lines: 120, unit: "api.service" }, output: { summary: "读取 120 行日志，发现 3 条数据库连接错误" }, startedAt: "2026-09-21T07:54:10.000Z", endedAt: "2026-09-21T07:54:11.000Z", durationMs: 1000 },
          { traceId: "trace_preview", stepId: "step_preview_services", callId: "call_preview_services", toolName: "server.services.list", serverId: "srv_previewdb", environment: "staging", riskLevel: "read", decision: "auto", approvedBy: "policy", status: "success", input: { source: "systemd" }, output: { summary: "3 项服务，2 项运行中，1 项失败" }, startedAt: "2026-09-21T07:54:12.000Z", endedAt: "2026-09-21T07:54:13.000Z", durationMs: 1000 },
        ],
      };
    // A fixture session is intentionally transport-silent: it exercises the xterm
    // layout without pretending that a browser preview has opened an SSH channel.
    case IPC_COMMANDS.terminalOpen:
      return { terminalSessionId: "preview-terminal" };
    case IPC_COMMANDS.terminalWrite:
    case IPC_COMMANDS.terminalResize:
    case IPC_COMMANDS.terminalClose:
      return {};
    case IPC_COMMANDS.providerList:
      return { providers: PROVIDERS };
    case IPC_COMMANDS.providerModels:
      return { models: PROVIDERS.find((provider) => provider.id === paramString(params, "providerId", PROVIDERS[0]!.id))?.models ?? MODELS };
    case IPC_COMMANDS.agentTeamTasks:
      return { tasks: [] };
    case IPC_COMMANDS.agentStatus:
      return { running: true, pid: 1, protocolVersion: "1.0", agentVersion: "1.0.0-preview", toolCount: 12, entry: "browser-fixture", startedAt: AT, lastExit: null };
    case IPC_COMMANDS.agentLogs:
      return { lines: ["[preview] Agent fixture is ready; no process or network connection was opened.", "[preview] Provider catalog: 3 enabled gateways, 1 disabled fallback.", "[preview] Long diagnostic entry: staging-checkout-api-with-background-migration.service / connection-pool-observation / readonly collection completed.", "[preview] MCP catalog and investigation history are in-memory examples only."], capacity: 256 };
    case IPC_COMMANDS.chatSessionList: {
      const request = typeof params === "object" && params !== null ? params as Record<string, unknown> : {};
      const query = typeof request.query === "string" ? request.query.trim().toLocaleLowerCase() : "";
      const inScope = !("serverId" in request) || request.serverId === PREVIEW_CHAT_SESSION.serverId;
      const matches = inScope && (!query || [PREVIEW_CHAT_SESSION.title, ...PREVIEW_CHAT_MESSAGES.map((message) => message.content)].some((content) => content.toLocaleLowerCase().includes(query)));
      const offset = typeof request.offset === "number" ? request.offset : 0;
      const limit = typeof request.limit === "number" ? request.limit : 50;
      return { sessions: matches && request.archived !== true ? [PREVIEW_CHAT_SESSION].slice(offset, offset + limit) : [], counts: { active: matches ? 1 : 0, archived: 0 } };
    }
    case IPC_COMMANDS.chatSessionGet:
      if (paramString(params, "sessionId", PREVIEW_CHAT_SESSION.id) !== PREVIEW_CHAT_SESSION.id) throw new Error("找不到这段模拟对话。");
      return { session: PREVIEW_CHAT_SESSION, messages: PREVIEW_CHAT_MESSAGES };
    case IPC_COMMANDS.investigationTaskList:
      return { tasks: [TASK] };
    case IPC_COMMANDS.investigationTaskGet:
      if (paramString(params, "taskId", TASK.id) !== TASK.id) throw new Error("找不到这项模拟任务。");
      return TASK_DETAIL;
    case IPC_COMMANDS.investigationScheduleList:
      return { schedules: SCHEDULES };
    case IPC_COMMANDS.investigationScheduleRuns:
      return { runs: scheduleRuns(paramString(params, "scheduleId", SCHEDULES[0]!.id)) };
    case IPC_COMMANDS.mcpServerList:
      return { servers: MCP_SERVERS };
    case IPC_COMMANDS.networkProxyGet:
      return { mode: "direct", hasCredential: false, resolution: { kind: "direct" } };
    case IPC_COMMANDS.serverHostKeyStatus:
      return hostKey(params);
    default:
      throw new Error("模拟数据模式仅供界面评估，此操作不会执行。退出模拟后可使用真实功能。");
  }
}
