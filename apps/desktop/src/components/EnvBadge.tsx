import type { Environment } from "@yukinal/shared";

import { environmentLabel } from "../lib/labels.js";

/**
 * Environment identity: always visible, always explicit.
 * "PRODUCTION / API / Singapore" is a safety feature, not decoration.
 */
export function EnvBadge({
  environment,
  serverName,
  region,
}: {
  environment: Environment;
  serverName: string;
  region?: string;
}) {
  return (
    <span
      className={`env-badge env-badge-${environment}`}
      title={`${environmentLabel(environment)} · ${serverName}${region ? ` · ${region}` : ""}`}
    >
      <span className="env-badge-dot" aria-hidden="true" />
      <span>{environmentLabel(environment)}</span>
      <span className="env-badge-divider" aria-hidden="true">·</span>
      <span className="env-badge-server">{serverName}</span>
      {region ? <><span className="env-badge-divider" aria-hidden="true">·</span><span className="env-badge-region">{region}</span></> : null}
    </span>
  );
}
