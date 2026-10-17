#!/usr/bin/env node
/**
 * C++ desktop host packaging pipeline (the replacement for the retired Tauri
 * `tauri build` path).
 *
 * `pnpm package` stages a self-contained distributable payload from the pieces the
 * repository already produces — the C++ desktop host, the frozen Python Agent
 * (PyInstaller onedir) and the C++ SSH service — then writes `SHA256SUMS.txt` and
 * `release-manifest.json` next to it. `pnpm package -- --verify` re-reads every
 * payload and fails when a byte changed or a file was added or removed.
 *
 * What this deliberately does *not* do: it does not wrap the payload in an
 * installer. NSIS/MSI/DMG/deb generation, code signing, notarisation and the
 * three-platform install/upgrade/data-retention acceptance still need their
 * tooling and real machines, so the manifest records that gap instead of implying
 * a release. Missing host, Agent or SSH-service artifacts fail the run instead of
 * producing a partial payload.
 *
 * Layout written under `target/release/bundle/` (override with `--out`):
 *
 *   release-manifest.json
 *   SHA256SUMS.txt
 *   yukinal-<version>-<platform>-<arch>/
 *     yukinal-desktop-host[.exe]
 *     python-agent/yukinal-agent[.exe]
 *     python-agent/THIRD_PARTY_LICENSES.txt
 *     ssh-service/yukinal-ssh-service[.exe]
 *     ssh-service/OPENSSL-LICENSE.txt
 *     NOTICE
 *     LICENSE
 *
 * The launcher starts the host with `YUKINAL_AGENT_ENTRY` pointed at
 * `python-agent/yukinal-agent[.exe]` (the frozen onedir entrypoint; no Python
 * install is required) and `YUKINAL_SSH_SERVICE_BINARY` pointed at
 * `ssh-service/yukinal-ssh-service[.exe]`. Those names are the ones the C++ host
 * already reads in `configure_sidecar()` and the terminal/SFTP backends.
 */

import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync } from "node:fs";
import { cp, readFile, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  CHECKSUMS_FILE,
  RELEASE_MANIFEST_FILE,
  buildReleaseManifest,
  listPayloadFiles,
  renderChecksums,
  renderManifest,
  sha256File,
  verifyReleaseManifest,
} from "./lib/release-artifacts.mjs";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const executableSuffix = process.platform === "win32" ? ".exe" : "";
const NATIVE_BUILD_DIRECTORIES = [
  "native/build-nmake",
  "native/build-cpp-host",
  "native/build-vs",
  "native/build-desktop",
  "native/build",
];

const USAGE = `Usage: node scripts/package.mjs [mode] [options]

Modes (default: stage):
  (default)       Stage the payload from existing artifacts, then write
                  SHA256SUMS.txt and release-manifest.json.
  --manifest      Recompute the checksums and manifest for an already-staged
                  payload without copying anything.
  --verify        Re-read every payload and fail if it disagrees with the
                  manifest or the checksum listing.

Options:
  --out <dir>              Output root (default target/release/bundle).
  --host-exe <path>        Explicit yukinal-desktop-host executable.
  --agent-runtime <dir>    Explicit frozen Agent onedir (contains yukinal-agent).
  --ssh-service-exe <path> Explicit yukinal-ssh-service executable.
  -h, --help               Show this message.

Environment overrides: YUKINAL_DESKTOP_HOST_BINARY (host executable),
YUKINAL_PACKAGE_OUT (output root).`;

function fail(message) {
  console.error(message);
  process.exit(1);
}

function parseArguments(argv) {
  const options = {
    mode: "stage",
    out: process.env.YUKINAL_PACKAGE_OUT ?? join(root, "target", "release", "bundle"),
    hostExe: process.env.YUKINAL_DESKTOP_HOST_BINARY,
    agentRuntime: undefined,
    sshServiceExe: undefined,
  };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    const value = () => {
      const next = argv[index + 1];
      if (next === undefined || next.startsWith("--")) fail(`${argument} requires a value`);
      index += 1;
      return next;
    };
    if (argument === "-h" || argument === "--help") {
      console.log(USAGE);
      process.exit(0);
    } else if (argument === "--verify") options.mode = "verify";
    else if (argument === "--manifest") options.mode = "manifest";
    else if (argument === "--out") options.out = value();
    else if (argument === "--host-exe") options.hostExe = value();
    else if (argument === "--agent-runtime") options.agentRuntime = value();
    else if (argument === "--ssh-service-exe") options.sshServiceExe = value();
    else fail(`unknown argument: ${argument}\n\n${USAGE}`);
  }
  return options;
}

function readApplicationVersion() {
  const source = readFileSync(join(root, "packages", "shared", "src", "version.ts"), "utf8");
  const match = /APP_VERSION\s*=\s*"([^"]+)"/.exec(source);
  if (!match) fail("Could not read APP_VERSION from packages/shared/src/version.ts");
  return match[1];
}

