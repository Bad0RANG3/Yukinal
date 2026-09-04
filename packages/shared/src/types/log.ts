/** Bounded read-only log data returned by a connected server. */

export const LOG_LEVELS = ["error", "warning", "info"] as const;
export type LogLevel = (typeof LOG_LEVELS)[number];

export const LOG_SOURCES = ["journalctl", "syslog", "messages", "unavailable"] as const;
export type LogSource = (typeof LOG_SOURCES)[number];

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
