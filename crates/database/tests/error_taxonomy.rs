//! Parity gate for the canonical error taxonomy.
//!
//! `packages/shared/fixtures/error-taxonomy.json` is the single source of truth
//! for which wire code maps to which actionable category and which codes are
//! retryable. `packages/shared/src/types/errors.test.ts` asserts the TypeScript
//! side against the same file; this test asserts the Rust enum. A category added
//! on one side only turns one of the two red.

use std::collections::BTreeSet;

use serde::Deserialize;
use yukinal_database::models::{ErrorCategory, TaskFailureCode};

const TAXONOMY: &str = include_str!("../../../packages/shared/fixtures/error-taxonomy.json");

#[derive(Deserialize)]
struct TaskFailureEntry {
    code: TaskFailureCode,
    category: ErrorCategory,
    retryable: bool,
}

#[derive(Deserialize)]
struct ToolErrorEntry {
    code: String,
    category: ErrorCategory,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Taxonomy {
    task_failures: Vec<TaskFailureEntry>,
    tool_errors: Vec<ToolErrorEntry>,
}

fn taxonomy() -> Taxonomy {
    serde_json::from_str(TAXONOMY).expect("the shared taxonomy fixture must parse")
}

#[test]
fn every_task_failure_code_has_exactly_one_taxonomy_entry() {
    let taxonomy = taxonomy();
    let fixture: BTreeSet<&str> = taxonomy
        .task_failures
        .iter()
        .map(|entry| entry.code.as_str())
        .collect();
    let rust: BTreeSet<&str> = TaskFailureCode::ALL
        .iter()
        .map(|code| code.as_str())
        .collect();
    assert_eq!(
        fixture, rust,
        "the fixture and TaskFailureCode::ALL must list the same codes"
    );
    assert_eq!(
        taxonomy.task_failures.len(),
        rust.len(),
        "a duplicate fixture entry would hide a missing code"
    );
}

#[test]
fn the_rust_mapping_matches_the_canonical_fixture() {
    for entry in taxonomy().task_failures {
        assert_eq!(
            entry.code.category(),
            entry.category,
            "task failure {} mapped to the wrong category",
            entry.code.as_str()
        );
        assert_eq!(
            entry.code.retryable(),
            entry.retryable,
            "task failure {} has the wrong retryability",
            entry.code.as_str()
        );
    }
}

#[test]
fn every_category_in_the_vocabulary_is_produced_by_a_wire_code() {
    let taxonomy = taxonomy();
    let reachable: BTreeSet<ErrorCategory> = taxonomy
        .task_failures
        .iter()
        .map(|entry| entry.category)
        .chain(taxonomy.tool_errors.iter().map(|entry| entry.category))
        .collect();
    for category in ErrorCategory::ALL {
        assert!(
            reachable.contains(category),
            "category {} is never produced by a wire code",
            category.as_str()
        );
    }
}

#[test]
fn the_tool_error_mapping_covers_every_shared_tool_code() {
    // The Rust side turns a tool error into a task failure code first
    // (`task_failure_code_from_tool_error` in the desktop crate), then into a
    // category. Keeping the fixture's tool list and its categories here proves the
    // shared vocabulary stays complete even though the mapping function lives one
    // layer up.
    let taxonomy = taxonomy();
    assert!(
        taxonomy.tool_errors.len() >= 12,
        "the tool-error list must not shrink silently"
    );
    for entry in taxonomy.tool_errors {
        assert!(!entry.code.is_empty());
        // The category must be a value the Rust enum knows how to serialize.
        assert_eq!(
            ErrorCategory::from_db(entry.category.as_str()),
            Some(entry.category)
        );
    }
}
