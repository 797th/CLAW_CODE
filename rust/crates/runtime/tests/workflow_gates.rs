use runtime::{
    ApiClient, ApiRequest, AssistantEvent, ContentBlock, ConversationRuntime, GateCheck,
    GateDecision, GateEvidence, MessageRole, PermissionMode, PermissionPolicy, RuntimeError,
    RuntimeFeatureConfig, Session, StaticToolExecutor, WorkflowGateMode, WorkflowPhase,
    WorkflowState,
};

fn started_state() -> WorkflowState {
    let mut state = WorkflowState::default();
    state.start("TASK-7");
    state
}

#[test]
fn try_advance_from_idle_is_blocked_until_a_task_is_started() {
    let mut state = WorkflowState::default();
    assert_eq!(state.phase, WorkflowPhase::Idle);

    match state.try_advance() {
        GateCheck::Blocked { reason } => {
            assert!(
                reason.to_lowercase().contains("start"),
                "reason should tell the caller to start a task first: {reason}"
            );
        }
        GateCheck::Pass => panic!("Idle should never auto-advance"),
    }
    assert_eq!(state.phase, WorkflowPhase::Idle);
}

#[test]
fn start_moves_idle_to_spec_with_task_ref() {
    let mut state = WorkflowState::default();
    state.start("TASK-7");
    assert_eq!(state.phase, WorkflowPhase::Spec);
    assert_eq!(state.task_ref.as_deref(), Some("TASK-7"));
}

#[test]
fn spec_to_implement_blocked_without_acceptance_criteria() {
    let mut state = started_state();

    match state.try_advance() {
        GateCheck::Blocked { reason } => {
            assert!(
                reason.to_lowercase().contains("ask"),
                "reason should instruct the model to ask the user for ACs, not invent them: {reason}"
            );
        }
        GateCheck::Pass => panic!("Spec -> Implement must be blocked without acceptance criteria"),
    }
    assert_eq!(state.phase, WorkflowPhase::Spec);
}

#[test]
fn spec_to_implement_passes_with_acceptance_criteria() {
    let mut state = started_state();
    state
        .acceptance_criteria
        .push("returns 200 on success".to_string());

    let check = state.try_advance();
    assert!(matches!(check, GateCheck::Pass));
    assert_eq!(state.phase, WorkflowPhase::Implement);
}

#[test]
fn spec_to_implement_blocked_with_only_whitespace_acceptance_criteria() {
    let mut state = started_state();
    state.acceptance_criteria.push(String::new());
    state.acceptance_criteria.push("   ".to_string());
    state.acceptance_criteria.push("\t\n".to_string());

    match state.try_advance() {
        GateCheck::Blocked { reason } => {
            assert!(
                reason.to_lowercase().contains("ask"),
                "whitespace-only ACs must not satisfy the gate: {reason}"
            );
        }
        GateCheck::Pass => panic!(
            "Spec -> Implement must be blocked when acceptance_criteria contains only whitespace"
        ),
    }
    assert_eq!(state.phase, WorkflowPhase::Spec);
}

#[test]
fn spec_to_implement_passes_with_one_real_ac_among_empties() {
    let mut state = started_state();
    state.acceptance_criteria.push(String::new());
    state.acceptance_criteria.push("   ".to_string());
    state
        .acceptance_criteria
        .push("returns 200 on success".to_string());

    let check = state.try_advance();
    assert!(matches!(check, GateCheck::Pass));
    assert_eq!(state.phase, WorkflowPhase::Implement);
}

#[test]
fn implement_to_verify_always_passes() {
    let mut state = started_state();
    state.acceptance_criteria.push("AC1".to_string());
    assert!(matches!(state.try_advance(), GateCheck::Pass)); // Spec -> Implement
    assert_eq!(state.phase, WorkflowPhase::Implement);

    let check = state.try_advance();
    assert!(matches!(check, GateCheck::Pass));
    assert_eq!(state.phase, WorkflowPhase::Verify);
}

#[test]
fn verify_to_review_blocked_without_passing_test_run_evidence() {
    let mut state = started_state();
    state.acceptance_criteria.push("AC1".to_string());
    state.try_advance(); // Spec -> Implement
    state.try_advance(); // Implement -> Verify
    assert_eq!(state.phase, WorkflowPhase::Verify);

    match state.try_advance() {
        GateCheck::Blocked { reason } => {
            assert!(
                reason.to_lowercase().contains("test"),
                "reason should mention missing test evidence: {reason}"
            );
        }
        GateCheck::Pass => panic!("Verify -> Review must be blocked without test_run evidence"),
    }
    assert_eq!(state.phase, WorkflowPhase::Verify);
}

