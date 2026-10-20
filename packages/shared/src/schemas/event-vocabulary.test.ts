/**
 * The event vocabulary, pinned as an explicit list — with producer state per name.
 *
 * `EVENT_NAMES` (declared names) and `EVENT_SCHEMAS` (the runtime gate that also defines
 * `DesktopEventName` / `AgentEventName`) overlap without being equal, and neither of them
 * says anything about who *emits*. That gap is what this file records.
 *
 * The earlier version of this header claimed the only gap was "Rust-only events", and that
 * every agent member had "a producer, a channel, and a consumer". Both were false: the
 * producers named were in files that no longer exist (`apps/agent/src/agent-loop.ts`,
 * the retired `apps/desktop/src-tauri/`), and nine declared names have no emitter anywhere
 * in the repository. So the lists below are *measured* states — gated, unproduced, and
 * emitted-without-a-subscriber — and the tests assert they partition `EVENT_NAMES`. A name
 * may not sit in two cells of the same question, and a name that changes state has to be
 * changed here. The nine-name count is itself asserted: this is the C-2/B7 record.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { EVENT_NAMES, eventChannelName } from "../events/index.js";
import { TOOL_RESULT_STATUSES } from "../types/enums.js";
import { AGENT_EVENT_MEMBER_SCHEMAS, AGENT_EVENT_TYPES, AgentStreamEventSchema } from "./agent.js";
import { EVENT_SCHEMAS } from "./ipc.js";

/** Channels the UI can subscribe to: declared *and* gated. */
const SUBSCRIBABLE = [
  "agent.team_task",
  "agent.started",
  "agent.thinking",
  "agent.text",
  "agent.usage",
  "agent.tool_call",
  "agent.tool_result",
  "agent.waiting_approval",
  "agent.approval_expired",
  "agent.completed",
  "agent.failed",
  "terminal.data",
  "terminal.closed",
  "terminal.output_lost",
  "server.auth_challenge",
  "mcp.oauth_device_code",
  "activity.created",
  "investigation.schedule_notification",
  "investigation.schedule_activity",
  "file.local_dropped",
  "file.transfer_updated",
] as const;

/**
 * Channels the UI actually subscribes to and no process emits — all nine of them.
 *
 * The UI has a handler or a branch for each (so none is dead on the consumer side), but a
 * literal scan of every emitter in the repository — `native/**` and `agent/src/**`, which
 * are the only two — finds no emit for any of them. Six are gated (`SUBSCRIBABLE`) and
 * have a UI subscriber waiting; the three `server.*` names have neither a gate nor a
 * subscriber, so they are reserved vocabulary in the strictest sense.
 *
 * Kept, not deleted, and each one is annotated per name in `../events/index.ts`:
 *  - `server.connected` / `server.disconnected`: the host's `server_connect` /
 *    `server_disconnect` only write the status column (`ipc_router.cpp:8837`, `:8807`).
 *    The commands exist; emitting is an increment.
 *  - `server.updated`: no emitter at all. `consistency.ts` used to assert "Rust emits it",
 *    which was false when it was written and is unfalsifiable now.
 *  - `server.auth_challenge`: the missing half of interactive SSH auth (C-1); the modal
 *    subscribes and never receives.
 *  - `agent.thinking` / `agent.usage`: the sidecar forwards exactly eight notification
 *    types (`sidecar.py:1015-1019`) and neither is among them, although `useAgentRun`
 *    branches on both.
 *  - `terminal.output_lost`: no `Lagged` handling exists in the host's broadcast path.
 *  - `activity.created`: four UI subscribers, none of which can receive; they poll instead.
 *  - `investigation.schedule_notification`: the scheduler answers in command responses and
 *    never pushes.
 *
 * Deleting any of them would make the eventual fix a contract break rather than an
 * addition. What was wrong was pretending they were live.
 */
const NO_PRODUCER = [
  "server.connected",
  "server.disconnected",
  "server.updated",
  "server.auth_challenge",
  "agent.thinking",
  "agent.usage",
  "terminal.output_lost",
  "activity.created",
  "investigation.schedule_notification",
] as const;

