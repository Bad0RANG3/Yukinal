#!/usr/bin/env node
/**
 * The single local compile gate (every stage compiles, tests and is
 * checked for cross-platform problems before the next stage starts).
 *
 * Order is deliberate:
 *   1. check repository hygiene and docs
 *   2. rebuild the native test targets, then run the configured CTest suite
 *      (a rebuild is required: CTest alone can pass against stale binaries)
 *   3. run the control plane and the full Python Agent unittest suites
 *   4. build the contract libs (consumers import their dist/*.d.ts)
 *   5. typecheck and test JavaScript sources (including retained legacy Agent tests)
 *   6. smoke the Python Agent
 *
 * Exits non-zero on the first failure. The Python steps never report a green
 * "skipped" line for an environment that is installed but unusable: a missing or
 * broken interpreter stops the gate with the exact command that repairs it.
 */

import { spawn } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve, win32 as windowsPath } from "node:path";
import { pathToFileURL } from "node:url";
import { resolveCommandInvocation, spawnCommandSync } from "./lib/commands.mjs";

// ── Testable gate helpers ───────────────────────────────────────────────────
// Multi-config generators (Visual Studio, Xcode) put target binaries in a
// per-configuration subdirectory. The old root-only probe missed
// `native/build-check/Release/yukinal-ssh-service.exe`, so the Agent integration
// tests silently skipped on Windows CI even though the service had been built.
// Per-configuration output wins so a stale root-level binary from an earlier
// single-config configure cannot shadow the binary the current build produced.
// Single-config builds simply fall through to the root-level names.
export function agentServiceBinaryCandidates(nativeBuildDirectory) {
  const names = ["yukinal-ssh-service.exe", "yukinal-ssh-service"];
  if (!nativeBuildDirectory) return [];
  return [
    ...["Release", "RelWithDebInfo", "Debug"].flatMap((configuration) =>
      names.map((name) => join(nativeBuildDirectory, configuration, name)),
    ),
    ...names.map((name) => join(nativeBuildDirectory, name)),
  ];
}

export function resolveAgentServiceBinary(nativeBuildDirectory, fileExists = existsSync) {
  return agentServiceBinaryCandidates(nativeBuildDirectory).find(fileExists);
}

/**
 * The exact argv the "build native test targets" step runs. Exported so the
 * guardrail tests and manual verification exercise the same construction as the
 * gate itself. NMake ignores `--parallel`; CMake warns and continues.
 */
export function nativeBuildArgs(nativeBuildDirectory, extraArgs = []) {
  if (!nativeBuildDirectory) throw new Error("nativeBuildArgs requires a configured native build directory");
  return ["--build", nativeBuildDirectory, "--config", "Release", "--parallel", ...extraArgs];
}

/** Extra repair hint when a native build fails because the WebView2 SDK is too old. */
export function webView2SdkRepairHint(buildOutput) {
  return /WebView2|ICoreWebView2/iu.test(buildOutput)
    ? "local WebView2 SDK is older than native/CMakeLists.txt requires; " +
        "build individual native targets or update the SDK"
    : undefined;
}

// ── WebView2-scoped native build fallback ───────────────────────────────────
// The GUI target `yukinal-desktop-host` cannot compile against an older local
// WebView2 SDK, which made the whole gate permanently red on such machines. A
// permanently red gate is a regression, but skipping the native build entirely
// reopens the stale-binary hole. The third option: try the full build first, and
// only when the failure blames WebView2 retry every *other* target and announce
// loudly that local native coverage is incomplete. CI still builds the full set.

/** CMake helpers that are not build targets. */
const CMAKE_PSEUDO_TARGETS = new Set([
  "all",
  "clean",
  "depend",
  "edit_cache",
  "help",
  "install",
  "install/local",
  "list_install_components",
  "package",
  "package_source",
  "rebuild_cache",
  "test",
]);

/** The GUI target is the only one that needs WebView2; hyphen is the GUI spelling. */
export function isDesktopHostTarget(name) {
  return typeof name === "string" && name.toLowerCase().includes("desktop-host");
}

/**
 * Parse `cmake --build <dir> --target help` into real build targets: drop the
 * pseudo targets and the per-file entries Makefile generators list
 * (`desktop_host/src/main.obj`, `.../main.i`, `.../main.s`).
 */
export function nativeBuildTargetsFromHelp(helpText) {
  return String(helpText ?? "")
    .split(/\r?\n/u)
    .map((line) => line.match(/^\s*\.\.\.\s+(\S+)\s*$/u)?.[1])
    .filter((name) => name !== undefined)
    .filter((name) => !CMAKE_PSEUDO_TARGETS.has(name))
    .filter((name) => !name.includes("/") && !/\.(?:obj|i|s)$/iu.test(name));
}

