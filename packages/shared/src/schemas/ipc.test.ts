/**
 * Contract tests for the IPC command schemas.
 *
 * The fixtures in `packages/shared/fixtures/ipc/` are the canonical JSON the gates must
 * accept. They are *shared* fixtures, not a mirror: the retired Rust contract test that
 * asserted the same files against serde output is gone with `crates/`, so nothing on the
 * native side reads this directory today. The two sides are kept aligned by these
 * fixtures plus `consistency.ts` — not by one implementation consuming the other's file.
 *
 * Strictness is a property of *who authors the payload*:
 *  - a response the C++ host composes field by field (`agent_status`,
 *    `server_connect`, the host-key replies, ...) is strict — an unknown key there is a
 *    real disagreement between two files in this repo;
 *  - a response the host forwards verbatim from the Python sidecar
 *    (`SIDECAR_FORWARDED_COMMANDS`) tolerates unknown keys, because the sidecar is a
 *    separately-versioned process whose answer this repo does not own. Compatibility
 *    must not be a parse crash; the added key is reported instead. See the regression
 *    test further down.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import type { IpcCommandName } from "../ipc/index.js";
import { DockerRestartInputSchema, DockerRestartResultSchema } from "./docker.js";
import { FilesystemReadInputSchema, FilesystemWriteInputSchema } from "./file.js";
import { McpServerSaveInputSchema } from "./mcp.js";
import {
  AgentStatusSchema,
  EMPTY_PAYLOAD,
  EVENT_SCHEMAS,
  IpcServerIdSchema,
  IPC_SCHEMAS,
  SIDECAR_FORWARDED_COMMANDS,
  forwardedResponseSchema,
  strictResponseSchema,
  unknownResponseKeys,
} from "./ipc.js";

const FIXTURE_DIR = new URL("../../fixtures/ipc/", import.meta.url);

function fixture(name: string): unknown {
  return JSON.parse(readFileSync(new URL(`${name}.json`, FIXTURE_DIR), "utf8")) as unknown;
}

test("every command in IpcCommandMap has a fixture and its response schema parses it", () => {
  const commands = Object.keys(IPC_SCHEMAS) as IpcCommandName[];
  assert.ok(commands.length >= 14, "the command map must not silently shrink");
  for (const command of commands) {
    const payload = fixture(command);
    const parsed = IPC_SCHEMAS[command].response.safeParse(payload);
    assert.equal(
      parsed.success,
      true,
      `${command} response schema must parse packages/shared/fixtures/ipc/${command}.json`,
    );
  }
});

test("the exited agent_status variant parses too", () => {
  const parsed = AgentStatusSchema.safeParse(fixture("agent_status_exited"));
  assert.equal(parsed.success, true);
});

test("a status carrying an automatic-restart record parses, and the field stays optional", () => {
  // The shape parses — but nothing produces it. `agent_status` is composed field by field
  // by the C++ host, and that branch never sets `restart`; there is no supervisor in the
  // host that could restart a sidecar. The field is kept because the UI branches on it,
  // not because a producer exists; see the schema comment on `AgentStatusSchema.restart`.
  //
  // Not covered by the per-command loop above: there is only one fixture per command
  // name, and `agent_status.json` is the shape an idle supervisor reports.
  const parsed = AgentStatusSchema.safeParse(fixture("agent_status_restarted"));
  assert.equal(parsed.success, true, "a restart record must parse");
  const status = parsed.data as { restart?: { attempt: number; exhausted: boolean } };
  assert.equal(status.restart?.attempt, 2);
  assert.equal(status.restart?.exhausted, false);
  // Optional, not nullable: an idle supervisor omits it entirely, and the fixture from
  // before automatic recovery existed must keep parsing.
  const idle = AgentStatusSchema.safeParse(fixture("agent_status"));
  assert.equal(idle.success, true);
  assert.equal((idle.data as { restart?: unknown }).restart, undefined);
});

test("a sync run.start response carries the run's result", () => {
  // The async fixture (`agent_run_start.json`) is the one the per-command loop above
  // checks, and it has no `result` — which is correct for `delivery: "async"`. This is
  // the other half of the pair: the shape a `delivery: "sync"` call answers with, and
  // the reason that mode is worth waiting for. It is not covered by the loop above
  // because there is only one fixture per command name.
  const parsed = IPC_SCHEMAS.agent_run_start.response.safeParse(fixture("agent_run_start_sync"));
  assert.equal(parsed.success, true, "the sync response must parse");
  const response = parsed.data as { started: boolean; result?: { state: string; steps: number } };
  assert.equal(response.started, true);
  assert.equal(response.result?.state, "completed");
  assert.equal(response.result?.steps, 3);
});

test("run.start admission flags reach the IPC consumer", () => {
  const duplicate = IPC_SCHEMAS.agent_run_start.response.safeParse({
    runId: "run_existing",
    started: false,
    duplicate: true,
  });
  assert.equal(duplicate.success, true);

  const resumed = IPC_SCHEMAS.agent_run_start.response.safeParse({
    runId: "run_admitted",
    started: true,
    resumed: true,
  });
  assert.equal(resumed.success, true);
});

test("a result on a sync response is validated, not passed through", () => {
  // The result object reaches the UI through a response frame, so it does not pass the
  // event gate. If a sidecar ever sends a malformed one, this is where it has to fail.
  const malformed = {
    runId: "run_20260101",
    started: true,
    result: { runId: "run_20260101", state: "not-a-state", text: "", steps: 0, toolCalls: 0 },
  };
  assert.equal(IPC_SCHEMAS.agent_run_start.response.safeParse(malformed).success, false);
});

test("host-authored responses stay strict: an extra field must fail, not be stripped", () => {
  // `agent_status` is composed by the C++ host field by field, so an extra key means two
  // files in this repo disagree. Keeping this strict is what makes the looser gate below a
  // deliberate reading of *where a payload comes from* rather than a blanket weakening.
  const drifted = { ...(fixture("agent_status") as Record<string, unknown>), toolCount: 1, os: "klingon" };
  assert.equal(AgentStatusSchema.safeParse(drifted).success, false);
  assert.equal((SIDECAR_FORWARDED_COMMANDS as readonly string[]).includes("agent_status"), false);
});

test("the sidecar-forwarded set is exactly the commands the host forwards verbatim", () => {
  // Pinned by name: the set is a property of `IpcRouter::agent_run_command` /
  // `provider_command` (native/desktop_host/src/ipc_router.cpp), which hand the sidecar's
  // JSON back as the response frame without rebuilding it. A command moving in or out of
  // that list changes how much compatibility this contract owes a foreign process, so the
  // move has to be recorded here.
  assert.deepEqual(
    [...SIDECAR_FORWARDED_COMMANDS].sort(),
    [
      "agent_context_reset",
      "agent_context_reset_all",
      "agent_memory_delete",
      "agent_memory_list",
      "agent_memory_save",
      "agent_run_start",
      "agent_run_stop",
      "provider_activate",
      "provider_delete",
      "provider_list",
      "provider_models",
      "provider_save",
      "provider_test",
    ],
  );
});

/** A field the sidecar could add without the UI being updated: top level, and nested. */
function driftedForwardedResponse(command: string): Record<string, unknown> {
  const base = fixture(command) as Record<string, unknown>;
  const drifted: Record<string, unknown> = { ...base, sidecarAddedField: { nested: true } };
  for (const [key, value] of Object.entries(drifted)) {
    if (Array.isArray(value) && value.length > 0 && typeof value[0] === "object" && value[0] !== null) {
      drifted[key] = [{ ...(value[0] as Record<string, unknown>), sidecarAddedNestedField: 7 }, ...value.slice(1)];
    } else if (typeof value === "object" && value !== null && !Array.isArray(value)) {
      drifted[key] = { ...(value as Record<string, unknown>), sidecarAddedNestedField: 7 };
    }
  }
  return drifted;
}

