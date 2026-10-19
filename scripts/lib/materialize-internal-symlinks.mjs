import {
  chmod,
  copyFile,
  cp,
  lstat,
  readdir,
  realpath,
  unlink,
} from "node:fs/promises";
import { isAbsolute, relative, sep, join } from "node:path";

function isWithin(root, target) {
  const path = relative(root, target);
  return path !== "" && path !== ".." && !path.startsWith(`..${sep}`) && !isAbsolute(path);
}

/**
 * Replaces symlinks in a frozen runtime with copies of their targets, but only
 * when every target resolves inside that runtime. PyInstaller's macOS Python
 * framework contains internal aliases; the release manifest deliberately hashes
 * regular files only, so materialize those aliases before staging.
 */
export async function materializeInternalSymlinks(root) {
  const runtimeRoot = await realpath(root);
  const materialized = [];

  async function walk(directory, ancestors) {
    const canonicalDirectory = await realpath(directory);
    if (!isWithin(runtimeRoot, canonicalDirectory) && canonicalDirectory !== runtimeRoot) {
      throw new Error(`directory escaped the Agent runtime: ${directory}`);
    }
    if (ancestors.has(canonicalDirectory)) {
      throw new Error(`directory symlink cycle inside the Agent runtime: ${directory}`);
    }
    const nextAncestors = new Set(ancestors);
    nextAncestors.add(canonicalDirectory);

    const entries = await readdir(directory, { withFileTypes: true });
    for (const entry of entries) {
      const path = join(directory, entry.name);
      if (entry.isSymbolicLink()) {
        const target = await realpath(path);
        if (!isWithin(runtimeRoot, target)) {
          throw new Error(`symlink target escapes the Agent runtime: ${path} -> ${target}`);
        }
        const targetStats = await lstat(target);
        if (targetStats.isDirectory()) {
          if (nextAncestors.has(target)) {
            throw new Error(`directory symlink cycle inside the Agent runtime: ${path} -> ${target}`);
          }
          await unlink(path);
          await cp(target, path, { recursive: true, dereference: false });
          await walk(path, nextAncestors);
        } else if (targetStats.isFile()) {
          await unlink(path);
          await copyFile(target, path);
          await chmod(path, targetStats.mode & 0o7777);
        } else {
          throw new Error(`symlink target is not a regular file or directory: ${path} -> ${target}`);
        }
        materialized.push(relative(runtimeRoot, path).split(sep).join("/"));
      } else if (entry.isDirectory()) {
        await walk(path, nextAncestors);
      } else if (!entry.isFile()) {
        throw new Error(`unsupported special file inside the Agent runtime: ${path}`);
      }
    }
  }

  await walk(runtimeRoot, new Set());
  return materialized.sort();
}
