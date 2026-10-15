#!/usr/bin/env node
/** Check that the Tauri bundle, Python sidecar resolver, and build hook agree. */

import { readFileSync, existsSync, statSync } from "node:fs";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const tauriDir = join(root, "apps", "desktop", "src-tauri");
const confPath = join(tauriDir, "tauri.conf.json");
const problems = [];
const fail = (message) => problems.push(message);
const read = (path) => readFileSync(path, "utf8");

let conf;
try {
  conf = JSON.parse(read(confPath));
} catch (error) {
  console.error(`✗ ${confPath} is not valid JSON: ${error.message}`);
  process.exit(1);
}

const bundle = conf.bundle ?? {};
if (bundle.active !== true) fail("bundle.active is not true");

const resources = bundle.resources;
const expectedDestinations = ["NOTICE", "python-agent", "ssh-service"];
if (resources === null || typeof resources !== "object" || Array.isArray(resources)) {
  fail('bundle.resources must be a map ({"<source>": "<destination>"})');
} else {
  const entries = Object.entries(resources);
  const normalized = entries.map(([, destination]) => String(destination).replace(/\\/g, "/").replace(/\/+$/, ""));
  const actual = [...normalized].sort();
  if (actual.join(",") !== expectedDestinations.join(",")) {
    fail(`bundle.resources must contain only ${expectedDestinations.join(", ")}; found ${actual.join(", ")}`);
  }

  const expectedSources = new Map([
    ["python-agent", join(root, "agent", "runtime")],
    ["ssh-service", join(tauriDir, "resources", "ssh-service")],
    ["NOTICE", join(root, "NOTICE")],
  ]);
  for (const [source, destination] of entries) {
    const normalizedDestination = String(destination).replace(/\\/g, "/").replace(/\/+$/, "");
    const resolvedSource = resolve(tauriDir, source);
    const expectedSource = expectedSources.get(normalizedDestination);
    if (!expectedSource) continue;
    if (resolvedSource !== expectedSource) {
      fail(`bundle.resources source for ${normalizedDestination} resolves to ${resolvedSource}, expected ${expectedSource}`);
    }
    if (!existsSync(resolvedSource)) {
      fail(`bundle.resources source for ${normalizedDestination} does not exist: ${resolvedSource}`);
    } else if (normalizedDestination !== "NOTICE" && !statSync(resolvedSource).isDirectory()) {
      fail(`bundle.resources source for ${normalizedDestination} must be a directory`);
    }
  }

  for (const directory of [join(root, "agent", "runtime"), join(tauriDir, "resources", "ssh-service")]) {
    if (!existsSync(join(directory, ".gitkeep"))) {
      fail(`${directory} must remain present in a fresh checkout`);
    }
  }
  if (entries.some(([, destination]) => /^(?:agent\/|runtime\/?$)/.test(String(destination).replace(/\\/g, "/")))) {
    fail("the installer must not ship the legacy Node Agent or Node runtime");
  }
}

if (!existsSync(join(root, "scripts", "build-python-agent.mjs"))) {
  fail("scripts/build-python-agent.mjs is missing");
}
if (!existsSync(join(root, "scripts", "build-ssh-service.mjs"))) {
  fail("scripts/build-ssh-service.mjs is missing");
}

// The bundle must carry platform-specific icons and explicit package targets.
const REQUIRED_ICONS = ["32x32.png", "128x128.png", "128x128@2x.png", "icon.icns", "icon.ico"];
const icons = bundle.icon;
if (!Array.isArray(icons) || icons.length === 0) {
  fail("bundle.icon is missing");
} else {
  for (const icon of icons) {
    if (!existsSync(resolve(tauriDir, icon))) fail(`bundle.icon lists ${icon}, which does not exist`);
  }
  for (const required of REQUIRED_ICONS) {
    if (!icons.some((icon) => icon.replace(/\\/g, "/").endsWith(`/${required}`) || icon === required)) {
      fail(`bundle.icon does not include icons/${required}`);
    }
  }
}

