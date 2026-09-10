#!/usr/bin/env node
/**
 * Lightweight tracked-file secret gate.
 *
 * It deliberately reports only file, line, and rule. Secret values are never
 * echoed by this script, including when the check fails in CI.
 */

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

const RULES = [
  { id: "OpenAI-style API key", pattern: /\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b/ },
  { id: "GitHub token", pattern: /\b(?:ghp|gho|ghu|ghs)_[A-Za-z0-9_]{20,}\b|\bgithub_pat_[A-Za-z0-9_]{20,}\b/ },
  { id: "AWS access key", pattern: /\b(?:AKIA|ASIA)[0-9A-Z]{16}\b/ },
  { id: "Google API key", pattern: /\bAIza[0-9A-Za-z_-]{30,}\b/ },
  { id: "Slack token", pattern: /\bxox[baprs]-[A-Za-z0-9-]{20,}\b/ },
  {
    id: "private key material",
    pattern: /-----BEGIN(?: [A-Z0-9]+)? PRIVATE KEY-----\r?\n(?:[A-Za-z0-9+/=]{16,}\r?\n)+-----END(?: [A-Z0-9]+)? PRIVATE KEY-----/,
  },
  {
    id: "assigned credential",
    pattern: /(?:api[_-]?key|authorization|access[_-]?token)\s*[:=]\s*["'`]?[A-Za-z0-9_~+/=-]{20,}/i,
  },
];

const BINARY_EXTENSION = /\.(?:png|ico|icns|woff2?|ttf|otf|jpg|jpeg|pdf|zip|gz|7z)$/i;
const SKIPPED_FILES = new Set(["Cargo.lock", "pnpm-lock.yaml"]);

const tracked = spawnSync("git", ["ls-files", "--cached", "--others", "--exclude-standard", "-z"], {
  encoding: "utf8",
});
if (tracked.status !== 0) {
  console.error("secret scan: unable to enumerate repository files");
  process.exit(2);
}

const findings = [];
for (const file of tracked.stdout.split("\0").filter(Boolean)) {
  if (SKIPPED_FILES.has(file) || BINARY_EXTENSION.test(file)) continue;
  let source;
  try {
    source = readFileSync(file, "utf8");
  } catch {
    continue;
  }
  for (const rule of RULES) {
    const match = rule.pattern.exec(source);
    if (!match) continue;
    const line = source.slice(0, match.index).split("\n").length;
    findings.push({ file, line, rule: rule.id });
  }
}

if (findings.length > 0) {
  console.error(`secret scan: ${findings.length} potential credential(s) found`);
  for (const finding of findings) console.error(`  ${finding.file}:${finding.line}: ${finding.rule}`);
  console.error("Remove the credential, rotate it if it was real, and use the OS credential store at runtime.");
  process.exit(1);
}

console.log(`secret scan: green (${tracked.stdout.split("\0").filter(Boolean).length} files checked)`);