test("a sidecar response with an unknown extra field parses instead of breaking the UI", () => {
  // The regression this pins: the host does not re-shape what the sidecar answered
  // (`ipc_router.cpp` returns `client.request(...)` as the response frame), and the sidecar
  // is a separately-versioned process. Before this route, one added field anywhere in the
  // payload — top level or nested — turned a successful agent/provider call into a parse
  // exception in the UI.
  for (const command of SIDECAR_FORWARDED_COMMANDS) {
    const drifted = driftedForwardedResponse(command);
    const schema = IPC_SCHEMAS[command].response as unknown as { safeParse: (value: unknown) => { success: boolean } };
    let parsed: { success: boolean };
    assert.doesNotThrow(() => {
      parsed = schema.safeParse(drifted);
    }, `${command} threw on an unknown field`);
    assert.equal(parsed!.success, true, `${command} rejected a response carrying one added field`);
  }
});

/**
 * Commands the host implements but nothing in `apps/desktop/src` calls.
 *
 * Recorded rather than deleted, and recorded *here* so the state is auditable: each of
 * these is a specified, host-implemented operation that no UI surface reaches yet, and the
 * annotation on its row in `ipc.ts` says which case it is. A reviewer who sees a schema and
 * a fixture should not have to guess whether anything calls it.
 *
 * Two things must stay true, and this test is what keeps them true: the row still exists
 * (deleting one is a contract change, not cleanup), and it is still not called from the UI.
 * When a call site appears, the name moves out of this list — deliberately.
 */
const HOST_IMPLEMENTED_WITHOUT_A_UI_CALL_SITE = [
  "agent_kill",
  "investigation_task_status_update",
  "investigation_schedule_tick",
  "remote_file_read",
  "file_transfer_get",
] as const;

test("host-implemented commands with no UI call site stay implemented and stay recorded", () => {
  for (const command of HOST_IMPLEMENTED_WITHOUT_A_UI_CALL_SITE) {
    assert.ok(
      Object.hasOwn(IPC_SCHEMAS, command),
      `${command} is recorded as host-implemented with no UI call site, so its row must exist`,
    );
    // The fixture is the proof the host's answer still fits the declared shape; without it
    // "implemented" would be an assertion about code nobody exercises.
    const parsed = IPC_SCHEMAS[command as IpcCommandName].response.safeParse(fixture(command));
    assert.equal(parsed.success, true, `${command} has a fixture its response schema rejects`);
  }
});