/**
 * Decide whether a failed native build should be retried without the GUI target.
 * `retry` is true only when the build failed *and* its output blames WebView2; an
 * unrelated compile error must fail the gate immediately, with no retry.
 */
export function nativeBuildFallbackDecision({ status, output = "", targetList = [] }) {
  if (status === 0) return { retry: false, targets: [] };
  if (!webView2SdkRepairHint(output)) return { retry: false, targets: [] };
  const targets = targetList.filter((target) => !isDesktopHostTarget(target));
  return { retry: targets.length > 0, targets };
}

/** The MS WebView2 version native/CMakeLists.txt pins. */
export function requiredWebView2Version(cmakeListsText) {
  return String(cmakeListsText ?? "").match(/WEBVIEW_MSWEBVIEW2_VERSION\s+"([^"]+)"/u)?.[1];
}

/** The first build-output line that blames the WebView2 SDK, quoted in the warning. */
export function webView2ErrorLine(buildOutput) {
  const line = String(buildOutput ?? "")
    .split(/\r?\n/u)
    .map((candidate) => candidate.trim())
    .find((candidate) => /WebView2|ICoreWebView2/u.test(candidate));
  if (!line) return undefined;
  // A localized MSVC prints diagnostics in the console code page, which arrives here
  // as undecodable bytes. Keep the ASCII skeleton — file(line) plus the WebView2
  // identifier — so the warning stays readable whatever the machine language is.
  if (!/[^\x20-\x7e]/u.test(line)) return line;
  const location = line.match(/[\w.\\/:+-]+\.(?:cc|cpp|cxx|h|hpp)\(\d+\)/u)?.[0];
  const identifier = line.match(/(?:I?CoreWebView2\w+|WebView2\w+|WEBVIEW_MSWEBVIEW2_VERSION)/u)?.[0];
  const skeleton = [location, identifier].filter(Boolean).join(": ");
  return skeleton ? skeleton + " (localized compiler text omitted)" : "localized WebView2 compiler error";
}

/**
 * The WebView2 SDK version actually present in a build tree (the NuGet extraction
 * FetchContent produces), for the warning text only. A second build tree may point
 * at the first one's sources, so `MSWebView2_INCLUDE_DIR` from the cache is followed
 * as well.
 */
export function detectWebView2SdkVersion(nativeBuildDirectory, { exists = existsSync, readFile = readFileSync } = {}) {
  if (!nativeBuildDirectory) return undefined;
  const roots = [join(nativeBuildDirectory, "_deps", "microsoft_web_webview2-src")];
  const cache = join(nativeBuildDirectory, "CMakeCache.txt");
  if (exists(cache)) {
    const includeDirectory = String(readFile(cache, "utf8")).match(/^MSWebView2_INCLUDE_DIR:PATH=(.+)$/mu)?.[1]?.trim();
    // `<sdk>/build/native/include` -> `<sdk>`
    if (includeDirectory) roots.push(join(includeDirectory, "..", "..", ".."));
  }
  for (const root of roots) {
    for (const candidate of [
      join(root, "Microsoft.Web.WebView2.nuspec"),
      join(root, "build", "native", "Microsoft.Web.WebView2.nuspec"),
    ]) {
      if (!exists(candidate)) continue;
      const version = String(readFile(candidate, "utf8")).match(/<version>\s*([^<\s]+)\s*<\/version>/u)?.[1];
      if (version) return version;
    }
  }
  return undefined;
}

/**
 * The loud warning block printed when the full build failed only because of the
 * local WebView2 SDK and the scoped retry built everything else. It names both
 * versions, quotes the compiler error, and states the CTest consequence: the stale
 * GUI binary means `desktop_host_self_check` exercises the previous build.
 */
export function nativeDesktopHostSkippedWarning({ localVersion, requiredVersion, errorLine } = {}) {
  const requirement = requiredVersion ? "required " + requiredVersion : "the version native/CMakeLists.txt requires";
  const sdk = localVersion
    ? "WebView2 SDK " + localVersion + " < " + requirement
    : "local WebView2 SDK is older than " + requirement;
  return [
    "! native: desktop host target was NOT compiled locally (" + sdk + ").",
    ...(errorLine ? ["! native: first WebView2 error: " + errorLine] : []),
    "! native: all other targets built, but yukinal-desktop-host.exe is stale, so CTest's" +
      " desktop_host_self_check exercises the previous build.",
    "! native: local native coverage is NOT full; CI builds the full set.",
  ];
}

// ── Native toolchain discovery ──────────────────────────────────────────────
// The gate must work from a plain shell. Before this, `cmake --build` inherited a
// shell where nmake/cl were missing, so the step failed with text that looked like
// a compiler error and a `fix:` hint that blamed the source. Now the build tool is
// looked for on PATH, and when it is absent the Visual Studio environment is loaded
// from vcvars64.bat inside `cmd /c` instead of asking the operator to do it.

