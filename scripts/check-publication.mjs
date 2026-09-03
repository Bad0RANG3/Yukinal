#!/usr/bin/env node
/**
 * Publication hygiene gate.
 *
 * This repository is public; the design material it grew from is not. Pointers to that
 * material — section citations, internal step labels, links into `.private/` — are
 * useless to readers and describe documents that are deliberately not published here.
 *
 * Fails the build, because "we'll remember" does not survive a 468-citation cleanup.
 */

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";

const RULES = [
  { pattern: /§\s*\d/, label: "章节引用 §NN", why: "引用的是未发布的文档" },
  { pattern: /\bspec\s+§|\bspec\b\s*[:：]\s*\d/, label: "spec 指针", why: "同上" },
  { pattern: /\bS\d{2}\b/, label: "内部阶段号 SNN", why: "私有计划的编号，写成组件/能力名" },
  { pattern: /(?:MODULES\.md|AGENTS\.md)/, label: "私有文档名", why: "这些文件不在仓库里" },
  { pattern: /\.private\/[^\s)》，、]*\.(?:md|txt|pdf)/, label: "指向 .private 的链接", why: "读者打不开" },
  { pattern: /最高层级约束|规范原文|AI-Native SSH - Remote Development/, label: "私有规范措辞", why: "对外文档不指向内部规范" },
  { pattern: /\bOrbit\b/, label: "曾用名", why: "已废弃，只在私有计划里保留" },
];

const SKIP_FILES = new Set(["scripts/check-publication.mjs", "Cargo.lock", "pnpm-lock.yaml"]);
const SKIP_EXT = /\.(png|ico|icns|woff2?|jpg|jpeg)$/;

const tracked = spawnSync("git", ["ls-files", "-z"], { encoding: "utf8" }).stdout.split("\0").filter(Boolean);

const problems = [];
for (const file of tracked) {
  if (SKIP_FILES.has(file) || SKIP_EXT.test(file)) continue;
  let source;
  try {
    source = readFileSync(file, "utf8");
  } catch {
    continue;
  }
  source.split("\n").forEach((line, index) => {
    // A comment line in this gate's own neighbour files is still checked on purpose.
    for (const rule of RULES) {
      if (rule.pattern.test(line)) {
        problems.push(`${file}:${index + 1}: ${rule.label} — ${rule.why}\n      ${line.trim().slice(0, 120)}`);
      }
    }
  });
}

if (problems.length > 0) {
  console.error(`publication hygiene: ${problems.length} problem(s) in ${new Set(problems.map((p) => p.split(":")[0])).size} file(s)`);
  for (const problem of problems.slice(0, 40)) console.error("  " + problem);
  if (problems.length > 40) console.error(`  … ${problems.length - 40} more`);
  console.error("\n修复方式：把「为什么」直接写进注释，删掉指向私有材料的指针。");
  process.exit(1);
}

console.log(`publication hygiene: green (${tracked.length} tracked files)`);