test("a forwarded response keeps its known fields, and the addition stays observable", () => {
  // Tolerance must not mean "stop looking": the known fields still have to arrive intact
  // and the unknown key has to remain reportable, otherwise a drifting sidecar becomes
  // invisible. `unknownResponseKeys` is what the desktop gate turns into a diagnostic.
  const drifted = driftedForwardedResponse("provider_list");
  const parsed = IPC_SCHEMAS.provider_list.response.parse(drifted) as { providers: { id: string }[] };
  assert.equal(parsed.providers.length, (fixture("provider_list") as { providers: unknown[] }).providers.length);
  assert.equal(typeof parsed.providers[0]?.id, "string");

  const keys = unknownResponseKeys(forwardedResponseSchema(IPC_SCHEMAS.provider_list.response), drifted);
  assert.deepEqual([...keys].sort(), ["sidecarAddedField", "sidecarAddedNestedField"]);
});

test("tolerating an addition did not stop rejecting a missing field or a wrong type", () => {
  const missing = IPC_SCHEMAS.agent_run_start.response.safeParse({ started: true });
  assert.equal(missing.success, false, "a response without its required runId must still fail");

  const wrongType = IPC_SCHEMAS.provider_test.response.safeParse({ ok: false });
  assert.equal(wrongType.success, false, "`ok` is the literal true; false is still drift");

  const forwarded = fixture("provider_list") as Record<string, unknown>;
  assert.equal(
    IPC_SCHEMAS.provider_list.response.safeParse({ ...forwarded, sidecarAddedField: 1, providers: [] }).success,
    true,
  );
});

test("the strict reading of a forwarded response is still available", () => {
  // The loosening is a reading, not a deletion: `strictResponseSchema` reconstructs the
  // pre-tolerance gate from the same source, so a test (or a future audit) can still ask
  // "would this have failed before?" and get the same answer the strict rows give.
  const drifted = driftedForwardedResponse("agent_run_start");
  const tolerant = IPC_SCHEMAS.agent_run_start.response.safeParse(drifted);
  assert.equal(tolerant.success, true);

  const strict = strictResponseSchema(IPC_SCHEMAS.agent_run_start.response).safeParse(drifted);
  assert.equal(strict.success, false);
  assert.deepEqual(
    strict.success ? [] : strict.error.issues.map((issue) => issue.code).filter((code) => code === "unrecognized_keys"),
    ["unrecognized_keys"],
  );
});

test("empty payloads reject unknown keys", () => {
  assert.equal(EMPTY_PAYLOAD.safeParse({}).success, true);
  assert.equal(EMPTY_PAYLOAD.safeParse({ anything: 1 }).success, false);
});

test("MCP authentication keeps the secret write-only", () => {
  const input = McpServerSaveInputSchema.safeParse({
    id: "mcp_http",
    label: "Remote MCP",
    transport: "http",
    url: "https://mcp.example.com/mcp",
    httpAuthHeaders: [
      { name: "Authorization", secret: "Bearer test-secret" },
      { name: "X-Gateway-Key", secret: "gateway-secret" },
    ],
    enabled: true,
  });
  assert.equal(input.success, true);

  const response = IPC_SCHEMAS.mcp_server_list.response.safeParse(fixture("mcp_server_list"));
  assert.equal(response.success, true);
  if (!response.success) return;
  const remote = response.data.servers.find((server) => server.config.id === "mcp_remote");
  assert.deepEqual(remote?.config.httpAuthHeaders, [
    {
      name: "Authorization",
      credentialRef: "keychain://mcp/mcp_remote-cred",
    },
  ]);
  assert.equal(
    "secret" in (remote?.config.httpAuthHeaders[0] ?? {}),
    false,
    "the settings response must never carry the secret value",
  );
});

