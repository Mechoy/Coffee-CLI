use super::{AttemptState, RunState, TaskState};

pub fn validate_task_transition(from: TaskState, to: TaskState) -> Result<(), String> {
    if task_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(format!(
            "Workflow task cannot transition from {} to {}",
            from.as_str(),
            to.as_str()
        ))
    }
}

pub fn validate_attempt_transition(from: AttemptState, to: AttemptState) -> Result<(), String> {
    if attempt_transition_allowed(from, to) {
        Ok(())
    } else {
        Err(format!(
            "Workflow attempt cannot transition from {} to {}",
            from.as_str(),
            to.as_str()
        ))
    }
}

pub const fn task_transition_allowed(from: TaskState, to: TaskState) -> bool {
    matches!(
        (from, to),
        (
            TaskState::Pending,
            TaskState::Ready | TaskState::Skipped | TaskState::Cancelled
        ) | (
            TaskState::Ready,
            TaskState::Dispatching | TaskState::Cancelled
        ) | (
            TaskState::Dispatching,
            TaskState::Running | TaskState::Attention | TaskState::Cancelled
        ) | (
            TaskState::Running,
            TaskState::Succeeded | TaskState::Failed | TaskState::Attention | TaskState::Cancelled
        ) | (
            TaskState::Attention,
            TaskState::Ready | TaskState::Skipped | TaskState::Cancelled
        )
    )
}

pub const fn attempt_transition_allowed(from: AttemptState, to: AttemptState) -> bool {
    matches!(
        (from, to),
        (
            AttemptState::Dispatching,
            AttemptState::Running | AttemptState::Interrupted | AttemptState::Cancelled
        ) | (
            AttemptState::Running,
            AttemptState::Succeeded
                | AttemptState::Failed
                | AttemptState::Interrupted
                | AttemptState::Cancelled
        )
    )
}

pub fn derive_run_state(tasks: impl IntoIterator<Item = TaskState>, paused: bool) -> RunState {
    let mut saw_task = false;
    let mut has_dispatchable_or_active_work = false;
    let mut has_blocked_work = false;
    let mut has_attention = false;
    let mut has_failed = false;
    let mut has_cancelled = false;

    for task in tasks {
        saw_task = true;
        match task {
            TaskState::Pending => has_blocked_work = true,
            TaskState::Ready | TaskState::Dispatching | TaskState::Running => {
                has_dispatchable_or_active_work = true
            }
            TaskState::Attention => has_attention = true,
            TaskState::Failed => has_failed = true,
            TaskState::Cancelled => has_cancelled = true,
            TaskState::Succeeded | TaskState::Skipped => {}
        }
    }

    if !saw_task {
        return RunState::Failed;
    }
    if paused && (has_dispatchable_or_active_work || has_blocked_work) {
        return RunState::Paused;
    }
    if has_dispatchable_or_active_work {
        return RunState::Running;
    }
    if has_attention || has_blocked_work {
        return RunState::Attention;
    }
    if has_failed {
        return RunState::Failed;
    }
    if has_cancelled {
        return RunState::Cancelled;
    }
    RunState::Succeeded
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_transitions_do_not_allow_terminal_rewrites() {
        validate_task_transition(TaskState::Ready, TaskState::Dispatching).unwrap();
        validate_task_transition(TaskState::Running, TaskState::Attention).unwrap();
        assert!(validate_task_transition(TaskState::Succeeded, TaskState::Ready).is_err());
        assert!(validate_task_transition(TaskState::Pending, TaskState::Running).is_err());
    }

    #[test]
    fn attempt_transitions_require_a_live_attempt() {
        validate_attempt_transition(AttemptState::Dispatching, AttemptState::Interrupted).unwrap();
        validate_attempt_transition(AttemptState::Running, AttemptState::Succeeded).unwrap();
        assert!(
            validate_attempt_transition(AttemptState::Succeeded, AttemptState::Running).is_err()
        );
    }

    #[test]
    fn attention_does_not_block_other_ready_items() {
        assert_eq!(
            derive_run_state([TaskState::Attention, TaskState::Ready], false),
            RunState::Running
        );
        assert_eq!(
            derive_run_state([TaskState::Attention, TaskState::Succeeded], false),
            RunState::Attention
        );
    }

    #[test]
    fn blocked_successor_does_not_hide_attention() {
        assert_eq!(
            derive_run_state([TaskState::Attention, TaskState::Pending], false),
            RunState::Attention
        );
    }
}
