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
 * "Exactly" is enforced in both directions: every manifest entry must still hash the
 * same, and every file the walk finds on disk must be listed. Symlinks and NTFS
 * junctions are refused outright rather than followed, because following one would
 * hash bytes from outside the payload while attesting to a path inside it.
 *
 * The manifest is generated, not committed (`target/` is ignored), so there is no
 * cross-layer contract to keep in `packages/shared`: it is a build artifact whose
 * only consumer is the packaging pipeline itself.
 */

import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, readdir, readFile } from "node:fs/promises";
import { join } from "node:path";

/** Written into the payload root; listed in no manifest and never hashed. */
export const RELEASE_MANIFEST_FILE = "release-manifest.json";
export const CHECKSUMS_FILE = "SHA256SUMS.txt";
export const RELEASE_MANIFEST_SCHEMA_VERSION = 1;

/** How many unexpected paths a single failure message names before summarising. */
const MAX_REPORTED_EXTRAS = 10;

const SHA256_PATTERN = /^[0-9a-f]{64}$/;
const MAX_RELATIVE_PATH_LENGTH = 4096;

/**
 * Device names that Win32 resolves to hardware no matter which directory they
 * appear in, with or without an extension (`CON`, `NUL`, `aux`, `COM1.sys`…).
 * A payload path containing one cannot round-trip through extraction on Windows,
 * so it is refused before it can reach a manifest.
 */
const WINDOWS_RESERVED_NAMES = new Set([
  "con",
  "prn",
  "aux",
  "nul",
  ...Array.from({ length: 9 }, (_, index) => `com${index + 1}`),
  ...Array.from({ length: 9 }, (_, index) => `lpt${index + 1}`),
]);

const CONTROL_CHARACTERS = /[\u0000-\u001f\u007f]/;

/**
 * One path segment: no control characters, no `:` (a colon also addresses an NTFS
 * alternate data stream), no trailing dot or space (Win32 strips both, so
 * `NOTICE.` and `NOTICE` would collide), and no reserved device name.
 */
function isSafeSegment(segment) {
  if (segment.length === 0 || segment === "." || segment === "..") return false;
  if (CONTROL_CHARACTERS.test(segment) || segment.includes(":")) return false;
  if (segment.endsWith(".") || segment.endsWith(" ")) return false;
  return !WINDOWS_RESERVED_NAMES.has(segment.split(".")[0].toLowerCase());
}

/**
 * A manifest path is only trusted when it is a relative, forward-slash path whose
 * every segment is portable to Win32: it cannot leave the payload directory, cannot
 * address a Windows drive or UNC root, and cannot name something the extractor would
 * silently rename or resolve to a device. The verifier refuses anything else instead
 * of joining it onto the payload root.
 */
