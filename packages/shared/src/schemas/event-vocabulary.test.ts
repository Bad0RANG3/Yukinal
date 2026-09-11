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
