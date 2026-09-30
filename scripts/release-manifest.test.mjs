import assert from "node:assert/strict";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

import { PACKAGE_SUFFIXES, verifyReleaseManifest, writeReleaseManifest } from "./lib/release-manifest.mjs";

async function withBundle(run) {
  const parent = await mkdtemp(path.join(os.tmpdir(), "yukinal-release-manifest-"));
  const bundleDir = path.join(parent, "bundle");
  await mkdir(path.join(bundleDir, "nsis"), { recursive: true });
  await writeFile(path.join(bundleDir, "nsis", "Yukinal-setup.exe"), "installer bytes");
  await writeFile(path.join(bundleDir, "build.log"), "not a release artifact");
  try {
    await run(bundleDir);
  } finally {
    await rm(parent, { recursive: true, force: true });
  }
}

test("release manifest records package hashes and ignores non-artifact files", async () => {
  await withBundle(async (bundleDir) => {
    const manifest = await writeReleaseManifest({
      bundleDir,
      appVersion: "1.2.3",
      runtimeVersion: "24.21.0",
      generatedAt: "2026-09-29T00:00:00.000Z",
      platform: "win32",
      architecture: "x64",
      nodeVersion: "v24.21.0",
      source: { revision: "abc123", dirty: true },
    });

    assert.deepEqual(manifest.artifacts.map(({ path: artifactPath }) => artifactPath), ["nsis/Yukinal-setup.exe"]);
    assert.equal(manifest.artifacts[0].bytes, Buffer.byteLength("installer bytes"));
    assert.equal(manifest.application.bundledNodeRuntime, "24.21.0");
    assert.equal(manifest.build.source.dirty, true);
    assert.match(await readFile(path.join(bundleDir, "SHA256SUMS.txt"), "utf8"), /^[a-f0-9]{64}  nsis\/Yukinal-setup\.exe\n$/);
    assert.equal((await verifyReleaseManifest(bundleDir)).artifacts.length, 1);
    await writeReleaseManifest({ bundleDir, appVersion: "1.2.3", runtimeVersion: "24.21.0" });
    assert.equal((await verifyReleaseManifest(bundleDir)).artifacts.length, 1);
  });
});

test("release manifest verification detects changed payload bytes", async () => {
  await withBundle(async (bundleDir) => {
    await writeReleaseManifest({ bundleDir, appVersion: "1.2.3", runtimeVersion: "24.21.0" });
    await writeFile(path.join(bundleDir, "nsis", "Yukinal-setup.exe"), "changed bytes");
    await assert.rejects(verifyReleaseManifest(bundleDir), /size changed|checksum mismatch/);
  });
});

test("release manifest verification refuses an artifact added after generation", async () => {
  await withBundle(async (bundleDir) => {
    await writeReleaseManifest({ bundleDir, appVersion: "1.2.3", runtimeVersion: "24.21.0" });
    await writeFile(path.join(bundleDir, "Yukinal.msi"), "another installer");
    await assert.rejects(verifyReleaseManifest(bundleDir), /not covered by the release manifest/);
  });
});

test("release manifest verification rejects a path escaping the bundle", async () => {
  await withBundle(async (bundleDir) => {
    await writeReleaseManifest({ bundleDir, appVersion: "1.2.3", runtimeVersion: "24.21.0" });
    const manifestPath = path.join(bundleDir, "release-manifest.json");
    const manifest = JSON.parse(await readFile(manifestPath, "utf8"));
    manifest.artifacts[0].path = "../outside.exe";
    await writeFile(manifestPath, `${JSON.stringify(manifest)}\n`);
    await assert.rejects(verifyReleaseManifest(bundleDir), /artifact path escapes bundle directory/);
  });
});

test("CI uploads every artifact suffix accepted by the release manifest", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const workflow = await readFile(path.join(root, ".github", "workflows", "package.yml"), "utf8");
  for (const suffix of PACKAGE_SUFFIXES) {
    const glob = `target/release/bundle/**/*${suffix}`;
    const caseVariantExists = suffix === ".appimage" && workflow.includes("target/release/bundle/**/*.AppImage");
    assert.ok(workflow.includes(glob) || caseVariantExists, `package workflow does not upload ${suffix} artifacts`);
  }
});

