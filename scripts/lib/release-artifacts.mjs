/**
 * Release artifact helpers: hashing, checksum listings and the distributable
 * manifest that `scripts/package.mjs` writes next to the staged payload.
 *
 * The functions here are deliberately free of repository knowledge so the tests
 * can drive them against a temporary directory. `package.mjs` owns the staging
 * layout and the "where do the pieces come from" decisions; this module owns the
 * integrity contract: a payload directory either matches its manifest exactly or
 * `verifyReleaseManifest` throws with every mismatch it found.
 *
 * The manifest is generated, not committed (`target/` is ignored), so there is no
 * cross-layer contract to keep in `packages/shared`: it is a build artifact whose
 * only consumer is the packaging pipeline itself.
 */

import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readdir, readFile, stat } from "node:fs/promises";
import { join } from "node:path";

/** Written into the payload root; listed in no manifest and never hashed. */
export const RELEASE_MANIFEST_FILE = "release-manifest.json";
export const CHECKSUMS_FILE = "SHA256SUMS.txt";
export const RELEASE_MANIFEST_SCHEMA_VERSION = 1;

const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const MAX_RELATIVE_PATH_LENGTH = 4096;

/**
 * A manifest path is only trusted when it is a relative, forward-slash path that
 * cannot leave the payload directory or address a Windows drive/UNC root. The
 * verifier refuses anything else instead of joining it onto the payload root.
 */
export function isSafeRelativePath(value) {
  if (typeof value !== "string" || value.length === 0 || value.length > MAX_RELATIVE_PATH_LENGTH) {
    return false;
  }
  if (value.includes("\\") || value.includes("\0")) return false;
  if (value.startsWith("/")) return false;
  if (/^[A-Za-z]:/.test(value)) return false;
  return value
    .split("/")
    .every((segment) => segment.length > 0 && segment !== "." && segment !== "..");
}

/** Lowercase hex SHA-256 of a file, streamed so large payloads do not sit in memory. */
export async function sha256File(path) {
  const hash = createHash("sha256");
  await new Promise((resolveResult, reject) => {
    const stream = createReadStream(path);
    stream.on("data", (chunk) => hash.update(chunk));
    stream.on("error", reject);
    stream.on("end", resolveResult);
  });
  return hash.digest("hex");
}

/**
 * Every regular file under `root`, as sorted forward-slash relative paths. The
 * manifest and checksum files themselves are skipped at the root so re-running the
 * packaging step does not try to hash its own output.
 */
export async function listPayloadFiles(root, options = {}) {
  const skip = new Set(options.skip ?? [RELEASE_MANIFEST_FILE, CHECKSUMS_FILE]);
  const results = [];

  async function walk(relative) {
    const absolute = relative === "" ? root : join(root, relative);
    const entries = await readdir(absolute, { withFileTypes: true });
    for (const entry of entries) {
      const childRelative = relative === "" ? entry.name : `${relative}/${entry.name}`;
      if (relative === "" && skip.has(entry.name)) continue;
      if (entry.isDirectory()) {
        await walk(childRelative);
      } else if (entry.isFile()) {
        results.push(childRelative);
      }
    }
  }

  await walk("");
  return results.sort();
}

/** GNU coreutils `sha256sum` format: `<hex>  <path>`, one line per file. */
export function renderChecksums(entries) {
  return (
    [...entries]
      .sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0))
      .map((entry) => `${entry.sha256}  ${entry.path}`)
      .join("\n") + "\n"
  );
}

export function parseChecksums(text) {
  const parsed = new Map();
  for (const line of text.split(/\r?\n/)) {
    if (line.length === 0) continue;
    const match = /^([0-9a-f]{64}) {2}(.+)$/.exec(line);
    if (!match) throw new Error(`unparseable ${CHECKSUMS_FILE} line: ${line}`);
    parsed.set(match[2], match[1]);
  }
  return parsed;
}

/**
 * Builds the manifest object from already-hashed payloads. Validation lives here so
 * a malformed manifest is rejected before it is ever written to disk.
 */