export function isSafeRelativePath(value) {
  if (typeof value !== "string" || value.length === 0 || value.length > MAX_RELATIVE_PATH_LENGTH) {
    return false;
  }
  if (value.includes("\\") || value.includes("\0")) return false;
  if (value.startsWith("/")) return false;
  if (/^[A-Za-z]:/.test(value)) return false;
  return value.split("/").every(isSafeSegment);
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
 *
 * Symlinks and NTFS junctions are never followed. `entry.isSymbolicLink()` is true
 * for both, so the walk stops there and hands the path to `onReparse`; the default
 * refuses the payload, because hashing through a reparse point would attest to a
 * path inside the payload while reading bytes that live outside it. Callers that
 * gather diagnostics instead of validating (the verifier) can pass their own
 * `onReparse` to collect every offending path before failing once.
 */
export async function listPayloadFiles(root, options = {}) {
  const skip = new Set(options.skip ?? [RELEASE_MANIFEST_FILE, CHECKSUMS_FILE]);
  const onReparse = options.onReparse ?? ((path) => {
    const error = new Error(`payload path is a reparse point: ${path}`);
    error.reparsePoint = path;
    throw error;
  });
  const results = [];

  async function walk(relative) {
    const absolute = relative === "" ? root : join(root, relative);
    const entries = await readdir(absolute, { withFileTypes: true });
    for (const entry of entries) {
      const childRelative = relative === "" ? entry.name : `${relative}/${entry.name}`;
      if (relative === "" && skip.has(entry.name)) continue;
      if (entry.isSymbolicLink()) {
        onReparse(childRelative);
      } else if (entry.isDirectory()) {
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
    inputs = [],
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

  // Provenance for the build inputs the payload was assembled from. Recorded so a
  // reader can tell which binary a manifest describes without re-running git, and so a
  // stale input is visible in the artifact rather than only in the build log. The section
  // is cross-checked against `payloads` by `inputConsistencyProblems`; omitting it stays
  // legal (a manifest written before provenance existed), but a malformed one is refused
  // rather than silently dropped — dropping it would lose the whole point.
  if (inputs !== undefined && !Array.isArray(inputs)) {
    throw new Error(`manifest inputs must be an array when present: ${JSON.stringify(inputs)}`);
  }
  const normalizedInputs = (inputs ?? []).map((entry) => {
    if (entry === null || typeof entry !== "object") throw new Error("manifest input is not an object");
    for (const key of ["role", "path", "sourceDirectory", "sha256"]) {
      if (typeof entry[key] !== "string" || entry[key].length === 0) {
        throw new Error(`manifest input is missing ${key}: ${JSON.stringify(entry)}`);
      }
    }
    // Same portability rule as payload paths: provenance is read by tools that join
    // it onto a checkout, so an escaping or Win32-ambiguous path is refused here too.
    if (!isSafeRelativePath(entry.path)) {
      throw new Error(`manifest input path is unsafe: ${entry.path}`);
    }
    if (!isSafeRelativePath(entry.sourceDirectory)) {
      throw new Error(`manifest input sourceDirectory is unsafe: ${entry.sourceDirectory}`);
    }
    if (!SHA256_PATTERN.test(entry.sha256)) {
      throw new Error(`invalid input sha256 for ${entry.path}`);
    }
    return {
      role: entry.role,
      path: entry.path,
      sourceDirectory: entry.sourceDirectory,
      sourceRevision: typeof entry.sourceRevision === "string" ? entry.sourceRevision : "unknown",
      modifiedAt: typeof entry.modifiedAt === "string" ? entry.modifiedAt : null,
      // The staleness check compares the artifact against the directory's last *commit*,
      // so an uncommitted edit in that directory is invisible to it. Recording the flag
      // keeps the manifest from implying the binary matches a revision it may predate.
      dirty: entry.dirty === true,
      sha256: entry.sha256,
    };
  });
  normalizedInputs.sort((left, right) => (left.role < right.role ? -1 : left.role > right.role ? 1 : 0));

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
    inputs: normalizedInputs,
    launch,
    notes: [...notes],
  };
}

export function renderManifest(manifest) {
  return `${JSON.stringify(manifest, null, 2)}\n`;
}

/**
 * Compares one build input's modification time with the last commit that touched the
 * source directory it was built from.
 *
 * The manifest used to attribute the whole payload to `HEAD`. That is not the same
 * claim: the three executables are staged from whatever happens to sit in the build
 * directories, and a binary built before the last source change to its directory is
 * silently presented as a product of the current revision. `dirty` cannot catch this
 * because it only reports that the worktree has uncommitted changes.
 *
 * Returns a problem string, or `null` when the input is fresh (or when either
 * timestamp is unavailable, which is reported separately rather than guessed at).
 */
export function staleInputProblem(entry) {
  const { role, artifactPath, artifactTime, sourceDirectory, sourceTime } = entry;
  if (!Number.isFinite(artifactTime) || !Number.isFinite(sourceTime)) return null;
  if (artifactTime >= sourceTime) return null;
  return (
    `${role} is older than the last commit that touched ${sourceDirectory}: ` +
    `${artifactPath} was built ${new Date(artifactTime).toISOString()}, but ` +
    `${sourceDirectory} last changed ${new Date(sourceTime).toISOString()}`
  );
}

/**
 * Checks the manifest's `inputs` section against its own `payloads` listing.
 *
 * Provenance is only worth recording if something reads it back. Left unverified it
 * is a liability: an edited or stale `inputs[].sha256` looks like evidence. This is
 * deliberately disk-free — it compares two sections of one manifest — so `--manifest`
 * can run it against the freshly hashed payloads before writing anything.
 *
 * `undefined` inputs (a manifest written before provenance existed) is legal and
 * yields no problems; a malformed section does not.
 */
export function inputConsistencyProblems(manifest) {
  const problems = [];
  if (manifest === null || typeof manifest !== "object") return ["manifest is not an object"];
  if (manifest.inputs === undefined) return problems;
  if (!Array.isArray(manifest.inputs)) return ["manifest.inputs is not an array"];

  const payloads = Array.isArray(manifest.payloads) ? manifest.payloads : [];
  const roles = new Set();
  for (const entry of manifest.inputs) {
    if (entry === null || typeof entry !== "object") {
      problems.push("manifest.inputs contains a non-object entry");
      continue;
    }
    if (typeof entry.role !== "string" || entry.role.length === 0) {
      problems.push("manifest.inputs entry has no role");
    } else if (roles.has(entry.role)) {
      problems.push(`manifest.inputs lists ${entry.role} twice`);
    } else {
      roles.add(entry.role);
    }
    if (!isSafeRelativePath(entry.path)) {
      problems.push(`unsafe manifest.inputs path: ${JSON.stringify(entry.path)}`);
      continue;
    }
    if (!isSafeRelativePath(entry.sourceDirectory)) {
      problems.push(`unsafe manifest.inputs sourceDirectory: ${JSON.stringify(entry.sourceDirectory)}`);
    }
    if (typeof entry.sha256 !== "string" || !SHA256_PATTERN.test(entry.sha256)) {
      problems.push(`invalid manifest.inputs sha256 for ${entry.path}`);
      continue;
    }
    // The input is a file inside the payload, so the manifest must agree with itself
    // about its digest. Without this the section survives any edit.
    const listed = payloads.find((payload) => payload?.path === entry.path);
    if (listed === undefined) {
      problems.push(`manifest.inputs ${entry.role} names a path the manifest does not list: ${entry.path}`);
    } else if (listed.sha256 !== entry.sha256) {
      problems.push(
        `manifest.inputs ${entry.role} sha256 disagrees with the payload entry for ${entry.path}`,
      );
    }
  }
  return problems;
}

/**
 * Reads the `Name`/`Version` pair from a Python `METADATA` file's RFC 822 headers.
 *
 * Deliberately a tiny reader rather than a dependency: the header block is
 * `Key: value` with folded continuations, and only two keys are needed. A missing or
 * unreadable `Name` falls back to the `.dist-info` directory stem, which is the same
 * fallback the collector uses, so the two inventories stay comparable.
 */
export function readDistributionIdentity(metadataText, directoryName) {
  const fields = new Map();
  let current = null;
  for (const rawLine of metadataText.split(/\r?\n/)) {
    if (rawLine === "") break; // the header block ends at the first blank line
    if (/^\s/.test(rawLine)) {
      if (current !== null) fields.set(current, `${fields.get(current)} ${rawLine.trim()}`);
      continue;
    }
    const separator = rawLine.indexOf(":");
    if (separator <= 0) continue;
    current = rawLine.slice(0, separator).trim().toLowerCase();
    if (!fields.has(current)) fields.set(current, rawLine.slice(separator + 1).trim());
  }
  const stem = directoryName.endsWith(".dist-info")
    ? directoryName.slice(0, -".dist-info".length)
    : directoryName;
  return { name: fields.get("name") ?? stem, version: fields.get("version") ?? "unknown" };
}

/**
 * Checks that the Python licence notice shipped next to the Agent describes the
 * distributions actually present in the staged onedir.
 *
 * The notice is generated by `collect-python-licenses.py` from the frozen payload, so
 * the two inventories are supposed to agree. Nothing forced that: staging copied
 * `agent/runtime/THIRD_PARTY_LICENSES.txt` from the checkout, so packaging a *different*
 * onedir with `--agent-runtime` shipped a notice describing the other tree, byte for
 * byte, with no gate turning red. Comparing the notice's `## Name Version` sections with
 * the staged `.dist-info` metadata catches both that and a stale notice.
 *
 * Returns a list of human-readable problems; empty means consistent. A payload with no
 * `.dist-info` metadata at all is itself a problem rather than a vacuous pass: the
 * notice claims attribution for Python distributions, so a payload that has none is
 * either not a frozen Agent onedir or is incomplete. `collect-python-licenses.py`
 * fails closed on the same condition, so the two agree.
 */
export function agentNoticeProblems(noticeText, distributions) {
  const problems = [];
  if (typeof noticeText !== "string") return ["the Python licence notice could not be read"];
  if (distributions.length === 0) {
    return [
      "no *.dist-info metadata was found in the staged Agent payload, so the Python " +
        "licence notice describes distributions the payload does not contain",
    ];
  }

  const announced = new Map();
  for (const line of noticeText.split(/\r?\n/)) {
    const match = /^## (.+?) (.+)$/.exec(line);
    if (match === null) continue;
    announced.set(`${match[1]} ${match[2]}`, false);
  }
  for (const { name, version } of distributions) {
    const key = `${name} ${version}`;
    if (!announced.has(key)) {
      problems.push(
        `the Python licence notice does not mention ${key}, which is present in the staged payload`,
      );
    } else {
      announced.set(key, true);
    }
  }
  for (const [key, matched] of announced) {
    if (!matched) {
      problems.push(
        `the Python licence notice mentions ${key}, which is not present in the staged payload`,
      );
    }
  }
  return problems;
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

  // The provenance section describes bytes the manifest also lists, so it is checked
  // against them rather than read as advisory text. A section nothing verifies is worse
  // than no section: a fabricated or stale `inputs` entry reads as evidence. Absent
  // entirely (manifests written before provenance existed) stays legal.
  problems.push(...inputConsistencyProblems(manifest));

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

  // Walk the payload itself. `expected` alone can only prove that listed files are
  // unchanged; anything added after staging — a dropped binary, a whole stale sibling
  // payload directory — is invisible unless the disk is enumerated and compared.
  let scanned = [];
  const reparsePoints = new Set();
  if (payloadDirectory.length > 0 && isSafeRelativePath(payloadDirectory)) {
    try {
      scanned = await listPayloadFiles(root, {
        onReparse: (path) => reparsePoints.add(path),
      });
    } catch (error) {
      problems.push(`could not enumerate the payload: ${error.message}`);
    }
    const extras = scanned.filter((path) => !expected.has(path));
    if (extras.length > 0) {
      const named = extras.slice(0, MAX_REPORTED_EXTRAS).join(", ");
      const rest = extras.length > MAX_REPORTED_EXTRAS ? `, and ${extras.length - MAX_REPORTED_EXTRAS} more` : "";
      problems.push(`${extras.length} file(s) on disk are not in the manifest: ${named}${rest}`);
    }
  }

  let bytes = 0;
  for (const [path, payload] of expected) {
    let stats;
    try {
      // lstat, not stat: following a reparse point here would hash bytes from outside
      // the payload while reporting a path inside it.
      stats = await lstat(join(root, path));
    } catch {
      problems.push(`${path}: missing on disk`);
      continue;
    }
    if (stats.isSymbolicLink()) {
      reparsePoints.add(path);
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

  for (const path of [...reparsePoints].sort()) {
    problems.push(`payload path is a reparse point: ${path}`);
  }

  // The launch section is what a launcher reads to start the payload, so a manifest
  // that claims a UI directory it does not ship describes an unlaunchable build: the
  // host exits 2 when `--ui-dir` has no readable index.html.
  const uiDirectory = manifest.launch?.uiDirectory;
  if (uiDirectory !== undefined) {
    if (typeof uiDirectory !== "string" || !isSafeRelativePath(uiDirectory)) {
      problems.push(`unsafe launch.uiDirectory: ${JSON.stringify(uiDirectory)}`);
    } else if (payloadDirectory.length > 0 && !uiDirectory.startsWith(`${payloadDirectory}/`)) {
      problems.push(`launch.uiDirectory escapes ${payloadDirectory}: ${uiDirectory}`);
    } else if (!expected.has(`${uiDirectory}/index.html`)) {
      problems.push(`launch.uiDirectory ${uiDirectory} ships no index.html in the manifest`);
    }
  }

  // `launch.agentEntry` is the other promise the host depends on: `YUKINAL_AGENT_ENTRY`
  // starts the sidecar, so a manifest that names a path it does not ship is as broken as
  // a UI directory without an entry document. This also makes the licence notice
  // load-bearing: it sits in the same directory as the entry, so deleting it is caught
  // here rather than shipping an unattributed Python payload through a green gate.
  const agentEntry = manifest.launch?.agentEntry;
  if (agentEntry !== undefined) {
    if (typeof agentEntry !== "string" || !isSafeRelativePath(agentEntry)) {
      problems.push(`unsafe launch.agentEntry: ${JSON.stringify(agentEntry)}`);
    } else if (payloadDirectory.length > 0 && !agentEntry.startsWith(`${payloadDirectory}/`)) {
      problems.push(`launch.agentEntry escapes ${payloadDirectory}: ${agentEntry}`);
    } else if (!expected.has(agentEntry)) {
      problems.push(`launch.agentEntry ${agentEntry} is not in the manifest`);
    } else {
      const notice = `${agentEntry.slice(0, agentEntry.lastIndexOf("/"))}/THIRD_PARTY_LICENSES.txt`;
      if (!expected.has(notice)) {
        problems.push(
          `the Python Agent entry ${agentEntry} ships without ${notice}; the bundled ` +
            `distributions would be unattributed`,
        );
      }
    }
  }

  if (problems.length > 0) {
    const error = new Error(`release manifest verification failed:\n  - ${problems.join("\n  - ")}`);
    error.problems = problems;
    throw error;
  }
  return { files: expected.size, bytes, scanned: scanned.length };
}
