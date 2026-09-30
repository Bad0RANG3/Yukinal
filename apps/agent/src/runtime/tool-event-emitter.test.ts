import assert from "node:assert/strict";
import test from "node:test";

import { serverExecInterruptionState } from "./tool-event-emitter.js";

test("the stream gets only a validated interruption state for the built-in command tool", () => {
  assert.equal(
    serverExecInterruptionState("server.exec", {
      state: "result_unknown",
      exitCode: null,
      stdout: "remote output",
      stderr: "",
      stdoutTruncated: false,
      stderrTruncated: false,
      durationMs: 10,
    }),
    "result_unknown",
  );
  assert.equal(serverExecInterruptionState("mcp.command", { state: "result_unknown" }), undefined);
  assert.equal(serverExecInterruptionState("server.exec", { state: "completed" }), undefined);
  assert.equal(serverExecInterruptionState("server.exec", undefined), undefined);
});
