/** Bounded package-manager operations exposed to the Agent host bridge. */

export const PACKAGE_MANAGERS = ["apt", "dnf"] as const;
export type PackageManager = (typeof PACKAGE_MANAGERS)[number];

export interface PackageInspectInput {
  manager: PackageManager;
  package: string;
}

export interface PackageInspectResult {
  manager: PackageManager;
  package: string;
  installed: boolean;
  version?: string;
}

export interface PackageInstallInput {
  manager: PackageManager;
  package: string;
  version?: string;
  timeoutSeconds?: number;
}

export interface PackageInstallResult {
  manager: PackageManager;
  package: string;
  version?: string;
  installed: boolean;
}
