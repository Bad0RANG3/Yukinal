/**
 * Parity gate for the canonical error taxonomy.
 *
 * `packages/shared/fixtures/error-taxonomy.json` is the single source of truth for
 * which wire code maps to which actionable category, and whether an unchanged
 * retry is allowed. This test asserts the TypeScript side against it; the Rust
 * side (`crates/database/tests/error_taxonomy.rs`) asserts its own enum against
 * the same file. A category added on one side only turns one of the two red.
 */

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import {
  ERROR_CATEGORIES,
  errorCategoryFromTaskFailure,
  errorCategoryFromToolError,
  isRetryableTaskFailure,
  type ErrorCategory,
} from "./errors.js";
import { TASK_FAILURE_CODES, type TaskFailureCode } from "./investigation.js";
import { TOOL_ERROR_CODES } from "./tool.js";

interface TaskFailureEntry {
  code: TaskFailureCode;
  category: ErrorCategory;
  retryable: boolean;
}

interface ToolErrorEntry {
  code: (typeof TOOL_ERROR_CODES)[number];
  category: ErrorCategory;
}

interface Taxonomy {
  taskFailures: TaskFailureEntry[];
  toolErrors: ToolErrorEntry[];
}

const taxonomy = JSON.parse(
  readFileSync(new URL("../../fixtures/error-taxonomy.json", import.meta.url), "utf8"),
) as Taxonomy;

test("every task failure code has exactly one taxonomy entry", () => {
  assert.deepEqual(
    taxonomy.taskFailures.map((entry) => entry.code).sort(),
    [...TASK_FAILURE_CODES].sort(),
    "the fixture and the wire tuple must list the same codes",
  );
});

test("every tool error code has exactly one taxonomy entry", () => {
  assert.deepEqual(
    taxonomy.toolErrors.map((entry) => entry.code).sort(),
    [...TOOL_ERROR_CODES].sort(),
  );
});

test("the TypeScript mapping matches the canonical fixture", () => {
  for (const entry of taxonomy.taskFailures) {
    assert.equal(
      errorCategoryFromTaskFailure(entry.code),
      entry.category,
      `task failure ${entry.code} mapped to the wrong category`,
    );
    assert.equal(
      isRetryableTaskFailure(entry.code),
      entry.retryable,
      `task failure ${entry.code} has the wrong retryability`,
    );
  }
  for (const entry of taxonomy.toolErrors) {
    assert.equal(
      errorCategoryFromToolError(entry.code),
      entry.category,
      `tool error ${entry.code} mapped to the wrong category`,
    );
  }
});

test("every category in the vocabulary is reachable from a wire code", () => {
  const reachable = new Set<string>([
    ...taxonomy.taskFailures.map((entry) => entry.category),
    ...taxonomy.toolErrors.map((entry) => entry.category),
  ]);
  for (const category of ERROR_CATEGORIES) {
    assert.ok(reachable.has(category), `category ${category} is never produced`);
  }
});
