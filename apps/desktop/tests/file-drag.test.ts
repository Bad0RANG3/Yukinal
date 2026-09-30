import assert from "node:assert/strict";
import { test } from "node:test";
import type { LocalPathHandle, RemoteFileEntry } from "@yukinal/shared";

import { localHandleForDrop, remoteFileForDrop } from "../src/features/files/file-drag.js";

const handle: LocalPathHandle = {
  handleId: "local_path_0123456789abcdef0123456789abcdef0123456789abcdef",
  name: "report.txt",
  kind: "file",
  size: 12,
};

const entries: RemoteFileEntry[] = [
  { name: "report.txt", path: "/var/tmp/report.txt", type: "file", size: 12 },
  { name: "archive", path: "/var/tmp/archive", type: "directory", size: 0 },
];

test("local-to-remote drops resolve only a handle already held in the upload queue", () => {
  assert.equal(localHandleForDrop(handle.handleId, [handle]), handle);
  assert.equal(localHandleForDrop("C:\\private\\report.txt", [handle]), undefined);
  assert.equal(localHandleForDrop("unknown_handle", [handle]), undefined);
  assert.equal(localHandleForDrop("x".repeat(129), [handle]), undefined);
});

test("remote-to-local drops resolve only regular files from the current listing", () => {
  assert.equal(remoteFileForDrop("/var/tmp/report.txt", entries), entries[0]);
  assert.equal(remoteFileForDrop("/var/tmp/archive", entries), undefined);
  assert.equal(remoteFileForDrop("/etc/shadow", entries), undefined);
  assert.equal(remoteFileForDrop("x".repeat(4_097), entries), undefined);
});
