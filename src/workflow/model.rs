use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;

pub const MAX_TEMPLATE_BYTES: usize = 128 * 1024;
pub const MAX_ITEM_INPUT_BYTES: usize = 64 * 1024;
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;
pub const MAX_JSON_DEPTH: usize = 32;
pub const MAX_JSON_NODES: usize = 4_096;
pub const MAX_ITEMS_PER_RUN: usize = 1_000;
pub const MAX_STAGES_PER_TEMPLATE: usize = 32;
pub const MAX_TASKS_PER_RUN: usize = 4_000;
pub const MAX_TOTAL_ITEM_INPUT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TOTAL_RUN_OUTPUT_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunTemplate {
    #[serde(default = "default_template_version")]
    pub version: u32,
    pub name: String,
    pub stages: Vec<AgentStage>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct AgentStage {
    pub id: String,
    pub title: String,
    pub instruction: String,
    #[serde(default)]
    pub required_output_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RunItemInput {
    pub client_key: String,
    pub input: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Running,
    Paused,
    Attention,
    Succeeded,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Pending,
    Ready,
    Dispatching,
    Running,
    Succeeded,
    Failed,
    Attention,
    Skipped,
    Cancelled,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AttemptState {
    Dispatching,
    Running,
    Succeeded,
    Failed,
    Interrupted,
    Cancelled,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum AttemptReport {
    Succeeded { output: Value },
    Failed { summary: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunRecord {
    pub id: String,
    pub template: RunTemplate,
    pub state: RunState,
    pub revision: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ItemRecord {
    pub id: String,
    pub run_id: String,
    pub client_key: String,
    pub input: Value,
    pub ordinal: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TaskRecord {
    pub id: String,
    pub run_id: String,
    pub item_id: String,
    pub stage_id: String,
    pub stage_index: u32,
    pub state: TaskState,
    pub current_attempt_id: Option<String>,
    pub output: Option<Value>,
    pub failure_reason: Option<String>,
    pub revision: u64,
    pub created_at: u64,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AttemptRecord {
    pub id: String,
    pub task_id: String,
    pub number: u32,
    pub state: AttemptState,
    pub worker_id: String,
    pub result: Option<Value>,
    pub reason: Option<String>,
    pub created_at: u64,
    pub started_at: Option<u64>,
    pub ended_at: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowEvent {
    pub sequence: u64,
    pub run_id: String,
    pub task_id: Option<String>,
    pub attempt_id: Option<String>,
    pub kind: String,
    pub payload: Value,
    pub created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunSnapshot {
    pub run: RunRecord,
    pub items: Vec<ItemRecord>,
    pub tasks: Vec<TaskRecord>,
    pub attempts: Vec<AttemptRecord>,
    pub events: Vec<WorkflowEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TaskCounts {
    pub pending: u32,
    pub ready: u32,
    pub dispatching: u32,
    pub running: u32,
    pub succeeded: u32,
    pub failed: u32,
    pub attention: u32,
    pub skipped: u32,
    pub cancelled: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RunSummary {
    pub id: String,
    pub name: String,
    pub state: RunState,
    pub revision: u64,
    pub item_count: u32,
    pub task_counts: TaskCounts,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AcceptedStageOutput {
    pub stage_id: String,
    pub stage_index: u32,
    pub output: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ClaimedTask {
    pub run_id: String,
    pub task_id: String,
    pub attempt_id: String,
    pub worker_id: String,
    pub item_id: String,
    pub client_key: String,
    pub item_input: Value,
    /// Only accepted structured output from earlier stages of this same Item.
    /// Terminal text and output from other Items never cross this boundary.
    pub prior_outputs: Vec<AcceptedStageOutput>,
    pub stage: AgentStage,
}

impl RunState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Attention => "attention",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "running" => Ok(Self::Running),
            "paused" => Ok(Self::Paused),
            "attention" => Ok(Self::Attention),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("Unknown run state in workflow store: {raw}")),
        }
    }
}

impl TaskState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Ready => "ready",
            Self::Dispatching => "dispatching",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Attention => "attention",
            Self::Skipped => "skipped",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "pending" => Ok(Self::Pending),
            "ready" => Ok(Self::Ready),
            "dispatching" => Ok(Self::Dispatching),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "attention" => Ok(Self::Attention),
            "skipped" => Ok(Self::Skipped),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("Unknown task state in workflow store: {raw}")),
        }
    }
}

impl AttemptState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dispatching => "dispatching",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "dispatching" => Ok(Self::Dispatching),
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "interrupted" => Ok(Self::Interrupted),
            "cancelled" => Ok(Self::Cancelled),
            _ => Err(format!("Unknown attempt state in workflow store: {raw}")),
        }
    }
}

pub fn validate_template(template: &RunTemplate) -> Result<(), String> {
    validate_nonempty_text(&template.name, "Template name", 128)?;
    if template.version == 0 {
        return Err("Template version must be greater than zero".to_string());
    }
    if template.stages.is_empty() {
        return Err("A run template needs at least one agent stage".to_string());
    }
    if template.stages.len() > MAX_STAGES_PER_TEMPLATE {
        return Err(format!(
            "A run template supports at most {MAX_STAGES_PER_TEMPLATE} stages"
        ));
    }

    let mut stage_ids = HashSet::new();
    for stage in &template.stages {
        validate_identifier(&stage.id, "Stage id", 64)?;
        if !stage_ids.insert(stage.id.as_str()) {
            return Err(format!(
                "Template contains duplicate stage id: {}",
                stage.id
            ));
        }
        validate_nonempty_text(&stage.title, "Stage title", 160)?;
        validate_nonempty_text(&stage.instruction, "Stage instruction", 24 * 1024)?;

        let mut output_keys = HashSet::new();
        for key in &stage.required_output_keys {
            validate_nonempty_text(key, "Required output key", 128)?;
            if !output_keys.insert(key.as_str()) {
                return Err(format!(
                    "Stage {} contains duplicate required output key: {key}",
                    stage.id
                ));
            }
        }
    }

    let encoded = serde_json::to_vec(template)
        .map_err(|error| format!("Failed to encode template for validation: {error}"))?;
    if encoded.len() > MAX_TEMPLATE_BYTES {
        return Err(format!(
            "Run template exceeds its {} KiB limit",
            MAX_TEMPLATE_BYTES / 1024
        ));
    }
    Ok(())
}

pub fn validate_item_inputs(items: &[RunItemInput]) -> Result<(), String> {
    if items.is_empty() {
        return Err("A run needs at least one item".to_string());
    }
    if items.len() > MAX_ITEMS_PER_RUN {
        return Err(format!("A run supports at most {MAX_ITEMS_PER_RUN} items"));
    }

    let mut client_keys = HashSet::new();
    let mut total_bytes = 0usize;
    for item in items {
        validate_identifier(&item.client_key, "Item client key", 128)?;
        if !client_keys.insert(item.client_key.as_str()) {
            return Err(format!(
                "Run contains duplicate item client key: {}",
                item.client_key
            ));
        }
        validate_json_value(&item.input, "Item input", MAX_ITEM_INPUT_BYTES)?;
        let encoded = serde_json::to_vec(&item.input)
            .map_err(|error| format!("Failed to encode item input for validation: {error}"))?;
        total_bytes = total_bytes.saturating_add(encoded.len());
        if total_bytes > MAX_TOTAL_ITEM_INPUT_BYTES {
            return Err(format!(
                "Run item inputs exceed their {} MiB total limit",
                MAX_TOTAL_ITEM_INPUT_BYTES / (1024 * 1024)
            ));
        }
    }
    Ok(())
}

pub fn validate_run_request(template: &RunTemplate, items: &[RunItemInput]) -> Result<(), String> {
    validate_template(template)?;
    validate_item_inputs(items)?;
    let task_count = template
        .stages
        .len()
        .checked_mul(items.len())
        .ok_or_else(|| "Run task count overflowed".to_string())?;
    if task_count > MAX_TASKS_PER_RUN {
        return Err(format!(
            "Run expands to {task_count} tasks, above the {MAX_TASKS_PER_RUN} task limit"
        ));
    }
    Ok(())
}

pub fn validate_stage_output(stage: &AgentStage, output: &Value) -> Result<(), String> {
    validate_json_value(output, "Stage output", MAX_OUTPUT_BYTES)?;
    let Value::Object(object) = output else {
        return Err(format!(
            "Stage {} must report a JSON object, not a scalar or array",
            stage.id
        ));
    };
    for key in &stage.required_output_keys {
        if !object.contains_key(key) {
            return Err(format!(
                "Stage {} output is missing required key: {key}",
                stage.id
            ));
        }
    }
    Ok(())
}

pub fn validate_worker_id(worker_id: &str) -> Result<(), String> {
    validate_identifier(worker_id, "Worker id", 128)
}

pub fn validate_failure_summary(summary: &str) -> Result<(), String> {
    validate_nonempty_text(summary, "Failure summary", 4 * 1024)
}

fn default_template_version() -> u32 {
    1
}

fn validate_identifier(value: &str, label: &str, max_len: usize) -> Result<(), String> {
    if value.is_empty() || value.len() > max_len {
        return Err(format!("{label} must be between 1 and {max_len} bytes"));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    {
        return Err(format!(
            "{label} may only use ASCII letters, digits, hyphens, and underscores"
        ));
    }
    Ok(())
}

fn validate_nonempty_text(value: &str, label: &str, max_len: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max_len {
        return Err(format!(
            "{label} must be non-empty and at most {max_len} bytes"
        ));
    }
    Ok(())
}

pub fn validate_json_value(value: &Value, label: &str, max_bytes: usize) -> Result<(), String> {
    let encoded = serde_json::to_vec(value)
        .map_err(|error| format!("Failed to encode {label} for validation: {error}"))?;
    if encoded.len() > max_bytes {
        return Err(format!(
            "{label} exceeds its {} KiB limit",
            max_bytes / 1024
        ));
    }

    let mut nodes = 0usize;
    inspect_json(value, label, 0, &mut nodes)
}

fn inspect_json(value: &Value, label: &str, depth: usize, nodes: &mut usize) -> Result<(), String> {
    *nodes += 1;
    if *nodes > MAX_JSON_NODES {
        return Err(format!(
            "{label} exceeds its {MAX_JSON_NODES} JSON value limit"
        ));
    }
    if depth > MAX_JSON_DEPTH {
        return Err(format!(
            "{label} exceeds its {MAX_JSON_DEPTH} level nesting limit"
        ));
    }
    match value {
        Value::Array(values) => {
            for child in values {
                inspect_json(child, label, depth + 1, nodes)?;
            }
        }
        Value::Object(values) => {
            for child in values.values() {
                inspect_json(child, label, depth + 1, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn fixture_template() -> RunTemplate {
    RunTemplate {
        version: 1,
        name: "Fixture analysis".to_string(),
        stages: vec![
            AgentStage {
                id: "collect".to_string(),
                title: "Collect".to_string(),
                instruction: "Read the supplied item and return structured notes.".to_string(),
                required_output_keys: vec!["notes".to_string()],
            },
            AgentStage {
                id: "review".to_string(),
                title: "Review".to_string(),
                instruction: "Review the collected notes and return a conclusion.".to_string(),
                required_output_keys: vec!["conclusion".to_string()],
            },
        ],
    }
}

#[cfg(test)]
pub(crate) fn fixture_items() -> Vec<RunItemInput> {
    vec![
        RunItemInput {
            client_key: "item-a".to_string(),
            input: serde_json::json!({"source": "A"}),
        },
        RunItemInput {
            client_key: "item-b".to_string(),
            input: serde_json::json!({"source": "B"}),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_rejects_duplicate_stage_ids() {
        let mut template = fixture_template();
        template.stages[1].id = template.stages[0].id.clone();
        assert!(validate_template(&template)
            .unwrap_err()
            .contains("duplicate stage id"));
    }

    #[test]
    fn item_inputs_reject_duplicate_client_keys() {
        let mut items = fixture_items();
        items[1].client_key = items[0].client_key.clone();
        assert!(validate_item_inputs(&items)
            .unwrap_err()
            .contains("duplicate item client key"));
    }

    #[test]
    fn stage_output_requires_contract_keys() {
        let stage = fixture_template().stages.remove(0);
        assert!(validate_stage_output(&stage, &serde_json::json!({}))
            .unwrap_err()
            .contains("missing required key"));
        validate_stage_output(&stage, &serde_json::json!({"notes": []})).unwrap();
    }

    #[test]
    fn run_request_rejects_an_excessive_item_stage_expansion() {
        let template = RunTemplate {
            version: 1,
            name: "Many stages".to_string(),
            stages: (0..32)
                .map(|index| AgentStage {
                    id: format!("stage-{index}"),
                    title: "Stage".to_string(),
                    instruction: "Return a bounded object.".to_string(),
                    required_output_keys: Vec::new(),
                })
                .collect(),
        };
        let items = (0..126)
            .map(|index| RunItemInput {
                client_key: format!("item-{index}"),
                input: serde_json::json!({}),
            })
            .collect::<Vec<_>>();
        assert!(validate_run_request(&template, &items)
            .unwrap_err()
            .contains("task limit"));
    }
}