export function buildReleaseManifest(input) {
  const {
    version,
    agentVersion,
    platform,
    arch,
    payloadDirectory,
    sourceRevision,
    dirty,
    builtAt,
    payloads,
    launch,
    notes = [],
  } = input;

  if (typeof version !== "string" || version.length === 0) throw new Error("manifest requires a version");
  if (typeof agentVersion !== "string" || agentVersion.length === 0) {
    throw new Error("manifest requires an agentVersion");
  }
  if (typeof platform !== "string" || platform.length === 0) throw new Error("manifest requires a platform");
  if (typeof arch !== "string" || arch.length === 0) throw new Error("manifest requires an arch");
  if (!isSafeRelativePath(payloadDirectory) || payloadDirectory.includes("/")) {
    throw new Error(`payloadDirectory must be a single safe path segment: ${payloadDirectory}`);
  }
  if (!Array.isArray(payloads) || payloads.length === 0) {
    throw new Error("manifest requires at least one payload file");
  }

  const seen = new Set();
  const normalized = payloads.map((payload) => {
    if (payload === null || typeof payload !== "object") throw new Error("payload entry is not an object");
    if (!isSafeRelativePath(payload.path)) throw new Error(`unsafe payload path: ${payload.path}`);
    if (!payload.path.startsWith(`${payloadDirectory}/`)) {
      throw new Error(`payload path escapes ${payloadDirectory}: ${payload.path}`);
    }
    if (seen.has(payload.path)) throw new Error(`duplicate payload path: ${payload.path}`);
    seen.add(payload.path);
    if (!Number.isInteger(payload.size) || payload.size < 0) {
      throw new Error(`invalid size for ${payload.path}: ${payload.size}`);
    }
    if (typeof payload.sha256 !== "string" || !SHA256_PATTERN.test(payload.sha256)) {
      throw new Error(`invalid sha256 for ${payload.path}`);
    }
    return { path: payload.path, size: payload.size, sha256: payload.sha256 };
  });

  normalized.sort((left, right) => (left.path < right.path ? -1 : left.path > right.path ? 1 : 0));

  return {
    schemaVersion: RELEASE_MANIFEST_SCHEMA_VERSION,
    application: "yukinal",
    version,
    agentVersion,
    platform,
    arch,
    payloadDirectory,
    sourceRevision,
    dirty: Boolean(dirty),
    builtAt,
    payloads: normalized,
    launch,
    notes: [...notes],
  };
}

export function renderManifest(manifest) {
  return `${JSON.stringify(manifest, null, 2)}\n`;
}

/**
 * Re-reads every payload from disk and compares it with the manifest and the
 * checksum listing. Collects all problems and throws once, so a failing build
 * reports the whole picture instead of the first mismatch.
 */
export async function verifyReleaseManifest(root, manifest) {
  const problems = [];
  if (manifest === null || typeof manifest !== "object") {
    throw new Error("release manifest is missing or is not an object");
  }
  if (manifest.schemaVersion !== RELEASE_MANIFEST_SCHEMA_VERSION) {
    problems.push(
      `unsupported schemaVersion ${manifest.schemaVersion} (expected ${RELEASE_MANIFEST_SCHEMA_VERSION})`,
    );
  }
  if (!isSafeRelativePath(manifest.payloadDirectory) || manifest.payloadDirectory.includes("/")) {
    problems.push(`unsafe payloadDirectory: ${manifest.payloadDirectory}`);
  }

  const payloadDirectory = typeof manifest.payloadDirectory === "string" ? manifest.payloadDirectory : "";
  const payloads = Array.isArray(manifest.payloads) ? manifest.payloads : [];
  if (payloads.length === 0) problems.push("manifest lists no payload files");

  const expected = new Map();
  for (const payload of payloads) {
    if (payload === null || typeof payload !== "object" || !isSafeRelativePath(payload.path)) {
      problems.push(`unsafe payload path in manifest: ${JSON.stringify(payload?.path)}`);
      continue;
    }
    if (payloadDirectory.length > 0 && !payload.path.startsWith(`${payloadDirectory}/`)) {
      problems.push(`payload path escapes ${payloadDirectory}: ${payload.path}`);
      continue;
    }
    if (expected.has(payload.path)) {
      problems.push(`duplicate payload entry: ${payload.path}`);
      continue;
    }
    if (typeof payload.sha256 !== "string" || !SHA256_PATTERN.test(payload.sha256)) {
      problems.push(`invalid sha256 in manifest for ${payload.path}`);
      continue;
    }
    expected.set(payload.path, payload);
  }

  let checksums = new Map();
  try {
    checksums = parseChecksums(await readFile(join(root, CHECKSUMS_FILE), "utf8"));
  } catch (error) {
    problems.push(`could not read ${CHECKSUMS_FILE}: ${error.message}`);
  }

  for (const [path, payload] of expected) {
    if (!checksums.has(path)) problems.push(`${path}: missing from ${CHECKSUMS_FILE}`);
    else if (checksums.get(path) !== payload.sha256) {
      problems.push(`${path}: ${CHECKSUMS_FILE} sha256 disagrees with the manifest`);
    }
  }
  for (const path of checksums.keys()) {
    if (!expected.has(path)) problems.push(`${path}: in ${CHECKSUMS_FILE} but not in the manifest`);
  }

  let bytes = 0;
  for (const [path, payload] of expected) {
    let stats;
    try {
      stats = await stat(join(root, path));
    } catch {
      problems.push(`${path}: missing on disk`);
      continue;
    }
    if (!stats.isFile()) {
      problems.push(`${path}: not a regular file`);
      continue;
    }
    if (stats.size !== payload.size) {
      problems.push(`${path}: size ${stats.size} disagrees with the manifest (${payload.size})`);
    }
    const actual = await sha256File(join(root, path));
    if (actual !== payload.sha256) {
      problems.push(`${path}: sha256 ${actual} disagrees with the manifest`);
    }
    bytes += stats.size;
  }

  if (problems.length > 0) {
    const error = new Error(`release manifest verification failed:\n  - ${problems.join("\n  - ")}`);
    error.problems = problems;
    throw error;
  }
  return { files: expected.size, bytes };
}
