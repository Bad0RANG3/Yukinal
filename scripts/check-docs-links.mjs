#!/usr/bin/env node
/**
 * Documentation link gate.
 *
 * Splitting the documentation into a tree made relative links load-bearing: a
 * section that moves takes its anchors with it, and GitHub's renderer is the only
 * thing that would have noticed. This checks every markdown file in the worktree
 * (tracked and untracked-but-not-ignored, so a brand new document is checked
 * before its first commit) and verifies:
 *
 *   - a relative link points at a file that exists
 *   - a `#fragment` points at a heading that exists in that file, using the same
 *     normalisation GitHub uses (lowercase, punctuation dropped, spaces to dashes)
 *
 * Fenced code blocks and inline code spans are stripped first: `![alt](url)` in a
 * sentence about Markdown syntax is an example, not a link.
 *
 * Two controls run at the end. A checker that silently reads nothing reports
 * success, which has already happened once in this repository.
 */

import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";

const root = process.cwd();

const listed = spawnSync("git", ["ls-files", "-z", "--cached", "--others", "--exclude-standard"], {
  encoding: "utf8",
});
if (listed.status !== 0) {
  console.error("check-docs-links: `git ls-files` failed; cannot establish scope");
  process.exit(2);
}
const files = listed.stdout.split("\0").filter((file) => file.endsWith(".md"));
if (files.length === 0) {
  console.error("check-docs-links: no markdown files found -- this run proves nothing");
  process.exit(2);
}

const anchorCache = new Map();
function anchorsOf(file) {
  if (!anchorCache.has(file)) {
    const set = new Set();
    const source = readFileSync(path.join(root, file), "utf8");
    let fenced = false;
    for (const line of source.split("\n")) {
      if (/^\s*```/.test(line)) fenced = !fenced;
      if (fenced) continue;
      const heading = /^#{1,6}\s+(.*)$/.exec(line);
      if (heading) set.add(anchorize(heading[1]));
    }
    anchorCache.set(file, set);
  }
  return anchorCache.get(file);
}

function anchorize(heading) {
  return heading
    .trim()
    .toLowerCase()
    .replace(/[`*_~]/g, "")
    .replace(/[^\p{L}\p{N}\s-]/gu, "")
    .trim()
    .replace(/\s+/g, "-");
}

let links = 0;
const problems = [];

for (const file of files) {
  const raw = readFileSync(path.join(root, file), "utf8");
  const source = raw.replace(/```[\s\S]*?```/g, "").replace(/`[^`\n]*`/g, "");
  const seen = new Set();
  for (const match of source.matchAll(/\[[^\]]*\]\(([^)\s]+)\)/g)) {
    const target = match[1];
    if (/^[a-z][a-z0-9+.-]*:/i.test(target)) continue; // absolute URL or mailto
    if (seen.has(target)) continue;
    seen.add(target);
    links += 1;

    const hash = target.indexOf("#");
    const pathPart = hash === -1 ? target : target.slice(0, hash);
    const fragment = hash === -1 ? "" : decodeURIComponent(target.slice(hash + 1));
    const resolved =
      pathPart === "" ? file : path.posix.normalize(path.posix.join(path.posix.dirname(file), pathPart));

    if (pathPart !== "" && !existsSync(path.join(root, resolved))) {
      problems.push(`${file}: link target does not exist -> ${target}`);
      continue;
    }
    if (fragment !== "" && resolved.endsWith(".md") && !anchorsOf(resolved).has(fragment)) {
      problems.push(`${file}: no heading for anchor -> ${target}`);
    }
  }
}

// Controls: the run must have read real links, and must be able to fail.
const positive = anchorsOf("docs/README.md").has("阅读顺序");
const negative = anchorsOf("docs/README.md").has("这个锚点一定不存在");
if (links === 0 || !positive || negative) {
  console.error(
    `check-docs-links: controls failed (links=${links}, positive=${positive}, negative=${negative}) -- this run proves nothing`,
  );
  process.exit(2);
}

if (problems.length > 0) {
  console.error(`check-docs-links: ${problems.length} problem(s)`);
  for (const problem of problems.slice(0, 40)) console.error("  " + problem);
  if (problems.length > 40) console.error(`  … ${problems.length - 40} more`);
  process.exit(1);
}
console.log(`check-docs-links: green (${files.length} markdown files, ${links} links)`);
