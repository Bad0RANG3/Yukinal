import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import {
  CHECKSUMS_FILE,
  RELEASE_MANIFEST_FILE,
  agentNoticeProblems,
  buildReleaseManifest,
  inputConsistencyProblems,
  isSafeRelativePath,
  listPayloadFiles,
  parseChecksums,
  readDistributionIdentity,
  renderChecksums,
  renderManifest,
  sha256File,
  staleInputProblem,
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
    inputs: [
      {
        role: "desktop-host",
        path: `${payloadDirectory}/yukinal-desktop-host.exe`,
        sourceDirectory: "native/desktop_host",
        sourceRevision: "0".repeat(7),
        modifiedAt: "2026-01-01T00:00:00.000Z",
        sha256: digestOf("host-bytes"),
      },
    ],
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

test("staleInputProblem flags an artifact built before its source last changed", () => {
  const input = {
    role: "desktop-host",
    artifactPath: "native/build-nmake/yukinal-desktop-host.exe",
    sourceDirectory: "native/desktop_host",
    artifactTime: Date.parse("2026-10-17T12:04:16Z"),
    sourceTime: Date.parse("2026-10-17T17:06:46Z"),
  };
  // The old manifest attributed every payload to HEAD while a binary built five hours
  // earlier was the one actually staged; this is that case, made visible.
  const problem = staleInputProblem(input);
  assert.match(problem, /desktop-host is older than the last commit/);
  assert.match(problem, /native\/desktop_host/);

  assert.equal(staleInputProblem({ ...input, artifactTime: input.sourceTime }), null);
  assert.equal(staleInputProblem({ ...input, artifactTime: input.sourceTime + 1 }), null);
  // Missing timestamps must not be reported as staleness: an absent clock reading is
  // not evidence about freshness.
  assert.equal(staleInputProblem({ ...input, sourceTime: undefined }), null);
  assert.equal(staleInputProblem({ ...input, artifactTime: Number.NaN }), null);
});

test("buildReleaseManifest records executable provenance and rejects malformed entries", () => {
  const base = {
    version: "1.0.0",
    agentVersion: "0.1.0",
    platform: "win32",
    arch: "x64",
    payloadDirectory: "p",
    sourceRevision: "r",
    dirty: false,
    builtAt: "t",
    payloads: [{ path: "p/a", size: 1, sha256: "a".repeat(64) }],
  };
  const input = {
    role: "python-agent",
    path: "p/python-agent/yukinal-agent.exe",
    sourceDirectory: "agent/src",
    sourceRevision: "abc1234",
    modifiedAt: "2026-10-15T09:27:54.000Z",
    sha256: "b".repeat(64),
  };
  const manifest = buildReleaseManifest({ ...base, inputs: [input] });
  // `dirty` is normalized on every entry: freshness is judged against the directory's
  // last commit, so an uncommitted edit is invisible to it and the manifest must say so
  // rather than imply the binary matches that revision.
  assert.deepEqual(manifest.inputs, [{ ...input, dirty: false }]);
  assert.deepEqual(buildReleaseManifest({ ...base, inputs: [{ ...input, dirty: true }] }).inputs, [
    { ...input, dirty: true },
  ]);
  // Omitting the section stays legal so a manifest written before provenance existed
  // still builds; it is simply empty rather than absent-by-accident.
  assert.deepEqual(buildReleaseManifest(base).inputs, []);

  for (const broken of [
    { ...input, sha256: "nope" },
    { ...input, role: "" },
    { ...input, sourceDirectory: "../escape" },
  ]) {
    assert.throws(() => buildReleaseManifest({ ...base, inputs: [broken] }), /input/);
  }
});

test("verifyReleaseManifest accepts a matching payload and reports the totals", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  const result = await verifyReleaseManifest(directory, manifest);
  assert.equal(result.files, manifest.payloads.length);
  assert.ok(result.bytes > 0);
  // The walk and the manifest must agree; a payload that matches exactly is the only
  // case where these two numbers are allowed to be equal.
  assert.equal(result.scanned, result.files);
});

test("verifyReleaseManifest rejects a file added to the payload root", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  await writeFile(join(directory, "yukinal-1.0.0-win32-x64", "BACKDOOR.exe"), "not in the manifest");
  await assert.rejects(
    () => verifyReleaseManifest(directory, manifest),
    /1 file\(s\) on disk are not in the manifest: yukinal-1\.0\.0-win32-x64\/BACKDOOR\.exe/,
  );
});

