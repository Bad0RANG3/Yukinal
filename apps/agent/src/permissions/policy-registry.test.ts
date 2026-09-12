/**
 * The policy registry is what makes `policyId` in a run request real, so the two
 * properties a caller depends on are pinned here: the ids advertised resolve, and an
 * id that does not resolve is refused instead of being redirected to a default.
 */

import assert from "node:assert/strict";
import test from "node:test";

import {
  BUILTIN_POLICIES,
  BUILTIN_POLICY_IDS,
  ENVIRONMENTS,
  RPC_ERROR,
  defaultPolicyFor,
} from "@yukinal/shared";

import { RpcFailure } from "../errors.js";
import { KNOWN_POLICY_IDS, POLICIES_BY_ID, policyById, resolveRequestedPolicy } from "./policy-registry.js";

test("every advertised policy id resolves, and the registry holds no other id", () => {
  // The ids `system.describe` reports and the ids a run request may name are the same
  // set, derived from the same tuple — a policy that is advertised but unresolvable
  // would be a promise the run path cannot keep.
  assert.deepEqual([...KNOWN_POLICY_IDS], [...BUILTIN_POLICY_IDS]);
  assert.equal(POLICIES_BY_ID.size, BUILTIN_POLICIES.length);
  for (const id of KNOWN_POLICY_IDS) {
    assert.equal(policyById(id)?.id, id, `${id} is advertised but does not resolve`);
  }
});

test("every environment default is reachable by id", () => {
  // `defaultPolicyFor` is the only environment -> policy mapping; the registry must not
  // second-guess it. If an environment ever defaulted to a policy outside the registry,
  // a caller could not name the policy its run was actually decided under.
  for (const environment of ENVIRONMENTS) {
    const fallback = defaultPolicyFor(environment);
    assert.equal(
      policyById(fallback.id),
      fallback,
      `${environment} defaults to ${fallback.id}, which the registry does not know`,
    );
  }
});

test("an omitted policyId is 'no override', not 'unknown policy'", () => {
  assert.equal(resolveRequestedPolicy(undefined), undefined);
  assert.equal(resolveRequestedPolicy("policy.production")?.id, "policy.production");
});

test("an unknown policyId is refused and names the ids that exist", () => {
  // The alternative — falling back to the environment default — is the defect this
  // registry exists to remove: the run would be governed by a policy the caller never
  // asked for, and nothing in the response would say so.
  assert.throws(
    () => resolveRequestedPolicy("policy.terraform"),
    (error: unknown) => {
      assert.ok(error instanceof RpcFailure);
      assert.equal(error.code, RPC_ERROR.INVALID_PARAMS);
      assert.match(error.message, /policy\.terraform/);
      for (const id of KNOWN_POLICY_IDS) assert.match(error.message, new RegExp(id.replace(".", "\\.")));
      return true;
    },
  );
});
