/** Structured, bounded Docker diagnostics returned by the host bridge. */

export interface DockerLogsResult {
  container: string;
  lines: string[];
  truncated: boolean;
}

export interface DockerInspectResult {
  id: string;
  name: string;
  image: string;
  state: string;
  status: string;
  restartCount: number;
  startedAt?: string;
  health?: string;
}

export interface DockerRestartInput {
  container: string;
  timeoutSeconds?: number;
}

export interface DockerRestartResult {
  container: string;
  restarted: boolean;
}
