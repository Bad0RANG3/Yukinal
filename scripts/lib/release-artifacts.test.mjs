import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  CHECKSUMS_FILE,
  RELEASE_MANIFEST_FILE,
  buildReleaseManifest,
  isSafeRelativePath,
  listPayloadFiles,
  parseChecksums,
  renderChecksums,
  renderManifest,
  sha256File,
  verifyReleaseManifest,
} from "./release-artifacts.mjs";

function digestOf(text) {
  return createHash("sha256").update(text).digest("hex");
}

async function temporaryDirectory(t) {
  const directory = await mkdtemp(join(tmpdir(), "yukinal-release-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  return directory;
}

/** Stages the same shape `package.mjs` writes: a payload directory plus the two root files. */
async function stageSamplePayload(root) {
  const payloadDirectory = "yukinal-1.0.0-win32-x64";
  await mkdir(join(root, payloadDirectory, "python-agent"), { recursive: true });
  await mkdir(join(root, payloadDirectory, "ssh-service"), { recursive: true });
  const files = [
    [`${payloadDirectory}/yukinal-desktop-host.exe`, "host-bytes"],
    [`${payloadDirectory}/python-agent/yukinal-agent.exe`, "agent-bytes"],
    [`${payloadDirectory}/python-agent/THIRD_PARTY_LICENSES.txt`, "licenses"],
    [`${payloadDirectory}/ssh-service/yukinal-ssh-service.exe`, "ssh-bytes"],
    [`${payloadDirectory}/NOTICE`, "notice"],
  ];
  for (const [relative, contents] of files) {
    await writeFile(join(root, relative), contents);
  }

  const discovered = await listPayloadFiles(root);
  const payloads = await Promise.all(
    discovered.map(async (path) => ({
      path,
      size: (await readFile(join(root, path))).length,
      sha256: await sha256File(join(root, path)),
    })),
  );
  const manifest = buildReleaseManifest({
    version: "1.0.0",
    agentVersion: "0.1.0",
    platform: "win32",
    arch: "x64",
    payloadDirectory,
    sourceRevision: "0".repeat(40),
    dirty: false,
    builtAt: "2026-01-01T00:00:00.000Z",
    payloads,
    launch: { hostExecutable: `${payloadDirectory}/yukinal-desktop-host.exe` },
    notes: ["installer not built"],
  });
  await writeFile(join(root, CHECKSUMS_FILE), renderChecksums(manifest.payloads));
  await writeFile(join(root, RELEASE_MANIFEST_FILE), renderManifest(manifest));
  return manifest;
}

test("isSafeRelativePath accepts nested relative paths and rejects escapes", () => {
  assert.equal(isSafeRelativePath("dir/file.txt"), true);
  assert.equal(isSafeRelativePath("file.txt"), true);

  for (const unsafe of ["", "/abs", "C:\\abs", "C:/abs", "a/../b", "../b", "a//b", "a/./b", "a\\b", "a\0b"]) {
    assert.equal(isSafeRelativePath(unsafe), false, `expected ${JSON.stringify(unsafe)} to be unsafe`);
  }
});

test("sha256File hashes file bytes", async (t) => {
  const directory = await temporaryDirectory(t);
  const file = join(directory, "sample.bin");
  await writeFile(file, "hello yukinal");
  assert.equal(await sha256File(file), digestOf("hello yukinal"));
});

test("listPayloadFiles is recursive, sorted, forward-slash and skips its own output", async (t) => {
  const directory = await temporaryDirectory(t);
  await mkdir(join(directory, "payload", "nested"), { recursive: true });
  await writeFile(join(directory, "payload", "b.txt"), "b");
  await writeFile(join(directory, "payload", "nested", "a.txt"), "a");
  await writeFile(join(directory, CHECKSUMS_FILE), "root checksums");
  await writeFile(join(directory, RELEASE_MANIFEST_FILE), "root manifest");
  // A same-named file deeper down is still a payload and must be listed.
  await writeFile(join(directory, "payload", "nested", CHECKSUMS_FILE), "nested checksums");

  assert.deepEqual(await listPayloadFiles(directory), [
    "payload/b.txt",
    `payload/nested/${CHECKSUMS_FILE}`,
    "payload/nested/a.txt",
  ]);
});

test("checksum listings render in sha256sum format and round-trip", () => {
  const entries = [
    { path: "b/file", sha256: "b".repeat(64) },
    { path: "a/file", sha256: "a".repeat(64) },
  ];
  const rendered = renderChecksums(entries);
  assert.equal(rendered, `${"a".repeat(64)}  a/file\n${"b".repeat(64)}  b/file\n`);
  assert.deepEqual([...parseChecksums(rendered)], [["a/file", "a".repeat(64)], ["b/file", "b".repeat(64)]]);
  assert.throws(() => parseChecksums("not a checksum line"), /unparseable/);
});

test("buildReleaseManifest sorts payloads and rejects malformed input", () => {
  const base = {
    version: "1.0.0",
    agentVersion: "0.1.0",
    platform: "win32",
    arch: "x64",
    payloadDirectory: "p",
    sourceRevision: "r",
    dirty: true,
    builtAt: "t",
    payloads: [
      { path: "p/z", size: 1, sha256: "a".repeat(64) },
      { path: "p/a", size: 2, sha256: "b".repeat(64) },
    ],
  };
  assert.deepEqual(
    buildReleaseManifest(base).payloads.map((payload) => payload.path),
    ["p/a", "p/z"],
  );
  assert.equal(buildReleaseManifest(base).dirty, true);
  // The manifest records the worktree state, so a passing build is never confused
  // with a signed release.
  assert.equal(buildReleaseManifest({ ...base, dirty: false }).dirty, false);

  assert.throws(() => buildReleaseManifest({ ...base, payloads: [] }), /at least one payload/);
  assert.throws(
    () => buildReleaseManifest({ ...base, payloads: [{ path: "p/a", size: 1, sha256: "a".repeat(64) }, { path: "p/a", size: 1, sha256: "a".repeat(64) }] }),
    /duplicate payload path/,
  );
  assert.throws(
    () => buildReleaseManifest({ ...base, payloads: [{ path: "other/a", size: 1, sha256: "a".repeat(64) }] }),
    /escapes p/,
  );
  assert.throws(
    () => buildReleaseManifest({ ...base, payloads: [{ path: "p/a", size: 1, sha256: "nope" }] }),
    /invalid sha256/,
  );
  assert.throws(() => buildReleaseManifest({ ...base, payloadDirectory: "a/b" }), /single safe path segment/);
  assert.throws(() => buildReleaseManifest({ ...base, version: "" }), /version/);
});

test("verifyReleaseManifest accepts a matching payload and reports the totals", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  const result = await verifyReleaseManifest(directory, manifest);
  assert.equal(result.files, manifest.payloads.length);
  assert.ok(result.bytes > 0);
});

test("verifyReleaseManifest rejects a tampered payload", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  await writeFile(join(directory, "yukinal-1.0.0-win32-x64", "NOTICE"), "tampered");
  await assert.rejects(
    () => verifyReleaseManifest(directory, manifest),
    /NOTICE: (size|sha256)/,
  );
});

