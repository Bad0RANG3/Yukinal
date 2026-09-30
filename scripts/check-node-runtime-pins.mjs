#!/usr/bin/env node

/**
 * Check every bundled Node.js archive pin against the checksum list published with that
 * exact official release. This runs during packaging, not `pnpm check`, so routine local
 * development remains deterministic and offline.
 */

import { readFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const manifest = JSON.parse(await readFile(join(root, "scripts", "node-runtime.json"), "utf8"));
const expectedPlatforms = [
  "darwin-arm64",
  "darwin-x64",
  "linux-arm64",
  "linux-x64",
  "win32-arm64",
  "win32-x64",
];
const platforms = Object.keys(manifest.platforms ?? {}).sort();

if (!/^\d+\.\d+\.\d+$/.test(manifest.version)) {
  throw new Error("Node.js runtime manifest has an invalid version.");
}
if (JSON.stringify(platforms) !== JSON.stringify(expectedPlatforms)) {
  throw new Error(`Node.js runtime platform set is incomplete or unexpected: ${platforms.join(", ")}`);
}

const checksumUrl = `https://nodejs.org/dist/v${manifest.version}/SHASUMS256.txt`;
const response = await fetch(checksumUrl, { signal: AbortSignal.timeout(30_000) });
if (!response.ok) throw new Error(`Official Node.js checksum download failed: ${response.status} ${response.statusText}`);
const finalHost = new URL(response.url).hostname;
if (finalHost !== "nodejs.org" && finalHost !== "r2.nodejs.org") {
  throw new Error(`Unexpected host in Node.js checksum redirect: ${finalHost}`);
}
const checksumText = await response.text();
if (checksumText.length === 0 || checksumText.length > 128 * 1024) {
  throw new Error(`Unexpected Node.js checksum file size: ${checksumText.length} characters`);
}

const officialChecksums = new Map();
for (const line of checksumText.split(/\r?\n/)) {
  const match = line.match(/^([a-f\d]{64})[\t ]+\*?([^\s]+)$/i);
  if (!match) continue;
  const [, digest, filename] = match;
  if (officialChecksums.has(filename)) throw new Error(`Duplicate filename in official checksum list: ${filename}`);
  officialChecksums.set(filename, digest.toLowerCase());
}

const compressionExtensions = {
  gzip: ".tar.gz",
  xz: ".tar.xz",
  zip: ".zip",
};

for (const platform of expectedPlatforms) {
  const entry = manifest.platforms[platform];
  if (basename(entry.archive) !== entry.archive) {
    throw new Error(`Node.js archive must be a filename, not a path: ${entry.archive}`);
  }
  if (!Object.hasOwn(compressionExtensions, entry.compression) || !entry.archive.endsWith(compressionExtensions[entry.compression])) {
    throw new Error(`Archive extension/compression mismatch for ${platform}: ${entry.archive} (${entry.compression})`);
  }
  if (typeof entry.sha256 !== "string" || !/^[a-f\d]{64}$/i.test(entry.sha256)) {
    throw new Error(`Invalid SHA-256 pin for ${platform}`);
  }
  const archiveRoot = entry.archive.replace(/\.(?:zip|tar\.gz|tar\.xz)$/, "");
  const binaryPath = platform.startsWith("win32-") ? `${archiveRoot}/node.exe` : `${archiveRoot}/bin/node`;
  if (entry.binary !== binaryPath) throw new Error(`Unexpected binary path for ${platform}: ${entry.binary}`);

  const officialDigest = officialChecksums.get(entry.archive);
  if (!officialDigest) throw new Error(`Official checksum list has no entry for ${entry.archive}`);
  if (officialDigest !== entry.sha256.toLowerCase()) {
    throw new Error(`Official SHA-256 mismatch for ${platform} (${entry.archive}): pinned ${entry.sha256}, official ${officialDigest}`);
  }
}

console.log(`Node.js runtime checksum pins: green (${expectedPlatforms.length} official ${manifest.version} archives)`);
