/**
 * Guardrail tests for the gate runner's own configuration handling.
 *
 * The CTest step used to print `skipped (no configured CMake test build)` and exit
 * 0 when `YUKINAL_SSH_CMAKE_BUILD_DIR` named a directory that was not a configured
 * test build. One typo therefore removed all 13 native test targets from a gate
 * that still reported green. These tests pin the fail-closed behaviour: an explicit
 * build directory that cannot run tests is exit 2 with the repairing command, never
 * a skip, and no later step runs.
 *
 * They also pin the step list itself: the guardrail step is only worth having in the
 * gate if the gate still runs the suites this repository claims it runs. That
 * includes the two native-suite defects this file guards: CTest used to run against
 * whatever stale binaries were on disk (so a required `cmake --build` step must come
 * first), and CTest exits 0 when it finds zero tests (so `--no-tests=error` must be
 * passed). The service-binary candidate list is unit-tested too: a Windows
 * multi-config build puts `yukinal-ssh-service.exe` under `Release/`, which the old
 * root-only probe missed.
 */

import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { readFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

import {
  NATIVE_TOOLCHAIN_HINT,
  agentServiceBinaryCandidates,
  isMissingBuildToolOutput,
  nativeBuildArgs,
  nativeBuildFallbackDecision,
  nativeBuildRepairHint,
  nativeBuildTargetsFromHelp,
  nativeDesktopHostSkippedWarning,
  requiredWebView2Version,
  resetNativeToolchainCache,
  resolveAgentServiceBinary,
  resolveNativeBuildInvocation,
  runNativeBuildStep,
  runStepCapturingOutput,
  webView2ErrorLine,
  webView2SdkRepairHint,
} from "../check.mjs";

const repoRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..", "..");
const checkScript = path.join(repoRoot, "scripts", "check.mjs");
const ctestAvailable = spawnSync("ctest", ["--version"], { stdio: "ignore" }).status === 0;

test("an explicitly named build directory that cannot run tests stops the gate", () => {
  const missing = path.join(repoRoot, "native", "build-that-does-not-exist");
  assert.equal(existsSync(missing), false, "the probe directory must not exist");

  const result = spawnSync(process.execPath, [checkScript], {
    cwd: repoRoot,
    encoding: "utf8",
    env: { ...process.env, YUKINAL_SSH_CMAKE_BUILD_DIR: missing },
  });

  // Exit 2 is reserved for "the environment cannot run the gate as configured".
  assert.equal(result.status, 2, `expected exit 2, got ${result.status}\n${result.stderr}`);
  assert.match(result.stderr, /is not a configured test build/);
  assert.match(result.stderr, /YUKINAL_SSH_CMAKE_BUILD_DIR/);
  // The failure must name how to repair it, not only that something is wrong.
  assert.match(result.stderr, /cmake -S native -B/);

  // Refusing the configuration must happen before any step runs: a gate that
  // reports green after silently dropping the native suite is the bug being fixed.
  assert.doesNotMatch(result.stdout, /── publication hygiene/);
  assert.doesNotMatch(result.stdout, /check: green/);
});

test("the gate still declares the steps that catch what the review found", async () => {
  const source = await readFile(checkScript, "utf8");
  // Each of these covers a defect that shipped green before it existed. Removing one
  // is a silent regression, so an explicit assertion is the cheapest tripwire.
  for (const step of [
    "publication hygiene",
    "secret scan",
    "release artifact tests",
    "docs links",
    "build native test targets",
    "configured native CTest suite",
    "control plane tests (SQLite)",
    "Python Agent tests (full unittest discovery)",
    "desktop bundle (vite build)",
  ]) {
    assert.ok(source.includes(step), "gate step missing: " + step);
  }
  // The native suite must never be a soft skip when it was explicitly configured.
  assert.match(source, /YUKINAL_SSH_CMAKE_BUILD_DIR[\s\S]{0,600}?is not a configured test build/);
  // A stale native binary must not be able to satisfy CTest: the rebuild step has
  // to come before CTest and use the discovered build directory.
  const buildStep = source.indexOf('name: "build native test targets"');
  const ctestStep = source.indexOf('name: "configured native CTest suite"');
  assert.ok(buildStep !== -1 && ctestStep !== -1, "native build/CTest steps must both exist");
  assert.ok(buildStep < ctestStep, "native test targets must be built before CTest runs");
  const buildStepSource = source.slice(buildStep, ctestStep);
  assert.match(
    buildStepSource,
    /args: nativeBuildArgs\(nativeBuildDirectory \?\? "native\/build"\)/,
    "native build step must build the discovered directory via nativeBuildArgs",
  );
  // The argv itself is what CMake receives; pin it exactly.
  assert.deepEqual(nativeBuildArgs(path.join("native", "build-check")), [
    "--build",
    path.join("native", "build-check"),
    "--config",
    "Release",
    "--parallel",
  ]);
  assert.deepEqual(nativeBuildArgs(path.join("native", "build-check"), ["--target", "yukinal_ssh_tests"]), [
    "--build",
    path.join("native", "build-check"),
    "--config",
    "Release",
    "--parallel",
    "--target",
    "yukinal_ssh_tests",
  ]);
  assert.throws(() => nativeBuildArgs(undefined), /requires a configured native build directory/);
  // A too-old WebView2 SDK is environmental, not a test failure: the gate must say so.
  assert.match(
    webView2SdkRepairHint("error C2065: 'ICoreWebView2Settings9': undeclared identifier") ?? "",
    /local WebView2 SDK is older than native\/CMakeLists\.txt requires/,
  );
  assert.match(webView2SdkRepairHint("WebView2 SDK version mismatch") ?? "", /update the SDK/);
  assert.equal(webView2SdkRepairHint("error C2065: 'Unrelated': undeclared identifier"), undefined);
  // The build step must run through the fallback helper, which captures output and
  // is what makes the SDK hint (and the scoped retry) reachable at all.
  assert.match(
    buildStepSource,
    /run: \(\) => runNativeBuildStep\(nativeBuildDirectory\)/,
    "native build step must go through runNativeBuildStep",
  );
  // A failed rebuild has to stop the gate with an actionable hint, and a machine
  // with no configured native build must still skip rather than fail.
  assert.match(buildStepSource, /required: true/, "native build step must be required");
  assert.match(buildStepSource, /repairFromOutput: true/, "native build step must pick its repair hint from the failure");
  assert.match(buildStepSource, /skip: !nativeBuildDirectory/, "native build step must skip only when no build was discovered");
  // CTest with zero tests exits 0 by default; `--no-tests=error` is what makes an
  // empty suite fail the gate instead of printing a green result.
  assert.match(
    source.slice(ctestStep, ctestStep + 400),
    /"--no-tests=error"/,
    "CTest must be invoked with --no-tests=error",
  );
});

test("multi-config service binary resolution prefers the per-configuration directory", () => {
  const buildDirectory = path.join(repoRoot, "native", "build-check");
  const releaseBinary = path.join(buildDirectory, "Release", "yukinal-ssh-service.exe");

  // A Windows multi-config CI build has no binary in the build-dir root.
  assert.equal(
    resolveAgentServiceBinary(buildDirectory, (candidate) => candidate === releaseBinary),
    releaseBinary,
    "the Release/ service binary must be discovered",
  );
  // If both exist, the current multi-config output must win over a stale root binary.
  const rootBinary = path.join(buildDirectory, "yukinal-ssh-service");
  assert.equal(
    resolveAgentServiceBinary(buildDirectory, (candidate) => candidate === rootBinary || candidate === releaseBinary),
    releaseBinary,
    "a stale root-level binary must not shadow the current Release/ build",
  );
  // A single-config build has only the root-level names.
  assert.equal(
    resolveAgentServiceBinary(buildDirectory, (candidate) => candidate === rootBinary),
    rootBinary,
  );
  // No configured build directory means no candidates at all.
  assert.deepEqual(agentServiceBinaryCandidates(undefined), []);
  const candidates = agentServiceBinaryCandidates(buildDirectory);
  for (const configuration of ["Release", "RelWithDebInfo", "Debug"]) {
    for (const name of ["yukinal-ssh-service.exe", "yukinal-ssh-service"]) {
      assert.ok(
        candidates.includes(path.join(buildDirectory, configuration, name)),
        "missing candidate " + configuration + "/" + name,
      );
    }
  }
});

test("CTest with no tests is an error only with --no-tests=error", { skip: ctestAvailable ? false : "ctest is not on PATH" }, () => {
  // Pin the behaviour the gate depends on using a real empty CTest project: an empty
  // suite must exit 0 without the flag (the bug) and non-zero with it (the fix).
  const project = mkdtempSync(path.join(os.tmpdir(), "yukinal-ctest-empty-"));
  try {
    writeFileSync(path.join(project, "CTestTestfile.cmake"), "# deliberately empty test suite\n");
    const legacy = spawnSync("ctest", ["--test-dir", project, "-C", "Release"], { encoding: "utf8" });
    const fixed = spawnSync("ctest", ["--test-dir", project, "-C", "Release", "--no-tests=error"], { encoding: "utf8" });
    assert.equal(legacy.status, 0, "without --no-tests=error CTest must exit 0\n" + legacy.stdout + legacy.stderr);
    assert.notEqual(fixed.status, 0, "with --no-tests=error CTest must fail\n" + fixed.stdout + fixed.stderr);
    assert.match(fixed.stderr + fixed.stdout, /No tests were found|Errors while running CTest/i);
  } finally {
    rmSync(project, { recursive: true, force: true });
  }
});

const HELP_TEXT = [
  "The following are some of the valid targets for this Makefile:",
  "... all (the default if no target is provided)",
  "... clean",
  "... test",
  "... yukinal-desktop-host",
  "... yukinal-ssh-service",
  "... yukinal_desktop_host_core",
  "... yukinal_desktop_host_database_tests",
  "... yukinal_ssh_tests",
  "... desktop_host/src/main.obj",
  "... desktop_host/src/main.i",
  "... desktop_host/src/main.s",
].join("\n");

test("WebView2-only build failures retry without the desktop host target", () => {
  const targets = nativeBuildTargetsFromHelp(HELP_TEXT);
  // Per-file and pseudo targets must never be handed to `cmake --target`.
  assert.ok(!targets.includes("all") && !targets.includes("clean") && !targets.includes("test"));
  assert.ok(!targets.some((target) => target.includes("/")), "per-file targets must be filtered out");
  // Underscore spellings are library/test targets and stay buildable.
  assert.ok(targets.includes("yukinal_desktop_host_core"));

  const decision = nativeBuildFallbackDecision({
    status: 2,
    output: "windows_window_frame.cpp(296): error C2065: 'ICoreWebView2Settings9'",
    targetList: targets,
  });
  assert.equal(decision.retry, true, "a WebView2 failure must be retried without the GUI target");
  assert.ok(!decision.targets.includes("yukinal-desktop-host"), "the GUI target must be dropped from the retry");
  assert.ok(!decision.targets.some((target) => /desktop-host/u.test(target)));
  assert.ok(decision.targets.includes("yukinal_ssh_tests"), "every other target must be kept");

  // An unrelated compile error must fail immediately: no retry, no target list.
  assert.deepEqual(
    nativeBuildFallbackDecision({ status: 2, output: "error C2065: 'Unrelated'", targetList: targets }),
    { retry: false, targets: [] },
  );
  // A successful build never retries.
  assert.deepEqual(
    nativeBuildFallbackDecision({ status: 0, output: "", targetList: targets }),
    { retry: false, targets: [] },
  );
});

test("the native build step retries scoped and reports desktopHostSkipped", async () => {
  const calls = [];
  const webView2Failure =
    "yukinal-desktop-host.vcxproj -> windows_window_frame.cpp(296): error C2065: 'ICoreWebView2Settings9': undeclared identifier";
  const fakeRun = (responses) => async (command, args) => {
    calls.push([command, ...args]);
    const response = responses.shift();
    assert.ok(response, "unexpected extra runner call: " + JSON.stringify([command, ...args]));
    return response;
  };
  // Toolchain resolution is exercised by its own test; here it is pinned to the
  // plain form so this test only asserts the retry decision.
  const plainInvocation = (args) => ({ command: "cmake", args });

  // Path 1: full build fails on WebView2, help lists targets, scoped retry succeeds.
  const recovered = await runNativeBuildStep("native/build-check", {
    resolveInvocation: plainInvocation,
    run: fakeRun([
      { status: 2, output: webView2Failure },
      { status: 0, output: HELP_TEXT },
      { status: 0, output: "[100%] Built target yukinal_ssh_tests" },
    ]),
  });
  assert.equal(recovered.status, 0);
  assert.equal(recovered.desktopHostSkipped, true, "the partial build must be flagged for the warning block");
  assert.match(recovered.output, /ICoreWebView2Settings9/, "the failing output must be retained");
  // The full build came first, then help discovery, then one scoped retry with --target.
  assert.deepEqual(calls[0], ["cmake", ...nativeBuildArgs("native/build-check")]);
  assert.deepEqual(calls[1], ["cmake", "--build", "native/build-check", "--target", "help"]);
  assert.equal(calls[2][0], "cmake");
  assert.equal(calls[2].includes("-host"), false);
  assert.ok(calls[2].includes("--target") && !calls[2].some((arg) => /desktop-host/u.test(arg)));
  assert.equal(calls.length, 3, "exactly one scoped retry");

  // Path 2: the retry hits a real source error -> the step fails with the retry output.
  calls.length = 0;
  const failedRetry = await runNativeBuildStep("native/build-check", {
    resolveInvocation: plainInvocation,
    run: fakeRun([
      { status: 2, output: webView2Failure },
      { status: 0, output: HELP_TEXT },
      { status: 2, output: "desktop_ssh.cpp(238): fatal error C1020: unexpected #endif" },
    ]),
  });
  assert.equal(failedRetry.status, 2);
  assert.equal(failedRetry.desktopHostSkipped, false);
  assert.match(failedRetry.output, /unexpected #endif/, "the real retry failure must be reported");

  // Path 3: non-WebView2 failure -> exactly one call, no retry.
  calls.length = 0;
  const unrelated = await runNativeBuildStep("native/build-check", {
    resolveInvocation: plainInvocation,
    run: fakeRun([{ status: 2, output: "error C2065: 'Unrelated': undeclared identifier" }]),
  });
  assert.equal(unrelated.status, 2);
  assert.equal(unrelated.desktopHostSkipped, false);
  assert.equal(calls.length, 1, "an unrelated failure must not trigger a retry");
});

test("toolchain discovery keeps plain cmake when a build tool is on PATH", () => {
  resetNativeToolchainCache();
  const args = nativeBuildArgs("native/build-check");
  const invocation = resolveNativeBuildInvocation(args, {
    platform: "win32",
    env: { PATH: "C:\\Tools" },
    which: (name) => (name === "nmake" ? "C:\\Tools\\nmake.exe" : undefined),
    run: () => assert.fail("vcvars must not be discovered when nmake is on PATH"),
  });
  assert.deepEqual(invocation, { command: "cmake", args }, "plain cmake argv must be unchanged");
  assert.equal(invocation.args, args, "the caller's argv must be passed through untouched");

  // A generator-specific tool is what counts: an NMake tree needs nmake, not ninja.
  resetNativeToolchainCache();
  const nmakeVcvars = "C:\\VS2022\\VC\\Auxiliary\\Build\\vcvars64.bat";
  const needsNmake = resolveNativeBuildInvocation(args, {
    platform: "win32",
    env: { PATH: "C:\\Ninja" },
    generator: "NMake Makefiles",
    which: (name) => (name === "ninja" ? "C:\\Ninja\\ninja.exe" : undefined),
    exists: (candidate) => candidate === nmakeVcvars,
    // vcvars must be discovered here: ninja cannot drive an NMake tree.
    run: () => ({ status: 0, stdout: "C:\\VS2022\r\n" }),
  });
  assert.equal(needsNmake.command, "cmd");
  assert.match(needsNmake.args[3], /vcvars64\.bat/);
});

test("a missing build tool loads vcvars64.bat through cmd /c", () => {
  resetNativeToolchainCache();
  const vcvars = "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Auxiliary\\Build\\vcvars64.bat";
  const vswhereCalls = [];
  const invocation = resolveNativeBuildInvocation(nativeBuildArgs("native/build-check"), {
    platform: "win32",
    env: { PATH: "C:\\Windows" },
    which: () => undefined,
    exists: (candidate) => candidate === vcvars,
    run: (command, args) => {
      vswhereCalls.push([command, ...args]);
      return { status: 0, stdout: "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\r\n" };
    },
  });
  assert.equal(invocation.command, "cmd");
  assert.equal(invocation.viaVcvars, vcvars);
  // `/s /c "<line>"` is the only spelling that survives Node's Windows argv
  // quoting: the outer quotes are stripped by cmd and the vcvars path stays quoted.
  assert.deepEqual(invocation.args.slice(0, 3), ["/d", "/s", "/c"]);
  assert.equal(invocation.windowsVerbatimArguments, true);
  const commandLine = invocation.args[3];
  assert.match(commandLine, /^"call "C:\\Program Files\\Microsoft Visual Studio\\2022\\Community\\VC\\Auxiliary\\Build\\vcvars64\.bat" >nul 2>&1 && cmake /);
  assert.match(commandLine, /"$/);
  // Every cmake argument is still present, in order, after the cmake executable.
  assert.match(commandLine, /cmake --build native\/build-check --config Release --parallel"$/);
  // vswhere was asked for the C++ workload, not just any VS install.
  assert.equal(vswhereCalls[0][0], "C:\\Program Files (x86)\\Microsoft Visual Studio\\Installer\\vswhere.exe");
  assert.ok(vswhereCalls[0].includes("Microsoft.VisualStudio.Component.VC.Tools.x86.x64"));
  assert.ok(vswhereCalls[0].includes("installationPath"));

  // Arguments with spaces are quoted so cmd does not re-split them.
  resetNativeToolchainCache();
  const spaced = resolveNativeBuildInvocation(nativeBuildArgs("C:\\My Builds\\check"), {
    platform: "win32",
    env: { PATH: "" },
    which: () => undefined,
    exists: () => true,
    run: () => ({ status: 0, stdout: "C:\\VS\r\n" }),
  });
  assert.match(spaced.args[3], /--build "C:\\My Builds\\check"/);

  // Outside Windows nothing is discovered: the plain invocation is returned.
  resetNativeToolchainCache();
  assert.deepEqual(resolveNativeBuildInvocation(nativeBuildArgs("native/build"), { platform: "linux", which: () => undefined }), {
    command: "cmake",
    args: nativeBuildArgs("native/build"),
  });
});

test("the native build step forwards windowsVerbatimArguments for the vcvars form", async () => {
  resetNativeToolchainCache();
  const vcvars = "C:\\VS\\VC\\Auxiliary\\Build\\vcvars64.bat";
  const seen = [];
  const result = await runNativeBuildStep("native/build-check", {
    platform: "win32",
    env: { PATH: "" },
    which: () => undefined,
    exists: (candidate) => candidate === vcvars,
    vswhereRun: () => ({ status: 0, stdout: "C:\\VS\r\n" }),
    run: async (command, args, options = {}) => {
      seen.push({ command, args, options });
      // A wrapper shell that reports success is enough: the step is only being
      // checked for how it spawns, not for what CMake does.
      return { status: 0, output: "" };
    },
  });
  assert.equal(result.status, 0);
  assert.equal(seen.length, 1);
  assert.equal(seen[0].command, "cmd");
  assert.equal(seen[0].options.windowsVerbatimArguments, true, "cmd quoting is only correct verbatim");
});
test("no toolchain and no vcvars is a precise toolchain failure, not a compiler error", async () => {
  resetNativeToolchainCache();
  const calls = [];
  const result = await runNativeBuildStep("native/build-check", {
    run: async (command, args) => {
      calls.push([command, ...args]);
      return { status: 0, output: "" };
    },
    which: () => undefined,
    platform: "win32",
    env: { PATH: "" },
    exists: () => false,
    // vswhere finds no Visual Studio install with the C++ workload.
    vswhereRun: () => ({ status: 1, stdout: "" }),
  });
  assert.equal(result.status, 1);
  assert.equal(result.toolchainMissing, true);
  assert.equal(result.desktopHostSkipped, false);
  assert.equal(result.hint, NATIVE_TOOLCHAIN_HINT);
  assert.equal(calls.length, 0, "no build may be attempted without a toolchain");
  // The hint says what to install and never blames the source.
  assert.match(NATIVE_TOOLCHAIN_HINT, /install Visual Studio 2022 with the C\+\+ workload/);
  assert.match(NATIVE_TOOLCHAIN_HINT, /developer shell \(vcvars64\.bat\)/);
  assert.doesNotMatch(NATIVE_TOOLCHAIN_HINT, /compiler\/linker error/);
});

test("the fix hint distinguishes a missing build tool from a compiler error", () => {
  const nmakeMissing = [
    "Warning: NMake does not support parallel builds. Ignoring parallel build command line option.",
    "no such file or directory",
    "CMake Error: Generator: build tool execution failed, command was: nmake -f Makefile /nologo",
  ].join("\n");
  assert.equal(isMissingBuildToolOutput(nmakeMissing), true);
  assert.equal(nativeBuildRepairHint({ output: nmakeMissing }), NATIVE_TOOLCHAIN_HINT);
  // The exact sentinel case wins even when the output is empty.
  assert.equal(nativeBuildRepairHint({ output: "", toolchainMissing: true }), NATIVE_TOOLCHAIN_HINT);

  const compilerError = "windows_window_frame.cpp(296): error C2065: 'ICoreWebView2Settings9': undeclared identifier";
  assert.equal(isMissingBuildToolOutput(compilerError), false);
  const generic = nativeBuildRepairHint({ output: compilerError, nativeBuildDirectory: "native/build-nmake" });
  assert.match(generic, /fix the first compiler\/linker error above/);
  assert.match(generic, /cmake --build native\/build-nmake --config Release --parallel/);
  assert.notEqual(generic, NATIVE_TOOLCHAIN_HINT);
});

test("the desktop-host warning names both versions and the CTest consequence", () => {
  const warning = nativeDesktopHostSkippedWarning({
    localVersion: "1.0.1150.38",
    requiredVersion: "1.0.2903.40",
    errorLine: "windows_window_frame.cpp(296): error C2065: 'ICoreWebView2Settings9'",
  });
  const text = warning.join("\n");
  assert.match(text, /desktop host target was NOT compiled locally/);
  assert.match(text, /WebView2 SDK 1\.0\.1150\.38 < required 1\.0\.2903\.40/);
  assert.match(text, /first WebView2 error: .*ICoreWebView2Settings9/);
  assert.match(text, /desktop_host_self_check exercises the previous build/);
  assert.match(text, /local native coverage is NOT full; CI builds the full set/);
  // The required version comes from the CMake pin, not a hardcoded literal.
  assert.equal(requiredWebView2Version('set(WEBVIEW_MSWEBVIEW2_VERSION "1.0.2903.40" CACHE STRING "")'), "1.0.2903.40");
  assert.equal(requiredWebView2Version("# nothing here"), undefined);
  assert.match(
    webView2ErrorLine("windows_window_frame.cpp(296): error C2065: 'ICoreWebView2Settings9'") ?? "",
    /ICoreWebView2/,
  );
  assert.equal(webView2ErrorLine("error C2065: 'Unrelated'"), undefined);
  // MSVC on a non-English Windows emits undecodable bytes; the line quoted in the
  // warning must still name the file and the missing WebView2 symbol.
  const localized = webView2ErrorLine(
    "D:\\yukinal\\native\\desktop_host\\src\\windows_window_frame.cpp(296): error C2065: '\uFFFD\uFFFDICoreWebView2Settings9\uFFFD': \uFFFD\uFFFD\uFFFD",
  );
  assert.match(localized ?? "", /windows_window_frame\.cpp\(296\)/);
  assert.match(localized ?? "", /ICoreWebView2Settings9/);
  assert.match(localized ?? "", /localized compiler text omitted/);
});

test("a captured step streams output and reports the child's exit status", async () => {
  // The native build step uses this helper so it can both show live progress and
  // inspect the output for a WebView2 SDK failure; a non-zero exit must survive.
  const ok = await runStepCapturingOutput(process.execPath, ["-e", "console.log('ok-output')"]);
  assert.equal(ok.status, 0);
  assert.match(ok.output, /ok-output/);

  const failed = await runStepCapturingOutput(process.execPath, [
    "-e",
    "console.error(\"error C2065: 'ICoreWebView2Settings9'\"); process.exit(3)",
  ]);
  assert.equal(failed.status, 3);
  assert.match(failed.output, /ICoreWebView2Settings9/);
  assert.match(webView2SdkRepairHint(failed.output) ?? "", /local WebView2 SDK is older/);
});
