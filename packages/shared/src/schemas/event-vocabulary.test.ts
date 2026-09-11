/**
 * The event vocabulary, pinned as an explicit list.
 *
 * `EVENT_NAMES` (declared protocol names) and `EVENT_SCHEMAS` (the runtime gate that
 * also defines `DesktopEventName` / `AgentEventName`) overlap without being equal.
 * The overlap is real and intentional — Rust emits `server.updated` and
 * `terminal.opened` and the UI has no subscriber yet — but *nothing* recorded which
 * names are in that state, so the difference drifted invisibly instead of being a
 * decision someone makes.
 *
 * `agent.text` is what that cost. It sat in `EVENT_NAMES`, in the `YukinalEvent`
 * union, in the `AgentStreamEvent` union with a full zod schema, and in the Rust
 * forwarder's allow-list — while `apps/agent` never emits it (`agent-loop.ts` emits
 * started / thinking / tool_call / tool_result / waiting_approval /
 * approval_expired / completed / failed) and it was absent from `EVENT_SCHEMAS`, so
 * it could not be subscribed to either. A channel with a producer nowhere and a
 * consumer nowhere, spelled out in five files.
 *
 * These tests do not resolve that (removing a name from the agent protocol is a
 * contract change, not a refactor). They make the sets visible, so adding or
 * removing a channel has to be deliberate.
 */

import assert from "node:assert/strict";
import test from "node:test";

import { EVENT_NAMES } from "../events/index.js";
import { tauriEventName } from "../events/index.js";
import { TOOL_RESULT_STATUSES } from "../types/enums.js";
import { AGENT_EVENT_MEMBER_SCHEMAS, AGENT_EVENT_TYPES, AgentStreamEventSchema } from "./agent.js";
import { EVENT_SCHEMAS } from "./ipc.js";

/** Channels the UI can subscribe to: declared *and* gated. */
const SUBSCRIBABLE = [
  "agent.started",
  "agent.thinking",
  "agent.tool_call",
  "agent.tool_result",
  "agent.waiting_approval",
  "agent.approval_expired",
  "agent.completed",
  "agent.failed",
  "terminal.data",
  "terminal.closed",
  "activity.created",
] as const;

/**
 * Declared and emitted by Rust, but with no UI subscriber. Listed so the gap is a
 * recorded state rather than an accident; a new entry here should be a decision.
 */
const DECLARED_WITHOUT_A_SUBSCRIBER = [
  "server.connected",
  "server.disconnected",
  "server.updated",
  "terminal.opened",
] as const;

/**
 * Declared, allow-listed by Rust, schema'd — and emitted by nobody.
 *
 * Kept in the list rather than deleted because removing it changes the agent
 * protocol contract, which is a feature decision. Flagged here so it stops looking
 * like a working channel.
 */
const DECLARED_WITHOUT_A_PRODUCER = ["agent.text"] as const;

test("the subscribable set is exactly the declared names that have a gate", () => {
  const gated = Object.keys(EVENT_SCHEMAS).sort();
  assert.deepEqual(gated, [...SUBSCRIBABLE].sort());
});

test("the three event sets partition EVENT_NAMES exactly", () => {
  // No name may fall outside the recorded buckets, and none may be in two: an
  // overlap would mean one of the lists above is lying about the channel's state.
  const recorded = [
    ...SUBSCRIBABLE,
    ...DECLARED_WITHOUT_A_SUBSCRIBER,
    ...DECLARED_WITHOUT_A_PRODUCER,
  ];
  assert.equal(new Set(recorded).size, recorded.length, "a channel is in two buckets");
  assert.deepEqual(
    [...recorded].sort(),
    [...EVENT_NAMES].sort(),
    "EVENT_NAMES changed without recording the channel's state here",
  );
});

test("every declared name maps to a distinct Tauri channel", () => {
  // `tauriEventName` is a plain `.` -> `:` substitution, so two logical names that
  // differ only by separator would collide into one channel and cross-deliver.
  const channels = EVENT_NAMES.map(tauriEventName);
  assert.equal(new Set(channels).size, channels.length, "two event names share a channel");
  assert.equal(new Set(EVENT_NAMES).size, EVENT_NAMES.length, "EVENT_NAMES has a duplicate");
});

test("the agent channels the loop emits are all subscribable", () => {
  // The producer side, transcribed from `apps/agent/src/agent-loop.ts`. If the loop
  // gains an emit and this list is not updated, the UI silently cannot listen —
  // which is the agent.text failure mode, so it is asserted rather than assumed.
  const emitted = [
    "agent.started",
    "agent.thinking",
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
      `the agent loop emits ${name} but the UI cannot subscribe to it`,
    );
  }
  // Deliberately not emitted by the loop; listing it here would hide the phantom.
  assert.equal(
    (emitted as readonly string[]).includes("agent.text"),
    false,
    "agent.text is now emitted: wire it into SUBSCRIBABLE and delete DECLARED_WITHOUT_A_PRODUCER",
  );
});

/**
 * A payload that is genuinely valid — as `agent.started`.
 *
 * Used as the *wrong member* below: it satisfies the stream schema, so before the
 * channels were narrowed it passed every `agent.*` gate.
 */
const VALID_BUT_WRONG_MEMBER = { type: "agent.started", runId: "run_1", at: "2026-01-01T00:00:00Z" };

/**
 * The members that also have a channel — every member except `agent.text`.
 *
 * Kept as a separate list so the tests below index `EVENT_SCHEMAS`, which does not have
 * an `agent.text` entry; using the full member list to index it is a type error, and
 * that error is the correct signal.
 */
const AGENT_CHANNELS = AGENT_EVENT_TYPES.filter(
  (name): name is Extract<keyof typeof EVENT_SCHEMAS, `agent.${string}`> => name in EVENT_SCHEMAS,
);

test("every agent channel rejects a payload that is valid for a different channel", () => {
  // This is the hole the per-channel schemas exist to close. All eight channels used to
  // point at `AgentStreamEventSchema`, so the gate could only ask "is this *some* valid
  // agent event" — this payload answered yes to all eight, and `useAgentRun`'s
  // `as Extract<…>` cast then told the handler it held a `result` that was not there.
  //
  // The channel name *is* the payload's discriminator on the Rust side
  // (`commands/mod.rs` emits on `tauri_event_name(params.type)`), so a mismatched pair
  // cannot come off the transport — which is why rejecting it costs nothing real.
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

test("a member with no channel is rejected by every channel", () => {
  // `agent.text` is a real union member with a full schema and no channel. It must not
  // be acceptable anywhere, or dropping it from EVENT_SCHEMAS would be unobservable.
  const text = { type: "agent.text", runId: "run_1", textDelta: "hi", at: "2026-01-01T00:00:00Z" };
  assert.equal(AgentStreamEventSchema.safeParse(text).success, true, "agent.text is a valid stream member");
  for (const name of AGENT_CHANNELS) {
    assert.equal(EVENT_SCHEMAS[name].safeParse(text).success, false, `${name} accepted an agent.text payload`);
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
    AGENT_CHANNELS.length + 1,
    "the only member without a channel is expected to be agent.text",
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
  for (const status of ["pending", "running", "waiting_approval"]) {
    assert.equal(
      schema.safeParse({ ...base, status }).success,
      false,
      `a tool result must not claim the in-flight status ${status}`,
    );
  }
});