#[test]
fn verify_to_review_passes_with_passing_test_run_evidence() {
    let mut state = started_state();
    state.acceptance_criteria.push("AC1".to_string());
    state.try_advance(); // Spec -> Implement
    state.try_advance(); // Implement -> Verify

    state.record_evidence(GateEvidence {
        gate: WorkflowPhase::Verify,
        kind: "test_run".to_string(),
        detail: "42 passed; 0 failed".to_string(),
    });

    let check = state.try_advance();
    assert!(matches!(check, GateCheck::Pass));
    assert_eq!(state.phase, WorkflowPhase::Review);
}

#[test]
fn review_to_done_blocked_without_review_evidence() {
    let mut state = started_state();
    state.acceptance_criteria.push("AC1".to_string());
    state.try_advance();
    state.try_advance();
    state.record_evidence(GateEvidence {
        gate: WorkflowPhase::Verify,
        kind: "test_run".to_string(),
        detail: "all green".to_string(),
    });
    state.try_advance(); // Verify -> Review
    assert_eq!(state.phase, WorkflowPhase::Review);

    match state.try_advance() {
        GateCheck::Blocked { reason } => {
            assert!(reason.to_lowercase().contains("review"));
        }
        GateCheck::Pass => panic!("Review -> Done must be blocked without review evidence"),
    }
    assert_eq!(state.phase, WorkflowPhase::Review);
}

#[test]
fn full_happy_path_spec_to_done() {
    let mut state = WorkflowState::default();
    state.start("TASK-7");
    state.acceptance_criteria.push("AC1".to_string());

    assert!(matches!(state.try_advance(), GateCheck::Pass)); // Spec -> Implement
    assert!(matches!(state.try_advance(), GateCheck::Pass)); // Implement -> Verify

    state.record_evidence(GateEvidence {
        gate: WorkflowPhase::Verify,
        kind: "test_run".to_string(),
        detail: "all green".to_string(),
    });
    assert!(matches!(state.try_advance(), GateCheck::Pass)); // Verify -> Review

    state.record_evidence(GateEvidence {
        gate: WorkflowPhase::Review,
        kind: "review".to_string(),
        detail: "approved".to_string(),
    });
    assert!(matches!(state.try_advance(), GateCheck::Pass)); // Review -> Done
    assert_eq!(state.phase, WorkflowPhase::Done);
}

#[test]
fn done_try_advance_is_blocked_with_complete_reason() {
    let mut state = WorkflowState::default();
    state.start("TASK-7");
    state.acceptance_criteria.push("AC1".to_string());
    state.try_advance();
    state.try_advance();
    state.record_evidence(GateEvidence {
        gate: WorkflowPhase::Verify,
        kind: "test_run".to_string(),
        detail: "all green".to_string(),
    });
    state.try_advance();
    state.record_evidence(GateEvidence {
        gate: WorkflowPhase::Review,
        kind: "review".to_string(),
        detail: "approved".to_string(),
    });
    state.try_advance();
    assert_eq!(state.phase, WorkflowPhase::Done);

    match state.try_advance() {
        GateCheck::Blocked { reason } => {
            assert!(reason.to_lowercase().contains("complete"));
        }
        GateCheck::Pass => panic!("Done should never advance further"),
    }
}

#[test]
fn workflow_state_round_trips_through_serde_json() {
    let mut state = WorkflowState::default();
    state.start("TASK-7");
    state.acceptance_criteria.push("AC1".to_string());
    state.record_evidence(GateEvidence {
        gate: WorkflowPhase::Verify,
        kind: "test_run".to_string(),
        detail: "all green".to_string(),
    });

    let json = serde_json::to_string(&state).expect("workflow state should serialize");
    let restored: WorkflowState =
        serde_json::from_str(&json).expect("workflow state should deserialize");

    assert_eq!(restored.phase, state.phase);
    assert_eq!(restored.task_ref, state.task_ref);
    assert_eq!(restored.acceptance_criteria, state.acceptance_criteria);
    assert_eq!(restored.evidence.len(), state.evidence.len());
    assert_eq!(restored.evidence[0].kind, state.evidence[0].kind);
    assert_eq!(restored.evidence[0].detail, state.evidence[0].detail);
    assert_eq!(restored.evidence[0].gate, state.evidence[0].gate);
}

