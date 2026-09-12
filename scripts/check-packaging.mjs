#!/usr/bin/env node
/**
 * Packaging contract: the places that must agree about *what an installed app
 * launches*, checked in one place.
 *
 * An installer that ships the wrong file is not caught by any unit test -- it is caught by
 * the user. Three of these facts are only true at once or not at all:
 *
 *   1. `apps/desktop/src-tauri/tauri.conf.json` stages the agent as `agent/index.js`
 *      inside the resource directory.
 *   2. `crates/core/src/sidecar/config.rs` resolves `<resources>/agent/index.js` and
 *      nothing else; a rename on one side alone is invisible until an installed user
 *      starts the app.
 *   3. `apps/agent`'s build emits exactly one file, `dist/index.js`, with no bare or
 *      relative imports left for `node_modules` to satisfy -- the packaged app has none.
 *   4. The bundler's icons and targets are configured, because `bundle.active: true`
 *      without them fails the build rather than producing an installer.
 *   5. The esbuild target and the repository's declared Node floor (`engines.node`) are
 *      the same number. The installed app runs the *user's* Node, so a target below the
 *      documented floor ships a bundle whose supported range contradicts what we tell
 *      users, and one above it silently raises the floor nobody agreed to.
 *
 * Run after `pnpm --filter @yukinal/agent build`; `pnpm check` orders it that way.
 */

