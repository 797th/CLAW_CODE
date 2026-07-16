use runtime::{GateCheck, GateEvidence, WorkflowPhase, WorkflowState};

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
    state.acceptance_criteria.push("returns 200 on success".to_string());

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
