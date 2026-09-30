/**
 * Local, deterministic data for the browser-only visual audit.
 *
 * It is reachable only from Vite development with `?fixture=1`.  Responses
 * still traverse the regular Zod IPC schema, but no credentials, sockets, SSH
 * operations, or Tauri commands are involved.
 */

import { IPC_COMMANDS, type IpcCommandName } from "@yukinal/shared";

const AT = "2026-09-21T08:00:00.000Z";
const FINGERPRINT = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";

const SERVERS = [
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
];

const PROVIDERS = [
  {
    id: "prv_preview",
    kind: "openai-compatible",
    label: "Preview Gateway",
    baseUrl: "https://preview.invalid/v1",
    model: "preview-model",
    apiKeyCredentialRef: "keychain://preview/no-secret",
    enabled: true,
    wireApi: "chat",
    createdAt: AT,
    updatedAt: AT,
  },
];

const TASK = {
  id: "task_preview",
  workspaceId: "ws_preview",
  serverId: "srv_previewdb",
  objective: "确认 staging API 延迟升高的原因",
  successCriteria: ["确认最近的延迟变化", "检查服务与日志是否有共同异常"],
  scope: { host: "remote", serverId: "srv_previewdb", environment: "staging" },
  guardrails: { forbiddenTools: ["filesystem.write", "docker.restart"], forbiddenPathPrefixes: ["/etc/production"] },
  mode: "readonly",
  permissionMode: "ask",
  automationLevel: "readonly",
  createdBy: "browser-fixture",
  phase: "investigating",
  status: "investigating",
  budget: { maxSteps: 12, maxRunMs: 300000, maxAttempts: 2 },
  createdAt: AT,
  updatedAt: AT,
  activeRunId: "run_preview",
};

const PREVIEW_CHAT_SESSION = {
  id: "ses_preview_triage",
  workspaceId: "ws_preview",
  serverId: "srv_previewdb",
  title: "排查 Staging API 延迟升高",
  createdAt: "2026-09-21T07:53:00.000Z",
  updatedAt: "2026-09-21T07:56:00.000Z",
  messageCount: 4,
  lastMessagePreview: "建议先扩大连接池前确认 migration worker 的失败原因。",
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
    content: "已确认连接池等待与 p95 1840ms 同时出现，migration worker 在 9 分钟前失败。建议先核对该 worker 的失败日志和连接池上限；当前证据不足以批准任何变更。",
    createdAt: "2026-09-21T07:56:00.000Z",
  },
];

