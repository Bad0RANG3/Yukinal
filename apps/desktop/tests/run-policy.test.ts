/**
 * 目标策略的选择表。
 *
 * 这些断言的理由和 `labels.test.ts` 里那些一样：类型能保证「少写一套策略」编译不过，
 * 但保证不了这张表**正好**覆盖内建策略、菜单里不多不少、以及「按环境自动」真的意味着
 * 不发送 `policyId`。这几件事都直接改变用户以为的审批边界，所以必须是断言。
 */

import assert from "node:assert/strict";
import { test } from "node:test";

import { BUILTIN_POLICIES, BUILTIN_POLICY_IDS } from "@yukinal/shared";

import {
  AUTO_RUN_POLICY,
  RUN_POLICY_ORDER,
  RUN_POLICY_SPECS,
  requestPolicyId,
  runPolicySpec,
  type RunPolicyChoice,
} from "../src/features/agent/run-policy.js";

test("the policy dictionary covers exactly the built-in policy ids", () => {
  assert.deepEqual(
    Object.keys(RUN_POLICY_SPECS).sort(),
    [...BUILTIN_POLICY_IDS].sort(),
    "RUN_POLICY_SPECS is out of step with the shared BUILTIN_POLICY_IDS",
  );
  // The ids are the ones the shared policy objects actually carry: a label keyed by a
  // typo would compile only if the tuple said so too.
  assert.deepEqual(
    Object.keys(RUN_POLICY_SPECS).sort(),
    BUILTIN_POLICIES.map((policy) => policy.id).sort(),
  );
});

test("every spec names the id it is filed under", () => {
  // The key is what the menu renders from; the `value` is what gets sent. If they ever
  // disagree the UI would send a different policy than the one it highlighted.
  for (const id of BUILTIN_POLICY_IDS) {
    assert.equal(RUN_POLICY_SPECS[id].value, id);
  }
  assert.equal(AUTO_RUN_POLICY.value, null);
});

test("the picker offers the automatic default plus every built-in policy, once", () => {
  assert.deepEqual([...RUN_POLICY_ORDER], [null, ...BUILTIN_POLICY_IDS]);
  assert.equal(new Set(RUN_POLICY_ORDER).size, RUN_POLICY_ORDER.length, "the picker lists a duplicate");
  for (const choice of RUN_POLICY_ORDER) {
    assert.equal(runPolicySpec(choice).value, choice);
  }
});

test("'按环境自动' is the default and sends no policyId", () => {
  // `null` is the only value that means "no override": the request then carries no
  // `policyId` at all, and the sidecar derives the policy from the target environment.
  // Any string here would be a policy the user never picked.
  assert.equal(AUTO_RUN_POLICY.value, null);
  assert.equal(AUTO_RUN_POLICY.label, "按环境自动");
  assert.equal(RUN_POLICY_ORDER[0], null);
});

test("the automatic choice leaves the field off the wire entirely", () => {
  assert.equal(requestPolicyId(null), undefined);
  for (const id of BUILTIN_POLICY_IDS) assert.equal(requestPolicyId(id), id);
  // Tauri serialises the args as JSON, so an `undefined` field really does leave the
  // request — which is what makes "按环境自动" the absence of an override rather than a
  // hidden default the UI chose on the user's behalf.
  assert.equal(
    JSON.stringify({ runId: "run_1", policyId: requestPolicyId(null) }),
    '{"runId":"run_1"}',
  );
  assert.equal(
    JSON.stringify({ runId: "run_1", policyId: requestPolicyId("policy.production") }),
    '{"runId":"run_1","policyId":"policy.production"}',
  );
});

test("every label and summary is present, unique and explains the policy", () => {
  const specs = RUN_POLICY_ORDER.map(runPolicySpec);
  const labels = specs.map((spec) => spec.label);
  const summaries = specs.map((spec) => spec.summary);
  assert.equal(new Set(labels).size, labels.length, "two policies share a label");
  assert.equal(new Set(summaries).size, summaries.length, "two policies share a summary");
  for (const spec of specs) {
    assert.ok(spec.label.length > 0, "a policy has an empty label");
    // 说清「对什么放行、对什么停下来」至少需要一句话，而不是一个词。
    assert.ok(spec.summary.length >= 12, `${spec.label} 的说明太短：${spec.summary}`);
  }
});

test("every documented choice round-trips through the spec lookup", () => {
  const choices: RunPolicyChoice[] = [null, ...BUILTIN_POLICY_IDS];
  for (const choice of choices) {
    const spec = runPolicySpec(choice);
    assert.equal(spec.value, choice);
    assert.equal(runPolicySpec(spec.value), spec);
  }
});