test("MCP OAuth config crosses IPC without carrying tokens", () => {
  const input = McpServerSaveInputSchema.safeParse({
    id: "mcp_oauth",
    label: "OAuth MCP",
    transport: "http",
    url: "https://mcp.example.com/mcp",
    oauth: {
      issuer: "https://auth.example.com",
      clientId: "desktop-client",
      scopes: ["mcp.read"],
    },
    enabled: true,
  });
  assert.equal(input.success, true);
  // A hand-entered secret rides along on the save input and nowhere else: the stored shape
  // has only a reference, so there is no field for it to come back in.
  const confidential = McpServerSaveInputSchema.safeParse({
    id: "mcp_oauth_confidential",
    label: "OAuth MCP with a secret",
    transport: "http",
    url: "https://mcp.example.com/mcp",
    oauth: {
      issuer: "https://auth.example.com",
      clientId: "confidential-client",
      clientAuth: "client_secret_basic",
      clientSecret: "s3cret",
      scopes: ["mcp.read"],
    },
    enabled: true,
  });
  assert.equal(confidential.success, true);
  assert.equal(
    McpServerSaveInputSchema.safeParse({
      id: "mcp_oauth_oversized",
      label: "OAuth MCP with an oversized secret",
      transport: "http",
      url: "https://mcp.example.com/mcp",
      oauth: {
        issuer: "https://auth.example.com",
        clientId: "confidential-client",
        clientAuth: "client_secret_post",
        clientSecret: "x".repeat(8_193),
        scopes: ["mcp.read"],
      },
      enabled: true,
    }).success,
    false,
    "a secret larger than the documented bound must not reach the host",
  );
  assert.equal(
    McpServerSaveInputSchema.safeParse({
      id: "mcp_oauth_unknown_method",
      label: "OAuth MCP with an invented method",
      transport: "http",
      url: "https://mcp.example.com/mcp",
      oauth: {
        issuer: "https://auth.example.com",
        clientId: "confidential-client",
        clientAuth: "client_secret_jwt",
        scopes: ["mcp.read"],
      },
      enabled: true,
    }).success,
    false,
    "an authentication method the host cannot perform must be refused here",
  );
  assert.equal(
    McpServerSaveInputSchema.safeParse({
      id: "mcp_oauth_auto",
      label: "OAuth auto discovery",
      transport: "http",
      url: "https://mcp.example.com/mcp",
      oauth: {
        issuer: "",
        clientId: "",
        scopes: ["mcp.read"],
      },
      enabled: true,
    }).success,
    true,
  );

  const remote = IPC_SCHEMAS.mcp_server_list.response.safeParse({
    servers: [
      {
        config: {
          id: "mcp_oauth",
          label: "OAuth MCP",
          transport: "http",
          url: "https://mcp.example.com/mcp",
          httpAuthHeaders: [],
          oauth: {
            issuer: "https://auth.example.com",
            clientId: "desktop-client",
            // The host always writes the flow: rows written before it existed default to
            // `authorization_code`, so the wire shape never has to guess.
            flow: "device_code",
            // Same for the client authentication: rows written before it existed are public.
            clientAuth: "client_secret_post",
            clientSecretRef: "keychain://mcp/oauth-secret",
            // And the same for sender-constrained tokens: absent means off, and a stored
            // key reference never carries the key itself.
            dpop: true,
            dpopKeyRef: "keychain://mcp/oauth-dpop",
            scopes: ["mcp.read"],
            tokenEndpoint: "https://auth.example.com/token",
            credentialRef: "keychain://mcp/oauth",
          },
          enabled: true,
          allowedTools: [],
          trustLevel: "unreviewed",
        },
        status: {
          serverId: "mcp_oauth",
          running: false,
          toolCount: 0,
          stderrTail: [],
          diagnostics: [],
        },
        tools: [],
      },
    ],
  });
  assert.equal(remote.success, true);
  if (!remote.success) return;
  assert.equal(remote.data.servers[0]?.config.oauth?.flow, "device_code");
  assert.equal(remote.data.servers[0]?.config.oauth?.clientAuth, "client_secret_post");
  assert.equal(
    remote.data.servers[0]?.config.oauth?.clientSecretRef,
    "keychain://mcp/oauth-secret",
    "the settings response may say *that* a secret is stored, never what it is",
  );
  assert.equal("accessToken" in (remote.data.servers[0]?.config.oauth ?? {}), false);
  assert.equal("refreshToken" in (remote.data.servers[0]?.config.oauth ?? {}), false);
  assert.equal("clientSecret" in (remote.data.servers[0]?.config.oauth ?? {}), false);

  // The flow is a closed vocabulary, and an omitted one is refused on a *stored* row:
  // there is no "unspecified flow" the UI would have to interpret.
  const withoutFlow = (
    remote.data.servers[0] as { config: { oauth?: Record<string, unknown> } }
  ).config.oauth;
  const { flow: _ignored, ...noFlow } = withoutFlow ?? {};
  const parsed = IPC_SCHEMAS.mcp_server_list.response.safeParse({
    servers: [{ ...remote.data.servers[0], config: { ...remote.data.servers[0]?.config, oauth: noFlow } }],
  });
  assert.equal(parsed.success, false, "a stored OAuth config must always carry its flow");
  assert.equal(
    IPC_SCHEMAS.mcp_server_list.response.safeParse({
      servers: [
        {
          ...remote.data.servers[0],
          config: {
            ...remote.data.servers[0]?.config,
            oauth: { ...withoutFlow, flow: "implicit" },
          },
        },
      ],
    }).success,
    false,
    "an unknown flow must be refused rather than defaulted",
  );
  assert.equal(
    IPC_SCHEMAS.mcp_server_list.response.safeParse({
      servers: [
        {
          ...remote.data.servers[0],
          config: {
            ...remote.data.servers[0]?.config,
            oauth: { ...withoutFlow, flow: "authorization_code", clientAuth: "client_secret_jwt" },
          },
        },
      ],
    }).success,
    false,
    "an authentication method the host cannot perform must be refused on the way out too",
  );
});

test("server ids on the wire must be opaque srv_ ids", () => {
  for (const name of [
    "server_connect",
    "server_disconnect",
    "server_snapshot",
    "server_host_key_status",
    "server_host_key_probe",
    "server_host_key_forget",
  ] as const) {
    assert.equal(
      IPC_SCHEMAS[name].params.safeParse({ serverId: "api.example.com:22" }).success,
      false,
      `${name} must reject a host-derived id`,
    );
    assert.equal(IPC_SCHEMAS[name].params.safeParse({ serverId: "srv_01abc" }).success, true);
  }
  // `server_host_key_trust` 还带一个指纹，所以单独来一遍（上面的循环里它会因为
  // 缺指纹而失败，那样这半条断言就变成了「缺字段也报错」，什么都没证明）。
  assert.equal(
    IPC_SCHEMAS.server_host_key_trust.params.safeParse({
      serverId: "api.example.com:22",
      probeTicket: "probe_0000000000000000000000000000000000000000000000000000000000000000",
      fingerprint: "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU",
    }).success,
    false,
    "server_host_key_trust must reject a host-derived id",
  );
  const open = IPC_SCHEMAS.terminal_open.params.safeParse({
    serverId: "api.example.com:22",
    cols: 120,
    rows: 30,
  });
  assert.equal(open.success, false, "terminal_open must reject a host-derived id");
  assert.equal(IpcServerIdSchema.safeParse("srv_01abc").success, true);
  assert.equal(IpcServerIdSchema.safeParse("production").success, false);
});

