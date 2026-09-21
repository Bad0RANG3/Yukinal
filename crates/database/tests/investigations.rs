//! 调查任务/计划/证据/调度的持久化验收。
//!
//! 从 `persistence.rs` 拆出来：这一段是同一个 SQLite 领域里最大的一组验收，单独成篇后
//! `persistence.rs` 只保留服务器、身份、Provider、工具执行与 MCP 的行级往返。

mod common;
use common::*;

#[test]
fn investigation_objects_round_trip_and_task_status_updates() {
    let (path, db) = temp_db("investigation");
    let task = sample_task("task_1");
    db.investigations().create_task(&task).expect("create task");

    let content = json!({ "service": "api", "p95Ms": 1200 });
    db.investigations()
        .add_evidence(&Evidence {
            id: "ev_1".into(),
            task_id: task.id.clone(),
            run_id: None,
            scope: task.scope.clone(),
            kind: EvidenceKind::Log,
            source_tool: "server.logs".into(),
            collected_at: "2026-01-01T00:01:00.000Z".into(),
            input_summary: "last 100 lines".into(),
            content_type: EvidenceContentType::Json,
            content: content.clone(),
            content_hash: content_hash(&content),
            truncated: false,
            redaction_status: EvidenceRedactionStatus::Clean,
        })
        .expect("insert evidence");

    db.investigations()
        .add_finding(&Finding {
            id: "finding_1".into(),
            task_id: task.id.clone(),
            title: "Latency is elevated".into(),
            kind: FindingKind::Fact,
            statement: "The API p95 is 1200 ms in the collected sample".into(),
            evidence_ids: vec!["ev_1".into()],
            confidence: FindingConfidence::High,
            next_verification: Some("Compare a second sample".into()),
            created_at: "2026-01-01T00:01:01.000Z".into(),
        })
        .expect("insert finding");

    db.investigations()
        .save_decision_brief(&DecisionBrief {
            id: "brief_1".into(),
            task_id: task.id.clone(),
            plan_id: Some("plan_brief_1".into()),
            generated_at: "2026-01-01T00:02:00.000Z".into(),
            status: DecisionBriefStatus::Presented,
            finding_ids: vec!["finding_1".into()],
            options: vec![DecisionOption {
                id: "option_1".into(),
                title: "Collect another sample".into(),
                summary: "Stay read-only and compare the next window".into(),
                impact: "No remote state change".into(),
                risk_level: RiskLevel::Read,
                evidence_ids: vec!["ev_1".into()],
                finding_ids: vec!["finding_1".into()],
                preview: None,
                verification: "p95 should be comparable".into(),
                rollback: None,
                requires_approval: false,
                status: DecisionOptionStatus::Available,
                continuation: None,
            }],
            selected_option_id: None,
        })
        .expect("insert decision brief");

    let updated = db
        .investigations()
        .update_task_status(
            &task.id,
            TaskStatus::WaitingUser,
            "2026-01-01T00:03:00.000Z",
            None,
        )
        .expect("update task");
    assert_eq!(updated.status, TaskStatus::WaitingUser);
    assert_eq!(
        db.investigations()
            .list_evidence(&task.id, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        db.investigations()
            .list_findings(&task.id, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        db.investigations()
            .latest_decision_brief(&task.id)
            .unwrap()
            .unwrap()
            .options[0]
            .id,
        "option_1"
    );
    let selected = db
        .investigations()
        .select_decision_brief_option(&task.id, "brief_1", "option_1")
        .expect("select decision option");
    assert_eq!(selected.status, DecisionBriefStatus::Selected);
    assert_eq!(selected.selected_option_id.as_deref(), Some("option_1"));
    assert_eq!(selected.options[0].status, DecisionOptionStatus::Selected);
    assert_eq!(selected.plan_id.as_deref(), Some("plan_brief_1"));

    drop(db);
    let reopened = Database::open(&path).expect("reopen database");
    assert_eq!(
        reopened.investigations().get_task(&task.id).unwrap().status,
        TaskStatus::WaitingUser
    );
    assert_eq!(
        reopened
            .investigations()
            .list_evidence(&task.id, 10)
            .unwrap()[0]
            .content,
        content
    );
    drop(reopened);
    cleanup(&path);
}

#[test]
fn investigation_evidence_dedupes_only_within_one_run() {
    let (path, db) = temp_db("investigation-evidence-dedupe");
    let task = sample_task("task_evidence_dedupe");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");

    let run_id = "run_evidence_dedupe";
    repo.create_run(&InvestigationRun {
        id: run_id.into(),
        task_id: task.id.clone(),
        session_id: None,
        message_id: None,
        trace_id: None,
        attempt: 1,
        phase: TaskPhase::Investigating,
        status: InvestigationRunStatus::Running,
        started_at: "2026-01-01T00:00:00.000Z".into(),
        updated_at: "2026-01-01T00:00:00.000Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    })
    .expect("create run");
    let content = json!({ "service": "api", "status": "healthy" });
    let evidence = Evidence {
        id: "ev_evidence_dedupe".into(),
        task_id: task.id.clone(),
        run_id: Some(run_id.into()),
        scope: task.scope.clone(),
        kind: EvidenceKind::Service,
        source_tool: "server.services".into(),
        collected_at: "2026-01-01T00:01:00.000Z".into(),
        input_summary: "api service status".into(),
        content_type: EvidenceContentType::Json,
        content: content.clone(),
        content_hash: content_hash(&content),
        truncated: false,
        redaction_status: EvidenceRedactionStatus::Clean,
    };
    repo.add_evidence(&evidence).expect("insert evidence");

    let reused = repo
        .find_evidence_in_run(&evidence)
        .expect("find identical evidence")
        .expect("existing evidence");
    assert_eq!(reused.id, evidence.id);

    // A later scheduled sample is a separate observation even when its content
    // happens to be byte-for-byte identical.
    let mut later_run = evidence.clone();
    later_run.run_id = Some("run_evidence_dedupe_later".into());
    assert!(repo
        .find_evidence_in_run(&later_run)
        .expect("query other run")
        .is_none());
    let mut different_input = evidence.clone();
    different_input.input_summary = "different input".into();
    assert!(repo
        .find_evidence_in_run(&different_input)
        .expect("query different input")
        .is_none());

    drop(db);
    cleanup(&path);
}

#[test]
fn evidence_search_filters_by_source_kind_time_and_exact_scope() {
    let (_path, db) = temp_db("investigation-evidence-search");
    let task = sample_task("task_evidence_search");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");
    for (id, kind, source_tool, collected_at, content) in [
        (
            "ev_search_latest",
            EvidenceKind::Log,
            "docker.logs",
            "2026-01-01T00:03:00.000Z",
            json!({ "line": "timeout" }),
        ),
        (
            "ev_search_old",
            EvidenceKind::Log,
            "docker.logs",
            "2026-01-01T00:01:00.000Z",
            json!({ "line": "ok" }),
        ),
        (
            "ev_search_other",
            EvidenceKind::Container,
            "docker.ps",
            "2026-01-01T00:03:30.000Z",
            json!({ "name": "api" }),
        ),
    ] {
        repo.add_evidence(&Evidence {
            id: id.into(),
            task_id: task.id.clone(),
            run_id: None,
            scope: task.scope.clone(),
            kind,
            source_tool: source_tool.into(),
            collected_at: collected_at.into(),
            input_summary: "bounded fixture".into(),
            content_type: EvidenceContentType::Json,
            content: content.clone(),
            content_hash: content_hash(&content),
            truncated: false,
            redaction_status: EvidenceRedactionStatus::Clean,
        })
        .expect("insert evidence");
    }

    let results = repo
        .search_evidence(
            &task.id,
            &EvidenceSearchQuery {
                source_tool: Some("docker.logs".into()),
                kind: Some(EvidenceKind::Log),
                from: Some("2026-01-01T00:02:00.000Z".into()),
                to: Some("2026-01-01T00:03:00.000Z".into()),
                scope: Some(task.scope.clone()),
                limit: 8,
            },
        )
        .expect("search evidence");
    assert_eq!(
        results.into_iter().map(|item| item.id).collect::<Vec<_>>(),
        vec!["ev_search_latest"]
    );

    let wrong_scope = InvestigationTarget {
        host: InvestigationTargetHost::Local,
        server_id: None,
        workspace_id: None,
        environment: Environment::Local,
    };
    assert!(repo
        .search_evidence(
            &task.id,
            &EvidenceSearchQuery {
                scope: Some(wrong_scope),
                limit: 8,
                ..Default::default()
            },
        )
        .expect("wrong scope search")
        .is_empty());
}

#[test]
fn investigation_task_event_writes_are_fenced_by_the_active_run() {
    let (path, db) = temp_db("investigation-run-fence");
    let task = sample_task("task_run_fence");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");
    repo.update_task_progress(&TaskProgressUpdate {
        id: &task.id,
        status: TaskStatus::Investigating,
        phase: TaskPhase::Investigating,
        active_run_id: Some("run_new"),
        last_failure: None,
        updated_at: "2026-01-01T00:01:00.000Z",
        completed_at: None,
    })
    .expect("admit current run");

    let stale_status = repo
        .update_task_status_if_active(
            &task.id,
            TaskStatus::WaitingUser,
            "2026-01-01T00:02:00.000Z",
            None,
            "run_old",
        )
        .expect("stale status update should be handled");
    assert!(stale_status.is_none());
    assert_eq!(
        repo.get_task(&task.id).unwrap().status,
        TaskStatus::Investigating
    );

    let stale_finalisation = repo
        .update_task_progress_if_active(
            &TaskProgressUpdate {
                id: &task.id,
                status: TaskStatus::Failed,
                phase: TaskPhase::Recovery,
                active_run_id: None,
                last_failure: None,
                updated_at: "2026-01-01T00:03:00.000Z",
                completed_at: Some("2026-01-01T00:03:00.000Z"),
            },
            "run_old",
        )
        .expect("stale finalisation should be handled");
    assert!(stale_finalisation.is_none());
    assert_eq!(
        repo.get_task(&task.id).unwrap().active_run_id.as_deref(),
        Some("run_new")
    );

    let finalised = repo
        .update_task_progress_if_active(
            &TaskProgressUpdate {
                id: &task.id,
                status: TaskStatus::Failed,
                phase: TaskPhase::Recovery,
                active_run_id: None,
                last_failure: None,
                updated_at: "2026-01-01T00:04:00.000Z",
                completed_at: Some("2026-01-01T00:04:00.000Z"),
            },
            "run_new",
        )
        .expect("current finalisation");
    assert_eq!(finalised.unwrap().status, TaskStatus::Failed);
    assert!(repo.get_task(&task.id).unwrap().active_run_id.is_none());

    drop(db);
    cleanup(&path);
}

#[test]
fn investigation_retention_preview_and_prune_preserve_referenced_history() {
    let (path, db) = temp_db("investigation-retention");
    let task = sample_task("task_retention");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");
    repo.update_task_status(
        &task.id,
        TaskStatus::Completed,
        "2026-09-20T00:00:00Z",
        Some("2026-09-20T00:00:00Z"),
    )
    .expect("finish task");

    let keep_content = json!({ "state": "keep" });
    repo.add_evidence(&Evidence {
        id: "ev_keep".into(),
        task_id: task.id.clone(),
        run_id: None,
        scope: task.scope.clone(),
        kind: EvidenceKind::Service,
        source_tool: "server.services".into(),
        collected_at: "2026-01-01T00:00:00Z".into(),
        input_summary: "referenced sample".into(),
        content_type: EvidenceContentType::Json,
        content: keep_content.clone(),
        content_hash: content_hash(&keep_content),
        truncated: false,
        redaction_status: EvidenceRedactionStatus::Clean,
    })
    .expect("insert referenced evidence");
    let drop_content = json!({ "state": "drop" });
    repo.add_evidence(&Evidence {
        id: "ev_drop".into(),
        task_id: task.id.clone(),
        run_id: None,
        scope: task.scope.clone(),
        kind: EvidenceKind::Log,
        source_tool: "server.logs".into(),
        collected_at: "2026-01-02T00:00:00Z".into(),
        input_summary: "unreferenced sample".into(),
        content_type: EvidenceContentType::Json,
        content: drop_content.clone(),
        content_hash: content_hash(&drop_content),
        truncated: false,
        redaction_status: EvidenceRedactionStatus::Clean,
    })
    .expect("insert unreferenced evidence");
    let new_content = json!({ "state": "new" });
    repo.add_evidence(&Evidence {
        id: "ev_new".into(),
        task_id: task.id.clone(),
        run_id: None,
        scope: task.scope.clone(),
        kind: EvidenceKind::Snapshot,
        source_tool: "server.snapshot".into(),
        collected_at: "2026-09-19T00:00:00Z".into(),
        input_summary: "new sample".into(),
        content_type: EvidenceContentType::Json,
        content: new_content.clone(),
        content_hash: content_hash(&new_content),
        truncated: false,
        redaction_status: EvidenceRedactionStatus::Clean,
    })
    .expect("insert new evidence");
    repo.add_finding(&Finding {
        id: "finding_keep".into(),
        task_id: task.id.clone(),
        title: "Keep this history".into(),
        kind: FindingKind::Fact,
        statement: "The referenced sample remains useful audit history".into(),
        evidence_ids: vec!["ev_keep".into()],
        confidence: FindingConfidence::High,
        next_verification: None,
        created_at: "2026-01-03T00:00:00Z".into(),
    })
    .expect("insert finding");

    let old_artifact = InvestigationArtifact {
        id: "artifact_old".into(),
        task_id: task.id.clone(),
        run_id: None,
        plan_id: None,
        plan_step_id: None,
        phase: TaskPhase::Recovery,
        kind: TaskArtifactKind::Failure,
        status: TaskArtifactStatus::Superseded,
        title: "Superseded recovery note".into(),
        summary: "old note".into(),
        content: json!({ "old": true }),
        evidence_ids: vec![],
        created_at: "2026-01-04T00:00:00Z".into(),
        updated_at: "2026-01-04T00:00:00Z".into(),
    };
    repo.upsert_artifact(&old_artifact)
        .expect("insert old artifact");
    repo.upsert_artifact(&InvestigationArtifact {
        id: "artifact_ready".into(),
        status: TaskArtifactStatus::Ready,
        title: "Keep ready note".into(),
        summary: "not superseded".into(),
        content: json!({ "ready": true }),
        updated_at: "2026-01-04T00:00:00Z".into(),
        ..old_artifact.clone()
    })
    .expect("insert ready artifact");

    let preview = db
        .investigation_retention()
        .preview(&task.id, "2026-02-01T00:00:00Z", 16)
        .expect("preview retention");
    assert_eq!(preview.protected_count, 1);
    assert!(!preview.truncated);
    assert_eq!(
        preview
            .candidates
            .iter()
            .map(|item| (item.kind, item.id.as_str()))
            .collect::<Vec<_>>(),
        vec![
            (InvestigationRetentionKind::Evidence, "ev_drop"),
            (InvestigationRetentionKind::Artifact, "artifact_old"),
        ]
    );

    let result = db
        .investigation_retention()
        .prune(
            &task.id,
            "2026-02-01T00:00:00Z",
            &[
                InvestigationRetentionRequestItem {
                    id: "ev_drop",
                    kind: InvestigationRetentionKind::Evidence,
                },
                InvestigationRetentionRequestItem {
                    id: "ev_keep",
                    kind: InvestigationRetentionKind::Evidence,
                },
                InvestigationRetentionRequestItem {
                    id: "artifact_old",
                    kind: InvestigationRetentionKind::Artifact,
                },
                InvestigationRetentionRequestItem {
                    id: "artifact_ready",
                    kind: InvestigationRetentionKind::Artifact,
                },
            ],
            "retention_audit_1",
            "2026-09-20T00:01:00Z",
        )
        .expect("prune retention");
    assert_eq!(result.deleted.len(), 2);
    assert_eq!(result.skipped.len(), 2);
    assert!(matches!(
        result
            .skipped
            .iter()
            .find(|item| item.id == "ev_keep")
            .map(|item| item.reason.as_str()),
        Some("referenced_by_task_history")
    ));
    assert!(matches!(
        result
            .skipped
            .iter()
            .find(|item| item.id == "artifact_ready")
            .map(|item| item.reason.as_str()),
        Some("artifact_not_superseded")
    ));
    assert!(repo.get_evidence("ev_drop").is_err());
    assert!(repo.get_evidence("ev_keep").is_ok());
    assert!(repo.get_evidence("ev_new").is_ok());
    assert_eq!(
        repo.list_artifacts(&task.id, 10)
            .unwrap()
            .iter()
            .map(|artifact| artifact.id.as_str())
            .collect::<Vec<_>>(),
        vec!["artifact_ready"]
    );
    let audit = db.activities().list_recent(10).unwrap();
    assert_eq!(audit[0].id, "retention_audit_1");
    assert_eq!(audit[0].outcome, Some(ActivityOutcome::Success));

    drop(db);
    cleanup(&path);
}

#[test]
fn investigation_runs_steps_and_recovery_survive_restart_and_multiple_retry_cycles() {
    let (path, db) = temp_db("investigation-runs");
    let task = sample_task("task_runs");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");

    repo.create_run(&InvestigationRun {
        id: "run_1".into(),
        task_id: task.id.clone(),
        session_id: Some("ses_1".into()),
        message_id: Some("msg_1".into()),
        trace_id: None,
        attempt: 1,
        phase: TaskPhase::Investigating,
        status: InvestigationRunStatus::Admitted,
        started_at: "2026-01-01T00:01:00.000Z".into(),
        updated_at: "2026-01-01T00:01:00.000Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    })
    .expect("create run");
    repo.upsert_step(&InvestigationStep {
        id: "step_1".into(),
        task_id: task.id.clone(),
        run_id: "run_1".into(),
        ordinal: 1,
        kind: InvestigationStepKind::Evidence,
        title: "读取服务状态".into(),
        status: InvestigationStepStatus::Running,
        attempt: 1,
        tool_name: Some("server.snapshot".into()),
        plan_id: Some("plan_1".into()),
        plan_step_id: Some("plan_step_1".into()),
        target: Some(task.scope.clone()),
        input_summary: Some("只读快照".into()),
        output_summary: None,
        evidence_ids: vec![],
        started_at: Some("2026-01-01T00:01:01.000Z".into()),
        ended_at: None,
        failure: None,
    })
    .expect("create step");
    repo.upsert_step(&InvestigationStep {
        id: "step_2".into(),
        task_id: task.id.clone(),
        run_id: "run_1".into(),
        ordinal: 2,
        kind: InvestigationStepKind::Verification,
        title: "后续验证".into(),
        status: InvestigationStepStatus::Pending,
        attempt: 1,
        tool_name: Some("server.snapshot".into()),
        plan_id: Some("plan_1".into()),
        plan_step_id: Some("plan_step_2".into()),
        target: Some(task.scope.clone()),
        input_summary: Some("尚未开始".into()),
        output_summary: None,
        evidence_ids: vec![],
        started_at: None,
        ended_at: None,
        failure: None,
    })
    .expect("create pending step");
    let running = repo
        .update_task_progress(&TaskProgressUpdate {
            id: &task.id,
            status: TaskStatus::Investigating,
            phase: TaskPhase::Investigating,
            active_run_id: Some("run_1"),
            last_failure: None,
            updated_at: "2026-01-01T00:01:02.000Z",
            completed_at: None,
        })
        .expect("mark task active");
    assert_eq!(running.active_run_id.as_deref(), Some("run_1"));
    let persisted_steps = repo.list_steps(&task.id, 10).expect("list persisted steps");
    assert_eq!(persisted_steps[0].plan_id.as_deref(), Some("plan_1"));
    assert_eq!(
        persisted_steps[0].plan_step_id.as_deref(),
        Some("plan_step_1")
    );

    let failure = InvestigationFailure {
        code: TaskFailureCode::Transport,
        message: "sidecar disconnected".into(),
        retryable: true,
        attempt: 1,
        at: "2026-01-01T00:02:00.000Z".into(),
        detail: None,
        options: None,
    };
    let recovered = repo
        .recover_task(&task.id, "2026-01-01T00:02:00.000Z", &failure)
        .expect("recover task");
    assert_eq!(recovered.status, TaskStatus::Investigating);
    assert_eq!(recovered.phase, TaskPhase::Recovery);
    assert!(recovered.active_run_id.is_none());
    assert_eq!(
        recovered.last_failure.as_ref().unwrap().code,
        TaskFailureCode::Transport
    );
    assert_eq!(
        repo.get_run("run_1").unwrap().status,
        InvestigationRunStatus::Interrupted
    );
    let recovered_steps = repo.list_steps(&task.id, 10).unwrap();
    assert_eq!(recovered_steps.len(), 2);
    assert_eq!(recovered_steps[0].status, InvestigationStepStatus::Failed);
    assert_eq!(
        recovered_steps[0]
            .failure
            .as_ref()
            .expect("interrupted step failure")
            .code,
        TaskFailureCode::Transport
    );
    assert_eq!(
        recovered_steps[0].ended_at.as_deref(),
        Some("2026-01-01T00:02:00.000Z")
    );
    assert_eq!(recovered_steps[1].status, InvestigationStepStatus::Skipped);
    assert!(recovered_steps[1].failure.is_none());
    assert_eq!(
        recovered_steps[1].ended_at.as_deref(),
        Some("2026-01-01T00:02:00.000Z")
    );

    drop(db);
    let reopened = Database::open(&path).expect("reopen database");
    let reopened_task = reopened.investigations().get_task(&task.id).unwrap();
    assert_eq!(reopened_task.phase, TaskPhase::Recovery);
    assert_eq!(
        reopened.investigations().list_runs(&task.id, 10).unwrap()[0].status,
        InvestigationRunStatus::Interrupted
    );
    assert_eq!(
        reopened.investigations().list_steps(&task.id, 10).unwrap()[0].status,
        InvestigationStepStatus::Failed
    );
    assert_eq!(
        reopened.investigations().list_steps(&task.id, 10).unwrap()[1].status,
        InvestigationStepStatus::Skipped
    );

    // A recovery choice can start a new attempt after the application has been
    // reopened.  The old run stays interrupted, and a late finalisation from it
    // must not change the task now owned by the new run.
    reopened
        .investigations()
        .create_run(&InvestigationRun {
            id: "run_2".into(),
            task_id: task.id.clone(),
            session_id: Some("ses_2".into()),
            message_id: Some("msg_2".into()),
            trace_id: None,
            attempt: 2,
            phase: TaskPhase::Investigating,
            status: InvestigationRunStatus::Admitted,
            started_at: "2026-01-01T00:03:00.000Z".into(),
            updated_at: "2026-01-01T00:03:00.000Z".into(),
            ended_at: None,
            checkpoint: None,
            failure: None,
        })
        .expect("create retry run");
    reopened
        .investigations()
        .update_task_progress(&TaskProgressUpdate {
            id: &task.id,
            status: TaskStatus::Investigating,
            phase: TaskPhase::Investigating,
            active_run_id: Some("run_2"),
            last_failure: None,
            updated_at: "2026-01-01T00:03:01.000Z",
            completed_at: None,
        })
        .expect("admit retry run");
    assert_eq!(
        reopened
            .investigations()
            .get_task(&task.id)
            .unwrap()
            .active_run_id,
        Some("run_2".into())
    );
    let stale_from_first_retry = reopened
        .investigations()
        .update_task_progress_if_active(
            &TaskProgressUpdate {
                id: &task.id,
                status: TaskStatus::Failed,
                phase: TaskPhase::Recovery,
                active_run_id: None,
                last_failure: None,
                updated_at: "2026-01-01T00:03:02.000Z",
                completed_at: Some("2026-01-01T00:03:02.000Z"),
            },
            "run_1",
        )
        .expect("stale first run finalisation should be handled");
    assert!(stale_from_first_retry.is_none());
    assert_eq!(
        reopened
            .investigations()
            .get_task(&task.id)
            .unwrap()
            .active_run_id,
        Some("run_2".into())
    );

    // Exercise a second recovery cycle instead of assuming the first retry is
    // the end of the story.  This is the shape a transport failure followed by
    // a target/permission failure takes in a durable task timeline.
    let second_failure = InvestigationFailure {
        code: TaskFailureCode::Authentication,
        message: "target rejected the retry session".into(),
        retryable: false,
        attempt: 2,
        at: "2026-01-01T00:04:00.000Z".into(),
        detail: None,
        options: None,
    };
    let recovered_again = reopened
        .investigations()
        .recover_task(&task.id, "2026-01-01T00:04:00.000Z", &second_failure)
        .expect("recover retry run");
    assert_eq!(recovered_again.status, TaskStatus::Investigating);
    assert!(recovered_again.active_run_id.is_none());
    assert_eq!(
        reopened.investigations().get_run("run_1").unwrap().status,
        InvestigationRunStatus::Interrupted
    );
    assert_eq!(
        reopened.investigations().get_run("run_2").unwrap().status,
        InvestigationRunStatus::Interrupted
    );
    assert_eq!(
        reopened
            .investigations()
            .get_task(&task.id)
            .unwrap()
            .last_failure
            .as_ref()
            .unwrap()
            .code,
        TaskFailureCode::Authentication
    );

    reopened
        .investigations()
        .create_run(&InvestigationRun {
            id: "run_3".into(),
            task_id: task.id.clone(),
            session_id: Some("ses_3".into()),
            message_id: Some("msg_3".into()),
            trace_id: None,
            attempt: 3,
            phase: TaskPhase::Investigating,
            status: InvestigationRunStatus::Admitted,
            started_at: "2026-01-01T00:05:00.000Z".into(),
            updated_at: "2026-01-01T00:05:00.000Z".into(),
            ended_at: None,
            checkpoint: None,
            failure: None,
        })
        .expect("create final retry run");
    reopened
        .investigations()
        .update_task_progress(&TaskProgressUpdate {
            id: &task.id,
            status: TaskStatus::Investigating,
            phase: TaskPhase::Investigating,
            active_run_id: Some("run_3"),
            last_failure: None,
            updated_at: "2026-01-01T00:05:01.000Z",
            completed_at: None,
        })
        .expect("admit final retry run");
    let mut finished_run = reopened
        .investigations()
        .get_run("run_3")
        .expect("read final retry run");
    finished_run.status = InvestigationRunStatus::Completed;
    finished_run.updated_at = "2026-01-01T00:06:00.000Z".into();
    finished_run.ended_at = Some("2026-01-01T00:06:00.000Z".into());
    reopened
        .investigations()
        .update_run(&finished_run)
        .expect("finish final retry run");
    let stale_from_second_retry = reopened
        .investigations()
        .update_task_progress_if_active(
            &TaskProgressUpdate {
                id: &task.id,
                status: TaskStatus::Failed,
                phase: TaskPhase::Recovery,
                active_run_id: None,
                last_failure: None,
                updated_at: "2026-01-01T00:05:02.000Z",
                completed_at: Some("2026-01-01T00:05:02.000Z"),
            },
            "run_2",
        )
        .expect("stale second run finalisation should be handled");
    assert!(stale_from_second_retry.is_none());
    let completed = reopened
        .investigations()
        .update_task_progress_if_active(
            &TaskProgressUpdate {
                id: &task.id,
                status: TaskStatus::Completed,
                phase: TaskPhase::Completed,
                active_run_id: None,
                last_failure: None,
                updated_at: "2026-01-01T00:06:00.000Z",
                completed_at: Some("2026-01-01T00:06:00.000Z"),
            },
            "run_3",
        )
        .expect("current retry finalisation");
    assert_eq!(completed.unwrap().status, TaskStatus::Completed);
    assert!(reopened
        .investigations()
        .get_task(&task.id)
        .unwrap()
        .active_run_id
        .is_none());
    assert_eq!(
        reopened.investigations().get_run("run_3").unwrap().status,
        InvestigationRunStatus::Completed
    );
    drop(reopened);
    cleanup(&path);
}

#[test]
fn investigation_plan_revisions_are_persisted_and_supersede_old_work() {
    let (path, db) = temp_db("investigation-plans");
    let task = sample_task("task_plans");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");
    let first = InvestigationPlan {
        id: "plan_1".into(),
        task_id: task.id.clone(),
        revision: 1,
        status: yukinal_database::models::PlanStatus::Active,
        created_at: "2026-01-01T00:01:00.000Z".into(),
        updated_at: "2026-01-01T00:01:00.000Z".into(),
        current_step_id: Some("plan_step_1".into()),
        approval: Some(InvestigationPlanApproval {
            status: PlanApprovalStatus::Approved,
            source: Some(PlanApprovalSource::User),
            option_id: Some("option_1".into()),
            approved_at: Some("2026-01-01T00:01:30.000Z".into()),
            note: Some("fixture approval".into()),
        }),
        observation_window: None,
        steps: vec![InvestigationPlanStep {
            id: "plan_step_1".into(),
            ordinal: 0,
            kind: PlanStepKind::Evidence,
            title: "读取服务状态".into(),
            purpose: "建立当前服务基线".into(),
            allowed_tools: vec!["server.info".into()],
            input_bindings: None,
            idempotency: Some(yukinal_database::models::PlanIdempotency::Safe),
            risk_level: Some(RiskLevel::Read),
            requires_baseline: None,
            preconditions: Some(vec!["目标身份已校验".into()]),
            verification_criteria: Some(vec!["得到服务状态".into()]),
            preview: None,
            rollback: None,
            target: Some(task.scope.clone()),
            evidence_ids: vec![],
            success_criteria: vec!["得到服务状态".into()],
            requires_approval: false,
            max_attempts: 2,
            attempts: 0,
            status: PlanStepStatus::Running,
            started_at: Some("2026-01-01T00:01:00.000Z".into()),
            ended_at: None,
            last_deviation: None,
        }],
    };
    repo.save_plan(&first).expect("save first plan");
    assert_eq!(repo.latest_plan(&task.id).unwrap().unwrap().revision, 1);

    let mut second = first.clone();
    second.id = "plan_2".into();
    second.revision = 2;
    second.updated_at = "2026-01-01T00:02:00.000Z".into();
    second.steps[0].title = "读取并比较服务状态".into();
    repo.save_plan(&second).expect("save second plan");
    let active = repo.latest_plan(&task.id).unwrap().expect("active plan");
    assert_eq!(active.id, "plan_2");
    assert_eq!(active.revision, 2);
    assert_eq!(
        repo.get_plan("plan_1").unwrap().status,
        yukinal_database::models::PlanStatus::Superseded
    );

    drop(db);
    let reopened = Database::open(&path).expect("reopen database");
    assert_eq!(
        reopened
            .investigations()
            .latest_plan(&task.id)
            .unwrap()
            .unwrap()
            .id,
        "plan_2"
    );
    assert_eq!(
        reopened
            .investigations()
            .get_plan("plan_2")
            .unwrap()
            .approval
            .as_ref()
            .and_then(|approval| approval.option_id.as_deref()),
        Some("option_1")
    );
    drop(reopened);
    cleanup(&path);
}

#[test]
fn investigation_schedules_claim_once_and_recover_in_flight_runs() {
    let (path, db) = temp_db("investigation-schedules");
    let task = sample_task("task_schedule");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");
    let schedule = InvestigationSchedule {
        id: "schedule_1".into(),
        task_id: task.id.clone(),
        status: InvestigationScheduleStatus::Active,
        interval_seconds: 60,
        cooldown_seconds: 30,
        dedupe_window_seconds: 60,
        max_concurrent_runs: 1,
        budget: task.budget.clone(),
        notification_policy: InvestigationNotificationPolicy::OnChange,
        next_run_at: "2026-01-01T00:00:00Z".into(),
        baseline_run_id: None,
        last_run_at: None,
        last_outcome: None,
        last_error: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    };
    repo.create_schedule(&schedule).expect("create schedule");
    let (claimed, skipped) = repo
        .claim_due_schedule_runs("2026-01-01T00:00:10Z", 10)
        .expect("claim due");
    assert_eq!(claimed.len(), 1);
    assert!(skipped.is_empty());
    assert_eq!(claimed[0].status, InvestigationScheduleRunStatus::Claimed);
    repo.start_schedule_run(&claimed[0].id, "2026-01-01T00:00:11Z")
        .expect("start schedule run");
    repo.create_run(&InvestigationRun {
        id: claimed[0].id.clone(),
        task_id: task.id.clone(),
        session_id: Some("session_schedule_1".into()),
        message_id: Some("message_schedule_1".into()),
        trace_id: None,
        attempt: 1,
        phase: TaskPhase::Investigating,
        status: InvestigationRunStatus::Running,
        started_at: "2026-01-01T00:00:11Z".into(),
        updated_at: "2026-01-01T00:00:11Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    })
    .expect("persist schedule investigation run");
    repo.update_task_progress(&TaskProgressUpdate {
        id: &task.id,
        status: TaskStatus::Investigating,
        phase: TaskPhase::Investigating,
        active_run_id: Some(&claimed[0].id),
        last_failure: None,
        updated_at: "2026-01-01T00:00:11Z",
        completed_at: None,
    })
    .expect("bind schedule run to task");
    let (claimed_again, skipped_again) = repo
        .claim_due_schedule_runs("2026-01-01T00:00:10Z", 10)
        .expect("dedupe second tick");
    assert!(claimed_again.is_empty());
    assert!(skipped_again.is_empty());

    let interrupted = repo
        .interrupt_schedule_runs("2026-01-01T00:00:20Z")
        .expect("interrupt");
    assert_eq!(interrupted, 1);
    let run = repo
        .list_schedule_runs(&schedule.id, 10)
        .expect("list runs")
        .into_iter()
        .next()
        .expect("run");
    assert_eq!(run.status, InvestigationScheduleRunStatus::Interrupted);
    let interrupted_schedule = repo
        .get_schedule(&schedule.id)
        .expect("read interrupted schedule");
    assert_eq!(
        interrupted_schedule.last_outcome.as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        interrupted_schedule.last_error.as_deref(),
        Some("application_restarted")
    );
    let recovered_task = repo.get_task(&task.id).expect("read recovered task");
    assert_eq!(recovered_task.status, TaskStatus::Investigating);
    assert_eq!(recovered_task.phase, TaskPhase::Recovery);
    assert_eq!(recovered_task.active_run_id, None);
    assert_eq!(
        recovered_task
            .last_failure
            .as_ref()
            .map(|failure| failure.code),
        Some(TaskFailureCode::Transport)
    );
    assert_eq!(
        repo.get_run(&claimed[0].id)
            .expect("read interrupted run")
            .status,
        InvestigationRunStatus::Interrupted
    );

    let mut due_again = repo.get_schedule(&schedule.id).expect("get schedule");
    due_again.next_run_at = "2026-01-01T00:01:00Z".into();
    due_again.updated_at = "2026-01-01T00:00:21Z".into();
    repo.update_schedule(&due_again).expect("rewind schedule");
    let (claimed_after_restart, _) = repo
        .claim_due_schedule_runs("2026-01-01T00:01:30Z", 10)
        .expect("reclaim after restart");
    assert_eq!(claimed_after_restart.len(), 1);
    repo.finish_schedule_run(
        &claimed_after_restart[0].id,
        InvestigationScheduleRunStatus::Succeeded,
        Some("no_change"),
        None,
        "2026-01-01T00:00:31Z",
    )
    .expect("finish schedule run");
    assert_eq!(repo.list_schedule_runs(&schedule.id, 10).unwrap().len(), 2);

    // A claimed run is not a terminal outcome.  The previous comparison must
    // remain available to the sidecar prompt until this run finishes.
    let mut third_due = repo
        .get_schedule(&schedule.id)
        .expect("get finished schedule");
    assert_eq!(third_due.last_outcome.as_deref(), Some("no_change"));
    third_due.next_run_at = "2026-01-01T00:02:00Z".into();
    third_due.updated_at = "2026-01-01T00:01:31Z".into();
    repo.update_schedule(&third_due)
        .expect("rewind third schedule");
    let (third_claim, _) = repo
        .claim_due_schedule_runs("2026-01-01T00:02:01Z", 10)
        .expect("claim third schedule run");
    assert_eq!(third_claim.len(), 1);
    assert_eq!(
        repo.get_schedule(&schedule.id)
            .expect("read claimed schedule")
            .last_outcome
            .as_deref(),
        Some("no_change")
    );

    drop(db);
    let reopened = Database::open(&path).expect("reopen database");
    assert_eq!(
        reopened.investigations().list_schedules(10).unwrap().len(),
        1
    );
    cleanup(&path);
}

#[test]
fn active_run_recovery_closes_manual_and_scheduled_runs() {
    let (path, db) = temp_db("investigation-active-run-recovery");
    let repo = db.investigations();

    let manual_task = sample_task("task_manual_recovery");
    repo.create_task(&manual_task).expect("create manual task");
    repo.create_run(&InvestigationRun {
        id: "run_manual_recovery".into(),
        task_id: manual_task.id.clone(),
        session_id: Some("session_manual_recovery".into()),
        message_id: Some("message_manual_recovery".into()),
        trace_id: None,
        attempt: 1,
        phase: TaskPhase::Investigating,
        status: InvestigationRunStatus::Running,
        started_at: "2026-01-01T00:00:10Z".into(),
        updated_at: "2026-01-01T00:00:10Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    })
    .expect("create manual run");
    repo.update_task_progress(&TaskProgressUpdate {
        id: &manual_task.id,
        status: TaskStatus::Investigating,
        phase: TaskPhase::Investigating,
        active_run_id: Some("run_manual_recovery"),
        last_failure: None,
        updated_at: "2026-01-01T00:00:10Z",
        completed_at: None,
    })
    .expect("bind manual run");
    repo.upsert_step(&InvestigationStep {
        id: "step_manual_running".into(),
        task_id: manual_task.id.clone(),
        run_id: "run_manual_recovery".into(),
        ordinal: 1,
        kind: InvestigationStepKind::Evidence,
        title: "Collect evidence".into(),
        status: InvestigationStepStatus::Running,
        attempt: 1,
        tool_name: Some("server.snapshot".into()),
        plan_id: None,
        plan_step_id: None,
        target: Some(manual_task.scope.clone()),
        input_summary: None,
        output_summary: None,
        evidence_ids: vec![],
        started_at: Some("2026-01-01T00:00:11Z".into()),
        ended_at: None,
        failure: None,
    })
    .expect("create running step");
    repo.upsert_step(&InvestigationStep {
        id: "step_manual_pending".into(),
        task_id: manual_task.id.clone(),
        run_id: "run_manual_recovery".into(),
        ordinal: 2,
        kind: InvestigationStepKind::Verification,
        title: "Verify evidence".into(),
        status: InvestigationStepStatus::Pending,
        attempt: 1,
        tool_name: Some("server.snapshot".into()),
        plan_id: None,
        plan_step_id: None,
        target: Some(manual_task.scope.clone()),
        input_summary: None,
        output_summary: None,
        evidence_ids: vec![],
        started_at: None,
        ended_at: None,
        failure: None,
    })
    .expect("create pending step");

    let scheduled_task = sample_task("task_scheduled_recovery");
    repo.create_task(&scheduled_task)
        .expect("create scheduled task");
    let schedule = InvestigationSchedule {
        id: "schedule_recovery".into(),
        task_id: scheduled_task.id.clone(),
        status: InvestigationScheduleStatus::Active,
        interval_seconds: 60,
        cooldown_seconds: 0,
        dedupe_window_seconds: 60,
        max_concurrent_runs: 1,
        budget: scheduled_task.budget.clone(),
        notification_policy: InvestigationNotificationPolicy::OnChange,
        next_run_at: "2026-01-01T00:00:00Z".into(),
        baseline_run_id: None,
        last_run_at: None,
        last_outcome: None,
        last_error: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    };
    repo.create_schedule(&schedule).expect("create schedule");
    let (claimed, _) = repo
        .claim_due_schedule_runs("2026-01-01T00:00:01Z", 10)
        .expect("claim schedule");
    let schedule_run_id = claimed[0].id.clone();
    repo.start_schedule_run(&schedule_run_id, "2026-01-01T00:00:02Z")
        .expect("start schedule run");
    repo.create_run(&InvestigationRun {
        id: schedule_run_id.clone(),
        task_id: scheduled_task.id.clone(),
        session_id: Some(format!("session_{schedule_run_id}")),
        message_id: Some(format!("message_{schedule_run_id}")),
        trace_id: None,
        attempt: 1,
        phase: TaskPhase::Investigating,
        status: InvestigationRunStatus::Running,
        started_at: "2026-01-01T00:00:02Z".into(),
        updated_at: "2026-01-01T00:00:02Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    })
    .expect("create schedule investigation run");
    repo.update_task_progress(&TaskProgressUpdate {
        id: &scheduled_task.id,
        status: TaskStatus::Investigating,
        phase: TaskPhase::Investigating,
        active_run_id: Some(&schedule_run_id),
        last_failure: None,
        updated_at: "2026-01-01T00:00:02Z",
        completed_at: None,
    })
    .expect("bind schedule investigation run");

    let recovered = repo
        .interrupt_active_investigation_runs("2026-01-01T00:00:03Z", "sidecar_exited")
        .expect("recover active runs");
    assert_eq!(recovered, 2);

    let manual = repo.get_task(&manual_task.id).expect("read manual task");
    assert_eq!(manual.status, TaskStatus::Investigating);
    assert_eq!(manual.phase, TaskPhase::Recovery);
    assert!(manual.active_run_id.is_none());
    assert_eq!(
        manual.last_failure.as_ref().map(|failure| failure.code),
        Some(TaskFailureCode::Transport)
    );
    assert_eq!(
        manual
            .last_failure
            .as_ref()
            .and_then(|failure| failure.detail.as_ref())
            .and_then(|detail| detail.get("reason"))
            .and_then(serde_json::Value::as_str),
        Some("sidecar_exited")
    );
    let manual_run = repo
        .get_run("run_manual_recovery")
        .expect("read manual run");
    assert_eq!(manual_run.status, InvestigationRunStatus::Interrupted);
    assert_eq!(manual_run.ended_at.as_deref(), Some("2026-01-01T00:00:03Z"));
    let manual_steps = repo
        .list_steps(&manual_task.id, 10)
        .expect("list manual steps");
    assert_eq!(manual_steps.len(), 2);
    assert_eq!(manual_steps[0].status, InvestigationStepStatus::Failed);
    assert_eq!(manual_steps[1].status, InvestigationStepStatus::Skipped);

    let scheduled = repo
        .get_task(&scheduled_task.id)
        .expect("read scheduled task");
    assert_eq!(scheduled.status, TaskStatus::Investigating);
    assert_eq!(scheduled.phase, TaskPhase::Recovery);
    assert!(scheduled.active_run_id.is_none());
    assert_eq!(
        repo.get_run(&schedule_run_id)
            .expect("read schedule investigation run")
            .status,
        InvestigationRunStatus::Interrupted
    );
    let schedule_run = repo
        .list_schedule_runs(&schedule.id, 10)
        .expect("list schedule runs")
        .into_iter()
        .next()
        .expect("schedule run");
    assert_eq!(
        schedule_run.status,
        InvestigationScheduleRunStatus::Interrupted
    );
    assert_eq!(schedule_run.outcome.as_deref(), Some("interrupted"));
    assert_eq!(schedule_run.error.as_deref(), Some("sidecar_exited"));
    let recovered_schedule = repo.get_schedule(&schedule.id).expect("read schedule");
    assert_eq!(
        recovered_schedule.last_outcome.as_deref(),
        Some("interrupted")
    );
    assert_eq!(
        recovered_schedule.last_error.as_deref(),
        Some("sidecar_exited")
    );

    assert_eq!(
        repo.interrupt_active_investigation_runs("2026-01-01T00:00:04Z", "sidecar_exited",)
            .expect("repeat recovery"),
        0
    );

    drop(db);
    cleanup(&path);
}

#[test]
fn schedule_launch_failure_closes_run_task_steps_and_schedule_atomically() {
    let (path, db) = temp_db("investigation-schedule-launch-failure");
    let repo = db.investigations();
    let task = sample_task("task_schedule_launch_failure");
    repo.create_task(&task).expect("create task");
    repo.create_schedule(&InvestigationSchedule {
        id: "schedule_launch_failure".into(),
        task_id: task.id.clone(),
        status: InvestigationScheduleStatus::Active,
        interval_seconds: 60,
        cooldown_seconds: 0,
        dedupe_window_seconds: 60,
        max_concurrent_runs: 1,
        budget: task.budget.clone(),
        notification_policy: InvestigationNotificationPolicy::FailedRunsOnly,
        next_run_at: "2026-01-01T00:00:00Z".into(),
        baseline_run_id: None,
        last_run_at: None,
        last_outcome: None,
        last_error: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    })
    .expect("create schedule");
    let (claimed, _) = repo
        .claim_due_schedule_runs("2026-01-01T00:00:01Z", 10)
        .expect("claim schedule");
    let schedule_run_id = claimed[0].id.clone();
    repo.start_schedule_run(&schedule_run_id, "2026-01-01T00:00:02Z")
        .expect("start schedule run");
    repo.create_run(&InvestigationRun {
        id: schedule_run_id.clone(),
        task_id: task.id.clone(),
        session_id: Some(format!("session_{schedule_run_id}")),
        message_id: Some(format!("message_{schedule_run_id}")),
        trace_id: None,
        attempt: 1,
        phase: TaskPhase::Investigating,
        status: InvestigationRunStatus::Running,
        started_at: "2026-01-01T00:00:02Z".into(),
        updated_at: "2026-01-01T00:00:02Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    })
    .expect("create investigation run");
    repo.update_task_progress(&TaskProgressUpdate {
        id: &task.id,
        status: TaskStatus::Investigating,
        phase: TaskPhase::Investigating,
        active_run_id: Some(&schedule_run_id),
        last_failure: None,
        updated_at: "2026-01-01T00:00:02Z",
        completed_at: None,
    })
    .expect("bind investigation run");
    for (id, ordinal, status) in [
        (
            "step_schedule_launch_running",
            1,
            InvestigationStepStatus::Running,
        ),
        (
            "step_schedule_launch_pending",
            2,
            InvestigationStepStatus::Pending,
        ),
    ] {
        repo.upsert_step(&InvestigationStep {
            id: id.into(),
            task_id: task.id.clone(),
            run_id: schedule_run_id.clone(),
            ordinal,
            kind: InvestigationStepKind::Evidence,
            title: "Collect evidence".into(),
            status,
            attempt: 1,
            tool_name: Some("server.snapshot".into()),
            plan_id: None,
            plan_step_id: None,
            target: Some(task.scope.clone()),
            input_summary: None,
            output_summary: None,
            evidence_ids: vec![],
            started_at: Some("2026-01-01T00:00:02Z".into()),
            ended_at: None,
            failure: None,
        })
        .expect("create step");
    }

    let failed = repo
        .fail_schedule_run_launch(
            &task.id,
            &schedule_run_id,
            "sidecar did not start",
            TaskFailureCode::Transport,
            true,
            vec![],
            "transport_error",
            "2026-01-01T00:00:03Z",
        )
        .expect("fail scheduled launch");
    assert_eq!(failed.status, InvestigationScheduleRunStatus::Failed);
    assert_eq!(failed.outcome.as_deref(), Some("transport_error"));
    assert_eq!(failed.error.as_deref(), Some("sidecar did not start"));

    let run = repo
        .get_run(&schedule_run_id)
        .expect("read investigation run");
    assert_eq!(run.status, InvestigationRunStatus::Failed);
    assert_eq!(run.ended_at.as_deref(), Some("2026-01-01T00:00:03Z"));
    assert_eq!(
        run.failure.as_ref().map(|failure| failure.code),
        Some(TaskFailureCode::Transport)
    );
    let task = repo.get_task(&task.id).expect("read task");
    assert_eq!(task.status, TaskStatus::Failed);
    assert_eq!(task.phase, TaskPhase::Recovery);
    assert!(task.active_run_id.is_none());
    assert_eq!(task.completed_at.as_deref(), Some("2026-01-01T00:00:03Z"));
    let steps = repo.list_steps(&task.id, 10).expect("list steps");
    assert_eq!(steps[0].status, InvestigationStepStatus::Failed);
    assert_eq!(steps[1].status, InvestigationStepStatus::Skipped);
    let schedule_run = repo
        .list_schedule_runs("schedule_launch_failure", 10)
        .expect("list schedule runs")
        .into_iter()
        .next()
        .expect("schedule run");
    assert_eq!(schedule_run.status, InvestigationScheduleRunStatus::Failed);
    assert_eq!(schedule_run.error.as_deref(), Some("sidecar did not start"));
    let schedule = repo
        .get_schedule("schedule_launch_failure")
        .expect("read schedule");
    assert_eq!(schedule.last_outcome.as_deref(), Some("transport_error"));
    assert_eq!(
        schedule.last_error.as_deref(),
        Some("sidecar did not start")
    );

    let repeated = repo
        .fail_schedule_run_launch(
            &task.id,
            &schedule_run_id,
            "sidecar did not start",
            TaskFailureCode::Transport,
            true,
            vec![],
            "transport_error",
            "2026-01-01T00:00:04Z",
        )
        .expect("repeat launch failure");
    assert_eq!(repeated.status, InvestigationScheduleRunStatus::Failed);
    assert!(repo
        .get_task(&task.id)
        .expect("read repeated task")
        .active_run_id
        .is_none());

    drop(db);
    cleanup(&path);
}

#[test]
fn investigation_schedule_cooldown_skips_without_starving_a_short_interval() {
    let (path, db) = temp_db("investigation-schedule-cooldown");
    let task = sample_task("task_schedule_cooldown");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");
    let schedule = InvestigationSchedule {
        id: "schedule_cooldown".into(),
        task_id: task.id.clone(),
        status: InvestigationScheduleStatus::Active,
        interval_seconds: 10,
        cooldown_seconds: 30,
        dedupe_window_seconds: 10,
        max_concurrent_runs: 1,
        budget: task.budget.clone(),
        notification_policy: InvestigationNotificationPolicy::Silent,
        next_run_at: "2026-01-01T00:00:00Z".into(),
        baseline_run_id: None,
        last_run_at: None,
        last_outcome: None,
        last_error: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    };
    repo.create_schedule(&schedule).expect("create schedule");
    let (claimed, _) = repo
        .claim_due_schedule_runs("2026-01-01T00:00:01Z", 10)
        .expect("first claim");
    repo.finish_schedule_run(
        &claimed[0].id,
        InvestigationScheduleRunStatus::Succeeded,
        Some("no_change"),
        None,
        "2026-01-01T00:00:02Z",
    )
    .expect("finish first run");
    repo.update_task_progress(&TaskProgressUpdate {
        id: &task.id,
        status: TaskStatus::WaitingUser,
        phase: TaskPhase::Decision,
        active_run_id: None,
        last_failure: None,
        updated_at: "2026-01-01T00:00:02Z",
        completed_at: None,
    })
    .expect("move task to waiting user");

    for now in ["2026-01-01T00:00:12Z", "2026-01-01T00:00:23Z"] {
        let (claimed, skipped) = repo
            .claim_due_schedule_runs(now, 10)
            .expect("cooldown tick");
        assert!(claimed.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].outcome.as_deref(), Some("cooldown"));
        assert_eq!(
            repo.get_schedule(&schedule.id)
                .expect("get schedule")
                .last_run_at
                .as_deref(),
            Some("2026-01-01T00:00:01Z")
        );
    }
    let (claimed_after_cooldown, skipped) = repo
        .claim_due_schedule_runs("2026-01-01T00:00:34Z", 10)
        .expect("claim after cooldown");
    assert_eq!(claimed_after_cooldown.len(), 1);
    assert!(skipped.is_empty());

    drop(db);
    cleanup(&path);
}

#[test]
fn terminal_tasks_revoke_due_schedules_in_the_claim_transaction() {
    let (path, db) = temp_db("investigation-schedule-terminal");
    let mut task = sample_task("task_schedule_terminal");
    task.status = TaskStatus::Completed;
    task.phase = TaskPhase::Completed;
    task.completed_at = Some("2026-01-01T00:00:00Z".into());
    let repo = db.investigations();
    repo.create_task(&task).expect("create terminal task");
    repo.create_schedule(&InvestigationSchedule {
        id: "schedule_terminal".into(),
        task_id: task.id.clone(),
        status: InvestigationScheduleStatus::Active,
        interval_seconds: 60,
        cooldown_seconds: 0,
        dedupe_window_seconds: 60,
        max_concurrent_runs: 1,
        budget: task.budget.clone(),
        notification_policy: InvestigationNotificationPolicy::Silent,
        next_run_at: "2026-01-01T00:00:00Z".into(),
        baseline_run_id: None,
        last_run_at: None,
        last_outcome: None,
        last_error: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    })
    .expect("create schedule");

    let (claimed, skipped) = repo
        .claim_due_schedule_runs("2026-01-01T00:00:01Z", 10)
        .expect("claim terminal schedule");
    assert!(claimed.is_empty());
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].outcome.as_deref(), Some("task_terminal"));
    assert_eq!(
        repo.get_schedule("schedule_terminal")
            .expect("schedule")
            .status,
        InvestigationScheduleStatus::Revoked
    );
    // A repaired/legacy row can briefly be active again while its old terminal
    // dedupe record is still present. The existing-row branch must revoke it too.
    let mut repaired = repo
        .get_schedule("schedule_terminal")
        .expect("read repaired schedule");
    repaired.status = InvestigationScheduleStatus::Active;
    repaired.next_run_at = "2026-01-01T00:00:00Z".into();
    repaired.updated_at = "2026-01-01T00:00:02Z".into();
    repo.update_schedule(&repaired)
        .expect("repair schedule fixture");
    let (legacy_claimed, legacy_skipped) = repo
        .claim_due_schedule_runs("2026-01-01T00:00:03Z", 10)
        .expect("claim legacy terminal schedule");
    assert!(legacy_claimed.is_empty());
    assert_eq!(legacy_skipped.len(), 1);
    assert_eq!(
        repo.get_schedule("schedule_terminal")
            .expect("repaired schedule")
            .status,
        InvestigationScheduleStatus::Revoked
    );
    let (claimed_again, skipped_again) = repo
        .claim_due_schedule_runs("2026-01-01T00:01:01Z", 10)
        .expect("second claim");
    assert!(claimed_again.is_empty());
    assert!(skipped_again.is_empty());

    drop(db);
    cleanup(&path);
}

#[test]
fn scheduled_evidence_comparison_distinguishes_baseline_no_change_and_change() {
    let (path, db) = temp_db("investigation-schedule-comparison");
    let task = sample_task("task_schedule_comparison");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");
    repo.create_schedule(&InvestigationSchedule {
        id: "schedule_comparison".into(),
        task_id: task.id.clone(),
        status: InvestigationScheduleStatus::Active,
        interval_seconds: 60,
        cooldown_seconds: 0,
        dedupe_window_seconds: 60,
        max_concurrent_runs: 1,
        budget: task.budget.clone(),
        notification_policy: InvestigationNotificationPolicy::OnChange,
        next_run_at: "2026-01-01T00:00:00Z".into(),
        baseline_run_id: None,
        last_run_at: None,
        last_outcome: None,
        last_error: None,
        created_at: "2026-01-01T00:00:00Z".into(),
        updated_at: "2026-01-01T00:00:00Z".into(),
    })
    .expect("create schedule");

    let run_one = repo
        .claim_due_schedule_runs("2026-01-01T00:00:01Z", 10)
        .expect("claim first")
        .0
        .pop()
        .expect("first run");
    repo.start_schedule_run(&run_one.id, "2026-01-01T00:00:01Z")
        .expect("start first");
    repo.create_run(&InvestigationRun {
        id: run_one.id.clone(),
        task_id: task.id.clone(),
        session_id: None,
        message_id: None,
        trace_id: None,
        attempt: 1,
        phase: TaskPhase::Investigating,
        status: InvestigationRunStatus::Running,
        started_at: "2026-01-01T00:00:01Z".into(),
        updated_at: "2026-01-01T00:00:01Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    })
    .expect("create first run ledger");
    let first_content = json!({ "status": "healthy" });
    repo.add_evidence(&Evidence {
        id: "ev_schedule_one".into(),
        task_id: task.id.clone(),
        run_id: Some(run_one.id.clone()),
        scope: task.scope.clone(),
        kind: EvidenceKind::Service,
        source_tool: "server.services".into(),
        collected_at: "2026-01-01T00:00:02Z".into(),
        input_summary: "api".into(),
        content_type: EvidenceContentType::Json,
        content: first_content.clone(),
        content_hash: content_hash(&first_content),
        truncated: false,
        redaction_status: EvidenceRedactionStatus::Clean,
    })
    .expect("insert first evidence");
    let baseline = repo
        .compare_schedule_run(&run_one.id)
        .expect("baseline comparison");
    assert_eq!(
        baseline.status,
        InvestigationScheduleComparisonStatus::Baseline
    );
    repo.finish_schedule_run(
        &run_one.id,
        InvestigationScheduleRunStatus::Succeeded,
        Some("baseline"),
        None,
        "2026-01-01T00:00:03Z",
    )
    .expect("finish first");

    let mut schedule = repo
        .get_schedule("schedule_comparison")
        .expect("get schedule");
    schedule.next_run_at = "2026-01-01T00:01:00Z".into();
    schedule.updated_at = "2026-01-01T00:00:04Z".into();
    repo.update_schedule(&schedule).expect("rewind schedule");
    let run_two = repo
        .claim_due_schedule_runs("2026-01-01T00:01:01Z", 10)
        .expect("claim second")
        .0
        .pop()
        .expect("second run");
    repo.start_schedule_run(&run_two.id, "2026-01-01T00:01:01Z")
        .expect("start second");
    repo.create_run(&InvestigationRun {
        id: run_two.id.clone(),
        task_id: task.id.clone(),
        session_id: None,
        message_id: None,
        trace_id: None,
        attempt: 2,
        phase: TaskPhase::Investigating,
        status: InvestigationRunStatus::Running,
        started_at: "2026-01-01T00:01:01Z".into(),
        updated_at: "2026-01-01T00:01:01Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    })
    .expect("create second run ledger");
    repo.add_evidence(&Evidence {
        id: "ev_schedule_two".into(),
        task_id: task.id.clone(),
        run_id: Some(run_two.id.clone()),
        scope: task.scope.clone(),
        kind: EvidenceKind::Service,
        source_tool: "server.services".into(),
        collected_at: "2026-01-01T00:01:02Z".into(),
        input_summary: "api".into(),
        content_type: EvidenceContentType::Json,
        content: first_content.clone(),
        content_hash: content_hash(&first_content),
        truncated: false,
        redaction_status: EvidenceRedactionStatus::Clean,
    })
    .expect("insert second evidence");
    let no_change = repo
        .compare_schedule_run(&run_two.id)
        .expect("no-change comparison");
    assert_eq!(
        no_change.status,
        InvestigationScheduleComparisonStatus::NoChange
    );
    repo.finish_schedule_run(
        &run_two.id,
        InvestigationScheduleRunStatus::Succeeded,
        Some("no_change"),
        None,
        "2026-01-01T00:01:03Z",
    )
    .expect("finish second");

    let mut schedule = repo
        .get_schedule("schedule_comparison")
        .expect("get schedule");
    schedule.baseline_run_id = Some(run_one.id.clone());
    schedule.next_run_at = "2026-01-01T00:02:00Z".into();
    schedule.updated_at = "2026-01-01T00:01:04Z".into();
    repo.update_schedule(&schedule)
        .expect("rewind schedule again");
    let run_three = repo
        .claim_due_schedule_runs("2026-01-01T00:02:01Z", 10)
        .expect("claim third")
        .0
        .pop()
        .expect("third run");
    repo.start_schedule_run(&run_three.id, "2026-01-01T00:02:01Z")
        .expect("start third");
    repo.create_run(&InvestigationRun {
        id: run_three.id.clone(),
        task_id: task.id.clone(),
        session_id: None,
        message_id: None,
        trace_id: None,
        attempt: 3,
        phase: TaskPhase::Investigating,
        status: InvestigationRunStatus::Running,
        started_at: "2026-01-01T00:02:01Z".into(),
        updated_at: "2026-01-01T00:02:01Z".into(),
        ended_at: None,
        checkpoint: None,
        failure: None,
    })
    .expect("create third run ledger");
    let changed_content = json!({ "status": "degraded" });
    repo.add_evidence(&Evidence {
        id: "ev_schedule_three".into(),
        task_id: task.id,
        run_id: Some(run_three.id.clone()),
        scope: sample_task("scope_only").scope,
        kind: EvidenceKind::Service,
        source_tool: "server.services".into(),
        collected_at: "2026-01-01T00:02:02Z".into(),
        input_summary: "api".into(),
        content_type: EvidenceContentType::Json,
        content: changed_content.clone(),
        content_hash: content_hash(&changed_content),
        truncated: false,
        redaction_status: EvidenceRedactionStatus::Clean,
    })
    .expect("insert third evidence");
    let changed = repo
        .compare_schedule_run(&run_three.id)
        .expect("changed comparison");
    assert_eq!(
        changed.status,
        InvestigationScheduleComparisonStatus::Changed
    );
    assert_eq!(changed.previous_evidence_ids, vec!["ev_schedule_one"]);
    assert_eq!(changed.current_evidence_ids, vec!["ev_schedule_three"]);

    drop(db);
    cleanup(&path);
}

#[test]
fn investigation_phase_artifacts_round_trip_with_bounded_content() {
    let (path, db) = temp_db("investigation-artifacts");
    let task = sample_task("task_artifacts");
    let repo = db.investigations();
    repo.create_task(&task).expect("create task");
    repo.upsert_artifact(&InvestigationArtifact {
        id: "artifact_execution_1".into(),
        task_id: task.id.clone(),
        run_id: None,
        plan_id: None,
        plan_step_id: None,
        phase: TaskPhase::Execution,
        kind: TaskArtifactKind::Execution,
        status: TaskArtifactStatus::Succeeded,
        title: "受控执行结果".into(),
        summary: "本机 fixture 执行完成，未触碰远端网络".into(),
        content: json!({ "commands": ["echo fixture"], "changed": false }),
        evidence_ids: vec![],
        created_at: "2026-01-01T00:03:00.000Z".into(),
        updated_at: "2026-01-01T00:03:01.000Z".into(),
    })
    .expect("persist artifact");
    let artifacts = repo.list_artifacts(&task.id, 10).expect("list artifacts");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].kind, TaskArtifactKind::Execution);
    assert_eq!(artifacts[0].content["changed"], json!(false));
    drop(db);
    let reopened = Database::open(&path).expect("reopen database");
    assert_eq!(
        reopened
            .investigations()
            .list_artifacts(&task.id, 10)
            .unwrap()[0]
            .status,
        TaskArtifactStatus::Succeeded
    );
    drop(reopened);
    cleanup(&path);
}

#[test]
fn investigation_evidence_rejects_unredacted_mismatched_or_oversized_content() {
    let (path, db) = temp_db("investigation-validation");
    let task = sample_task("task_validation");
    db.investigations().create_task(&task).expect("create task");
    let content = json!({ "token": "should never be persisted" });
    let base = Evidence {
        id: "ev_validation".into(),
        task_id: task.id.clone(),
        run_id: None,
        scope: task.scope.clone(),
        kind: EvidenceKind::ToolResult,
        source_tool: "server.info".into(),
        collected_at: "2026-01-01T00:01:00.000Z".into(),
        input_summary: "info".into(),
        content_type: EvidenceContentType::Json,
        content: content.clone(),
        content_hash: content_hash(&content),
        truncated: false,
        redaction_status: EvidenceRedactionStatus::Unknown,
    };
    assert!(db.investigations().add_evidence(&base).is_err());

    let mut mismatched = base.clone();
    mismatched.redaction_status = EvidenceRedactionStatus::Redacted;
    mismatched.content_hash = "0".repeat(64);
    assert!(db.investigations().add_evidence(&mismatched).is_err());

    let oversized_content = json!("x".repeat(1_100_000));
    let oversized = Evidence {
        id: "ev_oversized".into(),
        content: oversized_content.clone(),
        content_hash: content_hash(&oversized_content),
        truncated: true,
        ..base
    };
    assert!(db.investigations().add_evidence(&oversized).is_err());
    drop(db);
    cleanup(&path);
}