/**
 * Declared and really emitted, but nothing in the UI listens.
 *
 * `terminal.opened` is the only member: the C++ terminal manager does emit it
 * (`native/desktop_host/src/desktop_terminal.cpp:391`); it has no `EVENT_SCHEMAS` gate
 * because no subscriber needs one. Listed so the gap is a recorded state rather than an
 * accident — a new entry here should be a decision.
 */
const EMITTED_WITHOUT_A_SUBSCRIBER = ["terminal.opened"] as const;

test("the subscribable set is exactly the declared names that have a gate", () => {
  const gated = Object.keys(EVENT_SCHEMAS).sort();
  assert.deepEqual(gated, [...SUBSCRIBABLE].sort());
});

test("the recorded event states partition EVENT_NAMES exactly", () => {
  // Two independent questions are asked of every declared name — *can the UI subscribe?*
  // (`SUBSCRIBABLE`) and *does anything emit it?* (`NO_PRODUCER`) — and their cross product
  // has four cells. Together they must cover every name exactly once, so a name cannot be
  // added to `EVENT_NAMES` without recording both answers. The two lists overlap by design:
  // "gated and unproduced" is a real, common state, not a bookkeeping error.
  const names = [...EVENT_NAMES].sort();
  const gated = new Set<string>(SUBSCRIBABLE);
  const unproduced = new Set<string>(NO_PRODUCER);
  const emitted = new Set<string>(EMITTED_WITHOUT_A_SUBSCRIBER);

  const cells = {
    "gated, no measured absence of a producer": names.filter((n) => gated.has(n) && !unproduced.has(n)),
    "gated, no producer": names.filter((n) => gated.has(n) && unproduced.has(n)),
    "ungated, no producer": names.filter((n) => !gated.has(n) && unproduced.has(n)),
    "ungated, emitted": names.filter((n) => emitted.has(n)),
  };
  for (const [label, members] of Object.entries(cells)) {
    for (const name of members) {
      assert.ok(
        (SUBSCRIBABLE as readonly string[]).includes(name) ||
          (NO_PRODUCER as readonly string[]).includes(name) ||
          (EMITTED_WITHOUT_A_SUBSCRIBER as readonly string[]).includes(name),
        `${name} is in the "${label}" cell but appears in none of the three lists`,
      );
    }
  }
  const covered = Object.values(cells).flat().sort();
  assert.deepEqual(covered, names, "EVENT_NAMES changed without recording the channel's state here");
  // Every list entry has to be a declared name, or a rename would leave a stale record.
  for (const name of [...SUBSCRIBABLE, ...NO_PRODUCER, ...EMITTED_WITHOUT_A_SUBSCRIBER]) {
    assert.ok((EVENT_NAMES as readonly string[]).includes(name), `${name} is recorded but not declared`);
  }
  assert.equal(new Set(names).size, names.length, "EVENT_NAMES has a duplicate");
});

test("the nine names with no producer are exactly the ones recorded as such", () => {
  // The count is the deliverable (C-2/B7): nine declared names have no emitter in this
  // repository. Pinning the number here means a tenth cannot appear unnoticed, and a fix
  // that adds a producer has to delete exactly one entry — which is the reminder that a
  // name moves out of this list by *emitting*, not by editing the list.
  assert.equal(NO_PRODUCER.length, 9);
  assert.deepEqual(
    [...new Set([...NO_PRODUCER])].sort(),
    [
      "activity.created",
      "agent.thinking",
      "agent.usage",
      "investigation.schedule_notification",
      "server.auth_challenge",
      "server.connected",
      "server.disconnected",
      "server.updated",
      "terminal.output_lost",
    ],
    "the zero-producer set changed; update the per-name note in ../events/index.ts too",
  );
});

test("every declared name maps to a distinct host channel", () => {
  // `eventChannelName` is the identity, so the assertion here is that the vocabulary has
  // no duplicate and no name that two different channels could share. It used to be a
  // `.`→`:` substitution worth testing for collisions; the C++ host consumes the dotted
  // names verbatim, so a rewrite would now be the bug.
  const channels = EVENT_NAMES.map(eventChannelName);
  assert.equal(new Set(channels).size, channels.length, "two event names share a channel");
  assert.equal(new Set(EVENT_NAMES).size, EVENT_NAMES.length, "EVENT_NAMES has a duplicate");
});