/* ── 主机指纹（ADR 0012） ───────────────────────────────────────────────────── */

test("the unpinned status shape has no fingerprint field at all", () => {
  // 未核验不等于「指纹是空字符串」也不等于「指纹是 null」：宿主会整个省掉这个字段，
  // 而界面要靠它的**缺席**显示「未核验」。这条钉住的是这个区别，不是 JSON 的美观。
  const unpinned = IPC_SCHEMAS.server_host_key_status.response.safeParse(
    fixture("server_host_key_status_unpinned"),
  );
  assert.equal(unpinned.success, true);
  assert.equal(
    Object.hasOwn(unpinned.data as object, "pinnedFingerprint"),
    false,
    "未核验时不该有 pinnedFingerprint",
  );

  // 反过来：显式给一个 null 是 drift，必须被拒。
  const nulled = { host: "api.example.com", port: 22, pinned: false, pinnedFingerprint: null };
  assert.equal(IPC_SCHEMAS.server_host_key_status.response.safeParse(nulled).success, false);
});

test("a probe answer is a comparison, and a mismatch carries both fingerprints", () => {
  // 主 fixture 就是 mismatch：契约里最要紧的那种形状（ADR 0012 第 3 条）必须是
  // 被逐字验证过的那个，而不是只存在于某个测试的局部对象里。
  const parsed = IPC_SCHEMAS.server_host_key_probe.response.safeParse(fixture("server_host_key_probe"));
  assert.equal(parsed.success, true);
  const probe = parsed.data as { comparison: string; pinnedFingerprint?: string; presentedFingerprint: string };
  assert.equal(probe.comparison, "mismatch");
  assert.notEqual(probe.pinnedFingerprint, probe.presentedFingerprint);

  const unpinned = IPC_SCHEMAS.server_host_key_probe.response.safeParse(
    fixture("server_host_key_probe_unpinned"),
  );
  assert.equal(unpinned.success, true);
  assert.equal((unpinned.data as { comparison: string }).comparison, "unpinned");

  // 只有三种比较结果，拼错的第四个词必须是 drift。
  const invented = { ...(fixture("server_host_key_probe") as Record<string, unknown>), comparison: "verified" };
  assert.equal(IPC_SCHEMAS.server_host_key_probe.response.safeParse(invented).success, false);
});

test("a fingerprint on the wire is a real SHA256 fingerprint, not a placeholder", () => {
  const params = IPC_SCHEMAS.server_host_key_trust.params;
  const probeTicket = "probe_0000000000000000000000000000000000000000000000000000000000000000";
  const real = "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU";
  assert.equal(
    params.safeParse({ serverId: "srv_01abc", probeTicket, fingerprint: real }).success,
    true,
  );

  for (const bogus of [
    // 这正是被替换掉的那个占位字符串：它曾经被塞进一个叫 fingerprint 的字段。
    "not pinned (first connect must be explicitly trusted)",
    "SHA256:",
    "MD5:aa:bb:cc",
    // 带 padding 的 base64 不是 ssh-key 的写法（ADR 0012 第 6 条）。
    "SHA256:47DEQpj8HBSa+/TImW+5JCeuQeRkm5NMpJWZG3hSuFU=",
    "",
  ]) {
    assert.equal(
      params.safeParse({ serverId: "srv_01abc", probeTicket, fingerprint: bogus }).success,
      false,
      `trust must refuse to pin ${JSON.stringify(bogus)}`,
    );
  }

  // 指纹是必填的：一个「钉住……什么？」的请求不存在。
  assert.equal(params.safeParse({ serverId: "srv_01abc", fingerprint: real }).success, false);
  assert.equal(params.safeParse({ serverId: "srv_01abc", probeTicket, fingerprint: real }).success, true);
});

test("terminal param shapes match the contract", () => {
  const open = IPC_SCHEMAS.terminal_open.params.safeParse({
    serverId: "srv_01abc",
    cols: 120,
    rows: 30,
  });
  assert.equal(open.success, true);

  const write = IPC_SCHEMAS.terminal_write.params.safeParse({
    terminalSessionId: "t_01",
    data: "ls\r",
  });
  assert.equal(write.success, true);
});

test("SSH keyboard-interactive response commands and challenge events stay bounded", () => {
  assert.equal(
    IPC_SCHEMAS.server_auth_respond.params.safeParse({
      authId: "auth_1",
      responses: ["654321"],
    }).success,
    true,
  );
  assert.equal(
    IPC_SCHEMAS.server_auth_respond.params.safeParse({
      authId: "auth_1",
      responses: Array.from({ length: 17 }, () => ""),
    }).success,
    false,
  );
  assert.equal(
    EVENT_SCHEMAS["server.auth_challenge"].safeParse({
      authId: "auth_1",
      serverId: "srv_01abc",
      username: "deploy",
      host: "api.example.test",
      name: "Two-factor authentication",
      instructions: "Enter the code",
      prompts: [{ prompt: "Verification code", echo: false }],
      expiresAt: "2026-01-01T00:02:00Z",
    }).success,
    true,
  );
  assert.equal(
    EVENT_SCHEMAS["server.auth_challenge"].safeParse({
      authId: "auth_1",
      serverId: "srv_01abc",
      username: "deploy",
      host: "api.example.test",
      name: "Two-factor authentication",
      instructions: "Enter the code",
      prompts: [{ prompt: "Verification code", echo: "no" }],
      expiresAt: "2026-01-01T00:02:00Z",
    }).success,
    false,
  );
});

