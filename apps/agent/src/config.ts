/**
 * Process configuration for the sidecar.
 *
 * Everything comes from the environment because Rust spawns this process
 * (the UI never manages the agent's lifecycle itself).
 */

export const AGENT_VERSION = "0.0.0";

export interface AgentConfig {
  /** Per-user data dir passed by Rust; used for SQLite/trace spool, never for secrets. */
  dataDir: string;
  logLevel: LogLevel;
  /** Safety net for the whole process: no run may outlive this. */
  maxRunMs: number;
}

export const LOG_LEVELS = ["debug", "info", "warn", "error"] as const;
export type LogLevel = (typeof LOG_LEVELS)[number];

export function readConfig(env: NodeJS.ProcessEnv = process.env): AgentConfig {
  return {
    dataDir: env.YUKINAL_DATA_DIR ?? "",
    logLevel: parseLogLevel(env.YUKINAL_LOG_LEVEL),
    maxRunMs: parsePositiveInt(env.YUKINAL_MAX_RUN_MS, 15 * 60_000),
  };
}

export function parseLogLevel(value: string | undefined): LogLevel {
  return (LOG_LEVELS as readonly string[]).includes(value ?? "") ? (value as LogLevel) : "info";
}

export function parsePositiveInt(value: string | undefined, fallback: number): number {
  const parsed = Number.parseInt(value ?? "", 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

/**
 * Logging MUST go to stderr: stdout carries NDJSON frames only (ADR 0006).
 * A stray `console.log` would corrupt the protocol.
 */
export interface AgentLogger {
  debug(message: string, meta?: Record<string, unknown>): void;
  info(message: string, meta?: Record<string, unknown>): void;
  warn(message: string, meta?: Record<string, unknown>): void;
  error(message: string, meta?: Record<string, unknown>): void;
  child(scope: string): AgentLogger;
}

const LEVEL_RANK: Record<LogLevel, number> = { debug: 0, info: 1, warn: 2, error: 3 };

export function createLogger(config: { level: LogLevel; scope?: string }): AgentLogger {
  const write = (level: LogLevel, message: string, meta?: Record<string, unknown>): void => {
    if (LEVEL_RANK[level] < LEVEL_RANK[config.level]) return;
    const scope = config.scope ? `[${config.scope}] ` : "";
    process.stderr.write(`${new Date().toISOString()} ${level.toUpperCase()} ${scope}${message}${meta ? ` ${safeJson(meta)}` : ""}\n`);
  };

  return {
    debug: (message, meta) => write("debug", message, meta),
    info: (message, meta) => write("info", message, meta),
    warn: (message, meta) => write("warn", message, meta),
    error: (message, meta) => write("error", message, meta),
    child: (scope) => createLogger({ level: config.level, scope: config.scope ? `${config.scope}:${scope}` : scope }),
  };
}

function safeJson(value: unknown): string {
  try {
    return JSON.stringify(value);
  } catch {
    return "{\"unserialisable\":true}";
  }
}