test("package CI invokes release manifest verification with the supported CLI argument", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const workflow = await readFile(path.join(root, ".github", "workflows", "package.yml"), "utf8");

  assert.match(workflow, /run: node scripts\/write-release-manifest\.mjs --verify/);
  assert.doesNotMatch(workflow, /run: pnpm release:manifest -- --verify/);
});

test("Windows package CI runs MSI lifecycle smoke and preserves failure diagnostics", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const workflow = await readFile(path.join(root, ".github", "workflows", "package.yml"), "utf8");
  const lifecycleSmoke = await readFile(path.join(root, "scripts", "smoke-msi-install-windows.ps1"), "utf8");

  assert.match(
    workflow,
    /- name: Smoke MSI install, packaged launch, shutdown, and uninstall[\s\S]*?if: matrix\.os == 'windows-latest'[\s\S]*?timeout-minutes: 10[\s\S]*?smoke-msi-install-windows\.ps1 -AllowInstalledNodeForPathIsolationSmoke/,
  );
  assert.match(
    workflow,
    /- name: Upload MSI lifecycle diagnostics[\s\S]*?if: failure\(\) && matrix\.os == 'windows-latest'[\s\S]*?target\/release\/msi-install-smoke\/\*\*\/\*\.log/,
  );
  assert.match(
    lifecycleSmoke,
    /\$installerSmokeRoot\s*=\s*Join-Path \$repoRoot 'target\/release\/installer-smoke'[\s\S]*?New-Item -ItemType Directory -Path \$installerSmokeRoot -Force[\s\S]*?\$smokeProcess = Start-Process/,
    "MSI lifecycle smoke must create the packaged-app helper's log directory before invoking it",
  );
});

test("installed Windows smoke matches the quoted sidecar entrypoint used under Program Files", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const smoke = await readFile(path.join(root, "scripts", "smoke-installed-windows.ps1"), "utf8");

  assert.ok(smoke.includes("index\\.js(?=$|[\\s\"''])"), "sidecar matcher must accept a quote after a spaced path argument");
  assert.equal(
    [...smoke.matchAll(/CommandLine -match \$sidecarCommandPattern/g)].length,
    3,
    "startup, shutdown verification, and cleanup must share the same sidecar matcher",
  );
});

test("Windows package CI runs NSIS uninstall data-preservation smoke and preserves diagnostics", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const workflow = await readFile(path.join(root, ".github", "workflows", "package.yml"), "utf8");

  assert.match(
    workflow,
    /- name: Smoke NSIS uninstall data preservation and update policy[\s\S]*?if: matrix\.os == 'windows-latest'[\s\S]*?smoke-nsis-checkbox-ui-windows\.ps1/,
  );
  assert.match(
    workflow,
    /- name: Upload NSIS checkbox UI diagnostics[\s\S]*?if: failure\(\) && matrix\.os == 'windows-latest'[\s\S]*?nsis-checkbox-ui-smoke\/\*\*\/\*\.log/,
  );
  assert.match(
    workflow,
    /- name: Smoke NSIS install, update, sidecar startup, and uninstall data preservation[\s\S]*?if: matrix\.os == 'windows-latest'[\s\S]*?timeout-minutes: 10[\s\S]*?smoke-nsis-install-windows\.ps1/,
  );
  assert.match(
    workflow,
    /- name: Upload NSIS lifecycle diagnostics[\s\S]*?if: failure\(\) && matrix\.os == 'windows-latest'[\s\S]*?target\/release\/nsis-install-smoke\/\*\*\/\*\.log/,
  );
});

