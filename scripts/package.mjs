#!/usr/bin/env node
/**
 * C++ desktop host packaging pipeline (the replacement for the retired Rust/Tauri
 * desktop host, which no longer exists in this repository).
 *
 * `pnpm package` stages a self-contained distributable payload from the pieces the
 * repository already produces — the C++ desktop host, the frozen Python Agent
 * (PyInstaller onedir) and the C++ SSH service — then writes `SHA256SUMS.txt` and
 * `release-manifest.json` next to it. `pnpm package -- --verify` re-reads every
 * payload and fails when a byte changed, a file was added or removed, or the payload
 * contains a symlink/junction instead of a real file.
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
 *     ui/index.html
 *     ui/assets/...
 *     python-agent/yukinal-agent[.exe]
 *     python-agent/THIRD_PARTY_LICENSES.txt
 *     ssh-service/yukinal-ssh-service[.exe]
 *     ssh-service/OPENSSL-LICENSE.txt
 *     NOTICE
 *     LICENSE
 *
 * The launcher starts the host with `--ui-dir <payload>/ui` (the host refuses to
 * start without exactly one of `--ui-dir`/`--dev-url` and a readable
 * `index.html`; see `resolve_ui_url` in `native/desktop_host/src/main.cpp`),
 * `YUKINAL_AGENT_ENTRY` pointed at `python-agent/yukinal-agent[.exe]` (the frozen
 * onedir entrypoint; no Python install is required) and
 * `YUKINAL_SSH_SERVICE_BINARY` pointed at
 * `ssh-service/yukinal-ssh-service[.exe]`. Those variable names are the ones the
 * C++ host already reads in `configure_sidecar()` and the terminal/SFTP backends.
 */

