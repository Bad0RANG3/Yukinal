import type { Environment } from "@yukinal/shared";

const STYLES: Record<Environment, string> = {
  production: "bg-red-500/15 text-red-300 ring-red-500/40",
  staging: "bg-amber-500/15 text-amber-300 ring-amber-500/30",
  development: "bg-emerald-500/15 text-emerald-300 ring-emerald-500/30",
  local: "bg-zinc-500/15 text-zinc-300 ring-zinc-500/30",
  unknown: "bg-zinc-500/15 text-zinc-300 ring-zinc-500/30",
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
      className={`inline-flex items-center gap-1 rounded px-2 py-0.5 text-xs font-medium uppercase tracking-wide ring-1 ${STYLES[environment]}`}
    >
      {environment === "production" ? "● production" : environment}
      <span className="text-zinc-400 normal-case">/ {serverName}</span>
      {region ? <span className="text-zinc-500 normal-case">/ {region}</span> : null}
    </span>
  );
}
