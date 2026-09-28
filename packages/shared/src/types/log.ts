/** Bounded read-only log data returned by a connected server. */

export const LOG_LEVELS = ["error", "warning", "info"] as const;
export type LogLevel = (typeof LOG_LEVELS)[number];

export const LOG_SOURCES = ["journalctl", "syslog", "messages", "unavailable"] as const;
export type LogSource = (typeof LOG_SOURCES)[number];

/** Optional bounded query for the host's read-only journal probe. */
export interface ServerLogsInput {
  /** Read at most this many seconds into the past; the host caps it at 7 days. */
  sinceSeconds?: number;
  /** Restrict journalctl to one validated systemd service unit. */
  unit?: string;
}

export interface ServerLogLine {
  /** Original remote line, kept intact for diagnosis and copy/paste. */
  text: string;
  level: LogLevel;
}

export interface ServerLogsResponse {
  source: LogSource;
  lines: ServerLogLine[];
  /** Present when no supported log source could be read. */
  message?: string;
}
