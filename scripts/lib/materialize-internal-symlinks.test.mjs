import assert from "node:assert/strict";
import { mkdtemp, lstat, mkdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { materializeInternalSymlinks } from "./materialize-internal-symlinks.mjs";

async function temporaryDirectory(t) {
  const directory = await mkdtemp(join(tmpdir(), "yukinal-agent-links-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

test("materializes an internal directory symlink", async (t) => {
  const directory = await temporaryDirectory(t);
  const runtime = join(directory, "runtime");
  const realDirectory = join(runtime, "Python.framework", "Versions", "3.14");
  await mkdir(realDirectory, { recursive: true });
  await writeFile(join(realDirectory, "Python"), "framework binary");
  await mkdir(join(realDirectory, "Resources"), { recursive: true });
  await writeFile(join(realDirectory, "Resources", "Info.plist"), "framework metadata");

  try {
    await symlink(
      realDirectory,
      join(runtime, "Python.framework", "Versions", "Current"),
      process.platform === "win32" ? "junction" : "dir",
    );
  } catch (error) {
    if (process.platform === "win32" && ["EPERM", "EACCES", "ENOTSUP"].includes(error.code)) {
      t.skip("creating directory symlinks requires Windows Developer Mode or elevated privileges");
      return;
    }
    throw error;
  }

  const links = await materializeInternalSymlinks(runtime);
  assert.deepEqual(links, ["Python.framework/Versions/Current"]);
  assert.equal((await lstat(join(runtime, "Python.framework", "Versions", "Current"))).isDirectory(), true);
  assert.equal(await readFile(join(runtime, "Python.framework", "Versions", "Current", "Python"), "utf8"), "framework binary");
});

test("materializes an internal file symlink and preserves its mode", async (t) => {
  const directory = await temporaryDirectory(t);
  const runtime = join(directory, "runtime");
  const target = join(runtime, "Python.framework", "Versions", "3.14", "Python");
  const link = join(runtime, "Python.framework", "Python");
  await mkdir(join(runtime, "Python.framework", "Versions", "3.14"), { recursive: true });
  await writeFile(target, "framework binary", { mode: 0o755 });
  await mkdir(join(runtime, "Python.framework"), { recursive: true });
  try {
    await symlink("Versions/3.14/Python", link, "file");
  } catch (error) {
    if (process.platform === "win32" && ["EPERM", "EACCES", "ENOTSUP"].includes(error.code)) {
      t.skip("creating file symlinks requires Windows Developer Mode or elevated privileges");
      return;
    }
    throw error;
  }
  const links = await materializeInternalSymlinks(runtime);
  assert.deepEqual(links, ["Python.framework/Python"]);
  const stats = await lstat(link);
  assert.equal(stats.isFile(), true);
  if (process.platform !== "win32") {
    assert.equal(stats.mode & 0o111, 0o111);
  }
  assert.equal(await readFile(link, "utf8"), "framework binary");
});

test("refuses a symlink that resolves outside the Agent runtime", async (t) => {
  const directory = await temporaryDirectory(t);
  const runtime = join(directory, "runtime");
  const outside = join(directory, "outside");
  await mkdir(runtime);
  await mkdir(outside);
  await writeFile(join(outside, "outside.bin"), "outside data");
  try {
    await symlink(outside, join(runtime, "external"), process.platform === "win32" ? "junction" : "dir");
  } catch (error) {
    if (process.platform === "win32" && ["EPERM", "EACCES", "ENOTSUP"].includes(error.code)) {
      t.skip("creating symlinks requires Windows Developer Mode or elevated privileges");
      return;
    }
    throw error;
  }
  await assert.rejects(materializeInternalSymlinks(runtime), /symlink target escapes the Agent runtime/);
  assert.equal((await lstat(join(runtime, "external"))).isSymbolicLink(), true);
});

test("refuses a directory symlink that loops to an active ancestor", async (t) => {
  const directory = await temporaryDirectory(t);
  const runtime = join(directory, "runtime");
  const versions = join(runtime, "Python.framework", "Versions");
  const version = join(versions, "3.14");
  await mkdir(version, { recursive: true });
  try {
    await symlink(versions, join(version, "Parent"), process.platform === "win32" ? "junction" : "dir");
  } catch (error) {
    if (process.platform === "win32" && ["EPERM", "EACCES", "ENOTSUP"].includes(error.code)) {
      t.skip("creating directory symlinks requires Windows Developer Mode or elevated privileges");
      return;
    }
    throw error;
  }
  await assert.rejects(materializeInternalSymlinks(runtime), /directory symlink cycle/);
});
