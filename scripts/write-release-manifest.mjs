#!/usr/bin/env node

import { execFileSync } from "node:child_process";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

import { verifyReleaseManifest, writeReleaseManifest } from "./lib/release-manifest.mjs";

const root = fileURLToPath(new URL("..", import.meta.url));
const bundleDir = path.join(root, "target", "release", "bundle");

function gitValue(args) {
  try {
    return execFileSync("git", args, { cwd: root, encoding: "utf8", stdio: ["ignore", "pipe", "ignore"] }).trim();
  } catch {
    return null;
  }
}

async function main() {
  const args = process.argv.slice(2);
  if (args.length > 1 || (args.length === 1 && args[0] !== "--verify")) {
    throw new Error("usage: node scripts/write-release-manifest.mjs [--verify]");
  }

  if (args[0] === "--verify") {
    const manifest = await verifyReleaseManifest(bundleDir);
    console.log(`release artifact manifest: green (${manifest.artifacts.length} artifacts verified)`);
    return;
  }

  const [tauriConfig, runtimePins] = await Promise.all([
    readFile(path.join(root, "apps", "desktop", "src-tauri", "tauri.conf.json"), "utf8").then(JSON.parse),
    readFile(path.join(root, "scripts", "node-runtime.json"), "utf8").then(JSON.parse),
  ]);
  const revision = gitValue(["rev-parse", "--verify", "HEAD"]);
  const status = gitValue(["status", "--porcelain"]);
  const source = {
    revision,
    dirty: status === null ? null : status.length > 0,
  };
  const manifest = await writeReleaseManifest({
    bundleDir,
    appVersion: tauriConfig.version,
    runtimeVersion: runtimePins.version,
    source,
  });
  console.log(`wrote release manifest for ${manifest.artifacts.length} artifact(s) to ${bundleDir}`);
  for (const artifact of manifest.artifacts) {
    console.log(`  ${artifact.path}  ${artifact.bytes} bytes  SHA-256 ${artifact.sha256}`);
  }
  console.log(`source revision ${revision ?? "unknown"}; working tree ${source.dirty === null ? "unknown" : source.dirty ? "dirty" : "clean"}`);
  console.log("This manifest is not signed and does not establish publisher identity.");
}

main().catch((error) => {
  console.error(error.message);
  process.exitCode = 1;
});
