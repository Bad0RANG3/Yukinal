import assert from "node:assert/strict";
import test from "node:test";

import { EVENT_NAMES, eventChannelName } from "./index.js";

test("an event name is already the channel name the host subscribes to", () => {
  // The C++ host keys its subscriber map on the exact string, and `publish_event`
  // forwards the emitter's own string to `dispatchEvent`, so there is no separator
  // rewriting left. The assertion is that this stays a no-op: a `.`→`:` rewrite here
  // would rename every channel out from under the UI.
  for (const name of EVENT_NAMES) {
    assert.equal(eventChannelName(name), name);
  }
  assert.equal(eventChannelName("agent.started"), "agent.started");
});
