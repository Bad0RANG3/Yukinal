import type { TransferSnapshot } from "@yukinal/shared";

/** Undefined keeps native progress indeterminate until the transfer knows its size. */
export function transferProgress(transfer: Pick<TransferSnapshot, "status" | "totalBytes" | "transferredBytes">): number | undefined {
  if (transfer.status === "completed") return 100;
  if (transfer.totalBytes === null || transfer.totalBytes <= 0) return undefined;
  return Math.max(0, Math.min(100, Math.round(transfer.transferredBytes * 100 / transfer.totalBytes)));
}