/** Precise, non-misleading failure text for a machine with no C++ toolchain. */
export const NATIVE_TOOLCHAIN_HINT =
  "the native build needs a C++ toolchain; install Visual Studio 2022 with the C++ workload, " +
  "or run from a developer shell (vcvars64.bat)";

/** Output of a generator that could not launch nmake/make at all. */
export function isMissingBuildToolOutput(buildOutput) {
  const text = String(buildOutput ?? "");
  return (
    /build tool execution failed/iu.test(text) ||
    /(?:nmake|make|ninja)[^\n]*no such file or directory/iu.test(text) ||
    /no such file or directory[^\n]*(?:nmake|make|ninja)/iu.test(text)
  );
}

/** Which build tool the configured generator needs, when the generator is known. */
export function nativeBuildToolForGenerator(generator) {
  const value = String(generator ?? "").toLowerCase();
  if (value.includes("nmake")) return "nmake";
  if (value.includes("ninja")) return "ninja";
  if (value.includes("makefiles")) return "make";
  return undefined;
}

/** Default PATH lookup; injectable so the guardrail tests never touch the process. */
function joinForPlatform(platform, ...parts) {
  return platform === "win32" ? windowsPath.join(...parts) : join(...parts);
}

export function defaultWhich(name, { env = process.env, platform = process.platform, exists = existsSync } = {}) {
  const extensions = platform === "win32" ? ["", ".exe", ".cmd", ".bat"] : [""];
  const pathValue = String(env.PATH ?? env.Path ?? "");
  for (const directory of pathValue.split(platform === "win32" ? ";" : ":")) {
    if (!directory) continue;
    for (const extension of extensions) {
      const candidate = joinForPlatform(platform, directory, name + extension);
      if (exists(candidate)) return candidate;
    }
  }
  return undefined;
}

/** The vswhere executable, from its documented default install location. */
export function vswherePath(env = process.env, platform = process.platform) {
  const programFilesX86 = env["ProgramFiles(x86)"] ?? env.ProgramFilesX86 ?? "C:\\Program Files (x86)";
  return joinForPlatform(platform, programFilesX86, "Microsoft Visual Studio", "Installer", "vswhere.exe");
}

let cachedVcvars64Path;
/** Test hook: vcvars discovery is cached per process, so tests must be able to reset it. */
export function resetNativeToolchainCache() {
  cachedVcvars64Path = undefined;
}

function defaultVswhereRun(command, args) {
  return spawnCommandSync(command, args, { encoding: "utf8" });
}

/**
 * Locate vcvars64.bat through vswhere; cached per process because it spawns a
 * process. `run` is injectable so tests never spawn anything.
 */
export function findVcvars64({
  run = defaultVswhereRun,
  exists = existsSync,
  platform = process.platform,
  env = process.env,
} = {}) {
  if (platform !== "win32") return undefined;
  if (cachedVcvars64Path && exists(cachedVcvars64Path)) return cachedVcvars64Path;
  const result = run(vswherePath(env, platform), [
    "-latest",
    "-products",
    "*",
    "-requires",
    "Microsoft.VisualStudio.Component.VC.Tools.x86.x64",
    "-property",
    "installationPath",
  ]);
  const installationPath = String(result?.stdout ?? result?.output ?? "")
    .split(/\r?\n/u)[0]
    ?.trim();
  if (!installationPath) return undefined;
  const vcvars = joinForPlatform(platform, installationPath, "VC", "Auxiliary", "Build", "vcvars64.bat");
  if (!exists(vcvars)) return undefined;
  cachedVcvars64Path = vcvars;
  return vcvars;
}

/** Quote one argv token for a `cmd /c` command line. */
export function quoteForCmd(argument) {
  const value = String(argument);
  return /^[A-Za-z0-9_./:@+=,-]+$/u.test(value) ? value : '"' + value.replaceAll('"', '""') + '"';
}

/**
 * Resolve how to run CMake for the native build. Windows only: when a build tool
 * (the generator's own tool, else any of nmake/ninja/make) is already on PATH the
 * plain `cmake` argv is returned; otherwise the Visual Studio environment is loaded
 * from vcvars64.bat for the duration of a `cmd /c` invocation. When neither a
 * toolchain nor vcvars64.bat exists, a sentinel is returned so the caller can fail
 * with an accurate message instead of pretending the compiler broke.
 */
