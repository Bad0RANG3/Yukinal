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
 * The gap is now only Rust-only events (server.updated / terminal.opened). Every
 * agent member has a producer, a channel, and a consumer; `agent.text` and
 * `agent.usage` complete the provider-to-UI path.
 *
 * These tests make the sets visible, so adding or removing a channel has to be
 * deliberate.
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
    "agent.text",
    "agent.usage",
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