test("Linux package CI installs, checks, and purges the exact Debian artifact", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const workflow = await readFile(path.join(root, ".github", "workflows", "package.yml"), "utf8");
  const smoke = await readFile(path.join(root, "scripts", "smoke-deb-install-linux.sh"), "utf8");

  assert.match(
    workflow,
    /- name: Smoke Debian install, bundled Agent, and purge[\s\S]*?if: matrix\.os == 'ubuntu-22\.04'[\s\S]*?YUKINAL_ALLOW_DEB_INSTALL_SMOKE: "1"[\s\S]*?smoke-deb-install-linux\.sh/,
  );
  assert.ok(smoke.includes('dpkg --install "$deb"'), "Linux smoke must install the built artifact");
  assert.ok(smoke.includes("trap cleanup EXIT"), "Linux smoke must arrange package cleanup before mutation");
  assert.ok(smoke.includes('dpkg --purge "$package"'), "Linux smoke must purge only the package it installed");
  assert.ok(smoke.includes("smoke-installed-agent.mjs"), "Linux smoke must handshake through the installed resources");
  assert.ok(smoke.includes("dpkg-query -W -f='${db:Status-Abbrev}'"), "Linux smoke must refuse a pre-existing package entry");
  assert.ok(smoke.includes("YUKINAL_ALLOW_DEB_INSTALL_SMOKE"), "Linux system install must be explicitly opt-in");
});

test("macOS package CI copies the DMG app into an isolated Applications directory and smokes installed resources", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const workflow = await readFile(path.join(root, ".github", "workflows", "package.yml"), "utf8");
  const smoke = await readFile(path.join(root, "scripts", "smoke-dmg-install-macos.sh"), "utf8");

  assert.match(
    workflow,
    /- name: Smoke macOS DMG copy and bundled Agent[\s\S]*?if: matrix\.os == 'macos-latest'[\s\S]*?smoke-dmg-install-macos\.sh/,
  );
  assert.ok(smoke.includes("hdiutil attach -readonly"), "macOS smoke must mount the built DMG read-only");
  assert.ok(smoke.includes('ditto "$app_bundle" "$installed_app"'), "macOS smoke must copy the app bundle");
  assert.ok(smoke.includes("hdiutil detach"), "macOS smoke must detach its temporary mount");
  assert.ok(smoke.includes("smoke-installed-agent.mjs"), "macOS smoke must handshake through copied app resources");
});

test("installed package Agent smoke checks the pinned bundled runtime and required resources", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const smoke = await readFile(path.join(root, "scripts", "smoke-installed-agent.mjs"), "utf8");
  const protocolSmoke = await readFile(path.join(root, "scripts", "lib", "sidecar-smoke.mjs"), "utf8");

  assert.ok(smoke.includes('join(resourceRoot, "runtime", process.platform === "win32" ? "node.exe" : "node")'));
  assert.ok(smoke.includes('join(resourceRoot, "agent", "index.js")'));
  assert.ok(smoke.includes('join(resourceRoot, "runtime", "LICENSE")'));
  assert.ok(smoke.includes('join(resourceRoot, "NOTICE")'));
  assert.ok(smoke.includes('`v${runtimeManifest.version}`'), "installed smoke must compare the packaged runtime with the repository pin");
  assert.ok(smoke.includes("dataDir"), "installed smoke must isolate Agent data in a temporary directory");
  assert.ok(protocolSmoke.includes("options.dataDir"), "shared sidecar smoke must pass its isolated data directory to the Agent");
});

test("package build runs the generated NSIS policy check only on Windows", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const packageScript = await readFile(path.join(root, "scripts", "package.mjs"), "utf8");
  const guardAt = packageScript.indexOf('if (process.platform === "win32")');
  const nsisCheckAt = packageScript.indexOf('console.log("\\n── generated NSIS uninstall policy")');
  const manifestAt = packageScript.indexOf('console.log("\\n── release artifact manifest")');

  assert.ok(guardAt !== -1, "package script must identify the Windows-only NSIS platform");
  assert.ok(nsisCheckAt > guardAt && manifestAt > nsisCheckAt, "NSIS validation must be guarded while manifest generation remains afterward");
  assert.ok(packageScript.slice(guardAt, manifestAt).includes('"scripts/check-nsis-uninstall.mjs"'));
});

