import type { Environment } from "@yukinal/shared";

const ENV_LABEL: Record<Environment, string> = {
  production: "生产环境",
  staging: "预发布环境",
  development: "开发环境",
  local: "本地环境",
  unknown: "未知环境",
};

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
      title={`${ENV_LABEL[environment]} · ${serverName}${region ? ` · ${region}` : ""}`}
    >
      <span className="env-badge-dot" aria-hidden="true" />
      <span>{ENV_LABEL[environment]}</span>
      <span className="env-badge-divider" aria-hidden="true">·</span>
      <span className="env-badge-server">{serverName}</span>
      {region ? <><span className="env-badge-divider" aria-hidden="true">·</span><span className="env-badge-region">{region}</span></> : null}
    </span>
  );
}
