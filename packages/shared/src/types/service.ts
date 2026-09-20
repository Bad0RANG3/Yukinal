/** Read-only service/container status discovered on a connected server. */

export const SERVICE_STATES = ["running", "stopped", "failed", "unknown"] as const;
export type ServiceState = (typeof SERVICE_STATES)[number];

export const SERVICE_SOURCES = ["systemd", "docker", "unavailable"] as const;
export type ServiceSource = (typeof SERVICE_SOURCES)[number];

/** Optional local filter for the bounded service/container inventory. */
export interface ServerServicesInput {
  /** Exact normalized service or container name. */
  name?: string;
  /** Keep only rows in this normalized state. */
  state?: ServiceState;
}

export interface ServerService {
  name: string;
  state: ServiceState;
  /** Source-native state, e.g. `active/running` or `Up 3 hours`. */
  status: string;
  /** systemd description or the Docker image name. */
  description?: string;
}

export interface ServerServicesResponse {
  source: ServiceSource;
  services: ServerService[];
  /** Present when the target has no supported service manager. */
  message?: string;
}

/** Bounded systemd unit reference accepted by the Agent service tools. */
export interface SystemdInspectInput {
  service: string;
}

export interface SystemdInspectResult {
  service: string;
  loadState: string;
  activeState: string;
  subState: string;
  description?: string;
}

export interface SystemdRestartInput {
  service: string;
  timeoutSeconds?: number;
}

export interface SystemdRestartResult {
  service: string;
  restarted: boolean;
}