import { readFileSync, existsSync, statSync } from "node:fs";
import { resolve, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const tauriDir = join(root, "apps", "desktop", "src-tauri");
const confPath = join(tauriDir, "tauri.conf.json");
const agentBundle = join(root, "apps", "agent", "dist", "index.js");

const problems = [];
const fail = (message) => problems.push(message);
const read = (path) => readFileSync(path, "utf8");

// ─────────────────────────────────────────────────────────── 1. tauri.conf.json
let conf;
try {
  conf = JSON.parse(read(confPath));
} catch (error) {
  console.error(`✗ ${confPath} is not valid JSON: ${error.message}`);
  process.exit(1);
}

const bundle = conf.bundle ?? {};
if (bundle.active !== true) {
  fail("bundle.active is not true — the build produces an app that was never packaged");
}

// The resource map (not the list form): the destination has to be explicit, because the
// runtime path is a contract with the Rust resolver, not a copy of the source layout.
//
// Two entries live under `agent/`, and exactly two: the bundle itself, and a one-line
// `package.json` declaring `"type": "module"`.
//
// The second one is not decoration. In the installed layout the bundle has no
// neighbouring `package.json`, so Node decides the module system by **syntax
// detection** (`--experimental-detect-module`, on by default since Node 22.7). That
// works, but it is a default we do not control: a user with
// `NODE_OPTIONS=--no-experimental-detect-module` in their environment would make the
// sidecar fail to parse, and a future Node could change the default. Staging the
// declaration next to the file makes the module system explicit at the location where
// it is actually loaded.
const resources = bundle.resources;
if (resources === null || typeof resources !== "object" || Array.isArray(resources)) {
  fail('bundle.resources must be a map ({"<source>": "<destination>"}), not a list or null');
} else {
  const entries = Object.entries(resources);
  const agentEntries = entries.filter(([, destination]) => String(destination).replace(/\\/g, "/").startsWith("agent/"));
  const destinations = agentEntries.map(([, destination]) => String(destination).replace(/\\/g, "/")).sort();
  const expected = ["agent/index.js", "agent/package.json"];
  if (destinations.join(",") !== expected.join(",")) {
    fail(
      `bundle.resources must stage exactly ${expected.join(" and ")} under agent/, found ${
        destinations.length > 0 ? destinations.join(" and ") : "nothing"
      }`,
    );
  }

  const bundleEntry = agentEntries.find(([, d]) => String(d).replace(/\\/g, "/") === "agent/index.js");
  if (bundleEntry) {
    const [source] = bundleEntry;
    // Sources are relative to src-tauri, which is where the bundler runs.
    const resolved = resolve(tauriDir, source);
    if (resolved !== agentBundle) {
      fail(`bundle.resources source "${source}" resolves to ${resolved}, expected ${agentBundle}`);
    }
    if (/[*?]/.test(source)) {
      fail(`bundle.resources source "${source}" is a glob — the agent bundle must be one named file`);
    }
  }

  const moduleEntry = agentEntries.find(([, d]) => String(d).replace(/\\/g, "/") === "agent/package.json");
  if (moduleEntry) {
    const [source] = moduleEntry;
    const resolved = resolve(tauriDir, source);
    if (!existsSync(resolved)) {
      fail(`bundle.resources source "${source}" does not exist — the staged declaration must be a real file`);
    }
    let declared;
    try {
      declared = JSON.parse(read(resolved));
    } catch (error) {
      fail(`the staged agent package.json is not valid JSON: ${error.message}`);
    }
    if (declared?.type !== "module") {
      fail(`the staged agent package.json must declare {"type": "module"}, found ${JSON.stringify(declared)}`);
    }
    if (Object.keys(declared).length !== 1) {
      fail(
        "the staged agent package.json must contain only `type` — anything else (a `name`, an `exports` map) would make Node treat the bundle directory as a package and change resolution",
      );
    }
  }
}

// Icons: the bundler refuses to run without them, and each platform needs its own format.
const REQUIRED_ICONS = ["32x32.png", "128x128.png", "128x128@2x.png", "icon.icns", "icon.ico"];
const icons = bundle.icon;
if (!Array.isArray(icons) || icons.length === 0) {
  fail("bundle.icon is missing — the bundler refuses to run without an icon set");
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

// Targets: listed per platform on purpose. tauri-bundler filters this list down to the
// current host's package types (Settings::package_types), so the same config is correct
// on all three -- and an omission here would silently produce no artifact on that OS.
const PLATFORM_TARGETS = {
  windows: ["nsis", "msi"],
  macOS: ["app", "dmg"],
  linux: ["deb", "rpm", "appimage"],
};
const targets = bundle.targets;
if (targets === "all") {
  fail('bundle.targets is "all" — list the targets so an unsupported one is a review decision, not a surprise');
} else if (!Array.isArray(targets) || targets.length === 0) {
  fail("bundle.targets must be a non-empty list");
} else {
  for (const [platform, required] of Object.entries(PLATFORM_TARGETS)) {
    for (const target of required) {
      if (!targets.includes(target)) fail(`bundle.targets (${platform}) is missing "${target}"`);
    }
  }
}

// A release build that ships a stale agent is a silent breakage, so the ordering of the
// hook is part of the contract: contract libs -> agent bundle -> frontend bundle.
const beforeBuild = conf.build?.beforeBuildCommand ?? "";
const order = ["build:libs", "@yukinal/agent", "@yukinal/desktop"];
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

// ─────────────────────────────────────────── 2. the Rust side of the same contract
const configRs = join(root, "crates", "core", "src", "sidecar", "config.rs");
const configSource = read(configRs);
// Test the index, not the slice: `slice(-1)` on a miss returns the file's last
// character, which is truthy, so `if (!packagedEntry)` could never fire and the
// diagnostic below was unreachable. The rename case then surfaced through the
// body check further down, with a message about the resource directory -- true
// but not the reason.
const packagedEntryAt = configSource.indexOf("pub fn packaged_entry");
if (packagedEntryAt === -1) {
  fail(
    `${configRs}: packaged_entry() not found — it is the other half of the resource path ` +
      "(if it was renamed, update this check and tauri.conf.json together)",
  );
} else {
  const packagedEntry = configSource.slice(packagedEntryAt);
  // Accept any spelling of the same subpath -- separate "agent"/"index.js" literals, one
  // "agent/index.js" literal, or a constant elsewhere in the file. What must not change is
  // the subpath itself: a rename on one side alone is invisible until an installed user
  // starts the app.
  const compact = configSource.replace(/\s+/g, "");
  const joinsAgentAndIndex = compact.includes('"agent"') && compact.includes('"index.js"');
  if (!compact.includes('"agent/index.js"') && !joinsAgentAndIndex) {
    fail(`${configRs}: no "agent/index.js" (or "agent" + "index.js") literal — update tauri.conf.json to match`);
  } else {
    const body = packagedEntry.slice(0, packagedEntry.indexOf("\n}"));
    if (!/resources|resource_dir|join/.test(body)) {
      fail(`${configRs}: packaged_entry() no longer derives from the resource directory: ${body.trim()}`);
    }
  }
}

// ───────────────────────────────────────── 3. the bundle is one self-contained file
const agentManifest = JSON.parse(read(join(root, "apps", "agent", "package.json")));
const buildScript = agentManifest.scripts?.build ?? "";
if (!buildScript.includes("--outfile=dist/index.js")) {
  fail(`apps/agent build script does not emit dist/index.js: ${buildScript}`);
}
if (!buildScript.includes("--bundle")) {
  fail(`apps/agent build script does not bundle: ${buildScript}`);
}
if (!buildScript.includes("--platform=node")) {
  fail(`apps/agent build script does not set --platform=node: ${buildScript}`);
}
// ESM output is what the source is (`"type": "module"`), and it is what Node loads in the
// installed layout: there is no package.json next to `<resources>/agent/index.js`, so Node
// falls back to syntax detection, which is a documented default only from Node 22.7 onward
// -- the same floor `engines.node` states below.
if (!buildScript.includes("--format=esm")) {
  fail(`apps/agent build script does not emit ESM: ${buildScript}`);
}
if (agentManifest.type !== "module") {
  fail('apps/agent/package.json is not "type": "module" — dist/index.js would be read as CommonJS');
}

// The compile target and the documented floor must be one number (see the header).
const enginesNode = JSON.parse(read(join(root, "package.json"))).engines?.node ?? "";
const targetMajor = /--target=node(\d+)/.exec(buildScript)?.[1];
const floorMajor = /(\d+)/.exec(enginesNode)?.[1];
if (!targetMajor) {
  fail(`apps/agent build script has no --target=node<major>: ${buildScript}`);
} else if (!floorMajor) {
  fail(`root package.json engines.node ("${enginesNode}") does not state a floor to compare with`);
} else if (targetMajor !== floorMajor) {
  fail(
    `esbuild --target=node${targetMajor} contradicts engines.node "${enginesNode}" — ` +
      "the installed app runs the user's Node, so the compile target and the documented floor must agree",
  );
}

if (!existsSync(agentBundle)) {
  fail(`${agentBundle} does not exist — run: pnpm --filter @yukinal/agent build`);
} else {
  const source = read(agentBundle);
  // `node:` builtins are the only legal external in a `--platform=node` bundle; anything
  // else is a specifier the installed app is expected to resolve without node_modules.
  const externals = [...source.matchAll(/^\s*import\s+(?:[^"']*?\sfrom\s+)?["']([^"']+)["']/gm)]
    .map((match) => match[1])
    .filter((specifier) => !specifier.startsWith("node:"));
  if (externals.length > 0) {
    fail(`${agentBundle} still imports ${[...new Set(externals)].join(", ")} — a packaged app has no node_modules`);
  }
  const size = statSync(agentBundle).size;
  // State both numbers rather than claiming they match: this line is also printed when the
  // target/floor comparison above has just failed.
  console.log(
    `  staged agent: ${size} bytes, one file, no non-builtin imports (target node${targetMajor}, engines.node "${enginesNode}")`,
  );
}

if (problems.length > 0) {
  console.error(`\n✗ packaging contract: ${problems.length} problem(s)`);
  for (const problem of problems) console.error(`  - ${problem}`);
  process.exit(1);
}

console.log("packaging contract: green");
