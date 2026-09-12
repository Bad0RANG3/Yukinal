/**
 * Host-key trust model (ADR 0012): pin, probe and forget, keyed by `host:port`.
 *
 * The shapes here are the IPC payloads of the four `server_host_key_*` commands. Two
 * things about them are load-bearing and must not be "simplified" away:
 *
 * - The probe's answer is **what the server claimed**, not a verification. It is called
 *   `presentedFingerprint` for that reason, and the UI must not label it 已验证 — a
 *   probe proves nothing until the user confirms it (ADR 0012 point 4).
 * - A mismatch always carries **both** fingerprints. "It used to be X, it is now Y" is
 *   the only information that lets a user tell a legitimate server key rotation from an
 *   interception, and neither is visible if only one side is reported (ADR 0012 point 3).
 */

/**
 * How a presented fingerprint relates to the pin.
 *
 * A tuple (not a bare union) because `schemas/ipc.ts` builds its `z.enum` from it — the
 * same pattern `types/enums.ts` and `types/risk.ts` use, so adding a state cannot leave
 * the runtime gate behind.
 */
export const HOST_KEY_COMPARISONS = ["unpinned", "matches", "mismatch"] as const;
export type HostKeyComparison = (typeof HOST_KEY_COMPARISONS)[number];

/** `server_host_key_status` — local only: no connection is made. */
export interface ServerHostKeyStatus {
  host: string;
  port: number;
  /** Whether this `host:port` currently has a pin. */
  pinned: boolean;
  /** The pinned fingerprint. Absent (not null) when `pinned` is false. */
  pinnedFingerprint?: string;
}

/** `server_host_key_probe` — one real connection; nothing is persisted. */
export interface ServerHostKeyProbeResult {
  host: string;
  port: number;
  /**
   * What the server presented during that handshake.
   *
   * Deliberately not named `verifiedFingerprint`: a probe answers "what does this server
   * claim to be", and the answer is untrustworthy until the user confirms it against a
   * fingerprint they got from somewhere else (ADR 0012 point 4).
   */
  presentedFingerprint: string;
  comparison: HostKeyComparison;
  /** The pin this was compared against, when there is one. */
  pinnedFingerprint?: string;
}

/** `server_host_key_trust` — pins the fingerprint the user confirmed. */
export interface ServerHostKeyTrustResult {
  host: string;
  port: number;
  /** The fingerprint now pinned (the one the user confirmed). */
  fingerprint: string;
  /**
   * True when this exact fingerprint was already pinned and nothing was written.
   *
   * What is *not* here is any way to accept a different fingerprint: that is refused,
   * with an error telling the user to forget the old pin first (ADR 0012 point 5).
   */
  alreadyPinned: boolean;
}

/** `server_host_key_forget` — removes the pin; the next connection is TOFU again. */
export interface ServerHostKeyForgetResult {
  host: string;
  port: number;
  /** False when there was no pin to remove (not an error, but not silent either). */
  forgotten: boolean;
}
