/**
 * Shared process-recovery vocabulary.
 *
 * The sidecar and MCP supervisors both report bounded automatic restarts with the same
 * fields. Keeping the shape here means the settings UI cannot learn two subtly different
 * meanings from the word "restart".
 */
export interface RestartRecord {
  attempt: number;
  maxAttempts: number;
  exhausted: boolean;
  at: string;
}
