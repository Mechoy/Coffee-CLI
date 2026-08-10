//! Durable domain foundation for Coffee CLI Agent Runs.
//!
//! This module intentionally does not launch a terminal or expose MCP tools
//! yet. It provides the authoritative state and persistence boundary that a
//! later dedicated Worker runtime will use. Keeping that boundary separate
//! prevents a workflow run from inheriting the lifecycle or permissions of a
//! normal terminal or a free-form multi-agent pane.

mod model;
mod state_machine;
mod store;

pub use model::{
    AcceptedStageOutput, AgentStage, AttemptRecord, AttemptReport, AttemptState, ClaimedTask,
    ItemRecord, RunItemInput, RunRecord, RunSnapshot, RunState, RunSummary, RunTemplate,
    TaskCounts, TaskRecord, TaskState, WorkflowEvent,
};
#[allow(unused_imports)]
pub use store::WorkflowStore;
