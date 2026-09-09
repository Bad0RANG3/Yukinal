/** Tauri can deliver started/completed before the start command resolves. */
export class RunLifecycle {
  runId: string | null = null;
  running = false;
  private requesting = false;
  private expectedRunId: string | null = null;

  begin(expectedRunId?: string): boolean {
    if (this.running || this.requesting) return false;
    this.requesting = true;
    this.running = true;
    this.runId = null;
    this.expectedRunId = expectedRunId ?? null;
    return true;
  }

  started(runId: string): boolean {
    if (!this.requesting || this.runId !== null || (this.expectedRunId !== null && this.expectedRunId !== runId)) return false;
    this.runId = runId;
    return true;
  }

  acknowledge(runId: string): boolean {
    if (this.expectedRunId !== null && this.expectedRunId !== runId) return false;
    const newlyStarted = this.running && this.runId === null;
    if (newlyStarted) this.runId = runId;
    this.requesting = false;
    return newlyStarted;
  }

  isActive(runId: string): boolean {
    return this.running && this.runId === runId;
  }

  finish(runId: string): boolean {
    if (!this.isActive(runId)) return false;
    this.running = false;
    return true;
  }

  fail(): void {
    this.requesting = false;
    this.running = false;
    this.expectedRunId = null;
  }
}