test("installer-sensitive pull requests run the Windows lifecycle gate without packaging macOS or Linux", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const workflow = await readFile(path.join(root, ".github", "workflows", "package.yml"), "utf8");
  const pathFilter = workflow.match(/pull_request:\s*\n\s*paths:\s*\n([\s\S]*?)(?=\n\S)/)?.[1];

  assert.ok(pathFilter, "package workflow must define pull-request paths for installer acceptance");
  for (const requiredPath of [
    "apps/desktop/src-tauri/nsis/**",
    "apps/desktop/src-tauri/tauri.conf.json",
    "scripts/check-desktop-window.ps1",
    "scripts/check-nsis-uninstall.mjs",
    "scripts/nsis-uninstall-policy.test.mjs",
    "scripts/lib/sidecar-smoke.mjs",
    "scripts/smoke-installed-agent.mjs",
    "scripts/smoke-deb-install-linux.sh",
    "scripts/smoke-dmg-install-macos.sh",
    "scripts/smoke-installed-windows.ps1",
    "scripts/smoke-msi-install-windows.ps1",
    "scripts/smoke-nsis-install-windows.ps1",
    "scripts/smoke-nsis-checkbox-ui-windows.ps1",
  ]) {
    assert.ok(pathFilter.includes(`- ${requiredPath}`), `installer path filter does not include ${requiredPath}`);
  }
  assert.match(
    workflow,
    /os:\s*\$\{\{\s*fromJSON\(github\.event_name == 'pull_request'\s*&&\s*'\["windows-latest"\]'\s*\|\|\s*'\["windows-latest","macos-latest","ubuntu-22\.04"\]'\)\s*\}\}/,
    "package matrix must select Windows for pull requests and all platforms otherwise",
  );
  assert.doesNotMatch(
    workflow,
    /^\s*if:\s*\$\{\{[^\n]*matrix\.os/m,
    "job-level conditions must not reference matrix context before expansion",
  );
});

test("release workflows pin Node 24-compatible GitHub Actions", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const checkWorkflow = await readFile(path.join(root, ".github", "workflows", "check.yml"), "utf8");
  const packageWorkflow = await readFile(path.join(root, ".github", "workflows", "package.yml"), "utf8");

  for (const workflow of [checkWorkflow, packageWorkflow]) {
    assert.match(workflow, /actions\/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1\s+# v7\.0\.1/);
    assert.match(workflow, /actions\/setup-node@820762786026740c76f36085b0efc47a31fe5020\s+# v7\.0\.0/);
    assert.match(workflow, /pnpm\/action-setup@008330803749db0355799c700092d9a85fd074e9\s+# v6\.0\.9/);
    assert.doesNotMatch(workflow, /(?:actions\/checkout|actions\/setup-node|pnpm\/action-setup)@[0-9a-f]{40}\s+# v4\b/);
  }

  assert.match(packageWorkflow, /actions\/upload-artifact@bbbca2ddaa5d8feaa63e36b76fdaad77386f024f\s+# v7\.0\.0/);
  assert.doesNotMatch(packageWorkflow, /actions\/upload-artifact@[0-9a-f]{40}\s+# v4\b/);
});

test("dependency advisory workflow audits version tags with read-only permissions", async () => {
  const root = fileURLToPath(new URL("..", import.meta.url));
  const workflow = await readFile(path.join(root, ".github", "workflows", "check.yml"), "utf8");
  const advisoryStart = workflow.indexOf("\n  advisory:");
  assert.notEqual(advisoryStart, -1, "check workflow must keep an independent advisory job");
  const advisory = workflow.slice(advisoryStart);
  const jobHeader = advisory.slice(0, advisory.indexOf("\n    steps:"));

  assert.match(workflow, /on:\s*\n\s+push:\s*\n\s+branches:\s*\[main\]\s*\n\s+tags:\s*\["v\*"\]/);
  assert.match(workflow, /gate:\s*\n[\s\S]*?if: github\.event_name != 'push' \|\| github\.ref_type != 'tag'/);
  assert.doesNotMatch(jobHeader, /^\s+if:/m, "the advisory job must not be skipped for release tags");
  assert.match(jobHeader, /permissions:\s*\n\s+contents:\s*read/);
  assert.doesNotMatch(jobHeader, /checks:\s*write/, "the advisory job must not request a check-write token");
  assert.match(advisory, /persist-credentials:\s*false/);
  assert.match(advisory, /run:\s*pnpm audit/);
  assert.match(advisory, /cargo install cargo-audit --locked --version 0\.22\.2/);
  assert.match(advisory, /run:\s*cargo audit/);
  assert.doesNotMatch(advisory, /rustsec\/audit-check@/, "the Node 20 action must not re-enter the advisory job");
});