test("add-server params are validated by the shared input schema", () => {
  const parsed = IPC_SCHEMAS.server_add.params.safeParse({
    name: "db",
    host: "10.0.0.5",
    username: "root",
    environment: "staging",
    authentication: { method: "password", password: "hunter2" },
  });
  assert.equal(parsed.success, true);
});

test("Agent run params carry the explicit permission delegation", () => {
  const parsed = IPC_SCHEMAS.agent_run_start.params.safeParse({
    sessionId: "ses_1",
    prompt: "restart the service",
    permissionMode: "auto",
  });
  assert.equal(parsed.success, true);
  assert.equal(IPC_SCHEMAS.agent_run_start.params.safeParse({
    sessionId: "ses_1",
    prompt: "restart the service",
    permissionMode: "policy",
  }).success, false);
});

test("Agent run params accept bounded inline images and reject unbounded or mislabeled data", () => {
  const run = IPC_SCHEMAS.agent_run_start.params;
  const image = {
    type: "image",
    mediaType: "image/png",
    data: "aGVsbG8=",
    name: "screen.png",
  };
  assert.equal(run.safeParse({ sessionId: "ses_1", prompt: "", parts: [image] }).success, true);
  assert.equal(
    run.safeParse({ sessionId: "ses_1", prompt: "", parts: [{ ...image, data: "data:image/png;base64,aGVsbG8=" }] }).success,
    false,
  );
  assert.equal(
    run.safeParse({ sessionId: "ses_1", prompt: "", parts: [{ ...image, mediaType: "image/svg+xml" }] }).success,
    false,
  );
  assert.equal(
    run.safeParse({
      sessionId: "ses_1",
      prompt: "",
      parts: [
        { type: "text", text: "x".repeat(50_001) },
        { type: "text", text: "y".repeat(50_001) },
      ],
    }).success,
    false,
  );
  assert.equal(run.safeParse({ sessionId: "ses_1", prompt: "" }).success, false);

  // 音频：四种格式都在词汇表里，但每一种的字节数、段数、名字与 base64 形状都各自受约束。
  const clip = { type: "audio", mediaType: "audio/wav", data: "aGVsbG8=", name: "note.wav" };
  assert.equal(run.safeParse({ sessionId: "ses_1", prompt: "", parts: [clip] }).success, true);
  assert.equal(
    run.safeParse({ sessionId: "ses_1", prompt: "", parts: [{ ...clip, name: undefined }] }).success,
    true,
    "a clip needs no visible name, the same way an image does not",
  );
  for (const invalid of [
    { ...clip, mediaType: "audio/aac" },
    { ...clip, data: "data:audio/wav;base64,aGVsbG8=" },
    { ...clip, name: "../note.wav" },
    { ...clip, data: "aGVsbG8" },
  ]) {
    assert.equal(
      run.safeParse({ sessionId: "ses_1", prompt: "", parts: [invalid] }).success,
      false,
      `audio part must be refused: ${JSON.stringify(invalid)}`,
    );
  }
  assert.equal(
    run.safeParse({ sessionId: "ses_1", prompt: "", parts: [clip, clip, clip, clip, clip] }).success,
    false,
    "a fifth clip exceeds the per-message count",
  );
  // 与图片、PDF **共用**同一个 12 MiB 原始字节预算：一张 5 MiB 上限的图片再加一段 7.5 MiB
  // 的音频就已经超过了，而两者各自都还在自己的上限内。
  const biggestImage = {
    type: "image",
    mediaType: "image/png",
    // 6_990_508 base64 字符 = 5_242_881 解码字节，刚好在单图上限（5 MiB）之内。
    data: "A".repeat(6_990_508),
  };
  const bigClip = { ...clip, data: "A".repeat(10_485_760) };
  assert.equal(
    run.safeParse({ sessionId: "ses_1", prompt: "", parts: [biggestImage] }).success,
    true,
  );
  assert.equal(
    run.safeParse({ sessionId: "ses_1", prompt: "", parts: [clip, bigClip] }).success,
    true,
  );
  assert.equal(
    run.safeParse({ sessionId: "ses_1", prompt: "", parts: [biggestImage, bigClip] }).success,
    false,
    "the inline budget is shared, so a big image plus a big clip must be refused",
  );

  const file = {
    type: "file",
    mediaType: "text/plain",
    data: "# Notes\nhello",
    name: "notes.md",
  };
  assert.equal(
    run.safeParse({ sessionId: "ses_1", prompt: "", parts: [file] }).success,
    true,
  );
  assert.equal(
    run.safeParse({
      sessionId: "ses_1",
      prompt: "",
      parts: [{ ...file, mediaType: "application/pdf" }],
    }).success,
    false,
  );
  assert.equal(
    run.safeParse({
      sessionId: "ses_1",
      prompt: "",
      parts: [file, file, file, file, file, file, file, file, file],
    }).success,
    false,
  );
  assert.equal(
    run.safeParse({
      sessionId: "ses_1",
      prompt: "",
      parts: [{ ...file, data: "hello\u0000world" }],
    }).success,
    false,
  );

  const chat = IPC_SCHEMAS.chat_message_append.params;
  assert.equal(
    chat.safeParse({ sessionId: "ses_1", role: "user", content: "", parts: [image] }).success,
    true,
  );
  assert.equal(
    chat.safeParse({ sessionId: "ses_1", role: "user", content: "", parts: [file] }).success,
    true,
  );
  assert.equal(chat.safeParse({ sessionId: "ses_1", role: "user", content: "" }).success, false);
});