export function resolveNativeBuildInvocation(
  baseArgs,
  {
    platform = process.platform,
    env = process.env,
    which,
    generator,
    run = defaultVswhereRun,
    exists = existsSync,
  } = {},
) {
  if (platform !== "win32") return { command: "cmake", args: baseArgs };
  const lookup = which ?? ((name) => defaultWhich(name, { env, platform, exists }));
  const required = nativeBuildToolForGenerator(generator);
  const candidates = required ? [required] : ["nmake", "ninja", "make"];
  if (candidates.some((name) => lookup(name))) return { command: "cmake", args: baseArgs };

  const vcvars = findVcvars64({ run, exists, platform, env });
  if (!vcvars) return { command: "cmake", args: baseArgs, missingToolchain: true, hint: NATIVE_TOOLCHAIN_HINT };
  // `cmd /s /c "<line>"` strips the outer quotes and runs the rest unchanged, so the
  // quoted vcvars path survives. The argv is handed to the child verbatim: letting
  // Node re-quote it on Windows mangles the embedded quotes and cmd silently exits 1
  // with no output at all, which would look like a build failure.
  const commandLine =
    'call "' + vcvars + '" >nul 2>&1 && cmake ' + baseArgs.map(quoteForCmd).join(" ");
  return {
    command: "cmd",
    args: ["/d", "/s", "/c", '"' + commandLine + '"'],
    windowsVerbatimArguments: true,
    viaVcvars: vcvars,
  };
}

/** The `fix:` text for a failed native build: toolchain advice only when no compiler ran. */
export function nativeBuildRepairHint({
  output = "",
  toolchainMissing = false,
  nativeBuildDirectory = "native/build",
} = {}) {
  if (toolchainMissing || isMissingBuildToolOutput(output)) return NATIVE_TOOLCHAIN_HINT;
  return (
    "fix the first compiler/linker error above, then re-run the gate (it rebuilds before CTest): " +
    "cmake --build " +
    nativeBuildDirectory +
    " --config Release --parallel. If the failure is environmental (for example an old WebView2 SDK), " +
    "reconfigure with cmake -S native -B " +
    nativeBuildDirectory +
    " -DYUKINAL_BUILD_TESTS=ON"
  );
}

/** The generator recorded in a configured build tree, used to pick the required tool. */
export function nativeBuildGenerator(nativeBuildDirectory, { exists = existsSync, readFile = readFileSync } = {}) {
  if (!nativeBuildDirectory) return undefined;
  const cache = join(nativeBuildDirectory, "CMakeCache.txt");
  if (!exists(cache)) return undefined;
  return String(readFile(cache, "utf8")).match(/^CMAKE_GENERATOR:INTERNAL=(.+)$/mu)?.[1]?.trim();
}

/**
 * Run the native build step: full build first (fail-closed), then — only for a
 * WebView2-only failure — a scoped retry of every other target. Every CMake call
 * goes through `resolveInvocation`, so a plain shell works as well as a developer
 * shell. Resolves to `{ status, output, desktopHostSkipped }`, so the caller can warn
 * about a partial build and the guardrail tests can assert both paths without CMake.
 */
export async function runNativeBuildStep(
  nativeBuildDirectory,
  {
    run,
    resolveInvocation = resolveNativeBuildInvocation,
    platform = process.platform,
    env = process.env,
    which,
    generator = nativeBuildGenerator(nativeBuildDirectory),
    exists = existsSync,
    vswhereRun,
    log = console.log,
  } = {},
) {
  const runner =
    run ?? ((command, args, options = {}) => runStepCapturingOutput(command, args, options));
  const resolve = (args) =>
    resolveInvocation(args, { platform, env, which, generator, exists, ...(vswhereRun ? { run: vswhereRun } : {}) });
  // An invocation may need verbatim argv on Windows (the `cmd /s /c` form above).
  const invoke = (invocation) =>
    runner(
      invocation.command,
      invocation.args,
      invocation.windowsVerbatimArguments ? { windowsVerbatimArguments: true } : {},
    );

  const first = resolve(nativeBuildArgs(nativeBuildDirectory));
  if (first.missingToolchain) {
    // No compiler can run here: fail precisely and never print the compiler hint.
    return { status: 1, output: "", desktopHostSkipped: false, toolchainMissing: true, hint: first.hint };
  }
  if (first.viaVcvars) log("native: build tool not on PATH; using " + first.viaVcvars);

  const full = await invoke(first);
  if (full.status === 0) return { status: 0, output: String(full.output ?? ""), desktopHostSkipped: false };

  // Not a WebView2 failure: fail exactly as before, with no retry and no discovery.
  if (!webView2SdkRepairHint(full.output ?? "")) {
    return { status: full.status, output: String(full.output ?? ""), desktopHostSkipped: false };
  }

  const helpInvocation = resolve(["--build", nativeBuildDirectory, "--target", "help"]);
  const help = await invoke(helpInvocation);
  const scoped = nativeBuildFallbackDecision({
    status: full.status,
    output: full.output,
    targetList: nativeBuildTargetsFromHelp(help.output),
  });
  if (!scoped.retry) {
    // The SDK hint matched but no usable target list came back: report the failure.
    return {
      status: full.status,
      output: String(full.output ?? "") + String(help.output ?? ""),
      desktopHostSkipped: false,
    };
  }

  const retryInvocation = resolve(nativeBuildArgs(nativeBuildDirectory, ["--target", ...scoped.targets]));
  const retry = await invoke(retryInvocation);
  if (retry.status !== 0) {
    // A real source error in a non-GUI target: the retry's output is the failure.
    return { status: retry.status, output: String(retry.output ?? ""), desktopHostSkipped: false };
  }
  return {
    status: 0,
    output: String(full.output ?? "") + String(retry.output ?? ""),
    desktopHostSkipped: true,
  };
}

