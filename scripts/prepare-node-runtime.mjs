#!/usr/bin/env node

import { createHash } from "node:crypto";
import { chmod, copyFile, mkdir, mkdtemp, readFile, rename, rm, stat, writeFile } from "node:fs/promises";
import { spawnSync } from "node:child_process";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const manifest = JSON.parse(await readFile(join(root, "scripts", "node-runtime.json"), "utf8"));
const platformKey = `${process.platform}-${process.arch}`;
const build = manifest.platforms[platformKey];

if (!build) {
  throw new Error(`No bundled Node.js ${manifest.version} runtime for ${platformKey}`);
}

const runtimeDir = join(root, "apps", "desktop", "src-tauri", "resources", "runtime");
const executableName = process.platform === "win32" ? "node.exe" : "node";
const executable = join(runtimeDir, executableName);
const license = join(runtimeDir, "LICENSE");
const cacheDir = join(root, "target", "node-runtime-cache");
const cachedArchive = join(cacheDir, build.archive);

await mkdir(runtimeDir, { recursive: true });
await mkdir(cacheDir, { recursive: true });

async function sha256(path) {
  const bytes = await readFile(path);
  return createHash("sha256").update(bytes).digest("hex");
}

async function verifyArchive(path) {
  try {
    const digest = await sha256(path);
    if (digest === build.sha256) return true;
    console.warn(`Discarding cached Node.js archive with unexpected SHA-256: ${basename(path)}`);
    await rm(path, { force: true });
  } catch (error) {
    if (error.code !== "ENOENT") throw error;
  }
  return false;
}

async function downloadArchive(path) {
  const url = `https://nodejs.org/dist/v${manifest.version}/${build.archive}`;
  console.log(`Downloading official Node.js ${manifest.version} runtime for ${platformKey}`);
  const response = await fetch(url, { signal: AbortSignal.timeout(180_000) });
  if (!response.ok) throw new Error(`Node.js download failed: ${response.status} ${response.statusText}`);
  const host = new URL(response.url).hostname;
  if (host !== "nodejs.org" && host !== "r2.nodejs.org") {
    throw new Error(`Unexpected host in Node.js download redirect: ${host}`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length === 0 || bytes.length > 100 * 1024 * 1024) {
    throw new Error(`Unexpected Node.js archive size: ${bytes.length} bytes`);
  }
  const digest = createHash("sha256").update(bytes).digest("hex");
  if (digest !== build.sha256) {
    throw new Error(`Node.js archive SHA-256 mismatch for ${build.archive}: ${digest}`);
  }
  await writeFile(path, bytes, { flag: "wx" });
}

async function extractRuntime(archivePath, destination) {
  const temporaryRoot = await mkdtemp(join(tmpdir(), "yukinal-node-runtime-"));
  try {
    const compressionFlag = { zip: "-xf", gzip: "-xzf", xz: "-xJf" }[build.compression];
    const result = spawnSync(
      "tar",
      [compressionFlag, archivePath, "-C", temporaryRoot, "--strip-components=1", build.binary, `${build.binary.split("/")[0]}/LICENSE`],
      { encoding: "utf8", windowsHide: true },
    );
    if (result.error) throw result.error;
    if (result.status !== 0) {
      throw new Error(`Could not extract Node.js archive: ${(result.stderr || result.stdout || "tar failed").trim()}`);
    }

    const extractedExecutable = join(temporaryRoot, build.binary.slice(build.binary.indexOf("/") + 1));
    const extractedLicense = join(temporaryRoot, "LICENSE");
    const executableInfo = await stat(extractedExecutable);
    if (!executableInfo.isFile() || executableInfo.size < 1_000_000) {
      throw new Error(`Extracted Node.js executable looks invalid: ${extractedExecutable}`);
    }
    const licenseInfo = await stat(extractedLicense);
    if (!licenseInfo.isFile() || licenseInfo.size < 1_000) {
      throw new Error(`Node.js distribution license is missing or incomplete: ${extractedLicense}`);
    }

    const stagedExecutable = join(destination, `${executableName}.new`);
    const stagedLicense = join(destination, "LICENSE.new");
    await copyFile(extractedExecutable, stagedExecutable);
    await copyFile(extractedLicense, stagedLicense);
    if (process.platform !== "win32") await chmod(stagedExecutable, 0o755);
    await rm(executable, { force: true });
    await rm(license, { force: true });
    await rename(stagedExecutable, executable);
    await rename(stagedLicense, license);
  } finally {
    await rm(temporaryRoot, { recursive: true, force: true });
  }
}

const archiveVerified = await verifyArchive(cachedArchive);
if (!archiveVerified) await downloadArchive(cachedArchive);

const existingVersion = await new Promise((resolve) => {
  const result = spawnSync(executable, ["--version"], { encoding: "utf8", windowsHide: true });
  resolve(result.status === 0 ? result.stdout.trim() : "");
});
const licenseExists = await stat(license).then((info) => info.isFile(), () => false);

if (existingVersion === `v${manifest.version}` && licenseExists) {
  console.log(`Bundled Node.js runtime already staged and verified: ${existingVersion}`);
} else {
  await extractRuntime(cachedArchive, runtimeDir);
}

const versionResult = spawnSync(executable, ["--version"], { encoding: "utf8", windowsHide: true });
if (versionResult.status !== 0 || versionResult.stdout.trim() !== `v${manifest.version}`) {
  throw new Error(`Staged Node.js runtime failed its version check: ${versionResult.stderr || versionResult.stdout}`);
}
if (!(await verifyArchive(cachedArchive))) {
  throw new Error(`Node.js archive failed its final checksum check: ${build.archive}`);
}

console.log(`Bundled Node.js ready: ${executable} (${versionResult.stdout.trim()})`);
