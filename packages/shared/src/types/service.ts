/** Read-only service/container status discovered on a connected server. */

export const SERVICE_STATES = ["running", "stopped", "failed", "unknown"] as const;
export type ServiceState = (typeof SERVICE_STATES)[number];

export const SERVICE_SOURCES = ["systemd", "docker", "unavailable"] as const;
export type ServiceSource = (typeof SERVICE_SOURCES)[number];

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
