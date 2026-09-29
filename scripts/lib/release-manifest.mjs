import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { lstat, mkdir, readFile, readdir, rename, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";

const HASH_MANIFEST = "SHA256SUMS.txt";
const RELEASE_MANIFEST = "release-manifest.json";
export const PACKAGE_SUFFIXES = [
  ".appimage",
  ".deb",
  ".dmg",
  ".exe",
  ".msi",
  ".pkg",
  ".rpm",
  ".tar.gz",
  ".tar.xz",
  ".tar.zst",
  ".zip",
];

function isPackageArtifact(relativePath) {
  const lower = relativePath.toLowerCase();
  return PACKAGE_SUFFIXES.some((suffix) => lower.endsWith(suffix));
}

function comparePath(left, right) {
  return left < right ? -1 : left > right ? 1 : 0;
}

async function hashFile(filePath) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(filePath)) hash.update(chunk);
  return hash.digest("hex");
}

async function collectArtifacts(bundleDir) {
  const root = await lstat(bundleDir);
  if (!root.isDirectory() || root.isSymbolicLink()) {
    throw new Error(`bundle directory is not a real directory: ${bundleDir}`);
  }

  const artifacts = [];
  const visit = async (absoluteDir, relativeDir = "") => {
    const entries = (await readdir(absoluteDir, { withFileTypes: true }))
      .sort((left, right) => comparePath(left.name, right.name));
    for (const entry of entries) {
      const relativePath = relativeDir ? `${relativeDir}/${entry.name}` : entry.name;
      const absolutePath = path.join(absoluteDir, entry.name);
      // A macOS .app directory is a build intermediate; the signed/distributable unit is
      // the .dmg (or an explicitly archived app), whose bytes can be hashed portably.
      if (entry.isDirectory() && entry.name.toLowerCase().endsWith(".app")) continue;
      if (entry.isDirectory()) {
        await visit(absolutePath, relativePath);
        continue;
      }
      if (entry.isSymbolicLink()) continue;
      if (!entry.isFile() || !isPackageArtifact(relativePath)) continue;
      if (/[\r\n]/.test(relativePath)) {
        throw new Error(`artifact path contains a line break: ${JSON.stringify(relativePath)}`);
      }
      const fileStat = await stat(absolutePath);
      artifacts.push({ path: relativePath, bytes: fileStat.size, sha256: await hashFile(absolutePath) });
    }
  };

  await visit(bundleDir);
  artifacts.sort((left, right) => comparePath(left.path, right.path));
  if (artifacts.length === 0) throw new Error(`no supported installer artifacts found under ${bundleDir}`);
  return artifacts;
}

function formatSums(artifacts) {
  return `${artifacts.map(({ path: artifactPath, sha256 }) => `${sha256}  ${artifactPath}`).join("\n")}\n`;
}

function resolveArtifactPath(bundleDir, relativePath) {
  if (typeof relativePath !== "string" || relativePath.length === 0 || relativePath.includes("\\")) {
    throw new Error(`invalid artifact path in manifest: ${JSON.stringify(relativePath)}`);
  }
  const segments = relativePath.split("/");
  if (segments.some((segment) => segment === "" || segment === "." || segment === "..")) {
    throw new Error(`artifact path escapes bundle directory: ${relativePath}`);
  }
  const absolutePath = path.resolve(bundleDir, ...segments);
  const relativeCheck = path.relative(path.resolve(bundleDir), absolutePath);
  if (relativeCheck === ".." || relativeCheck.startsWith(`..${path.sep}`) || path.isAbsolute(relativeCheck)) {
    throw new Error(`artifact path escapes bundle directory: ${relativePath}`);
  }
  return absolutePath;
}

async function atomicWrite(filePath, contents) {
  const temporaryPath = `${filePath}.${process.pid}.${Date.now()}.tmp`;
  try {
    await writeFile(temporaryPath, contents, { flag: "wx" });
    await rename(temporaryPath, filePath);
  } finally {
    await rm(temporaryPath, { force: true });
  }
}

