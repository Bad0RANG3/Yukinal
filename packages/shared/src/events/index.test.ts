import assert from "node:assert/strict";
import test from "node:test";

import { EVENT_NAMES, tauriEventName } from "./index.js";

test("Tauri event channels contain only supported characters", () => {
  for (const name of EVENT_NAMES) {
    const channel = tauriEventName(name);
    assert.match(channel, /^[A-Za-z0-9_\-/:]+$/);
    assert.equal(channel.includes("."), false);
  }
  assert.equal(tauriEventName("agent.started"), "agent:started");
});