const PREVIEW_TRANSFER = {
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

function paramString(params: unknown, name: string, fallback: string): string {
  if (typeof params !== "object" || params === null) return fallback;
  const value = Reflect.get(params, name);
  return typeof value === "string" ? value : fallback;
}

function serverFor(params: unknown) {
  const serverId = paramString(params, "serverId", SERVERS[0]?.id ?? "");
  return SERVERS.find((server) => server.id === serverId) ?? SERVERS[0];
}

function snapshot(params: unknown): unknown {
  const server = serverFor(params);
  const warning = server?.id === "srv_previewdb";
  return {
    snapshot: {
      id: "snap_preview",
      serverId: server?.id,
      collectedAt: AT,
      health: warning ? "warning" : "healthy",
      os: { distribution: "Ubuntu", version: "24.04", hostname: server?.connection.host, kernel: "6.8.0", arch: "x86_64" },
      cpu: { model: "Intel(R) Xeon(R) Platinum", cores: 8, usagePercent: warning ? 67.4 : 23.5, loadAverage: [1.2, 0.9, 0.7] },
      memory: { totalBytes: 17179869184, usedBytes: warning ? 14087492730 : 6012954214, availableBytes: warning ? 3092371454 : 11166914970, usagePercent: warning ? 82 : 35 },
      disks: [
        { device: "/dev/sda1", mountPoint: "/", totalBytes: 107374182400, usedBytes: 42949672960, usagePercent: 40 },
        ...(warning ? [{ device: "/dev/sdb1", mountPoint: "/var/lib/postgresql", totalBytes: 214748364800, usedBytes: 182536110080, usagePercent: 85 }] : []),
      ],
      uptimeSeconds: 172800,
      network: [{ name: "eth0", rxBytes: 1099511627776, txBytes: 2199023255552 }],
      docker: {
        available: true,
        containers: [
          { name: "api", image: "ghcr.io/example/api:2026.09", state: "running", status: "Up 12 hours", restartCount: 0 },
          ...(warning ? [
            { name: "postgres", image: "postgres:16.4", state: "running", status: "Up 3 days", restartCount: 1 },
            { name: "migration-worker", image: "ghcr.io/example/migrate:2026.09", state: "exited", status: "Exited (1) 9 minutes ago", restartCount: 4 },
          ] : []),
        ],
      },
      capabilities: server?.capabilities,
      collectors: [{ collectorId: "collector.cpu", collectedAt: AT, ok: true }],
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
  switch (command) {
    case IPC_COMMANDS.corePing:
      return { version: "1.0.0-preview", os: "browser-fixture" };
    case IPC_COMMANDS.serverList:
      return { servers: SERVERS };
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
          { name: "migration-worker.service", state: "failed", status: "failed/exit-code", description: "Database migration worker" },
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
    case IPC_COMMANDS.remoteFileList:
      return {
        path: paramString(params, "path", "/srv/app"),
        entries: [
          { name: "config", path: "/srv/app/config", type: "directory", size: 0 },
          { name: "docker-compose.yml", path: "/srv/app/docker-compose.yml", type: "file", size: 4820 },
          { name: ".env.example", path: "/srv/app/.env.example", type: "file", size: 812 },
          { name: "current-release", path: "/srv/app/current-release", type: "symlink", size: 0 },
        ],
      };
    case IPC_COMMANDS.remoteFileRead:
      return {
        path: paramString(params, "path", "/srv/app/docker-compose.yml"),
        content: "services:\n  api:\n    image: ghcr.io/example/api:2026.09\n    restart: unless-stopped\n  postgres:\n    image: postgres:16.4\n",
        truncated: false,
      };
    case IPC_COMMANDS.remoteFilePreview:
      return { name: "docker-compose.yml", size: 135, kind: "text", truncated: false, text: "services:\n  api:\n    image: ghcr.io/example/api:2026.09\n    restart: unless-stopped\n", dataUrl: null };
    case IPC_COMMANDS.filePrepareRemoteDrag:
      return { dragId: "remote_drag_0000000000000000000000000000000000000000000000000000000000000000", name: "docker-compose.yml", size: 135 };
    case IPC_COMMANDS.fileDragOutStart:
      return {};
    case IPC_COMMANDS.localFilePreview:
      return { name: "local-config.yml", size: 64, kind: "text", truncated: false, text: "service:\n  port: 8080\n  environment: staging\n", dataUrl: null };
    case IPC_COMMANDS.localPathPick:
      return { handles: [{ handleId: "fixture-local-config", name: "local-config.yml", kind: "file", size: 64 }] };
    case IPC_COMMANDS.fileTransferUpload:
    case IPC_COMMANDS.fileTransferDownload:
    case IPC_COMMANDS.fileTransferCancel:
    case IPC_COMMANDS.fileTransferResolveConflict:
    case IPC_COMMANDS.fileTransferGet:
      return { transfer: PREVIEW_TRANSFER };
    case IPC_COMMANDS.fileTransferList:
      return { transfers: [] };
    case IPC_COMMANDS.activityList:
      return {
        activities: [
          { id: "act_preview_health", serverId: "srv_previewdb", type: "health", title: "内存使用率接近阈值", description: "最近一次采集显示内存使用率 82%，建议检查数据库连接池。", source: "system", actor: "collector", outcome: "failure", traceId: "trace_preview", createdAt: AT },
          { id: "act_preview_agent", serverId: "srv_previewdb", type: "agent_action", title: "Agent 读取了应用日志", description: "只读排查：筛选最近 120 行并发现 3 条数据库连接错误。", source: "agent", actor: "Agent", outcome: "success", traceId: "trace_preview", createdAt: "2026-09-21T07:55:00.000Z" },
          { id: "act_preview_connection", serverId: "srv_previewrunner", type: "connection", title: "连接已断开", description: "本地审计 fixture：服务器等待重新连接。", source: "user", actor: "用户", outcome: "cancelled", createdAt: "2026-09-21T07:40:00.000Z" },
        ],
      };
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
      return { models: [{ id: "preview-model", label: "Preview model", contextWindow: 200000 }] };
    case IPC_COMMANDS.agentStatus:
      return { running: true, pid: 1, protocolVersion: "1.0", agentVersion: "1.0.0-preview", toolCount: 0, entry: "browser-fixture", startedAt: AT, lastExit: null };
    case IPC_COMMANDS.agentLogs:
      return { lines: ["[preview] Agent fixture is ready"], capacity: 256 };
    case IPC_COMMANDS.chatSessionList:
      return { sessions: [PREVIEW_CHAT_SESSION], counts: { active: 1, archived: 0 } };
    case IPC_COMMANDS.chatSessionGet:
      return { session: PREVIEW_CHAT_SESSION, messages: PREVIEW_CHAT_MESSAGES };
    case IPC_COMMANDS.investigationTaskList:
      return { tasks: [TASK] };
    case IPC_COMMANDS.investigationTaskGet:
      return { task: TASK, evidence: [], findings: [], artifacts: [], runs: [], steps: [] };
    case IPC_COMMANDS.investigationScheduleList:
      return { schedules: [] };
    case IPC_COMMANDS.mcpServerList:
      return { servers: [] };
    case IPC_COMMANDS.networkProxyGet:
      return { mode: "direct", hasCredential: false, resolution: { kind: "direct" } };
    case IPC_COMMANDS.serverHostKeyStatus:
      return hostKey(params);
    default:
      return {};
  }
}