test("the agent channels the sidecar actually emits are all subscribable", () => {
  // The producer side. This list was previously "transcribed from
  // `apps/agent/src/agent-loop.ts`" — a file that no longer exists — and it included
  // `agent.thinking` and `agent.usage`, which the Python sidecar never emits. That is how
  // a false producer claim survived: the list was copied from a deleted implementation
  // instead of from the one that runs.
  //
  // Verified against `agent/src/yukinal_agent/sidecar.py` (the notification sink at
  // `:1015-1019` forwards `params.type`) and `model_agent.py:793,824`. A member the
  // sidecar gains has to be added here, or the UI silently cannot listen to it.
  const emitted = [
    "agent.team_task",
    "agent.started",
    "agent.text",
    "agent.tool_call",
    "agent.tool_result",
    "agent.waiting_approval",
    "agent.approval_expired",
    "agent.completed",
    "agent.failed",
  ];
  for (const name of emitted) {
    assert.ok(
      (SUBSCRIBABLE as readonly string[]).includes(name),
      `the sidecar emits ${name} but the UI cannot subscribe to it`,
    );
    assert.equal(
      (NO_PRODUCER as readonly string[]).includes(name),
      false,
      `${name} is both recorded as emitted and as unproduced`,
    );
  }
  // The other direction: a name recorded as unproduced must not simultaneously be recorded
  // as emitted. `NO_PRODUCER` is now the complete nine-name list, so this check is what
  // stops a future producer from being added in one place and forgotten in the other.
  for (const name of NO_PRODUCER) {
    assert.equal(
      emitted.includes(name),
      false,
      `${name} has no producer in this repository`,
    );
  }
});

/**
 * A payload that is genuinely valid — as `agent.started`.
 *
 * Used as the *wrong member* below: it satisfies the stream schema, so before the
 * channels were narrowed it passed every `agent.*` gate.
 */
const VALID_BUT_WRONG_MEMBER = { type: "agent.started", runId: "run_1", at: "2026-01-01T00:00:00Z" };

/* The agent members that also have a UI channel. Every member is expected here. */

const AGENT_CHANNELS = AGENT_EVENT_TYPES.filter(
  (name): name is Extract<keyof typeof EVENT_SCHEMAS, `agent.${string}`> => name in EVENT_SCHEMAS,
);

test("every agent channel rejects a payload that is valid for a different channel", () => {
  // This is the hole the per-channel schemas exist to close. All ten channels used to
  // point at `AgentStreamEventSchema`, so the gate could only ask "is this *some* valid
  // agent event" — this payload answered yes to all eight, and `useAgentRun`'s
  // `as Extract<…>` cast then told the handler it held a `result` that was not there.
  //
  // The channel name *is* the payload's discriminator, and both come from the same
  // field: the sidecar's notification sink publishes an `agent.stream` notification, and
  // the host routes it by `params.type` (`native/desktop_host/src/ipc_router.cpp:8988-8990`)
  // without rewriting it. So a payload arriving on `agent.completed` cannot carry
  // `type: "agent.started"` — which is why rejecting it costs nothing real.
  for (const name of AGENT_CHANNELS) {
    const parsed = EVENT_SCHEMAS[name].safeParse(VALID_BUT_WRONG_MEMBER);
    if (name === "agent.started") {
      assert.equal(parsed.success, true, `${name} must accept its own payload`);
    } else {
      assert.equal(
        parsed.success,
        false,
        `${name} accepted an agent.started payload: the channel gate is not per-member`,
      );
    }
  }
});

test("each channel uses its own member schema, and the union is built from the same map", () => {
  // Reference equality, not merely behavioural equivalence: re-widening one channel back
  // to `AgentStreamEventSchema` would restore the coarse gate for that channel alone,
  // and the tests above would still pass for every other channel.
  for (const name of AGENT_CHANNELS) {
    assert.equal(
      EVENT_SCHEMAS[name],
      AGENT_EVENT_MEMBER_SCHEMAS[name],
      `${name} is not gated by its own member schema`,
    );
  }
  // The union still validates every member, so the sidecar transport gate (which does
  // not know the type before parsing) is unaffected by the narrowing.
  assert.equal(AgentStreamEventSchema.safeParse(VALID_BUT_WRONG_MEMBER).success, true);
  assert.deepEqual(
    [...AGENT_CHANNELS].sort(),
    (SUBSCRIBABLE as readonly string[]).filter((name) => name.startsWith("agent.")).sort(),
    "the channel set and the subscribable agent channels disagree",
  );
  assert.equal(
    AGENT_EVENT_TYPES.length,
    AGENT_CHANNELS.length,
    "every agent stream member must have a subscribable channel",
  );
});