const PLATFORM_TARGETS = {
  windows: ["nsis", "msi"],
  macOS: ["app", "dmg"],
  linux: ["deb", "rpm", "appimage"],
};
const targets = bundle.targets;
if (targets === "all") {
  fail('bundle.targets is "all"');
} else if (!Array.isArray(targets) || targets.length === 0) {
  fail("bundle.targets must be a non-empty list");
} else {
  for (const [platform, required] of Object.entries(PLATFORM_TARGETS)) {
    for (const target of required) {
      if (!targets.includes(target)) fail(`bundle.targets (${platform}) is missing "${target}"`);
    }
  }
}

// Tauri direct builds and package.mjs must stage current sidecars before the UI build.
const beforeBuild = conf.build?.beforeBuildCommand ?? "";
const order = ["build:libs", "build-python-agent.mjs", "build-ssh-service.mjs", "@yukinal/desktop"];
let cursor = -1;
for (const token of order) {
  const index = beforeBuild.indexOf(token);
  if (index === -1) {
    fail(`build.beforeBuildCommand does not run "${token}" (got: ${beforeBuild})`);
    break;
  }
  if (index < cursor) {
    fail(`build.beforeBuildCommand runs "${token}" out of order (got: ${beforeBuild})`);
    break;
  }
  cursor = index;
}

// Linux installs must declare the system libraries required by the WebView.
const debDepends = bundle.linux?.deb?.depends;
if (!Array.isArray(debDepends) || debDepends.length === 0) fail("bundle.linux.deb.depends is empty");
for (const required of ["libwebkit2gtk", "libgtk-3"]) {
  if (!debDepends?.some((dependency) => String(dependency).includes(required))) {
    fail(`bundle.linux.deb.depends does not include ${required}`);
  }
}
const rpmDepends = bundle.linux?.rpm?.depends;
if (!Array.isArray(rpmDepends) || rpmDepends.length === 0) fail("bundle.linux.rpm.depends is empty");
for (const required of ["webkit2gtk", "gtk3"]) {
  if (!rpmDepends?.some((dependency) => String(dependency).includes(required))) {
    fail(`bundle.linux.rpm.depends does not include ${required}`);
  }
}

// Rust and Tauri must resolve the same frozen Python Agent in packaged builds.
const configRs = join(root, "crates", "core", "src", "sidecar", "config.rs");
const configSource = read(configRs);
const resolverStart = configSource.indexOf("pub fn from_env_with_resources");
const pythonSelection = configSource.indexOf("map(packaged_python_entry)", resolverStart);
const legacyNodeSelection = configSource.indexOf("map(packaged_entry)", resolverStart);
if (resolverStart === -1 || pythonSelection === -1) {
  fail(`${configRs}: packaged resource resolution must select packaged_python_entry()`);
} else if (legacyNodeSelection !== -1 && pythonSelection > legacyNodeSelection) {
  fail(`${configRs}: the Python Agent must be selected before the legacy Node fallback`);
}
const pythonEntryAt = configSource.indexOf("pub fn packaged_python_entry");
if (pythonEntryAt === -1) {
  fail(`${configRs}: packaged_python_entry() is missing`);
} else {
  const body = configSource.slice(pythonEntryAt, configSource.indexOf("\n}", pythonEntryAt));
  if (!body.includes('"python-agent"') || !body.includes('"yukinal-agent"')) {
    fail(`${configRs}: packaged_python_entry() must resolve python-agent/yukinal-agent`);
  }
}

if (problems.length > 0) {
  console.error(`\n✗ packaging contract: ${problems.length} problem(s)`);
  for (const problem of problems) console.error(`  - ${problem}`);
  process.exit(1);
}

console.log("packaging contract: green (Python Agent and C++ SSH service only)");