test("Agent run params accept bounded PDF documents and reject mislabeled or excessive input", () => {
  const run = IPC_SCHEMAS.agent_run_start.params;
  const document = {
    type: "document",
    mediaType: "application/pdf",
    data: "JVBERi0xLjQ=",
    name: "guide.pdf",
  };
  assert.equal(
    run.safeParse({ sessionId: "ses_1", prompt: "", parts: [document] }).success,
    true,
  );
  assert.equal(
    run.safeParse({
      sessionId: "ses_1",
      prompt: "",
      parts: [{ ...document, data: "data:application/pdf;base64,JVBERi0xLjQ=" }],
    }).success,
    false,
  );
  assert.equal(
    run.safeParse({
      sessionId: "ses_1",
      prompt: "",
      parts: [{ ...document, mediaType: "application/octet-stream" }],
    }).success,
    false,
  );
  assert.equal(
    run.safeParse({
      sessionId: "ses_1",
      prompt: "",
      parts: Array.from({ length: 5 }, (_, index) => ({
        ...document,
        name: `guide-${index}.pdf`,
      })),
    }).success,
    false,
  );
});

test("activity_list accepts an optional server filter and bounded limit", () => {
  const parsed = IPC_SCHEMAS.activity_list.params.safeParse({ serverId: "srv_01abc", limit: 25 });
  assert.equal(parsed.success, true);
  assert.equal(IPC_SCHEMAS.activity_list.params.safeParse({ serverId: "api.example.com:22" }).success, false);
  assert.equal(IPC_SCHEMAS.activity_list.params.safeParse({ limit: 0 }).success, false);
  assert.equal(IPC_SCHEMAS.activity_list.params.safeParse({ limit: 101 }).success, false);
});

test("tool_execution_list accepts trace/server filters and bounded limit", () => {
  assert.equal(
    IPC_SCHEMAS.tool_execution_list.params.safeParse({ traceId: "trc_1", limit: 25 }).success,
    true,
  );
  assert.equal(
    IPC_SCHEMAS.tool_execution_list.params.safeParse({ serverId: "srv_01abc", limit: 25 }).success,
    true,
  );
  assert.equal(
    IPC_SCHEMAS.tool_execution_list.params.safeParse({ serverId: "api.example.com:22" }).success,
    false,
  );
  assert.equal(IPC_SCHEMAS.tool_execution_list.params.safeParse({ limit: 0 }).success, false);
  assert.equal(IPC_SCHEMAS.tool_execution_list.params.safeParse({ limit: 101 }).success, false);
});

test("chat_session_list pages by a bounded offset and reports per-state counts", () => {
  const params = IPC_SCHEMAS.chat_session_list.params;
  assert.equal(params.safeParse({ query: "nginx", archived: false, offset: 50, limit: 50 }).success, true);
  assert.equal(params.safeParse({ offset: 0 }).success, true);
  // A negative or absurd offset would ask SQLite to walk the table for nothing.
  assert.equal(params.safeParse({ offset: -1 }).success, false);
  assert.equal(params.safeParse({ offset: 10_001 }).success, false);

  // `counts` is required, not optional: the filter tabs label themselves with these
  // numbers, and a response without them would leave the UI guessing page-local totals.
  const withoutCounts = IPC_SCHEMAS.chat_session_list.response.safeParse({ sessions: [] });
  assert.equal(withoutCounts.success, false);
  assert.equal(
    IPC_SCHEMAS.chat_session_list.response.safeParse({
      sessions: [],
      counts: { active: 3, archived: 1 },
    }).success,
    true,
  );
  assert.equal(
    IPC_SCHEMAS.chat_session_list.response.safeParse({ sessions: [], counts: { active: -1, archived: 0 } })
      .success,
    false,
  );
});

test("chat_session_rename carries a bounded title and answers with the stored row", () => {
  const params = IPC_SCHEMAS.chat_session_rename.params;
  assert.equal(params.safeParse({ sessionId: "ses_01hqx9", title: "排查 staging 的 nginx 502" }).success, true);
  // Same bound as create: a renamed title is the same field, so it cannot be longer.
  assert.equal(params.safeParse({ sessionId: "ses_01hqx9", title: "" }).success, false);
  assert.equal(params.safeParse({ sessionId: "ses_01hqx9", title: "x".repeat(201) }).success, false);
  assert.equal(IPC_SCHEMAS.chat_session_rename.response.safeParse({ session: {} }).success, false);
});