test("verifyReleaseManifest rejects a file added inside a payload subdirectory", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  await writeFile(join(directory, "yukinal-1.0.0-win32-x64", "python-agent", "dropped.dll"), "not in the manifest");
  await assert.rejects(
    () => verifyReleaseManifest(directory, manifest),
    /not in the manifest: yukinal-1\.0\.0-win32-x64\/python-agent\/dropped\.dll/,
  );
});

test("verifyReleaseManifest names at most ten unexpected paths", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  for (let index = 0; index < 12; index += 1) {
    await writeFile(join(directory, "yukinal-1.0.0-win32-x64", `extra-${index}.bin`), "x");
  }
  await assert.rejects(
    () => verifyReleaseManifest(directory, manifest),
    /12 file\(s\) on disk are not in the manifest: .*and 2 more/,
  );
});

test("isSafeRelativePath rejects names that Win32 would rename, alias or resolve to a device", () => {
  for (const unsafe of [
    // Reserved device names, with and without an extension, and in any segment.
    "CON",
    "con",
    "payload/CON",
    "payload/nul.txt",
    "AUX",
    "payload/aux",
    "COM1",
    "payload/com9.log",
    "LPT1",
    "payload/lpt1",
    // Win32 strips a trailing dot or space, so these would collide with the real name.
    "NOTICE.",
    "NOTICE ",
    "payload/subdir /file.txt",
    // A colon addresses an NTFS alternate data stream.
    "NOTICE:ads",
    "payload/file.txt:hidden",
    // UNC and drive-absolute forms.
    "\\\\server\\share",
    "//server/share",
    "\\\\?\\C:\\payload",
    // Control characters.
    "payload/bad\u0007name",
    "payload/bad\u007fname",
  ]) {
    assert.equal(isSafeRelativePath(unsafe), false, `expected ${JSON.stringify(unsafe)} to be unsafe`);
  }

  // Names that merely contain "con"/"com" as a prefix or substring are still fine.
  for (const safe of ["payload/console.txt", "payload/com10", "payload/nulify", "payload/NOTICE.md"]) {
    assert.equal(isSafeRelativePath(safe), true, `expected ${JSON.stringify(safe)} to be safe`);
  }
});

test("listPayloadFiles refuses a symlinked entry instead of following it", async (t) => {
  const directory = await temporaryDirectory(t);
  const outside = join(directory, "outside");
  await mkdir(join(directory, "payload"), { recursive: true });
  await mkdir(outside, { recursive: true });
  await writeFile(join(outside, "host.exe"), "bytes that live outside the payload");
  const link = join(directory, "payload", "linked-dir");
  // On Windows a directory symlink needs Developer Mode or elevation, while a
  // junction only needs the directory to exist locally — hence the fallback chain.
  const failure = await createReparsePoint(outside, link);
  if (failure !== undefined) {
    t.skip(`cannot create a reparse point here: ${failure.code}`);
    return;
  }

  await assert.rejects(() => listPayloadFiles(directory), /payload path is a reparse point: payload\/linked-dir/);

  // A verifier run must reject it too, even though the link target hashes fine and
  // no manifest entry points at it.
  await writeFile(join(directory, CHECKSUMS_FILE), "");
  await assert.rejects(
    () => verifyReleaseManifest(directory, { schemaVersion: 1, payloadDirectory: "payload", payloads: [] }),
    /payload path is a reparse point: payload\/linked-dir/,
  );
});