/**
 * Run a step while both streaming its output to the terminal and retaining it, so
 * the gate can react to what the tool printed (for example a WebView2 SDK error)
 * without hiding live build progress. Resolves to `{ status, output }`.
 */
export function runStepCapturingOutput(command, args, options = {}) {
  return new Promise((resolvePromise) => {
    const invocation = resolveCommandInvocation(command, args, { env: options.env ?? process.env });
    const child = spawn(invocation.command, invocation.args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    const chunks = [];
    const forward = (stream, target) => {
      stream.on("data", (chunk) => {
        chunks.push(chunk);
        target.write(chunk);
      });
    };
    forward(child.stdout, process.stdout);
    forward(child.stderr, process.stderr);
    child.on("error", (error) => {
      chunks.push(Buffer.from(String(error?.message ?? error)));
      resolvePromise({ status: 1, output: Buffer.concat(chunks).toString("utf8") });
    });
    child.on("close", (status) => {
      resolvePromise({ status, output: Buffer.concat(chunks).toString("utf8") });
    });
  });
}

async function main() {
  const nativeBuildCandidates = process.env.YUKINAL_SSH_CMAKE_BUILD_DIR
    ? [resolve(process.env.YUKINAL_SSH_CMAKE_BUILD_DIR)]
    : ["native/build-check", "native/build-desktop", "native/build", "native/build-nmake", "native/build-cpp-host"];
  // A directory counts only when it is a *configured test* build: without
  // CTestTestfile.cmake there is nothing to run, without the cache we cannot prove
  // YUKINAL_BUILD_TESTS is on, and an ON-less build would report "no tests" as a
  // pass. Auto-discovery may find none and say so; an explicit
  // YUKINAL_SSH_CMAKE_BUILD_DIR may not, because a typo there used to print
  // "skipped" and exit 0.
  const isConfiguredNativeTestBuild = (directory) =>
    existsSync(join(directory, "CTestTestfile.cmake")) &&
    existsSync(join(directory, "CMakeCache.txt")) &&
    /^YUKINAL_BUILD_TESTS:BOOL=ON\r?$/mu.test(readFileSync(join(directory, "CMakeCache.txt"), "utf8"));
  const nativeBuildDirectory = nativeBuildCandidates.find(isConfiguredNativeTestBuild);

  // ── Python interpreter discovery ────────────────────────────────────────────
  // A Python-backed step may only report "skipped" when its dependencies are absent
  // everywhere. "Installed but unusable" is a hard failure carrying the exact command
  // that repairs it: a silent skip is how the control-plane suite and the four Agent
  // test modules that no glob covered went unexecuted while `pnpm check` stayed green.
  const agentRoot = resolve("agent");
  const agentSourcePath = join(agentRoot, "src");
  const agentTestsPath = join(agentRoot, "tests");
  const agentImportProbe = "import yukinal_agent";

  const controlPlaneRoot = resolve("services/control-plane");
  // The control plane has its own virtualenv: it needs FastAPI/SQLAlchemy, which the
  // Agent does not. Core = runtime imports; full adds the `test` extra (httpx for
  // TestClient, aiosqlite for the SQLite engine) that the suite actually needs.
  const controlPlaneCoreProbe = "import fastapi, sqlalchemy, alembic, pydantic_settings";
  const controlPlaneFullProbe = `${controlPlaneCoreProbe}, httpx, aiosqlite`;

  // `spawnSync` resolves a relative `command` against the child's `cwd`, so every
  // explicit candidate is absolute while the PATH fallbacks stay bare names.
  function probeInterpreter(candidate, source, env) {
    if (!candidate) return "missing";
    const result = spawnCommandSync(candidate, ["-c", source], {
      stdio: "ignore",
      windowsHide: true,
      ...(env ? { env } : {}),
    });
    return !result.error && result.status === 0 ? "ok" : "broken";
  }

  // Exit 2 means "the environment cannot run the gate as configured"; exit 1 stays
  // reserved for "a gate step ran and failed".
  function failGate(stepName, problem, repair) {
    console.error(`\n✗ ${stepName}: ${problem}`);
    console.error(`  fix: ${repair}`);
    process.exit(2);
  }

  // An explicitly named build directory that is not a configured test build is a
  // configuration error, not a reason to skip. It used to print
  // "skipped (no configured CMake test build)" and exit 0, so one typo in
  // YUKINAL_SSH_CMAKE_BUILD_DIR silently dropped all 13 native test targets from a
  // gate that still reported green. Auto-discovery may legitimately find nothing on
  // a machine that never configured a native build; an explicit path may not.
  if (process.env.YUKINAL_SSH_CMAKE_BUILD_DIR && !nativeBuildDirectory) {
    failGate(
      "configured native CTest suite",
      `YUKINAL_SSH_CMAKE_BUILD_DIR=${process.env.YUKINAL_SSH_CMAKE_BUILD_DIR} is not a configured test build ` +
        "(needs CTestTestfile.cmake, CMakeCache.txt and YUKINAL_BUILD_TESTS:BOOL=ON)",
      `configure tests there: cmake -S native -B "${process.env.YUKINAL_SSH_CMAKE_BUILD_DIR}" -DYUKINAL_BUILD_TESTS=ON, ` +
        "or unset YUKINAL_SSH_CMAKE_BUILD_DIR to let the gate auto-discover one",
    );
  }

  // The Agent step is always required: there is no allowed skip, so a missing
  // dependency set fails the gate instead of turning it green.
  const agentPythonCandidates = [
    join(agentRoot, ".venv", "Scripts", "python.exe"),
    join(agentRoot, ".venv", "bin", "python"),
    process.env.YUKINAL_AGENT_PYTHON,
    "python",
  ].filter(Boolean);
  const agentProbeEnvironment = { ...process.env, PYTHONPATH: agentSourcePath };
  const agentPython = agentPythonCandidates.find(
    (candidate) => probeInterpreter(candidate, agentImportProbe, agentProbeEnvironment) === "ok",
  );
  if (!agentPython) {
    failGate(
      "Python Agent tests",
      `no interpreter on any candidate can \`${agentImportProbe}\` with PYTHONPATH=${agentSourcePath} ` +
        `(tried: ${agentPythonCandidates.join(", ")})`,
      'install into an interpreter or create the venv first: python -m venv agent/.venv && ' +
        'agent/.venv/Scripts/python -m pip install -e "agent[agent,models,test]" ' +
        "(or point YUKINAL_AGENT_PYTHON at an interpreter that has them)",
    );
  }

  // Candidate order: repository venv -> explicit override -> PATH. `python3` is last
  // because on Windows it may be an App Execution Alias that exits non-zero.
  const controlPlaneCandidates = [
    join(controlPlaneRoot, ".venv", "Scripts", "python.exe"),
    join(controlPlaneRoot, ".venv", "bin", "python"),
    process.env.YUKINAL_CONTROL_PLANE_PYTHON,
    "python",
    "python3",
  ].filter(Boolean);
  const controlPlaneStates = controlPlaneCandidates.map((candidate) => ({
    candidate,
    core: probeInterpreter(candidate, controlPlaneCoreProbe),
    full: probeInterpreter(candidate, controlPlaneFullProbe),
  }));
  const controlPlanePython = controlPlaneStates.find((state) => state.full === "ok")?.candidate;
  const controlPlaneCorePresent = controlPlaneStates.some((state) => state.core === "ok");
  const controlPlaneConfigured = process.env.YUKINAL_CONTROL_PLANE_PYTHON?.trim()
    ? process.env.YUKINAL_CONTROL_PLANE_PYTHON
    : undefined;
  const controlPlaneRequired = process.env.YUKINAL_CONTROL_PLANE_REQUIRED === "1";
  const controlPlaneRepair =
    'python -m pip install -e "services/control-plane[test]" ' +
    '(or create services/control-plane/.venv and install it there)';

  // An interpreter the operator named explicitly must work: falling back silently
  // would hide exactly the broken pin this gate exists to catch.
  if (controlPlaneConfigured) {
    const configuredState = controlPlaneStates.find((state) => state.candidate === controlPlaneConfigured);
    if (configuredState?.full !== "ok") {
      failGate(
        "control plane tests (SQLite)",
        `YUKINAL_CONTROL_PLANE_PYTHON=${controlPlaneConfigured} is set but ${
          configuredState?.core === "ok"
            ? "is missing the `test` extra (httpx, aiosqlite)"
            : "cannot run `" + controlPlaneFullProbe + "`"
        }`,
        controlPlaneRepair,
      );
    }
  }

  // Core dependencies are installed somewhere, yet no interpreter has the full test
  // set: that is the "green but never executed" hole, not a legitimate skip.
  if (!controlPlanePython && (controlPlaneCorePresent || controlPlaneRequired)) {
    const reason = controlPlaneRequired
      ? "YUKINAL_CONTROL_PLANE_REQUIRED=1"
      : "control-plane core dependencies are installed but the test extra is missing";
    failGate(
      "control plane tests (SQLite)",
      `${reason}, and no interpreter has the full test dependency set ` +
        `(tried: ${controlPlaneCandidates.join(", ")})`,
      controlPlaneRepair,
    );
  }

  const steps = [
    { name: "publication hygiene", command: process.execPath, args: ["scripts/check-publication.mjs"], required: true },
    { name: "secret scan", command: process.execPath, args: ["scripts/check-secrets.mjs"], required: true },
    { name: "cross-platform command runner tests", command: process.execPath, args: ["--test", "scripts/lib/commands.test.mjs"], required: true },
    // The packaging integrity contract: a staged payload either matches its
    // manifest and checksum listing, or verification fails loudly.
    { name: "release artifact tests", command: process.execPath, args: ["--test", "scripts/lib/release-artifacts.test.mjs"], required: true },
    // The macOS Python framework in PyInstaller's onedir uses internal symlinks;
    // materialization must preserve their contents while refusing targets outside
    // the runtime before the regular-file release manifest is built.
    { name: "Agent runtime symlink tests", command: process.execPath, args: ["--test", "scripts/lib/materialize-internal-symlinks.test.mjs"], required: true },
    // Guardrails for the runner itself. An explicit build directory that cannot run
    // tests must stop the gate rather than drop 13 native targets behind a green line.
    { name: "gate runner guardrail tests", command: process.execPath, args: ["--test", "scripts/lib/gate.test.mjs"], required: true },
    // Documentation links are load-bearing now that the rules live in a tree: a
    // section that moves takes its anchors with it, and only the renderer notices.
    // The anchor helper is tested separately because the gate's own bug — CRLF
    // documents anchoring as the empty string — was invisible from its output.
    { name: "docs anchor helper tests", command: process.execPath, args: ["--test", "scripts/lib/doc-anchors.test.mjs"], required: true },
    { name: "docs links", command: process.execPath, args: ["scripts/check-docs-links.mjs"], required: true },
    // CTest runs whatever binaries are on disk; without this step a stale `.exe`
    // built before the current source change can keep the native suite green. The
    // build is required whenever a configured test build was discovered, but a
    // machine that never configured native tests still skips both native steps.
    // NMake ignores `--parallel`; generators that support it use every core.
    {
      name: "build native test targets",
      command: "cmake",
      args: nativeBuildArgs(nativeBuildDirectory ?? "native/build"),
      required: true,
      skip: !nativeBuildDirectory,
      skipReason: "no configured CMake test build (see README.md)",
      // The full build runs first (fail-closed); only a WebView2-SDK-only failure
      // retries without the GUI target, and that partial success is announced loudly.
      run: () => runNativeBuildStep(nativeBuildDirectory),
      // The hint depends on what actually failed: a missing toolchain gets toolchain
      // advice, and only a failure where a compiler ran gets the compiler-error hint.
      repairFromOutput: true,
    },
    {
      name: "configured native CTest suite",
      command: "ctest",
      // `--no-tests=error` closes a silent-green hole: CTest exits 0 when it finds
      // zero tests, which would let a misconfigured or disappeared suite pass.
      args: ["--test-dir", nativeBuildDirectory ?? "native/build", "-C", "Release", "--output-on-failure", "--no-tests=error"],
      required: true,
      skip: !nativeBuildDirectory,
      skipReason: "no configured CMake test build (see README.md)",
    },
    {
      name: "control plane tests (SQLite)",
      command: controlPlanePython ?? "python",
      args: ["-m", "unittest", "discover", "-s", "tests", "-p", "test_*.py"],
      cwd: controlPlaneRoot,
      required: true,
      // Skipping is allowed only when no candidate has these dependencies at all;
      // every "installed but unusable" case was already turned into a hard failure.
      skip: !controlPlanePython && !controlPlaneCorePresent && !controlPlaneRequired,
      skipReason: `no interpreter has fastapi/sqlalchemy/alembic (tried: ${controlPlaneCandidates.join(", ")}; run \`${controlPlaneRepair}\`)`,
    },
    // The Agent suite runs every `agent/tests/test_*.py` module. The CTest target
    // covers only the modules its glob happens to match, and the sidecar smoke below
    // only exercises the protocol, so this is the one place that proves the whole
    // suite (including the snapshot/provider/reasoning modules) still runs.
    {
      name: "Python Agent tests (full unittest discovery)",
      command: agentPython,
      args: ["-m", "unittest", "discover", "-s", agentTestsPath, "-p", "test_*.py", "-v"],
      required: true,
      // Resolved lazily at step execution: the build step above may be what produces
      // the binary on a first local run. `test_service_integration.py` refuses to run
      // without it and would otherwise be four silently skipped tests.
      env: () => {
        const agentServiceBinary = resolveAgentServiceBinary(nativeBuildDirectory);
        return {
          PYTHONPATH: agentSourcePath,
          ...(agentServiceBinary ? { YUKINAL_SSH_SERVICE_BINARY: agentServiceBinary } : {}),
        };
      },
    },
    {
      name: "build contract libs",
      command: "pnpm",
      args: ["--filter", "@yukinal/shared", "--filter", "@yukinal/provider-sdk", "--filter", "@yukinal/agent-sdk", "build"],
      required: true,
    },
    { name: "typecheck", command: "pnpm", args: ["-r", "--if-present", "typecheck"], required: true },
    { name: "unit tests", command: "pnpm", args: ["-r", "--if-present", "test"], required: true },
    { name: "desktop bundle (vite build)", command: "pnpm", args: ["--filter", "@yukinal/desktop", "build"], required: true },
    { name: "Python Agent source sidecar smoke", command: process.execPath, args: ["scripts/smoke-python-dev-agent.mjs"], required: true },
  ];

  if (!existsSync("pnpm-workspace.yaml")) {
    console.error("run this from the repository root");
    process.exit(2);
  }

  let failures = 0;
  for (const step of steps) {
    if (step.skip) {
      console.log(`\n── ${step.name}: skipped (${step.skipReason ?? "prerequisite unavailable"})`);
      continue;
    }
    console.log(`\n── ${step.name}`);
    const stepEnvironment = typeof step.env === "function" ? step.env() : step.env;
    const options = {
      ...(step.cwd ? { cwd: step.cwd } : {}),
      env: stepEnvironment ? { ...process.env, ...stepEnvironment } : process.env,
    };
    // Build output is streamed *and* retained so a WebView2-specific failure can be
    // recognised without buffering the whole build before the operator sees it.
    const result = step.run
      ? await step.run()
      : step.captureOutput
        ? await runStepCapturingOutput(step.command, step.args, options)
        : spawnCommandSync(step.command, step.args, { ...options, stdio: "inherit" });
    if (result.desktopHostSkipped) {
      // Green, but this is a partial native build and must never read as full coverage.
      const cmakeLists = existsSync("native/CMakeLists.txt") ? readFileSync("native/CMakeLists.txt", "utf8") : "";
      const warning = nativeDesktopHostSkippedWarning({
        localVersion: detectWebView2SdkVersion(nativeBuildDirectory),
        requiredVersion: requiredWebView2Version(cmakeLists),
        errorLine: webView2ErrorLine(result.output),
      });
      for (const line of warning) console.warn(line);
      console.log("✓ " + step.name + " (desktop host skipped: WebView2 SDK too old)");
      continue;
    }
    // Helper-run and output-capturing steps have already teed what they printed, so
    // their retained output is available for the SDK-hint check.
    const sdkHint =
      step.run || step.captureOutput ? webView2SdkRepairHint(String(result.output ?? "")) : undefined;
    if (result.status !== 0) {
      failures += 1;
      console.error(`✗ ${step.name} failed (exit ${result.status})`);
      // A native build can fail before any compiler runs (no nmake/cl on PATH). That
      // is not a source defect, so it gets toolchain advice, never the compiler hint.
      const repair = step.repairFromOutput
        ? nativeBuildRepairHint({
            output: String(result.output ?? ""),
            toolchainMissing: result.toolchainMissing === true,
            nativeBuildDirectory: nativeBuildDirectory ?? "native/build",
          })
        : step.repair;
      if (repair) console.error("  fix: " + repair);
      if (sdkHint) console.error("  hint: " + sdkHint);
      if (step.required) break;
    } else {
      console.log(`✓ ${step.name}`);
    }
  }

  console.log(failures === 0 ? "\ncheck: green" : `\ncheck: ${failures} step(s) failed`);
  process.exit(failures === 0 ? 0 : 1);
}

// Importing this module from the guardrail tests must not execute the gate; only a
// direct `node scripts/check.mjs` invocation runs it.
const invokedDirectly =
  process.argv[1] !== undefined && import.meta.url === pathToFileURL(resolve(process.argv[1])).href;
if (invokedDirectly) {
  main().catch((error) => {
    console.error(error);
    process.exit(1);
  });
}
