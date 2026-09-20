import assert from "node:assert/strict";
import test from "node:test";

import {
  PackageInstallInputSchema,
  PackageInstallResultSchema,
  PackageInspectInputSchema,
  PackageInspectResultSchema,
} from "./package.js";

test("package schemas keep manager, package and version bounded", () => {
  assert.deepEqual(
    PackageInspectInputSchema.parse({ manager: "apt", package: "nginx" }),
    { manager: "apt", package: "nginx" },
  );
  assert.deepEqual(
    PackageInstallInputSchema.parse({
      manager: "dnf",
      package: "nginx",
      version: "1.27.0-1",
      timeoutSeconds: 120,
    }),
    { manager: "dnf", package: "nginx", version: "1.27.0-1", timeoutSeconds: 120 },
  );
  assert.throws(() => PackageInspectInputSchema.parse({ manager: "apk", package: "nginx" }));
  assert.throws(() => PackageInstallInputSchema.parse({ manager: "apt", package: "nginx;rm" }));
  assert.throws(() => PackageInstallInputSchema.parse({ manager: "apt", package: "nginx", timeoutSeconds: 601 }));
});

test("package result schemas distinguish an absent package from an installed version", () => {
  assert.equal(
    PackageInspectResultSchema.parse({ manager: "apt", package: "nginx", installed: false }).installed,
    false,
  );
  assert.equal(
    PackageInstallResultSchema.parse({ manager: "dnf", package: "nginx", installed: true, version: "1.27" }).installed,
    true,
  );
  assert.throws(() => PackageInspectResultSchema.parse({ manager: "apt", package: "nginx", installed: true, extra: true }));
});