export async function writeReleaseManifest({
  bundleDir,
  appVersion,
  runtimeVersion,
  generatedAt = new Date().toISOString(),
  platform = process.platform,
  architecture = process.arch,
  nodeVersion = process.version,
  source = { revision: null, dirty: null },
}) {
  await mkdir(bundleDir, { recursive: true });
  const artifacts = await collectArtifacts(bundleDir);
  const manifest = {
    schemaVersion: 1,
    generatedAt,
    application: { name: "Yukinal", version: appVersion, bundledNodeRuntime: runtimeVersion },
    build: { platform, architecture, nodeVersion, source },
    artifacts,
  };
  const manifestText = `${JSON.stringify(manifest, null, 2)}\n`;
  const sumsText = formatSums(artifacts);
  await atomicWrite(path.join(bundleDir, HASH_MANIFEST), sumsText);
  await atomicWrite(path.join(bundleDir, RELEASE_MANIFEST), manifestText);
  return manifest;
}

function parseSums(text) {
  const lines = text.trimEnd().split(/\r?\n/);
  const entries = new Map();
  for (const line of lines) {
    const match = /^([a-f0-9]{64})  (.+)$/.exec(line);
    if (!match) throw new Error(`invalid SHA256SUMS line: ${line}`);
    if (entries.has(match[2])) throw new Error(`duplicate artifact in SHA256SUMS: ${match[2]}`);
    entries.set(match[2], match[1]);
  }
  return entries;
}

export async function verifyReleaseManifest(bundleDir) {
  const [manifestText, sumsText] = await Promise.all([
    readFile(path.join(bundleDir, RELEASE_MANIFEST), "utf8"),
    readFile(path.join(bundleDir, HASH_MANIFEST), "utf8"),
  ]);
  let manifest;
  try {
    manifest = JSON.parse(manifestText);
  } catch (error) {
    throw new Error(`release manifest is not valid JSON: ${error.message}`);
  }
  if (manifest?.schemaVersion !== 1 || !Array.isArray(manifest.artifacts) || manifest.artifacts.length === 0) {
    throw new Error("release manifest has an unsupported schema or no artifacts");
  }

  const recorded = new Map();
  for (const artifact of manifest.artifacts) {
    const artifactPath = artifact?.path;
    if (recorded.has(artifactPath)) throw new Error(`duplicate artifact in release manifest: ${artifactPath}`);
    if (!isPackageArtifact(artifactPath) || !Number.isSafeInteger(artifact.bytes) || artifact.bytes < 0 || !/^[a-f0-9]{64}$/.test(artifact.sha256 ?? "")) {
      throw new Error(`invalid artifact record in release manifest: ${JSON.stringify(artifact)}`);
    }
    resolveArtifactPath(bundleDir, artifactPath);
    recorded.set(artifactPath, artifact);
  }

  const sums = parseSums(sumsText);
  if (sums.size !== recorded.size) throw new Error("SHA256SUMS and release manifest contain different artifact counts");
  for (const [artifactPath, artifact] of recorded) {
    if (sums.get(artifactPath) !== artifact.sha256) throw new Error(`SHA256SUMS does not match the release manifest: ${artifactPath}`);
    const absolutePath = resolveArtifactPath(bundleDir, artifactPath);
    const fileStat = await lstat(absolutePath);
    if (!fileStat.isFile() || fileStat.isSymbolicLink()) throw new Error(`artifact is missing or not a regular file: ${artifactPath}`);
    if (fileStat.size !== artifact.bytes) throw new Error(`artifact size changed after manifest generation: ${artifactPath}`);
    if (await hashFile(absolutePath) !== artifact.sha256) throw new Error(`artifact checksum mismatch: ${artifactPath}`);
  }

  const actual = await collectArtifacts(bundleDir);
  if (actual.length !== recorded.size || actual.some(({ path: artifactPath }) => !recorded.has(artifactPath))) {
    throw new Error("the bundle contains artifacts that are not covered by the release manifest");
  }
  return manifest;
}
