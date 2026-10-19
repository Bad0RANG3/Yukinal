import assert from "node:assert/strict";
import { test } from "node:test";

import {
  ASSISTANT_REPLY_NOT_SAVED,
  assistantReplyNotSaved,
} from "../src/features/agent/session-persistence.js";

test("a dropped assistant reply is reported instead of silently lost", () => {
  // The session disappeared between the run starting and the reply landing (deleted,
  // or a brand-new panel that never had a session). The UI must say so.
  assert.equal(assistantReplyNotSaved(false), ASSISTANT_REPLY_NOT_SAVED);
  assert.ok(ASSISTANT_REPLY_NOT_SAVED.includes("未能保存"));
  assert.ok(ASSISTANT_REPLY_NOT_SAVED.includes("当前动态"), "the text is still on screen, and the notice should say so");
});

test("a successful or non-tracked write produces no notice", () => {
  assert.equal(assistantReplyNotSaved(true), null);
  // Browser preview / callers that do not track persistence return nothing: not a failure.
  assert.equal(assistantReplyNotSaved(undefined), null);
  assert.equal(assistantReplyNotSaved(), null);
});