test("the waiting_approval payload makes session grantability explicit and typed", () => {
  const base = {
    type: "agent.waiting_approval",
    runId: "run_1",
    approval: {
      approvalId: "apr_1",
      runId: "run_1",
      toolName: "docker.restart",
      input: {},
      reason: "why",
      factsSummary: [],
      target: { host: "remote", serverId: "srv_abc", environment: "staging" },
      expiresAt: "2026-01-01T00:01:00Z",
    },
    at: "2026-01-01T00:00:00Z",
  };
  const schema = AGENT_EVENT_MEMBER_SCHEMAS["agent.waiting_approval"];
  // An older sidecar omits the field, so the omission must keep parsing (fail-open on
  // the *shape*, not on the permission: the UI keeps the button until told otherwise).
  assert.equal(schema.safeParse(base).success, true);
  for (const sessionGrantable of [true, false]) {
    assert.equal(schema.safeParse({ ...base, approval: { ...base.approval, sessionGrantable } }).success, true);
  }
  assert.equal(
    schema.safeParse({ ...base, approval: { ...base.approval, sessionGrantable: "yes" } }).success,
    false,
    "a truthy string must not be accepted as a boolean",
  );

  const serverExecApproval = {
    ...base,
    approval: {
      ...base.approval,
      toolName: "server.exec",
      traceId: "trace_1",
      callId: "call_1",
      inputFingerprint: "a".repeat(64),
      planId: "plan_1",
      planStepId: "step_1",
    },
  };
  assert.equal(schema.safeParse(serverExecApproval).success, true);
  assert.equal(
    schema.safeParse({
      ...serverExecApproval,
      approval: { ...serverExecApproval.approval, inputFingerprint: undefined },
    }).success,
    false,
    "server.exec approval cards need the host binding fields",
  );
});

test("a tool result reports a finished status, not a lifecycle status", () => {
  // `TOOL_RESULT_STATUSES` is deliberately a *subset* of `TOOL_EXECUTION_STATUSES`,
  // and this is the test that keeps the distinction honest. Sharing the six-value
  // lifecycle tuple would have been the obvious-looking simplification and would have
  // silently widened this schema — by the time a result exists, the call has stopped,
  // so `pending` / `running` / `waiting_approval` describe a state that cannot be real.
  const base = {
    type: "agent.tool_result",
    runId: "run_1",
    traceId: "trace_1",
    stepId: "step_1",
    callId: "call_1",
    toolName: "filesystem.read",
    input: {},
    target: { host: "remote", serverId: "srv_abc", environment: "production" },
    riskLevel: "read",
    decision: "auto",
    outputSummary: "ok",
    startedAt: "2026-01-01T00:00:00Z",
    endedAt: "2026-01-01T00:00:01Z",
    durationMs: 1000,
    at: "2026-01-01T00:00:01Z",
  };
  const schema = AGENT_EVENT_MEMBER_SCHEMAS["agent.tool_result"];
  for (const status of TOOL_RESULT_STATUSES) {
    assert.equal(schema.safeParse({ ...base, status }).success, true, `a real result may be ${status}`);
  }
  assert.equal(
    schema.safeParse({ ...base, status: "failed", executionState: "result_unknown" }).success,
    true,
    "server.exec may report a bounded unknown outcome state",
  );
  assert.equal(
    schema.safeParse({ ...base, status: "failed", executionState: "completed" }).success,
    false,
    "executionState is limited to known interruption states",
  );
  for (const status of ["pending", "running", "waiting_approval"]) {
    assert.equal(
      schema.safeParse({ ...base, status }).success,
      false,
      `a tool result must not claim the in-flight status ${status}`,
    );
  }
});