function readAgentVersion() {
  const source = readFileSync(join(root, "agent", "pyproject.toml"), "utf8");
  const match = /^version\s*=\s*"([^"]+)"/m.exec(source);
  return match ? match[1] : "unknown";
}

function git(arguments_) {
  const result = spawnSync("git", arguments_, { cwd: root, encoding: "utf8", windowsHide: true });
  if (result.error !== undefined || result.status !== 0) return undefined;
  return result.stdout.trim();
}

function sourceRevision() {
  return git(["rev-parse", "HEAD"]) ?? "unknown";
}

/** Includes untracked-but-not-ignored files, so a build from a dirty tree is marked. */
function worktreeIsDirty() {
  const status = git(["status", "--porcelain", "--untracked-files=normal"]);
  return status === undefined ? true : status.length > 0;
}

function firstExisting(candidates) {
  return candidates.find((candidate) => existsSync(candidate));
}

function requireFile(path, guidance) {
  if (!existsSync(path) || !statSync(path).isFile()) {
    fail(`Required packaging input is missing: ${path}\n  ${guidance}`);
  }
  return path;
}

function resolveHostExecutable(options) {
  if (options.hostExe) {
    return requireFile(
      resolve(root, options.hostExe),
      "Point --host-exe at the built host, or fix YUKINAL_DESKTOP_HOST_BINARY.",
    );
  }
  const name = `yukinal-desktop-host${executableSuffix}`;
  const candidates = NATIVE_BUILD_DIRECTORIES.flatMap((directory) => [
    join(root, directory, "Release", name),
    join(root, directory, name),
  ]);
  const found = firstExisting(candidates);
  if (found === undefined) {
    fail(
      `The C++ desktop host was not found in any known build directory.\n` +
        `  Build it first (see docs/cpp-desktop-handoff.md) or pass --host-exe.`,
    );
  }
  return found;
}

function resolveAgentRuntime(options) {
  const runtime = options.agentRuntime
    ? resolve(root, options.agentRuntime)
    : join(root, "agent", "runtime", "yukinal-agent");
  if (!existsSync(runtime)) {
    fail(
      `The frozen Python Agent runtime was not found: ${runtime}\n` +
        `  Build it with: pnpm build:python-agent (needs the agent[package] extra and PyInstaller).`,
    );
  }
  const entry = join(runtime, `yukinal-agent${executableSuffix}`);
  requireFile(entry, "The onedir runtime must contain the yukinal-agent executable.");
  return runtime;
}

function resolveSshServiceExecutable(options) {
  if (options.sshServiceExe) {
    return requireFile(
      resolve(root, options.sshServiceExe),
      "Point --ssh-service-exe at the built SSH service.",
    );
  }
  const name = `yukinal-ssh-service${executableSuffix}`;
  const candidates = [
    join(root, "apps", "desktop", "resources", "ssh-service", name),
    ...NATIVE_BUILD_DIRECTORIES.flatMap((directory) => [
      join(root, directory, "Release", name),
      join(root, directory, name),
    ]),
  ];
  const found = firstExisting(candidates);
  if (found === undefined) {
    fail(
      `The C++ SSH service was not found.\n` +
        `  Build it with: pnpm build:ssh-service, or pass --ssh-service-exe.`,
    );
  }
  return found;
}

function opensslLicensePath() {
  const staged = join(root, "apps", "desktop", "resources", "ssh-service", "OPENSSL-LICENSE.txt");
  if (existsSync(staged)) return staged;
  return requireFile(
    join(root, "native", "licenses", "OPENSSL-LICENSE.txt"),
    "The bundled OpenSSL licence text is part of the SSH service payload.",
  );
}

async function collectPayloads(out, payloadDirectory) {
  const payloadRoot = join(out, payloadDirectory);
  if (!existsSync(payloadRoot)) fail(`Payload directory does not exist: ${payloadRoot}`);
  const relative = await listPayloadFiles(payloadRoot);
  if (relative.length === 0) fail(`Payload directory is empty: ${payloadRoot}`);
  const payloads = [];
  for (const name of relative) {
    const absolute = join(payloadRoot, name);
    payloads.push({
      path: `${payloadDirectory}/${name}`,
      size: statSync(absolute).size,
      sha256: await sha256File(absolute),
    });
  }
  return payloads;
}

function manifestNotes() {
  return [
    "This directory is a staged distributable, not an installed release.",
    "No installer is generated (NSIS/MSI/DMG/deb, upgrade and data retention are not implemented).",
    "Artifacts are unsigned; signing and notarisation are not configured.",
  ];
}

function launchSection(payloadDirectory) {
  return {
    hostExecutable: `${payloadDirectory}/yukinal-desktop-host${executableSuffix}`,
    agentEntry: `${payloadDirectory}/python-agent/yukinal-agent${executableSuffix}`,
    sshServiceBinary: `${payloadDirectory}/ssh-service/yukinal-ssh-service${executableSuffix}`,
    environmentVariables: ["YUKINAL_AGENT_ENTRY", "YUKINAL_SSH_SERVICE_BINARY", "YUKINAL_DATA_DIR"],
  };
}