import { spawnSync } from "node:child_process";
import { copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { cp, readdir, readFile, writeFile } from "node:fs/promises";
import { basename, dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

import {
  CHECKSUMS_FILE,
  RELEASE_MANIFEST_FILE,
  agentNoticeProblems,
  buildReleaseManifest,
  inputConsistencyProblems,
  listPayloadFiles,
  readDistributionIdentity,
  renderChecksums,
  renderManifest,
  sha256File,
  staleInputProblem,
  verifyReleaseManifest,
} from "./lib/release-artifacts.mjs";
import { renderUnixLauncher, renderWindowsLauncher } from "./lib/launchers.mjs";

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
  --ui-dir <dir>           Explicit built desktop UI (must contain index.html).
  --allow-stale-inputs     Stage executables that predate the last commit touching
                           their source directory. Recorded in the manifest notes.
  -h, --help               Show this message.

Environment overrides: YUKINAL_DESKTOP_HOST_BINARY (host executable),
YUKINAL_DESKTOP_UI_DIR (built desktop UI), YUKINAL_PACKAGE_OUT (output root).`;

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
    uiDir: process.env.YUKINAL_DESKTOP_UI_DIR,
    allowStaleInputs: false,
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
    else if (argument === "--ui-dir") options.uiDir = value();
    else if (argument === "--allow-stale-inputs") options.allowStaleInputs = true;
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

/** Commit date of the last commit that touched `directory`, in epoch milliseconds. */
function directoryCommitTime(directory) {
  const iso = git(["log", "-1", "--format=%cI", "--", directory]);
  if (iso === undefined || iso.length === 0) return undefined;
  const parsed = Date.parse(iso);
  return Number.isFinite(parsed) ? parsed : undefined;
}

function directoryCommitSha(directory) {
  return git(["log", "-1", "--format=%h", "--", directory]) ?? "unknown";
}

/**
 * Whether the source directory has uncommitted changes. Freshness is judged against the
 * last commit, so an uncommitted edit to that directory is invisible to the staleness
 * check: the build could be older than the working tree and still look fresh. This flag
 * exists so the manifest says so instead of implying "built from exactly this revision".
 */
function directoryIsDirty(directory) {
  const status = git(["status", "--porcelain", "--", directory]);
  return status === undefined ? undefined : status.length > 0;
}

/**
 * Records what each staged input actually is: its bytes, its modification time and
 * the last commit that touched the source directory it was built from. Without this
 * the manifest attributes the whole payload to `HEAD`, which is a stronger claim than
 * the staging step can support — a binary built before the newest source change to
 * its own directory would still be published as a product of the current revision.
 *
 * Returns the provenance entries and the list of stale ones; `stage()` decides
 * whether staleness is fatal (it is, unless `--allow-stale-inputs` was passed).
 */
async function collectBuildInputs(entries) {
  const recorded = [];
  const stale = [];
  for (const entry of entries) {
    const stats = statSync(entry.artifactPath);
    // A directory input (the built UI) is dated by its newest file: Vite only rewrites
    // `index.html` when its contents change, so the entry document's own mtime can
    // predate a rebuild that replaced every asset around it. The recorded sha256 is
    // still the entry document, which is the file `launch.uiDirectory` promises.
    const artifactTime = entry.directory ? await newestFileTime(entry.artifactPath) : stats.mtimeMs;
    const sourceTime = directoryCommitTime(entry.sourceDirectory);
    const problem = staleInputProblem({
      role: entry.role,
      artifactPath: entry.artifactPath,
      artifactTime,
      sourceDirectory: entry.sourceDirectory,
      sourceTime,
    });
    if (problem !== null) stale.push(problem);
    recorded.push({
      role: entry.role,
      path: entry.payloadPath,
      sourceDirectory: entry.sourceDirectory,
      sourceRevision: directoryCommitSha(entry.sourceDirectory),
      modifiedAt: new Date(artifactTime).toISOString(),
      dirty: directoryIsDirty(entry.sourceDirectory) ?? true,
      sha256: await sha256File(entry.directory ? join(entry.artifactPath, "index.html") : entry.artifactPath),
    });
  }
  return { recorded, stale };
}

/** Modification time of the newest file below `directory`, or `NaN` when it is empty. */
async function newestFileTime(directory) {
  const entries = await readdir(directory, { withFileTypes: true });
  let newest = Number.NaN;
  for (const entry of entries) {
    const child = join(directory, entry.name);
    const time = entry.isDirectory() ? await newestFileTime(child) : statSync(child).mtimeMs;
    if (Number.isFinite(time) && (!Number.isFinite(newest) || time > newest)) newest = time;
  }
  return newest;
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

/** Same reparse-point refusal as the UI source: `cp` preserves links, it does not resolve them. */
async function assertAgentRuntimeIsStageable(runtime) {
  await assertNoReparsePoints(runtime, `The frozen Agent runtime ${runtime}`);
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

/** Refuses a staging source that contains a symlink/junction anywhere inside it. */
async function assertNoReparsePoints(directory, label) {
  const reparsePoints = [];
  await listPayloadFiles(directory, { onReparse: (path) => reparsePoints.push(path) });
  if (reparsePoints.length > 0) {
    fail(
      `${label} contains ${reparsePoints.length} symlink/junction(s) and cannot be staged:\n` +
        reparsePoints.map((path) => `  ${path}`).join("\n") +
        `\n  Replace them with real files or directories; the manifest only covers regular files.`,
    );
  }
}

/**
 * The built desktop UI. The host refuses to start without exactly one of
 * `--ui-dir`/`--dev-url` and a readable `index.html`, so a payload staged without
 * these files could be fully "verified" and still never launch; require them here,
 * with the same fail-closed treatment as the three executables. Reparse points are
 * refused up front because `cp(..., { dereference: false })` preserves them, and a
 * copied link either fails outright (Windows without the symlink privilege) or
 * silently points the payload at whatever the source pointed at.
 */
async function resolveUiDirectory(options) {
  const ui = options.uiDir ? resolve(root, options.uiDir) : join(root, "apps", "desktop", "dist");
  if (!existsSync(ui) || !statSync(ui).isDirectory()) {
    fail(
      `The built desktop UI was not found: ${ui}\n` +
        `  Build it with: pnpm --filter @yukinal/desktop build (or pass --ui-dir), ` +
        `or set YUKINAL_DESKTOP_UI_DIR.`,
    );
  }
  requireFile(
    join(ui, "index.html"),
    "The UI directory must contain index.html; the host exits 2 without a readable entry document.",
  );
  await assertNoReparsePoints(ui, `The desktop UI directory ${ui}`);
  return ui;
}

async function collectPayloads(out, payloadDirectory) {
  const payloadRoot = join(out, payloadDirectory);
  if (!existsSync(payloadRoot)) fail(`Payload directory does not exist: ${payloadRoot}`);
  let relative;
  try {
    relative = await listPayloadFiles(payloadRoot);
  } catch (error) {
    fail(
      `The staged payload contains an entry that cannot be hashed:\n  ${error.message}\n` +
        `  Remove the symlink/junction before packaging; the manifest only covers regular files.`,
    );
  }
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

/**
 * Reads the `Name`/`Version` of every Python distribution staged under the Agent payload.
 *
 * `.dist-info` directories can sit at any depth inside a PyInstaller onedir, so the walk
 * is recursive; the identity comes from each one's `METADATA`, mirroring what
 * `collect-python-licenses.py` records in the notice.
 */
async function stagedPythonDistributions(payloadRoot) {
  const found = [];
  async function walk(directory) {
    let entries;
    try {
      entries = await readdir(directory, { withFileTypes: true });
    } catch {
      return;
    }
    for (const entry of entries) {
      const absolute = join(directory, entry.name);
      if (entry.isDirectory()) {
        if (entry.name.endsWith(".dist-info")) {
          let metadata = "";
          try {
            metadata = await readFile(join(absolute, "METADATA"), "utf8");
          } catch {
            metadata = "";
          }
          found.push(readDistributionIdentity(metadata, entry.name));
        } else {
          await walk(absolute);
        }
      }
    }
  }
  await walk(payloadRoot);
  found.sort((left, right) =>
    `${left.name} ${left.version}` < `${right.name} ${right.version}` ? -1 : 1,
  );
  return found;
}

function manifestNotes(inputs, staleExempted) {
  const notes = [
    "This directory is the verified application payload used by the platform installer builders.",
    "The Windows, macOS, and Debian installers are assembled by the release-candidate workflow.",
    "Application data is stored outside the install directory and is retained on uninstall.",
    "Artifacts are unsigned; signing and notarisation are not configured.",
    "`sourceRevision`/`dirty` describe the worktree the staging ran in; the `inputs` section " +
      "records the sha256 and build time of each build input actually staged (the three " +
      "executables and the UI entry document).",
  ];
  if (staleExempted) {
    notes.push(
      "Stale inputs were staged with --allow-stale-inputs: at least one executable predates " +
        "the last commit that touched its source directory. This payload does not correspond " +
        "to `sourceRevision`.",
    );
  }
  for (const input of inputs) {
    const dirty = input.dirty ? " [source directory has uncommitted changes]" : "";
    notes.push(
      `input ${input.role}: built ${input.modifiedAt} from ${input.sourceDirectory} ` +
        `at ${input.sourceRevision}${dirty} (sha256 ${input.sha256.slice(0, 16)}…)`,
    );
  }
  return notes;
}

function launchSection(payloadDirectory) {
  return {
    hostExecutable: `${payloadDirectory}/yukinal-desktop-host${executableSuffix}`,
    uiDirectory: `${payloadDirectory}/ui`,
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
  const uiDirectory = await resolveUiDirectory(options);
  await assertAgentRuntimeIsStageable(agentRuntime);

  // The notice is generated from whatever onedir `--payload` named when
  // `build-python-agent` last ran, and it is copied into the payload verbatim. Staging a
  // *different* `--agent-runtime` therefore shipped a notice describing the other tree —
  // byte for byte, no gate red — because the notice is not a payload entry `--verify` can
  // cross-check. Compare the two inventories here, before anything is deleted or copied,
  // so a mismatch leaves the output directory untouched.
  const agentNotice = join(root, "agent", "runtime", "THIRD_PARTY_LICENSES.txt");
  requireFile(
    agentNotice,
    "The frozen Agent's third-party licence notice is missing; rebuild it with " +
      "pnpm build:python-agent (it writes agent/runtime/THIRD_PARTY_LICENSES.txt).",
  );
  const noticeProblems = agentNoticeProblems(
    readFileSync(agentNotice, "utf8"),
    await stagedPythonDistributions(agentRuntime),
  );
  if (noticeProblems.length > 0) {
    fail(
      `The Python licence notice does not describe the Agent payload being staged:\n` +
        noticeProblems.map((problem) => `  - ${problem}`).join("\n") +
        `\n  Regenerate it against this payload: python scripts/collect-python-licenses.py ` +
        `--payload <onedir> --destination agent/runtime/THIRD_PARTY_LICENSES.txt`,
    );
  }

  // Check freshness before copying: a stale binary is a packaging-input problem, so
  // failing here keeps the output directory untouched instead of leaving a payload
  // whose manifest was never written.
  const buildInputs = [
    {
      role: "desktop-host",
      artifactPath: hostExecutable,
      sourceDirectory: "native/desktop_host",
      payloadPath: `${payloadDirectory}/${basename(hostExecutable)}`,
    },
    {
      role: "ssh-service",
      artifactPath: sshServiceExecutable,
      sourceDirectory: "native/src",
      payloadPath: `${payloadDirectory}/ssh-service/${basename(sshServiceExecutable)}`,
    },
    {
      role: "python-agent",
      artifactPath: join(agentRuntime, `yukinal-agent${executableSuffix}`),
      sourceDirectory: "agent/src",
      payloadPath: `${payloadDirectory}/python-agent/yukinal-agent${executableSuffix}`,
    },
    // The UI is a build input too: `launch.uiDirectory` points the host at
    // `ui/index.html`, so a `apps/desktop/dist` built before the last UI source change
    // would ship an interface that does not match the revision the manifest names.
    {
      role: "desktop-ui",
      artifactPath: uiDirectory,
      directory: true,
      sourceDirectory: "apps/desktop",
      payloadPath: `${payloadDirectory}/ui/index.html`,
    },
  ];
  const { recorded, stale } = await collectBuildInputs(buildInputs);
  if (stale.length > 0 && !options.allowStaleInputs) {
    fail(
      `Refusing to stage stale build input(s):\n` +
        stale.map((problem) => `  - ${problem}`).join("\n") +
        `\n  Rebuild the affected artifact, or pass --allow-stale-inputs to stage it ` +
        `anyway (the exemption is recorded in the manifest notes).`,
    );
  }
  for (const problem of stale) console.warn(`warning: staging a stale input — ${problem}`);

  // The freshness check reads commit times out of git. When git is unavailable (or a
  // source directory has no commit, e.g. an exported tarball) there is nothing to
  // compare against, so the check cannot run at all. Saying so is the difference
  // between "verified fresh" and "not verified"; silence would read as the former.
  const unverified = recorded.filter((input) => input.sourceRevision === "unknown");
  if (unverified.length > 0) {
    console.warn(
      `warning: could not establish a source revision for ${unverified.map((input) => input.role).join(", ")}; ` +
        `the staleness check did not run for these inputs (is git available, and do those ` +
        `directories have commits?)`,
    );
  }

  console.log(`Staging ${payloadDirectory}`);
  rmSync(payloadRoot, { recursive: true, force: true });
  mkdirSync(join(payloadRoot, "ssh-service"), { recursive: true });

  copyFileSync(hostExecutable, join(payloadRoot, basename(hostExecutable)));
  // Both sources were already checked for reparse points by resolveUiDirectory and
  // assertAgentRuntimeIsStageable; `dereference: false` keeps that guarantee rather
  // than silently freezing bytes that live outside the staged directory.
  await cp(uiDirectory, join(payloadRoot, "ui"), { recursive: true, dereference: false });
  await cp(agentRuntime, join(payloadRoot, "python-agent"), { recursive: true, dereference: false });
  // Presence and consistency with the staged payload were established above, before the
  // output directory was touched, so this is a plain copy of an already-validated file.
  copyFileSync(agentNotice, join(payloadRoot, "python-agent", "THIRD_PARTY_LICENSES.txt"));
  copyFileSync(sshServiceExecutable, join(payloadRoot, "ssh-service", basename(sshServiceExecutable)));
  copyFileSync(opensslLicensePath(), join(payloadRoot, "ssh-service", "OPENSSL-LICENSE.txt"));
  copyFileSync(join(root, "NOTICE"), join(payloadRoot, "NOTICE"));
  copyFileSync(join(root, "LICENSE"), join(payloadRoot, "LICENSE"));
  mkdirSync(join(payloadRoot, "assets"), { recursive: true });
  copyFileSync(
    join(root, "apps", "desktop", "src", "assets", "brand-mark.png"),
    join(payloadRoot, "assets", "yukinal.png"),
  );
  copyFileSync(
    join(root, "apps", "desktop", "design", "app-icon.png"),
    join(payloadRoot, "assets", "app-icon.png"),
  );
  copyFileSync(
    join(root, "apps", "desktop", "design", "yukinal.ico"),
    join(payloadRoot, "assets", "yukinal.ico"),
  );
  await writeFile(join(payloadRoot, "yukinal.cmd"), renderWindowsLauncher(), "utf8");
  await writeFile(join(payloadRoot, "yukinal"), renderUnixLauncher(), { encoding: "utf8", mode: 0o755 });

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
    inputs: recorded,
    launch: launchSection(payloadDirectory),
    notes: manifestNotes(recorded, stale.length > 0),
  });
  await writeManifestAndChecksums(out, manifest);

  console.log(
    `Staged ${manifest.payloads.length} file(s) into ${payloadRoot}\n` +
      `Wrote ${join(out, CHECKSUMS_FILE)} and ${join(out, RELEASE_MANIFEST_FILE)}`,
  );
  console.log(
    `Launch it with:\n` +
      `  ${join(payloadRoot, basename(hostExecutable))} --ui-dir ${join(payloadRoot, "ui")}\n` +
      `with YUKINAL_AGENT_ENTRY and YUKINAL_SSH_SERVICE_BINARY pointing at the staged ` +
      `python-agent and ssh-service executables. The host exits 2 (silently) when --ui-dir ` +
      `is missing or its index.html is unreadable.`,
  );
}

async function refreshManifest(options) {
  const out = resolve(root, options.out);
  const manifestPath = join(out, RELEASE_MANIFEST_FILE);
  if (!existsSync(manifestPath)) fail(`No manifest to refresh: ${manifestPath}`);
  const existing = JSON.parse(await readFile(manifestPath, "utf8"));
  // Silently coercing a malformed section to `[]` would drop the provenance and still
  // print "Refreshed", which is worse than refusing: the manifest would come out of the
  // refresh claiming to have no build inputs rather than saying it could not read them.
  if (existing.inputs !== undefined && !Array.isArray(existing.inputs)) {
    fail(
      `Refusing to refresh ${manifestPath}: its inputs section is not an array ` +
        `(${JSON.stringify(existing.inputs)}). Re-stage the payload instead.`,
    );
  }
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
    // `--manifest` re-reads the staged bytes; it does not rebuild anything, so the
    // provenance recorded at staging time still describes these executables and is
    // carried over rather than recomputed from the current worktree. It is re-checked
    // against the freshly hashed payloads below, so a fabricated entry cannot be blessed
    // here: an `inputs[].sha256` that disagrees with the payload entry it names is fatal.
    inputs: Array.isArray(existing.inputs) ? existing.inputs : [],
    launch: existing.launch,
    notes: Array.isArray(existing.notes) ? existing.notes : manifestNotes([], false),
  });
  // Refreshing rewrites `payloads` from disk. Provenance that no longer matches the
  // freshly hashed payloads must fail here rather than be carried into a manifest that
  // `--verify` would accept. Only the manifest-internal consistency check runs: `--manifest`
  // exists to re-attest the staged bytes, so a payload that changed on disk is its whole
  // point, not an error.
  const provenanceProblems = inputConsistencyProblems(manifest);
  if (provenanceProblems.length > 0) {
    fail(
      `Refusing to refresh the manifest from inconsistent provenance:\n` +
        provenanceProblems.map((problem) => `  - ${problem}`).join("\n"),
    );
  }
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
    `Verified ${result.files} file(s), ${result.bytes} byte(s), ${result.scanned} scanned against ${manifestPath}`,
  );
  if (result.scanned !== result.files) {
    fail(
      `Verification is inconsistent: ${result.files} file(s) listed but ${result.scanned} scanned.`,
    );
  }
}

const options = parseArguments(process.argv.slice(2));
if (!existsSync(join(root, "pnpm-workspace.yaml"))) fail("Run this script from the repository root.");

if (options.mode === "verify") await verify(options);
else if (options.mode === "manifest") await refreshManifest(options);
else await stage(options);
