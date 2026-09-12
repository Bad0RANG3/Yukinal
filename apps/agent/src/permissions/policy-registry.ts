/**
 * Policy registry — the one place that answers "which policy decides this run?".
 *
 * The four built-in policies live in `@yukinal/shared` next to their ids (one source of
 * truth for the policy tables themselves), while `defaultPolicyFor` is what maps a
 * target environment to a policy. This module deliberately does not repeat that
 * mapping: it only adds *lookup by id*, which is what a run request needs.
 *
 * Two properties are the whole point of the indirection:
 *
 *  - A caller that asked for a policy either gets that policy or an error. Falling back
 *    to the environment default would run the request under a different policy than the
 *    one it named — silently, and in whichever direction the environment happens to
 *    point. That is why `resolveRequestedPolicy` throws instead of returning `undefined`
 *    for an unknown id, and why "no policy asked for" is a *separate* answer (`undefined`)
 *    from "policy not found".
 *  - `KNOWN_POLICY_IDS` is derived from the same tuple `policyById` searches, so the ids
 *    advertised by `system.describe` and named in that error cannot drift from the ids
 *    that actually resolve.
 */

import {
  BUILTIN_POLICIES,
  BUILTIN_POLICY_IDS,
  RPC_ERROR,
  type PermissionPolicy,
} from "@yukinal/shared";

import { RpcFailure } from "../errors.js";

/** Registry order: the order `system.describe` and the policy pickers list them in. */
export const POLICIES_BY_ID: ReadonlyMap<string, PermissionPolicy> = new Map(
  BUILTIN_POLICIES.map((policy) => [policy.id, policy]),
);

/** The ids `policyById` can resolve; everything else is a contract violation. */
export const KNOWN_POLICY_IDS: readonly string[] = BUILTIN_POLICY_IDS;

/** `undefined` means "no such policy", never "use the default". */
export function policyById(policyId: string): PermissionPolicy | undefined {
  return POLICIES_BY_ID.get(policyId);
}

/**
 * The policy an explicit run request names, or `undefined` when the request named none
 * (the environment default then applies, decided by the Permission Engine, which owns
 * that mapping).
 *
 * An id outside the registry is `INVALID_PARAMS` rather than `INTERNAL_ERROR`: the
 * caller sent an id we never agreed to honour. The message names both the rejected id
 * and the known ones, because the useful report to a caller is "you asked for X, and
 * these are the ids that exist".
 */
export function resolveRequestedPolicy(policyId: string | undefined): PermissionPolicy | undefined {
  if (policyId === undefined) return undefined;
  const policy = policyById(policyId);
  if (!policy) {
    throw new RpcFailure(
      RPC_ERROR.INVALID_PARAMS,
      `unknown policyId "${policyId}"; known policies: ${KNOWN_POLICY_IDS.join(", ")}`,
    );
  }
  return policy;
}
