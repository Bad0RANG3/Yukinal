import { z } from "zod";

import { PACKAGE_MANAGERS } from "../types/package.js";

export const PackageManagerSchema = z.enum(PACKAGE_MANAGERS);

const PackageRefSchema = z
  .string()
  .regex(/^[A-Za-z0-9][A-Za-z0-9+._:-]{0,127}$/, "invalid package reference");

const PackageVersionSchema = z
  .string()
  .regex(/^[A-Za-z0-9][A-Za-z0-9+._:~^-]{0,127}$/, "invalid package version");

export const PackageInspectInputSchema = z.strictObject({
  manager: PackageManagerSchema,
  package: PackageRefSchema,
});

export const PackageInspectResultSchema = z.strictObject({
  manager: PackageManagerSchema,
  package: PackageRefSchema,
  installed: z.boolean(),
  version: z.string().min(1).max(128).optional(),
});

export const PackageInstallInputSchema = z.strictObject({
  manager: PackageManagerSchema,
  package: PackageRefSchema,
  version: PackageVersionSchema.optional(),
  timeoutSeconds: z.number().int().min(1).max(600).optional(),
});

export const PackageInstallResultSchema = z.strictObject({
  manager: PackageManagerSchema,
  package: PackageRefSchema,
  version: z.string().min(1).max(128).optional(),
  installed: z.boolean(),
});