// ---------------------------------------------------------------------------
// Task 9: gate enforcement in the agent loop (runtime-level, through run_turn).
// ---------------------------------------------------------------------------

/// One tool call then a natural stop. Emits a `Write` tool use on the first
/// turn; once it sees a Tool result it stops with text.
struct WriteThenStopClient {
    calls: usize,
}

impl ApiClient for WriteThenStopClient {
    fn stream(&mut self, request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.calls += 1;
        if request
            .messages
            .iter()
            .any(|message| message.role == MessageRole::Tool)
        {
            return Ok(vec![
                AssistantEvent::TextDelta("done".to_string()),
                AssistantEvent::MessageStop,
            ]);
        }
        Ok(vec![
            AssistantEvent::ToolUse {
                id: "tool-1".to_string(),
                name: "Write".to_string(),
                input: r#"{"path":"secret.txt","content":"x"}"#.to_string(),
            },
            AssistantEvent::MessageStop,
        ])
    }
}

/// Never calls a tool: every response is a natural stopping point, forcing the
/// Stop gate to fire on each iteration.
struct AlwaysStopsClient {
    calls: usize,
}

impl ApiClient for AlwaysStopsClient {
    fn stream(&mut self, _request: ApiRequest) -> Result<Vec<AssistantEvent>, RuntimeError> {
        self.calls += 1;
        Ok(vec![
            AssistantEvent::TextDelta(format!("response #{}", self.calls)),
            AssistantEvent::MessageStop,
        ])
    }
}

fn spec_session() -> Session {
    let mut session = Session::new();
    let mut workflow = WorkflowState::default();
    workflow.start("TASK-9");
    assert_eq!(workflow.phase, WorkflowPhase::Spec);
    session.workflow = Some(workflow);
    session
}

fn verify_session(with_evidence: bool) -> Session {
    let mut session = Session::new();
    let mut workflow = WorkflowState::default();
    workflow.start("TASK-9");
    workflow.acceptance_criteria.push("AC1".to_string());
    workflow.try_advance(); // Spec -> Implement
    workflow.try_advance(); // Implement -> Verify
    assert_eq!(workflow.phase, WorkflowPhase::Verify);
    if with_evidence {
        workflow.record_evidence(GateEvidence {
            gate: WorkflowPhase::Verify,
            kind: "test_run".to_string(),
            detail: "42 passed; 0 failed".to_string(),
        });
    }
    session.workflow = Some(workflow);
    session
}

fn features(mode: WorkflowGateMode) -> RuntimeFeatureConfig {
    RuntimeFeatureConfig::default().with_workflow_gates(mode)
}

fn write_runtime(
    session: Session,
    mode: WorkflowGateMode,
) -> ConversationRuntime<WriteThenStopClient, StaticToolExecutor> {
    ConversationRuntime::new_with_features(
        session,
        WriteThenStopClient { calls: 0 },
        StaticToolExecutor::new().register("Write", |_input| Ok("wrote file".to_string())),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec!["system".to_string()],
        &features(mode),
    )
}

#[test]
fn enforced_mode_blocks_write_in_spec_phase() {
    let mut runtime = write_runtime(spec_session(), WorkflowGateMode::Enforced);
    let summary = runtime
        .run_turn("edit the file", None)
        .expect("turn resolves");

    assert_eq!(summary.tool_results.len(), 1);
    let ContentBlock::ToolResult {
        is_error, output, ..
    } = &summary.tool_results[0].blocks[0]
    else {
        panic!("expected tool result");
    };
    assert!(*is_error, "enforced gate should deny the write: {output}");
    assert!(
        output.contains("Stop-the-line"),
        "denied output should carry the stop-the-line reason: {output}"
    );

    assert_eq!(summary.gate_events.len(), 1);
    let event = &summary.gate_events[0];
    assert_eq!(event.decision, GateDecision::Block);
    assert_eq!(event.phase, WorkflowPhase::Spec);
    assert_eq!(event.rule, "stop-the-line-spec");
    let json = serde_json::to_value(event).expect("gate event serializes");
    assert_eq!(json["schema"], "claw.gate_check.v1");
    assert_eq!(json["event"], "gate_check");
}