async function writeManifestAndChecksums(out, manifest) {
  mkdirSync(out, { recursive: true });
  await writeFile(join(out, CHECKSUMS_FILE), renderChecksums(manifest.payloads));
  await writeFile(join(out, RELEASE_MANIFEST_FILE), renderManifest(manifest));
}

async function stage(options) {
  const version = readApplicationVersion();
  const agentVersion = readAgentVersion();
  const payloadDirectory = `yukinal-${version}-${process.platform}-${process.arch}`;
  const out = resolve(root, options.out);
  const payloadRoot = join(out, payloadDirectory);

  const hostExecutable = resolveHostExecutable(options);
  const agentRuntime = resolveAgentRuntime(options);
  const sshServiceExecutable = resolveSshServiceExecutable(options);

  console.log(`Staging ${payloadDirectory}`);
  rmSync(payloadRoot, { recursive: true, force: true });
  mkdirSync(join(payloadRoot, "ssh-service"), { recursive: true });

  copyFileSync(hostExecutable, join(payloadRoot, basename(hostExecutable)));
  await cp(agentRuntime, join(payloadRoot, "python-agent"), { recursive: true });
  const agentLicenses = join(root, "agent", "runtime", "THIRD_PARTY_LICENSES.txt");
  if (existsSync(agentLicenses)) {
    copyFileSync(agentLicenses, join(payloadRoot, "python-agent", "THIRD_PARTY_LICENSES.txt"));
  }
  copyFileSync(sshServiceExecutable, join(payloadRoot, "ssh-service", basename(sshServiceExecutable)));
  copyFileSync(opensslLicensePath(), join(payloadRoot, "ssh-service", "OPENSSL-LICENSE.txt"));
  copyFileSync(join(root, "NOTICE"), join(payloadRoot, "NOTICE"));
  copyFileSync(join(root, "LICENSE"), join(payloadRoot, "LICENSE"));

  const manifest = buildReleaseManifest({
    version,
    agentVersion,
    platform: process.platform,
    arch: process.arch,
    payloadDirectory,
    sourceRevision: sourceRevision(),
    dirty: worktreeIsDirty(),
    builtAt: new Date().toISOString(),
    payloads: await collectPayloads(out, payloadDirectory),
    launch: launchSection(payloadDirectory),
    notes: manifestNotes(),
  });
  await writeManifestAndChecksums(out, manifest);

  console.log(
    `Staged ${manifest.payloads.length} file(s) into ${payloadRoot}\n` +
      `Wrote ${join(out, CHECKSUMS_FILE)} and ${join(out, RELEASE_MANIFEST_FILE)}`,
  );
  console.log(
    "Launch the host with YUKINAL_AGENT_ENTRY and YUKINAL_SSH_SERVICE_BINARY pointing at " +
      "the staged python-agent and ssh-service executables.",
  );
}

async function refreshManifest(options) {
  const out = resolve(root, options.out);
  const manifestPath = join(out, RELEASE_MANIFEST_FILE);
  if (!existsSync(manifestPath)) fail(`No manifest to refresh: ${manifestPath}`);
  const existing = JSON.parse(await readFile(manifestPath, "utf8"));
  const manifest = buildReleaseManifest({
    version: existing.version,
    agentVersion: existing.agentVersion,
    platform: existing.platform,
    arch: existing.arch,
    payloadDirectory: existing.payloadDirectory,
    sourceRevision: sourceRevision(),
    dirty: worktreeIsDirty(),
    builtAt: new Date().toISOString(),
    payloads: await collectPayloads(out, existing.payloadDirectory),
    launch: existing.launch,
    notes: Array.isArray(existing.notes) ? existing.notes : manifestNotes(),
  });
  await writeManifestAndChecksums(out, manifest);
  console.log(`Refreshed ${manifest.payloads.length} file(s) in ${manifestPath}`);
}

async function verify(options) {
  const out = resolve(root, options.out);
  const manifestPath = join(out, RELEASE_MANIFEST_FILE);
  if (!existsSync(manifestPath)) fail(`No manifest to verify: ${manifestPath}`);
  const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
  let result;
  try {
    result = await verifyReleaseManifest(out, manifest);
  } catch (error) {
    fail(error instanceof Error ? error.message : String(error));
  }
  console.log(
    `Verified ${result.files} file(s), ${result.bytes} byte(s) against ${manifestPath}`,
  );
}

const options = parseArguments(process.argv.slice(2));
if (!existsSync(join(root, "pnpm-workspace.yaml"))) fail("Run this script from the repository root.");

if (options.mode === "verify") await verify(options);
else if (options.mode === "manifest") await refreshManifest(options);
else await stage(options);