test("agentNoticeProblems catches a notice that describes a different payload", () => {
  // Staging copied `agent/runtime/THIRD_PARTY_LICENSES.txt` from the checkout, so
  // `--agent-runtime <another onedir>` shipped a notice for the other tree byte for byte
  // and nothing noticed: the notice is not a payload entry `--verify` can cross-check.
  const distributions = [
    { name: "langchain", version: "1.2.15" },
    { name: "pydantic", version: "2.14.1" },
  ];
  const matching = [
    "Third-party Python distribution licenses included with the frozen Yukinal Agent.",
    "",
    "## langchain 1.2.15",
    "",
    "License: MIT",
    "",
    "## pydantic 2.14.1",
    "",
    "License: MIT",
    "",
  ].join("\n");
  assert.deepEqual(agentNoticeProblems(matching, distributions), []);

  // The phantom-package failure mode in miniature: a section nothing in the payload backs.
  assert.deepEqual(agentNoticeProblems(`${matching}\n## yukinal-agent-node 1.0.0\n`, distributions), [
    "the Python licence notice mentions yukinal-agent-node 1.0.0, which is not present in the staged payload",
  ]);
  // And the inverse: a distribution present in the staged payload but unattributed.
  assert.deepEqual(agentNoticeProblems("## langchain 1.2.15\n", distributions), [
    "the Python licence notice does not mention pydantic 2.14.1, which is present in the staged payload",
  ]);
  // A payload with no `.dist-info` at all is a failure, not a vacuous pass: the notice
  // claims attribution for distributions the payload does not have. The collector fails
  // closed on the same condition, so packaging a non-Agent directory cannot go green.
  assert.deepEqual(agentNoticeProblems("", []), [
    "no *.dist-info metadata was found in the staged Agent payload, so the Python licence notice describes distributions the payload does not contain",
  ]);
});

test("readDistributionIdentity reads RFC 822 headers and survives a missing Name", () => {
  assert.deepEqual(
    readDistributionIdentity("Metadata-Version: 2.4\nName: cryptography\nVersion: 50.0.2\n\nBody", "x.dist-info"),
    { name: "cryptography", version: "50.0.2" },
  );
  // Continuation lines fold into the previous header, and a `.dist-info` stem is the
  // fallback that keeps this comparable with the collector's own fallback.
  assert.deepEqual(readDistributionIdentity("Name: split\n  package\nVersion: 1\n", "x.dist-info"), {
    name: "split package",
    version: "1",
  });
  assert.deepEqual(readDistributionIdentity("Metadata-Version: 2.4\n", "fallback_pkg-9.9.dist-info"), {
    name: "fallback_pkg-9.9",
    version: "unknown",
  });
});

test("inputConsistencyProblems checks provenance against the payloads it names", () => {
  const payloads = [{ path: "p/host.exe", size: 1, sha256: "a".repeat(64) }];
  const base = {
    payloadDirectory: "p",
    payloads,
    inputs: [
      {
        role: "desktop-host",
        path: "p/host.exe",
        sourceDirectory: "native/desktop_host",
        sourceRevision: "abc1234",
        modifiedAt: "2026-10-17T04:04:16.825Z",
        sha256: "a".repeat(64),
      },
    ],
  };
  assert.deepEqual(inputConsistencyProblems(base), []);
  // A manifest written before provenance existed stays legal.
  assert.deepEqual(inputConsistencyProblems({ payloadDirectory: "p", payloads }), []);

  // These are the edits a "provenance" section that nothing reads would survive, and
  // each must now be caught without touching the disk: `--manifest` relies on that.
  const cases = [
    [{ ...base, inputs: [{ ...base.inputs[0], sha256: "0".repeat(64) }] }, /disagrees with the payload entry/],
    [{ ...base, inputs: [{ ...base.inputs[0], path: "p/other.exe" }] }, /names a path the manifest does not list/],
    [{ ...base, inputs: [...base.inputs, { ...base.inputs[0] }] }, /lists desktop-host twice/],
    [{ ...base, inputs: [{ ...base.inputs[0], role: "" }] }, /has no role/],
    [{ ...base, inputs: [{ ...base.inputs[0], sourceDirectory: "../outside" }] }, /unsafe manifest\.inputs sourceDirectory/],
    [{ ...base, inputs: [{ ...base.inputs[0], sha256: "nope" }] }, /invalid manifest\.inputs sha256/],
    [{ ...base, inputs: "not-an-array" }, /manifest\.inputs is not an array/],
    [{ ...base, inputs: [null] }, /non-object entry/],
  ];
  for (const [manifest, pattern] of cases) {
    const problems = inputConsistencyProblems(manifest);
    assert.ok(problems.some((problem) => pattern.test(problem)), `${pattern} not reported in ${JSON.stringify(problems)}`);
  }
});