#[test]
fn advisory_mode_allows_write_but_injects_context_and_warning() {
    let mut runtime = write_runtime(spec_session(), WorkflowGateMode::Advisory);
    let summary = runtime
        .run_turn("edit the file", None)
        .expect("turn resolves");

    assert_eq!(summary.tool_results.len(), 1);
    let ContentBlock::ToolResult {
        is_error, output, ..
    } = &summary.tool_results[0].blocks[0]
    else {
        panic!("expected tool result");
    };
    assert!(!*is_error, "advisory gate must not block: {output}");
    assert!(
        output.contains("wrote file"),
        "tool should have actually run: {output}"
    );
    assert!(
        output.contains("[workflow gate]") && output.contains("Stop-the-line"),
        "advisory reason should be injected as context: {output}"
    );

    assert_eq!(summary.gate_events.len(), 1);
    assert_eq!(summary.gate_events[0].decision, GateDecision::Advisory);
    assert!(
        summary
            .lifecycle_warnings
            .iter()
            .any(|warning| warning.contains("Stop-the-line")),
        "advisory warning should surface: {:?}",
        summary.lifecycle_warnings
    );
}

#[test]
fn off_mode_leaves_write_untouched() {
    let mut runtime = write_runtime(spec_session(), WorkflowGateMode::Off);
    let summary = runtime
        .run_turn("edit the file", None)
        .expect("turn resolves");

    let ContentBlock::ToolResult {
        is_error, output, ..
    } = &summary.tool_results[0].blocks[0]
    else {
        panic!("expected tool result");
    };
    assert!(!*is_error, "off mode should not block: {output}");
    assert!(output.contains("wrote file"));
    assert!(!output.contains("[workflow gate]"));
    assert!(summary.gate_events.is_empty());
}

#[test]
fn no_workflow_session_is_untouched_even_when_enforced() {
    // Session has no workflow at all.
    let mut runtime = write_runtime(Session::new(), WorkflowGateMode::Enforced);
    let summary = runtime
        .run_turn("edit the file", None)
        .expect("turn resolves");

    let ContentBlock::ToolResult { is_error, .. } = &summary.tool_results[0].blocks[0] else {
        panic!("expected tool result");
    };
    assert!(!*is_error, "no-workflow session must never gate");
    assert!(summary.gate_events.is_empty());
}

#[test]
fn enforced_stop_gate_reprompts_in_verify_without_evidence() {
    let mut runtime = ConversationRuntime::new_with_features(
        verify_session(false),
        AlwaysStopsClient { calls: 0 },
        StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec!["system".to_string()],
        &features(WorkflowGateMode::Enforced),
    );

    let summary = runtime
        .run_turn("finish up", None)
        .expect("capped stop loop resolves");

    // Re-prompted up to the 3-strike cap, then forced to stop: 4 iterations.
    assert_eq!(summary.iterations, 4);
    let reprompts = runtime
        .session()
        .messages
        .iter()
        .filter(|message| {
            message.role == MessageRole::User
                && message.blocks.iter().any(|block| {
                    matches!(
                        block,
                        ContentBlock::Text { text } if text.contains("QAS gate")
                    )
                })
        })
        .count();
    assert_eq!(reprompts, 3, "QAS gate should re-prompt exactly 3 times");
    // Dedupe: byte-identical QAS events across the capped loop collapse to one
    // audit record per turn.
    assert_eq!(summary.gate_events.len(), 1);
    assert_eq!(summary.gate_events[0].rule, "qas-verify-evidence");
}

#[test]
fn enforced_stop_gate_allows_stop_with_test_run_evidence() {
    let mut runtime = ConversationRuntime::new_with_features(
        verify_session(true),
        AlwaysStopsClient { calls: 0 },
        StaticToolExecutor::new(),
        PermissionPolicy::new(PermissionMode::DangerFullAccess),
        vec!["system".to_string()],
        &features(WorkflowGateMode::Enforced),
    );

    let summary = runtime.run_turn("finish up", None).expect("turn resolves");

    assert_eq!(
        summary.iterations, 1,
        "evidence present -> stop allowed immediately"
    );
    assert!(summary.gate_events.is_empty());
    let _ = GateCheck::Pass; // keep GateCheck import exercised
}
