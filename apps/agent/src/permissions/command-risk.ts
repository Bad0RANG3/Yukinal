/**
 * Dangerous command rules (layer 2 of ADR 0005).
 *
 * These rules only *raise* a risk signal. They are explicitly NOT the safety
 * boundary — the Permission Engine is (note,). Over-triggering is
 * acceptable and preferred: a false alarm costs one approval click, a miss costs
 * a production incident.
 */

import type { RiskLevel } from "@yukinal/shared";

export interface CommandRiskRule {
  id: string;
  pattern: RegExp;
  level: RiskLevel;
  note: string;
}

export const DANGEROUS_COMMAND_RULES: readonly CommandRiskRule[] = [
  { id: "rm-rf", pattern: /\brm\b[^\n;&|]*\s-\w*[rf]\w*\b/, level: "critical", note: "recursive/forced delete" },
  { id: "rm-root-path", pattern: /\brm\b[^\n;&|]*\s\/(\s|$)/, level: "critical", note: "delete targeting filesystem root" },
  { id: "mkfs", pattern: /\bmkfs(\.\w+)?\b/, level: "critical", note: "filesystem format" },
  { id: "dd-device", pattern: /\bdd\s+[^\n]*\bof=\/dev\//, level: "critical", note: "raw write to block device" },
  { id: "write-block-device", pattern: />[>&]?\s*\/dev\/(sd|nvme|hd|vd)/, level: "critical", note: "redirect onto block device" },
  { id: "drop-database", pattern: /\bdrop\s+database\b/i, level: "critical", note: "database drop" },
  { id: "chmod-recursive-root", pattern: /\bchmod\b[^\n;&|]*\s-[Rr]\b[^\n;&|]*\s\/(\s|$)/, level: "critical", note: "recursive permission change on /" },
  { id: "truncate-table", pattern: /\btruncate\s+(table\s+)?\w+/i, level: "high", note: "table truncation" },
  { id: "shutdown", pattern: /\b(shutdown|poweroff|halt)\b/, level: "high", note: "host shutdown" },
  { id: "reboot", pattern: /\breboot\b/, level: "high", note: "host reboot" },
  { id: "kubectl-delete", pattern: /\bkubectl\s+delete\b/, level: "high", note: "kubernetes resource deletion" },
  { id: "docker-system-prune", pattern: /\bdocker\s+system\s+prune\b/, level: "high", note: "docker prune" },
  { id: "curl-pipe-shell", pattern: /\b(curl|wget)\b[^\n;&|]*\|\s*(sudo\s+)?(ba|z)?sh\b/, level: "high", note: "remote script piped into a shell" },
  { id: "sudo", pattern: /\bsudo\b/, level: "medium", note: "privilege escalation" },
  { id: "systemctl-stop", pattern: /\bsystemctl\s+(stop|disable|mask)\b/, level: "medium", note: "service interruption" },
  { id: "docker-compose-down", pattern: /\bdocker-compose\s+down\b|\bdocker\s+compose\s+down\b/, level: "medium", note: "stack teardown" },
];

export interface CommandRisk {
  command: string;
  level: RiskLevel;
  matched: CommandRiskRule[];
}

const LEVEL_ORDER: readonly RiskLevel[] = ["read", "low", "medium", "high", "critical"];

function rank(level: RiskLevel): number {
  return LEVEL_ORDER.indexOf(level);
}

/** Returns undefined when there is nothing to analyse (no free-text command involved). */
export function analyzeCommand(command: string | undefined): CommandRisk | undefined {
  if (command === undefined || command.trim() === "") return undefined;

  const matched = DANGEROUS_COMMAND_RULES.filter((rule) => rule.pattern.test(command));
  if (matched.length === 0) return { command, level: "low", matched: [] };

  const level = matched.reduce<RiskLevel>(
    (highest, rule) => (rank(rule.level) > rank(highest) ? rule.level : highest),
    "read",
  );
  return { command, level, matched };
}

/**
 * Pull the shell command out of a structured tool input (tools are
 * structured, only a few of them carry raw shell text).
 */
export function extractCommand(input: unknown): string | undefined {
  if (typeof input !== "object" || input === null) return undefined;
  const candidate = input as { command?: unknown; argv?: unknown };
  if (typeof candidate.command === "string") return candidate.command;
  if (Array.isArray(candidate.argv) && candidate.argv.every((part) => typeof part === "string")) {
    return candidate.argv.join(" ");
  }
  return undefined;
}