test("verifyReleaseManifest rejects a missing payload file", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  await rm(join(directory, "yukinal-1.0.0-win32-x64", "NOTICE"));
  await assert.rejects(() => verifyReleaseManifest(directory, manifest), /NOTICE: missing on disk/);
});

test("verifyReleaseManifest rejects a checksum listing that disagrees or is incomplete", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);

  await writeFile(join(directory, CHECKSUMS_FILE), "");
  await assert.rejects(
    () => verifyReleaseManifest(directory, manifest),
    /missing from SHA256SUMS\.txt/,
  );

  const first = manifest.payloads[0];
  await writeFile(
    join(directory, CHECKSUMS_FILE),
    renderChecksums([
      ...manifest.payloads.slice(1),
      { path: first.path, sha256: "f".repeat(64) },
    ]),
  );
  await assert.rejects(() => verifyReleaseManifest(directory, manifest), /disagrees with the manifest/);

  await writeFile(
    join(directory, CHECKSUMS_FILE),
    renderChecksums([...manifest.payloads, { path: "yukinal-1.0.0-win32-x64/extra", sha256: "e".repeat(64) }]),
  );
  await assert.rejects(() => verifyReleaseManifest(directory, manifest), /not in the manifest/);
});

test("verifyReleaseManifest refuses a manifest whose files escape the payload directory", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  const escaped = {
    ...manifest,
    payloads: [...manifest.payloads, { path: "other/outside", size: 0, sha256: "a".repeat(64) }],
  };
  await assert.rejects(() => verifyReleaseManifest(directory, escaped), /escapes yukinal-1\.0\.0-win32-x64/);

  const unsafe = {
    ...manifest,
    payloads: [...manifest.payloads, { path: "../outside", size: 0, sha256: "a".repeat(64) }],
  };
  await assert.rejects(() => verifyReleaseManifest(directory, unsafe), /unsafe payload path/);
});

test("a verified copy of the staged payload still exists at the documented paths", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  await verifyReleaseManifest(directory, manifest);
  for (const relative of [
    "yukinal-1.0.0-win32-x64/yukinal-desktop-host.exe",
    "yukinal-1.0.0-win32-x64/python-agent/yukinal-agent.exe",
    "yukinal-1.0.0-win32-x64/ssh-service/yukinal-ssh-service.exe",
  ]) {
    assert.equal(existsSync(join(directory, relative)), true, relative);
  }
});
