"""Derive the operations stage from what the run actually did.

The stage is an observation, not a gate. It exists so the operator can see whether
the agent is still collecting evidence, is diagnosing, or has already changed the
target — without trusting the model's own narration. Because every transition is
computed from tool execution (name, risk and outcome), a model cannot claim a
stage it did not reach, and an ``agent.stage`` event never carries private text.
"""
from __future__ import annotations

# Ordered for display and for the C++/UI mapping tables; the loop itself only
# uses the transitions below.
STAGES = ("intake", "evidence", "diagnosis", "plan", "change", "verify", "report")
STAGE_SET = frozenset(STAGES)

#: Outcome phases that mean "the operation may have changed the target".
_CHANGE_PHASES = frozenset({"outcome_unknown"})

#: Effectful failures where the host refused or never dispatched the call. Any other
#: failure of an effectful call may still have reached the target.
_NO_EFFECT = frozenset({
    "approval_required", "approval_rejected", "approval_timeout", "tool_not_allowed",
    "target_not_allowed", "target_required", "risk_not_allowed", "authorization_failed",
    "permission_denied", "invalid_arguments", "unknown_tool", "tool_repetition_detected",
    "plan_deviation", "durable_plan_required", "read_only_verification_required",
})


def derive_stage(
    current: str | None,
    *,
    tool: str,
    risk: str,
    succeeded: bool,
    phase: str | None = None,
    error_code: str | None = None,
) -> str:
    """Return the stage after one tool result.

    ``current`` is where the run already is (``None`` before anything happened).
    ``risk`` is the registry's risk level, so a structured read can never be
    mistaken for a change. Denied or invalid calls change nothing: they did not
    touch the target, and they are not evidence about the target either.
    """
    stage = current if current in STAGE_SET else "intake"
    if tool == "investigation.plan" and succeeded:
        return "plan"
    if risk != "read":
        if succeeded or phase in _CHANGE_PHASES or error_code not in _NO_EFFECT:
            # An effectful call that failed for any reason other than the host
            # refusing it may still have reached the target, so the run is no
            # longer "only diagnosing" — the next step must verify real state.
            return "change"
        return stage
    if succeeded:
        if stage in {"change", "verify"}:
            # A successful read after a change is the verification itself; further
            # successful reads keep the run in that stage until it changes again.
            return "verify"
        if stage in {"intake", "evidence"}:
            return "evidence"
        # A recorded plan or an ongoing diagnosis is not replaced by another read.
        return stage
    if error_code in {"approval_rejected", "approval_timeout", "tool_not_allowed",
                      "target_not_allowed", "target_required", "risk_not_allowed",
                      "authorization_failed", "permission_denied", "invalid_arguments",
                      "unknown_tool", "tool_repetition_detected"}:
        return stage
    # A read that failed is evidence about the environment: that is diagnosing.
    return "diagnosis"


def stage_for_start(current: str | None) -> str:
    return current if current in STAGE_SET else "intake"


def derive_report_stage(current: str | None) -> str:
    """The stage a run enters when it produces its final answer.

    ``report`` used to be declared in ``STAGES`` and never produced: the desktop
    inferred it locally on a completed run, so the two sides disagreed about what
    the runtime can say. Deriving it from the actual answer keeps the declared
    vocabulary and the wire honest — the UI still falls back to the terminal state
    for a sidecar that predates this.
    """
    return "report" if current != "report" else "report"
