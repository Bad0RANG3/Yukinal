/**
 * Errors that are part of the runtime's vocabulary.
 *
 * `NotImplementedError` exists so the sidecar can answer honestly with
 * RPC_NOT_IMPLEMENTED instead of faking behaviour (no demo shortcuts that
 * bypass Permission / Credential / Tool abstractions).
 */

export class NotImplementedError extends Error {
  constructor(what: string, landsIn: string) {
    super(`${what} is not implemented yet — lands in ${landsIn}`);
    this.name = "NotImplementedError";
  }
}

/** An error that maps onto a JSON-RPC error frame with a specific code. */
export class RpcFailure extends Error {
  constructor(
    readonly code: number,
    message: string,
    readonly data?: unknown,
  ) {
    super(message);
    this.name = "RpcFailure";
  }
}
