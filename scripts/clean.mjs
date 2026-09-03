#!/usr/bin/env node
/** Remove build output. Never touches databases or credential stores. */
import { glob } from "node:fs/promises";
import { rm } from "node:fs/promises";

const targets = ["packages/*/dist", "apps/*/dist", "apps/*/src-tauri/target", "target"];

for (const pattern of targets) {
  for await (const target of glob(pattern)) {
    await rm(target, { recursive: true, force: true });
    console.log(`removed ${target}`);
  }
}