test("verifyReleaseManifest refuses a launch section whose UI directory ships no index.html", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  const payloadDirectory = "yukinal-1.0.0-win32-x64";

  // The host exits 2 without a readable entry document, so a manifest that points at a
  // UI directory it does not ship describes a payload nobody can launch.
  await assert.rejects(
    () => verifyReleaseManifest(directory, { ...manifest, launch: { uiDirectory: `${payloadDirectory}/ui` } }),
    /launch\.uiDirectory .* ships no index\.html in the manifest/,
  );

  await assert.rejects(
    () => verifyReleaseManifest(directory, { ...manifest, launch: { uiDirectory: "elsewhere/ui" } }),
    /launch\.uiDirectory escapes/,
  );

  // A UI directory that really is listed passes.
  const withUi = {
    ...manifest,
    payloads: [
      ...manifest.payloads,
      { path: `${payloadDirectory}/ui/index.html`, size: 4, sha256: digestOf("html") },
    ],
    launch: { uiDirectory: `${payloadDirectory}/ui` },
  };
  await mkdir(join(directory, payloadDirectory, "ui"), { recursive: true });
  await writeFile(join(directory, payloadDirectory, "ui", "index.html"), "html");
  await writeFile(join(directory, CHECKSUMS_FILE), renderChecksums(withUi.payloads));
  assert.equal((await verifyReleaseManifest(directory, withUi)).scanned, withUi.payloads.length);
});

test("verifyReleaseManifest refuses an Agent entry that is absent or unattributed", async (t) => {
  const directory = await temporaryDirectory(t);
  const manifest = await stageSamplePayload(directory);
  const payloadDirectory = "yukinal-1.0.0-win32-x64";
  const agentEntry = `${payloadDirectory}/python-agent/yukinal-agent.exe`;

  await assert.rejects(
    () => verifyReleaseManifest(directory, { ...manifest, launch: { agentEntry: `${payloadDirectory}/python-agent/missing.exe` } }),
    /launch\.agentEntry .* is not in the manifest/,
  );
  await assert.rejects(
    () => verifyReleaseManifest(directory, { ...manifest, launch: { agentEntry: "elsewhere/yukinal-agent.exe" } }),
    /launch\.agentEntry escapes/,
  );

  // Deleting the notice used to let `--verify` report a fully green payload: nothing
  // cross-checked attribution, so a redistributor could drop it and no gate noticed.
  const withoutNotice = {
    ...manifest,
    payloads: manifest.payloads.filter(
      (payload) => payload.path !== `${payloadDirectory}/python-agent/THIRD_PARTY_LICENSES.txt`,
    ),
    launch: { agentEntry },
  };
  await writeFile(join(directory, CHECKSUMS_FILE), renderChecksums(withoutNotice.payloads));
  await assert.rejects(
    () => verifyReleaseManifest(directory, withoutNotice),
    /ships without .*THIRD_PARTY_LICENSES\.txt/,
  );
});

/**
 * Creates a reparse point at `link` pointing at `target`, or reports why it cannot.
 * Windows needs Developer Mode or elevation for a file/dir symlink, but a directory
 * *junction* only needs the target to exist, so try that first there.
 */
async function createReparsePoint(target, link) {
  const types = process.platform === "win32" ? ["junction", "dir", "file"] : ["dir", "file"];
  let lastError;
  for (const type of types) {
    try {
      await symlink(target, link, type);
      return undefined;
    } catch (error) {
      lastError = error;
    }
  }
  return lastError;
}

test("verifyReleaseManifest refuses a manifest entry that is a reparse point", async (t) => {
  const directory = await temporaryDirectory(t);
  const payloadDirectory = "payload";
  const outside = join(directory, "outside-dir");
  await mkdir(join(directory, payloadDirectory), { recursive: true });
  await mkdir(outside, { recursive: true });
  await writeFile(join(outside, "host.exe"), "outside bytes");
  const link = join(directory, payloadDirectory, "linked");
  const failure = await createReparsePoint(outside, link);
  if (failure !== undefined) {
    t.skip(`cannot create a reparse point here: ${failure.code}`);
    return;
  }

  const manifest = {
    schemaVersion: 1,
    payloadDirectory,
    payloads: [
      { path: `${payloadDirectory}/linked/host.exe`, size: 13, sha256: await sha256File(join(outside, "host.exe")) },
    ],
  };
  await writeFile(join(directory, CHECKSUMS_FILE), renderChecksums(manifest.payloads));
  // The bytes the entry names are readable *through* the link; the verifier must
  // still refuse, because it would be attesting to a path it does not own.
  await assert.rejects(
    () => verifyReleaseManifest(directory, manifest),
    /payload path is a reparse point: payload\/linked/,
  );
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