test("filesystem tool schemas bound paths, reads and writes", () => {
  assert.equal(FilesystemReadInputSchema.safeParse({ path: "/etc/app.env", maxBytes: 4096 }).success, true);
  assert.equal(FilesystemReadInputSchema.safeParse({ path: "relative/path" }).success, false);
  assert.equal(FilesystemReadInputSchema.safeParse({ path: "/tmp/file", maxBytes: 0 }).success, false);
  assert.equal(FilesystemWriteInputSchema.safeParse({ path: "/etc/app.env", content: "PORT=8080" }).success, true);
  assert.equal(FilesystemWriteInputSchema.safeParse({ path: "/etc/app.env", content: "x", extra: true }).success, false);
});

test("docker restart schema keeps the mutating input bounded", () => {
  assert.equal(DockerRestartInputSchema.safeParse({ container: "api_1", timeoutSeconds: 30 }).success, true);
  assert.equal(DockerRestartInputSchema.safeParse({ container: "api;rm -rf /" }).success, false);
  assert.equal(DockerRestartInputSchema.safeParse({ container: "api_1", timeoutSeconds: 0 }).success, false);
  assert.equal(DockerRestartResultSchema.safeParse({ container: "api_1", restarted: true }).success, true);
});

/* ── kind 轴跨过 IPC 的那一段（ADR 0011） ──────────────────────────────────── */

test("provider_save is the one save command, and it carries the kind", () => {
  // 命令名曾经是 `provider_save_openai` —— 一个把「只有一种 kind」写进名字里的名字。
  // 这条钉住的是：保存路径不再对 kind 有隐含取值，缺了它就是一个非法请求。
  assert.equal(
    IPC_SCHEMAS.provider_save.params.safeParse({
      baseUrl: "https://api.example.com/v1",
      model: "model-id",
    }).success,
    false,
    "the save must not default the kind",
  );
  assert.equal(
    IPC_SCHEMAS.provider_save.params.safeParse({
      kind: "anthropic",
      baseUrl: "https://api.anthropic.com",
      model: "claude-sonnet-4-5",
    }).success,
    true,
  );
  // 未知 kind 在这里失败，而不是被当成 openai-compatible 落库。
  assert.equal(
    IPC_SCHEMAS.provider_save.params.safeParse({
      kind: "cohere",
      baseUrl: "https://api.example.com/v1",
      model: "model-id",
    }).success,
    false,
  );
});

test("a saved provider can be any of the three kinds, and native ones carry no wireApi", () => {
  // 每个 kind 一份 fixture：响应 schema 必须能解析全部三种，否则界面上根本显示不出来。
  for (const [fixtureName, kind] of [
    ["provider_save", "openai-compatible"],
    ["provider_save_anthropic", "anthropic"],
    ["provider_save_gemini", "gemini"],
  ] as const) {
    const parsed = IPC_SCHEMAS.provider_save.response.safeParse(fixture(fixtureName));
    assert.equal(parsed.success, true, `${fixtureName} must parse`);
    assert.equal((parsed.data as { provider: { kind: string } }).provider.kind, kind);
  }

  // 反过来：原生 kind 带着 wireApi 是 drift，不是可以忽略的字段。
  const drifted = { provider: { ...(fixture("provider_save_gemini") as { provider: object }).provider, wireApi: "responses" } };
  assert.equal(IPC_SCHEMAS.provider_save.response.safeParse(drifted).success, false);
});

test("provider_list can carry all three kinds at once", () => {
  // 读路径（数据库 → 侧车 → IPC → 界面）曾经把每一行都当成 openai-compatible 解码，
  // 所以这份 fixture 里三种 kind 同时存在，而不是只有被硬编码的那一种。
  const parsed = IPC_SCHEMAS.provider_list.response.safeParse(fixture("provider_list"));
  assert.equal(parsed.success, true);
  const kinds = (parsed.data as { providers: Array<{ kind: string; wireApi?: string }> }).providers;
  assert.deepEqual(kinds.map((provider) => provider.kind), ["openai-compatible", "anthropic", "gemini"]);
  assert.deepEqual(kinds.map((provider) => provider.wireApi), ["chat", undefined, undefined]);
});

test("provider_delete answers whether the credential went with the row", () => {
  // 删除会连带回收 keychain 条目 —— 但只在没有别的 Provider 引用同一份引用时。界面上的
  // 二次确认正是拿这句话在问用户，所以「密钥还在不在」必须由响应回答，而不是留给界面猜。
  const parsed = IPC_SCHEMAS.provider_delete.response.safeParse(fixture("provider_delete"));
  assert.equal(parsed.success, true);
  assert.equal((parsed.data as { credentialReclaimed: unknown }).credentialReclaimed, true);

  const kept = IPC_SCHEMAS.provider_delete.response.safeParse({ deleted: true, credentialReclaimed: false });
  assert.equal(kept.success, true);
  // 两个字段都是必填的：少一个字段不是「没删」，而是这一侧没说清。
  assert.equal(IPC_SCHEMAS.provider_delete.response.safeParse({ deleted: true }).success, false);
  // 参数是一个 id，不是下标或名称 —— 名称在真实数据里是会重复的。
  assert.equal(IPC_SCHEMAS.provider_delete.params.safeParse({ providerId: "prv_1a08a8e7bd95" }).success, true);
  assert.equal(IPC_SCHEMAS.provider_delete.params.safeParse({ label: "deepseek" }).success, false);
});
