use super::{
    model::{
        validate_failure_summary, validate_stage_output, validate_template, validate_worker_id,
        MAX_ITEM_INPUT_BYTES, MAX_OUTPUT_BYTES, MAX_TEMPLATE_BYTES, MAX_TOTAL_RUN_OUTPUT_BYTES,
    },
    state_machine::{derive_run_state, validate_attempt_transition, validate_task_transition},
    AgentStage, AttemptRecord, AttemptReport, AttemptState, ClaimedTask, ItemRecord, RunItemInput,
    RunRecord, RunSnapshot, RunState, RunSummary, RunTemplate, TaskCounts, TaskRecord, TaskState,
    WorkflowEvent,
};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use uuid::Uuid;

const SCHEMA_VERSION: i64 = 1;
const MAX_EVENT_PAYLOAD_BYTES: usize = 16 * 1024;
const MAX_DATABASE_BYTES: i64 = 64 * 1024 * 1024;
const MAX_EVENTS_PER_RUN: i64 = 20_000;
const MAX_STORED_JSON_BYTES: usize = MAX_TEMPLATE_BYTES;

/// SQLite-backed source of truth for Agent Runs.
///
/// Each public mutation opens an immediate transaction and commits entity
/// state and its event record together. The future runtime may run in a
/// different task or thread, so a connection is deliberately not shared.
#[derive(Debug, Clone)]
pub struct WorkflowStore {
    path: PathBuf,
}

impl WorkflowStore {
    #[allow(dead_code)]
    pub fn default_path() -> Result<PathBuf, String> {
        dirs::home_dir()
            .map(|home| home.join(".coffee-cli").join("workflows.db"))
            .ok_or_else(|| "Could not determine the user home directory".to_string())
    }

    #[allow(dead_code)]
    pub fn open_default() -> Result<Self, String> {
        Self::open_at(Self::default_path()?)
    }

    pub fn open_at(path: impl Into<PathBuf>) -> Result<Self, String> {
        let store = Self { path: path.into() };
        let _ = store.connection()?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn create_run(
        &self,
        template: RunTemplate,
        items: Vec<RunItemInput>,
    ) -> Result<RunSnapshot, String> {
        super::model::validate_run_request(&template, &items)?;

        let template_json = encode_json(&template, "template", MAX_TEMPLATE_BYTES)?;
        let run_id = Uuid::new_v4().to_string();
        let now = now_millis();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;

        transaction
            .execute(
                "INSERT INTO runs (id, template_json, state, revision, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 1, ?4, ?4)",
                params![
                    run_id,
                    template_json,
                    RunState::Running.as_str(),
                    now as i64
                ],
            )
            .map_err(database_error)?;

        for (item_index, item) in items.iter().enumerate() {
            let item_id = Uuid::new_v4().to_string();
            let input_json = encode_json(&item.input, "item input", MAX_ITEM_INPUT_BYTES)?;
            transaction
                .execute(
                    "INSERT INTO items (id, run_id, client_key, input_json, ordinal)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        item_id,
                        run_id,
                        item.client_key,
                        input_json,
                        item_index as i64
                    ],
                )
                .map_err(database_error)?;

            for (stage_index, stage) in template.stages.iter().enumerate() {
                let state = if stage_index == 0 {
                    TaskState::Ready
                } else {
                    TaskState::Pending
                };
                transaction
                    .execute(
                        "INSERT INTO tasks (
                            id, run_id, item_id, stage_id, stage_index, state,
                            current_attempt_id, output_json, failure_reason, revision, created_at, updated_at
                        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, NULL, NULL, NULL, 1, ?7, ?7)",
                        params![
                            Uuid::new_v4().to_string(),
                            run_id,
                            item_id,
                            stage.id,
                            stage_index as i64,
                            state.as_str(),
                            now as i64,
                        ],
                    )
                    .map_err(database_error)?;
            }
        }

        append_event(
            &transaction,
            &run_id,
            None,
            None,
            "run_created",
            &json!({
                "template_name": template.name,
                "template_version": template.version,
                "item_count": items.len(),
                "stage_count": template.stages.len(),
            }),
            now,
        )?;
        transaction.commit().map_err(database_error)?;
        self.get_run(&run_id)
    }

    /// Claim one ready task globally. Alpha intentionally enforces one active
    /// attempt across all Runs until the dedicated Scheduler owns fair
    /// multi-worker allocation.
    pub fn claim_next_ready_task(&self, worker_id: &str) -> Result<Option<ClaimedTask>, String> {
        validate_worker_id(worker_id)?;
        let now = now_millis();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;

        let active_attempts: i64 = transaction
            .query_row(
                "SELECT COUNT(*) FROM attempts WHERE state IN ('dispatching', 'running')",
                [],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        if active_attempts > 0 {
            return Ok(None);
        }

        let candidate: Option<(String, String, String, String, String, i64, String, String)> =
            transaction
                .query_row(
                    "SELECT
                    t.id, t.run_id, t.item_id, i.client_key, i.input_json, t.stage_index,
                    r.template_json, r.state
                 FROM tasks t
                 JOIN runs r ON r.id = t.run_id
                 JOIN items i ON i.id = t.item_id
                 WHERE t.state = 'ready' AND r.state = 'running'
                 ORDER BY r.updated_at ASC, r.created_at ASC, i.ordinal ASC, t.stage_index ASC
                 LIMIT 1",
                    [],
                    |row| {
                        Ok((
                            row.get(0)?,
                            row.get(1)?,
                            row.get(2)?,
                            row.get(3)?,
                            row.get(4)?,
                            row.get(5)?,
                            row.get(6)?,
                            row.get(7)?,
                        ))
                    },
                )
                .optional()
                .map_err(database_error)?;
        let Some((
            task_id,
            run_id,
            item_id,
            client_key,
            item_input_json,
            stage_index,
            template_json,
            _,
        )) = candidate
        else {
            return Ok(None);
        };

        let template: RunTemplate = decode_json(&template_json, "stored run template")?;
        let stage = template
            .stages
            .get(stage_index as usize)
            .cloned()
            .ok_or_else(|| format!("Stored task {task_id} references a missing template stage"))?;
        let item_input = decode_json(&item_input_json, "stored item input")?;
        let attempt_number: i64 = transaction
            .query_row(
                "SELECT COALESCE(MAX(number), 0) + 1 FROM attempts WHERE task_id = ?1",
                params![task_id],
                |row| row.get(0),
            )
            .map_err(database_error)?;
        let attempt_id = Uuid::new_v4().to_string();

        validate_task_transition(TaskState::Ready, TaskState::Dispatching)?;
        transaction
            .execute(
                "INSERT INTO attempts (
                    id, task_id, number, state, worker_id, result_json, reason, created_at, started_at, ended_at
                 ) VALUES (?1, ?2, ?3, 'dispatching', ?4, NULL, NULL, ?5, NULL, NULL)",
                params![attempt_id, task_id, attempt_number, worker_id, now as i64],
            )
            .map_err(database_error)?;
        let updated = transaction
            .execute(
                "UPDATE tasks
                 SET state = 'dispatching', current_attempt_id = ?1, revision = revision + 1, updated_at = ?2
                 WHERE id = ?3 AND state = 'ready'",
                params![attempt_id, now as i64, task_id],
            )
            .map_err(database_error)?;
        if updated != 1 {
            return Err("Ready workflow task changed while it was being claimed".to_string());
        }
        bump_run_revision(&transaction, &run_id, RunState::Running, now)?;
        append_event(
            &transaction,
            &run_id,
            Some(&task_id),
            Some(&attempt_id),
            "attempt_dispatched",
            &json!({"worker_id": worker_id, "attempt_number": attempt_number}),
            now,
        )?;
        transaction.commit().map_err(database_error)?;

        Ok(Some(ClaimedTask {
            run_id,
            task_id,
            attempt_id,
            worker_id: worker_id.to_string(),
            item_id,
            client_key,
            item_input,
            stage,
        }))
    }

    pub fn mark_attempt_running(&self, attempt_id: &str, worker_id: &str) -> Result<(), String> {
        validate_worker_id(worker_id)?;
        let now = now_millis();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let context = live_attempt_context(
            &transaction,
            attempt_id,
            worker_id,
            AttemptState::Dispatching,
        )?;
        validate_attempt_transition(AttemptState::Dispatching, AttemptState::Running)?;
        validate_task_transition(TaskState::Dispatching, TaskState::Running)?;

        let updated_attempt = transaction
            .execute(
                "UPDATE attempts SET state = 'running', started_at = ?1 WHERE id = ?2 AND state = 'dispatching'",
                params![now as i64, attempt_id],
            )
            .map_err(database_error)?;
        let updated_task = transaction
            .execute(
                "UPDATE tasks SET state = 'running', revision = revision + 1, updated_at = ?1
                 WHERE id = ?2 AND state = 'dispatching' AND current_attempt_id = ?3",
                params![now as i64, context.task_id, attempt_id],
            )
            .map_err(database_error)?;
        if updated_attempt != 1 || updated_task != 1 {
            return Err("Workflow attempt changed while it was starting".to_string());
        }
        bump_run_revision(&transaction, &context.run_id, RunState::Running, now)?;
        append_event(
            &transaction,
            &context.run_id,
            Some(&context.task_id),
            Some(attempt_id),
            "attempt_started",
            &json!({"worker_id": worker_id}),
            now,
        )?;
        transaction.commit().map_err(database_error)
    }

    pub fn submit_report(
        &self,
        attempt_id: &str,
        worker_id: &str,
        report: AttemptReport,
    ) -> Result<(), String> {
        validate_worker_id(worker_id)?;
        let now = now_millis();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let context =
            live_attempt_context(&transaction, attempt_id, worker_id, AttemptState::Running)?;
        let stage = stage_for_task(&transaction, &context.run_id, context.stage_index)?;

        match report {
            AttemptReport::Succeeded { output } => {
                validate_stage_output(&stage, &output)?;
                let output_json = encode_json(&output, "stage output", MAX_OUTPUT_BYTES)?;
                ensure_run_output_budget(&transaction, &context.run_id, output_json.len())?;
                validate_attempt_transition(AttemptState::Running, AttemptState::Succeeded)?;
                validate_task_transition(TaskState::Running, TaskState::Succeeded)?;
                let updated_attempt = transaction
                    .execute(
                        "UPDATE attempts
                         SET state = 'succeeded', result_json = NULL, reason = NULL, ended_at = ?1
                         WHERE id = ?2 AND state = 'running'",
                        params![now as i64, attempt_id],
                    )
                    .map_err(database_error)?;
                if updated_attempt != 1 {
                    return Err(
                        "Workflow attempt changed before its report was accepted".to_string()
                    );
                }
                let updated_task = transaction
                    .execute(
                        "UPDATE tasks
                         SET state = 'succeeded', output_json = ?1, failure_reason = NULL,
                             revision = revision + 1, updated_at = ?2
                         WHERE id = ?3 AND state = 'running' AND current_attempt_id = ?4",
                        params![output_json, now as i64, context.task_id, attempt_id],
                    )
                    .map_err(database_error)?;
                if updated_task != 1 {
                    return Err("Workflow task changed before its report was accepted".to_string());
                }
                let next_state_changed = transaction
                    .execute(
                        "UPDATE tasks
                         SET state = 'ready', revision = revision + 1, updated_at = ?1
                         WHERE run_id = ?2 AND item_id = ?3 AND stage_index = ?4 AND state = 'pending'",
                        params![
                            now as i64,
                            context.run_id,
                            context.item_id,
                            context.stage_index + 1
                        ],
                    )
                    .map_err(database_error)?;
                let run_state = derive_run_state_from_store(&transaction, &context.run_id, false)?;
                bump_run_revision(&transaction, &context.run_id, run_state, now)?;
                append_event(
                    &transaction,
                    &context.run_id,
                    Some(&context.task_id),
                    Some(attempt_id),
                    "attempt_succeeded",
                    &json!({
                        "worker_id": worker_id,
                        "next_stage_ready": next_state_changed == 1,
                    }),
                    now,
                )?;
            }
            AttemptReport::Failed { summary } => {
                validate_failure_summary(&summary)?;
                validate_attempt_transition(AttemptState::Running, AttemptState::Failed)?;
                validate_task_transition(TaskState::Running, TaskState::Attention)?;
                let updated_attempt = transaction
                    .execute(
                        "UPDATE attempts
                         SET state = 'failed', reason = ?1, ended_at = ?2
                         WHERE id = ?3 AND state = 'running'",
                        params![summary, now as i64, attempt_id],
                    )
                    .map_err(database_error)?;
                if updated_attempt != 1 {
                    return Err(
                        "Workflow attempt changed before its failure was recorded".to_string()
                    );
                }
                let updated_task = transaction
                    .execute(
                        "UPDATE tasks
                         SET state = 'attention', failure_reason = ?1,
                             revision = revision + 1, updated_at = ?2
                         WHERE id = ?3 AND state = 'running' AND current_attempt_id = ?4",
                        params![summary, now as i64, context.task_id, attempt_id],
                    )
                    .map_err(database_error)?;
                if updated_task != 1 {
                    return Err("Workflow task changed before its failure was recorded".to_string());
                }
                let run_state = derive_run_state_from_store(&transaction, &context.run_id, false)?;
                bump_run_revision(&transaction, &context.run_id, run_state, now)?;
                append_event(
                    &transaction,
                    &context.run_id,
                    Some(&context.task_id),
                    Some(attempt_id),
                    "attempt_failed",
                    &json!({"worker_id": worker_id}),
                    now,
                )?;
            }
        }

        transaction.commit().map_err(database_error)
    }

    /// Marks an active worker attempt as interrupted. The caller must supply
    /// its worker identity, so a stale worker cannot alter a newer attempt.
    pub fn mark_attempt_interrupted(
        &self,
        attempt_id: &str,
        worker_id: &str,
        reason: &str,
    ) -> Result<(), String> {
        validate_worker_id(worker_id)?;
        validate_failure_summary(reason)?;
        let now = now_millis();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let context = live_attempt_context_any_active(&transaction, attempt_id, worker_id)?;
        validate_attempt_transition(context.attempt_state, AttemptState::Interrupted)?;
        validate_task_transition(context.task_state, TaskState::Attention)?;
        let active_state = context.attempt_state.as_str();
        let updated_attempt = transaction
            .execute(
                "UPDATE attempts
                 SET state = 'interrupted', reason = ?1, ended_at = ?2
                 WHERE id = ?3 AND state = ?4",
                params![reason, now as i64, attempt_id, active_state],
            )
            .map_err(database_error)?;
        if updated_attempt != 1 {
            return Err(
                "Workflow attempt changed before its interruption was recorded".to_string(),
            );
        }
        let task_state = context.task_state.as_str();
        let updated_task = transaction
            .execute(
                "UPDATE tasks
                 SET state = 'attention', failure_reason = ?1,
                     revision = revision + 1, updated_at = ?2
                 WHERE id = ?3 AND state = ?4 AND current_attempt_id = ?5",
                params![reason, now as i64, context.task_id, task_state, attempt_id],
            )
            .map_err(database_error)?;
        if updated_task != 1 {
            return Err("Workflow task changed before its interruption was recorded".to_string());
        }
        let run_state = derive_run_state_from_store(&transaction, &context.run_id, false)?;
        bump_run_revision(&transaction, &context.run_id, run_state, now)?;
        append_event(
            &transaction,
            &context.run_id,
            Some(&context.task_id),
            Some(attempt_id),
            "attempt_interrupted",
            &json!({"worker_id": worker_id, "reason": reason}),
            now,
        )?;
        transaction.commit().map_err(database_error)
    }

    /// Requeue a task only after its current attempt is known terminal. This
    /// is intentionally manual: the runtime must never retry a potentially
    /// side-effecting task by itself.
    pub fn retry_task(&self, task_id: &str, expected_attempt_id: &str) -> Result<(), String> {
        let now = now_millis();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let context = task_context(&transaction, task_id)?;
        if context.task_state != TaskState::Attention {
            return Err("Only an attention workflow task can be retried".to_string());
        }
        if context.current_attempt_id.as_deref() != Some(expected_attempt_id) {
            return Err(
                "Workflow task has a different current attempt; reload before retrying".to_string(),
            );
        }
        let current_attempt_state = attempt_state(&transaction, expected_attempt_id)?;
        if matches!(
            current_attempt_state,
            AttemptState::Dispatching | AttemptState::Running
        ) {
            return Err(
                "Cannot retry a workflow task while its prior attempt may still be running"
                    .to_string(),
            );
        }
        validate_task_transition(TaskState::Attention, TaskState::Ready)?;
        let updated_task = transaction
            .execute(
                "UPDATE tasks
                 SET state = 'ready', failure_reason = NULL, revision = revision + 1, updated_at = ?1
                 WHERE id = ?2 AND state = 'attention' AND current_attempt_id = ?3",
                params![now as i64, task_id, expected_attempt_id],
            )
            .map_err(database_error)?;
        if updated_task != 1 {
            return Err("Workflow task changed before it could be retried".to_string());
        }
        bump_run_revision(&transaction, &context.run_id, RunState::Running, now)?;
        append_event(
            &transaction,
            &context.run_id,
            Some(task_id),
            Some(expected_attempt_id),
            "task_retried",
            &json!({}),
            now,
        )?;
        transaction.commit().map_err(database_error)
    }

    /// Crash/startup recovery. Active attempts are never resumed or retried;
    /// they become `interrupted` and their Tasks require human attention.
    pub fn recover_interrupted_attempts(&self, reason: &str) -> Result<u32, String> {
        validate_failure_summary(reason)?;
        let now = now_millis();
        let mut connection = self.connection()?;
        let transaction = connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(database_error)?;
        let active: Vec<(String, String, String, String, String, String)> = {
            let mut statement = transaction
                .prepare(
                    "SELECT a.id, a.task_id, a.worker_id, t.run_id, t.state, a.state
                     FROM attempts a
                     JOIN tasks t ON t.id = a.task_id
                     WHERE a.state IN ('dispatching', 'running')",
                )
                .map_err(database_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                })
                .map_err(database_error)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(database_error)?
        };
        let mut affected_runs = BTreeSet::new();
        for (attempt_id, task_id, worker_id, run_id, task_state, attempt_state) in &active {
            let parsed_attempt = AttemptState::parse(attempt_state)?;
            let parsed_task = TaskState::parse(task_state)?;
            validate_attempt_transition(parsed_attempt, AttemptState::Interrupted)?;
            validate_task_transition(parsed_task, TaskState::Attention)?;
            transaction
                .execute(
                    "UPDATE attempts
                     SET state = 'interrupted', reason = ?1, ended_at = ?2
                     WHERE id = ?3 AND state IN ('dispatching', 'running')",
                    params![reason, now as i64, attempt_id],
                )
                .map_err(database_error)?;
            transaction
                .execute(
                    "UPDATE tasks
                     SET state = 'attention', failure_reason = ?1,
                         revision = revision + 1, updated_at = ?2
                     WHERE id = ?3 AND current_attempt_id = ?4 AND state IN ('dispatching', 'running')",
                    params![reason, now as i64, task_id, attempt_id],
                )
                .map_err(database_error)?;
            append_event(
                &transaction,
                run_id,
                Some(task_id),
                Some(attempt_id),
                "attempt_interrupted",
                &json!({"worker_id": worker_id, "reason": reason, "recovered": true}),
                now,
            )?;
            affected_runs.insert(run_id.clone());
        }
        for run_id in affected_runs {
            let run_state = derive_run_state_from_store(&transaction, &run_id, false)?;
            bump_run_revision(&transaction, &run_id, run_state, now)?;
        }
        transaction.commit().map_err(database_error)?;
        Ok(active.len() as u32)
    }

    pub fn get_run(&self, run_id: &str) -> Result<RunSnapshot, String> {
        let connection = self.connection()?;
        let run = load_run(&connection, run_id)?;
        let items = load_items(&connection, run_id)?;
        let tasks = load_tasks(&connection, run_id)?;
        let task_ids = tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
        let attempts = load_attempts(&connection, &task_ids)?;
        let events = load_events(&connection, run_id)?;
        Ok(RunSnapshot {
            run,
            items,
            tasks,
            attempts,
            events,
        })
    }

    pub fn list_runs(&self) -> Result<Vec<RunSummary>, String> {
        let connection = self.connection()?;
        let ids: Vec<String> = {
            let mut statement = connection
                .prepare("SELECT id FROM runs ORDER BY updated_at DESC, created_at DESC")
                .map_err(database_error)?;
            let rows = statement
                .query_map([], |row| row.get(0))
                .map_err(database_error)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(database_error)?
        };
        ids.into_iter()
            .map(|id| self.get_run(&id).map(snapshot_summary))
            .collect()
    }

    fn connection(&self) -> Result<Connection, String> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| "Workflow database path has no parent directory".to_string())?;
        ensure_private_directory(parent)?;
        let mut connection = Connection::open(&self.path).map_err(database_error)?;
        connection
            .busy_timeout(Duration::from_secs(5))
            .map_err(database_error)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA wal_autocheckpoint = 100;
                 PRAGMA journal_size_limit = 4194304;",
            )
            .map_err(database_error)?;
        let page_size: i64 = connection
            .pragma_query_value(None, "page_size", |row| row.get(0))
            .map_err(database_error)?;
        let max_page_count = (MAX_DATABASE_BYTES + page_size - 1) / page_size;
        connection
            .pragma_update(None, "max_page_count", max_page_count)
            .map_err(database_error)?;
        let effective_max_page_count: i64 = connection
            .pragma_query_value(None, "max_page_count", |row| row.get(0))
            .map_err(database_error)?;
        if effective_max_page_count > max_page_count {
            return Err(format!(
                "Workflow database already exceeds its {} MiB storage limit",
                MAX_DATABASE_BYTES / (1024 * 1024)
            ));
        }
        migrate(&mut connection)?;
        ensure_private_file(&self.path)?;
        Ok(connection)
    }
}

#[derive(Debug)]
struct LiveAttemptContext {
    run_id: String,
    task_id: String,
    item_id: String,
    stage_index: i64,
    task_state: TaskState,
    attempt_state: AttemptState,
}

#[derive(Debug)]
struct TaskContext {
    run_id: String,
    task_state: TaskState,
    current_attempt_id: Option<String>,
}

fn migrate(connection: &mut Connection) -> Result<(), String> {
    let current: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(database_error)?;
    if current > SCHEMA_VERSION {
        return Err(format!(
            "Workflow database version {current} is newer than this Coffee CLI build supports"
        ));
    }
    if current == SCHEMA_VERSION {
        return Ok(());
    }
    if current != 0 {
        return Err(format!(
            "Unsupported workflow database migration from version {current}"
        ));
    }

    let transaction = connection
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(database_error)?;
    transaction
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS runs (
                id TEXT PRIMARY KEY,
                template_json TEXT NOT NULL,
                state TEXT NOT NULL,
                revision INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS items (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                client_key TEXT NOT NULL,
                input_json TEXT NOT NULL,
                ordinal INTEGER NOT NULL,
                UNIQUE(run_id, client_key),
                UNIQUE(run_id, ordinal)
            );
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY,
                run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                item_id TEXT NOT NULL REFERENCES items(id) ON DELETE CASCADE,
                stage_id TEXT NOT NULL,
                stage_index INTEGER NOT NULL,
                state TEXT NOT NULL,
                current_attempt_id TEXT,
                output_json TEXT,
                failure_reason TEXT,
                revision INTEGER NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                UNIQUE(run_id, item_id, stage_id),
                UNIQUE(item_id, stage_index)
            );
            CREATE TABLE IF NOT EXISTS attempts (
                id TEXT PRIMARY KEY,
                task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
                number INTEGER NOT NULL,
                state TEXT NOT NULL,
                worker_id TEXT NOT NULL,
                result_json TEXT,
                reason TEXT,
                created_at INTEGER NOT NULL,
                started_at INTEGER,
                ended_at INTEGER,
                UNIQUE(task_id, number)
            );
            CREATE TABLE IF NOT EXISTS events (
                sequence INTEGER PRIMARY KEY AUTOINCREMENT,
                run_id TEXT NOT NULL REFERENCES runs(id) ON DELETE CASCADE,
                task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
                attempt_id TEXT REFERENCES attempts(id) ON DELETE SET NULL,
                kind TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_tasks_ready ON tasks(state, run_id, stage_index);
            CREATE INDEX IF NOT EXISTS idx_attempts_task ON attempts(task_id, number);
            CREATE INDEX IF NOT EXISTS idx_events_run ON events(run_id, sequence);",
        )
        .map_err(database_error)?;
    transaction
        .pragma_update(None, "user_version", SCHEMA_VERSION)
        .map_err(database_error)?;
    transaction.commit().map_err(database_error)
}

fn live_attempt_context(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    worker_id: &str,
    expected_attempt_state: AttemptState,
) -> Result<LiveAttemptContext, String> {
    let context = live_attempt_context_any_active(transaction, attempt_id, worker_id)?;
    if context.attempt_state != expected_attempt_state {
        return Err(format!(
            "Workflow attempt is {}, not {}",
            context.attempt_state.as_str(),
            expected_attempt_state.as_str()
        ));
    }
    Ok(context)
}

fn live_attempt_context_any_active(
    transaction: &Transaction<'_>,
    attempt_id: &str,
    worker_id: &str,
) -> Result<LiveAttemptContext, String> {
    let raw: Option<(String, String, String, i64, String, String, String)> = transaction
        .query_row(
            "SELECT t.run_id, t.id, t.item_id, t.stage_index, t.state, a.state, a.worker_id
             FROM attempts a
             JOIN tasks t ON t.id = a.task_id
             WHERE a.id = ?1 AND t.current_attempt_id = a.id",
            params![attempt_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                ))
            },
        )
        .optional()
        .map_err(database_error)?;
    let Some((run_id, task_id, item_id, stage_index, task_state, attempt_state, actual_worker)) =
        raw
    else {
        return Err("Workflow report references an unknown or superseded attempt".to_string());
    };
    if actual_worker != worker_id {
        return Err("Workflow attempt does not belong to this worker".to_string());
    }
    let attempt_state = AttemptState::parse(&attempt_state)?;
    if !matches!(
        attempt_state,
        AttemptState::Dispatching | AttemptState::Running
    ) {
        return Err("Workflow attempt is no longer active".to_string());
    }
    Ok(LiveAttemptContext {
        run_id,
        task_id,
        item_id,
        stage_index,
        task_state: TaskState::parse(&task_state)?,
        attempt_state,
    })
}

fn task_context(transaction: &Transaction<'_>, task_id: &str) -> Result<TaskContext, String> {
    let raw: Option<(String, String, Option<String>)> = transaction
        .query_row(
            "SELECT run_id, state, current_attempt_id FROM tasks WHERE id = ?1",
            params![task_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(database_error)?;
    let Some((run_id, state, current_attempt_id)) = raw else {
        return Err("Workflow task was not found".to_string());
    };
    Ok(TaskContext {
        run_id,
        task_state: TaskState::parse(&state)?,
        current_attempt_id,
    })
}

fn attempt_state(transaction: &Transaction<'_>, attempt_id: &str) -> Result<AttemptState, String> {
    let raw: Option<String> = transaction
        .query_row(
            "SELECT state FROM attempts WHERE id = ?1",
            params![attempt_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(database_error)?;
    raw.ok_or_else(|| "Workflow attempt was not found".to_string())
        .and_then(|state| AttemptState::parse(&state))
}

fn stage_for_task(
    transaction: &Transaction<'_>,
    run_id: &str,
    stage_index: i64,
) -> Result<AgentStage, String> {
    let template_json: String = transaction
        .query_row(
            "SELECT template_json FROM runs WHERE id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    let template: RunTemplate = decode_json(&template_json, "stored run template")?;
    validate_template(&template)?;
    template
        .stages
        .get(stage_index as usize)
        .cloned()
        .ok_or_else(|| format!("Stored workflow task references missing stage {stage_index}"))
}

fn derive_run_state_from_store(
    transaction: &Transaction<'_>,
    run_id: &str,
    paused: bool,
) -> Result<RunState, String> {
    let states: Vec<String> = {
        let mut statement = transaction
            .prepare("SELECT state FROM tasks WHERE run_id = ?1")
            .map_err(database_error)?;
        let rows = statement
            .query_map(params![run_id], |row| row.get(0))
            .map_err(database_error)?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(database_error)?
    };
    states
        .into_iter()
        .map(|state| TaskState::parse(&state))
        .collect::<Result<Vec<_>, _>>()
        .map(|states| derive_run_state(states, paused))
}

fn bump_run_revision(
    transaction: &Transaction<'_>,
    run_id: &str,
    state: RunState,
    now: u64,
) -> Result<(), String> {
    let updated = transaction
        .execute(
            "UPDATE runs SET state = ?1, revision = revision + 1, updated_at = ?2 WHERE id = ?3",
            params![state.as_str(), now as i64, run_id],
        )
        .map_err(database_error)?;
    if updated != 1 {
        return Err("Workflow run was not found while updating its state".to_string());
    }
    Ok(())
}

fn append_event(
    transaction: &Transaction<'_>,
    run_id: &str,
    task_id: Option<&str>,
    attempt_id: Option<&str>,
    kind: &str,
    payload: &Value,
    now: u64,
) -> Result<(), String> {
    let current_count: i64 = transaction
        .query_row(
            "SELECT COUNT(*) FROM events WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    if current_count >= MAX_EVENTS_PER_RUN {
        return Err(format!(
            "Workflow run reached its {MAX_EVENTS_PER_RUN} event retention limit"
        ));
    }
    let payload_json = encode_json(payload, "workflow event payload", MAX_EVENT_PAYLOAD_BYTES)?;
    transaction
        .execute(
            "INSERT INTO events (run_id, task_id, attempt_id, kind, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![run_id, task_id, attempt_id, kind, payload_json, now as i64],
        )
        .map_err(database_error)?;
    Ok(())
}

fn load_run(connection: &Connection, run_id: &str) -> Result<RunRecord, String> {
    let raw: Option<(String, String, String, i64, i64, i64)> = connection
        .query_row(
            "SELECT id, template_json, state, revision, created_at, updated_at FROM runs WHERE id = ?1",
            params![run_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(database_error)?;
    let Some((id, template_json, state, revision, created_at, updated_at)) = raw else {
        return Err("Workflow run was not found".to_string());
    };
    let template: RunTemplate = decode_json(&template_json, "stored run template")?;
    validate_template(&template)?;
    Ok(RunRecord {
        id,
        template,
        state: RunState::parse(&state)?,
        revision: as_u64(revision, "run revision")?,
        created_at: as_u64(created_at, "run creation time")?,
        updated_at: as_u64(updated_at, "run update time")?,
    })
}

fn load_items(connection: &Connection, run_id: &str) -> Result<Vec<ItemRecord>, String> {
    let mut statement = connection
        .prepare(
            "SELECT id, run_id, client_key, input_json, ordinal
             FROM items WHERE run_id = ?1 ORDER BY ordinal ASC",
        )
        .map_err(database_error)?;
    let raw = statement
        .query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    raw.into_iter()
        .map(|(id, run_id, client_key, input_json, ordinal)| {
            Ok(ItemRecord {
                id,
                run_id,
                client_key,
                input: decode_json(&input_json, "stored item input")?,
                ordinal: as_u32(ordinal, "item ordinal")?,
            })
        })
        .collect()
}

fn load_tasks(connection: &Connection, run_id: &str) -> Result<Vec<TaskRecord>, String> {
    let mut statement = connection
        .prepare(
            "SELECT t.id, t.run_id, t.item_id, t.stage_id, t.stage_index, t.state, t.current_attempt_id,
                    t.output_json, t.failure_reason, t.revision, t.created_at, t.updated_at
             FROM tasks t
             JOIN items i ON i.id = t.item_id
             WHERE t.run_id = ?1 ORDER BY i.ordinal ASC, t.stage_index ASC",
        )
        .map_err(database_error)?;
    let raw = statement
        .query_map(params![run_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, i64>(11)?,
            ))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    raw.into_iter()
        .map(|raw| {
            Ok(TaskRecord {
                id: raw.0,
                run_id: raw.1,
                item_id: raw.2,
                stage_id: raw.3,
                stage_index: as_u32(raw.4, "task stage index")?,
                state: TaskState::parse(&raw.5)?,
                current_attempt_id: raw.6,
                output: raw
                    .7
                    .map(|output| decode_json(&output, "stored task output"))
                    .transpose()?,
                failure_reason: raw.8,
                revision: as_u64(raw.9, "task revision")?,
                created_at: as_u64(raw.10, "task creation time")?,
                updated_at: as_u64(raw.11, "task update time")?,
            })
        })
        .collect()
}

fn load_attempts(
    connection: &Connection,
    task_ids: &[String],
) -> Result<Vec<AttemptRecord>, String> {
    if task_ids.is_empty() {
        return Ok(Vec::new());
    }
    let placeholders = std::iter::repeat("?")
        .take(task_ids.len())
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT id, task_id, number, state, worker_id, result_json, reason, created_at, started_at, ended_at
         FROM attempts WHERE task_id IN ({placeholders}) ORDER BY task_id ASC, number ASC"
    );
    let mut statement = connection.prepare(&sql).map_err(database_error)?;
    let raw = statement
        .query_map(rusqlite::params_from_iter(task_ids.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<i64>>(8)?,
                row.get::<_, Option<i64>>(9)?,
            ))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    raw.into_iter()
        .map(|raw| {
            Ok(AttemptRecord {
                id: raw.0,
                task_id: raw.1,
                number: as_u32(raw.2, "attempt number")?,
                state: AttemptState::parse(&raw.3)?,
                worker_id: raw.4,
                result: raw
                    .5
                    .map(|result| decode_json(&result, "stored attempt result"))
                    .transpose()?,
                reason: raw.6,
                created_at: as_u64(raw.7, "attempt creation time")?,
                started_at: raw
                    .8
                    .map(|value| as_u64(value, "attempt start time"))
                    .transpose()?,
                ended_at: raw
                    .9
                    .map(|value| as_u64(value, "attempt end time"))
                    .transpose()?,
            })
        })
        .collect()
}

fn load_events(connection: &Connection, run_id: &str) -> Result<Vec<WorkflowEvent>, String> {
    let mut statement = connection
        .prepare(
            "SELECT sequence, run_id, task_id, attempt_id, kind, payload_json, created_at
             FROM events WHERE run_id = ?1 ORDER BY sequence ASC",
        )
        .map_err(database_error)?;
    let raw = statement
        .query_map(params![run_id], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, i64>(6)?,
            ))
        })
        .map_err(database_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(database_error)?;
    raw.into_iter()
        .map(|raw| {
            Ok(WorkflowEvent {
                sequence: as_u64(raw.0, "workflow event sequence")?,
                run_id: raw.1,
                task_id: raw.2,
                attempt_id: raw.3,
                kind: raw.4,
                payload: decode_json(&raw.5, "workflow event payload")?,
                created_at: as_u64(raw.6, "workflow event time")?,
            })
        })
        .collect()
}

fn snapshot_summary(snapshot: RunSnapshot) -> RunSummary {
    let mut task_counts = TaskCounts::default();
    for task in &snapshot.tasks {
        match task.state {
            TaskState::Pending => task_counts.pending += 1,
            TaskState::Ready => task_counts.ready += 1,
            TaskState::Dispatching => task_counts.dispatching += 1,
            TaskState::Running => task_counts.running += 1,
            TaskState::Succeeded => task_counts.succeeded += 1,
            TaskState::Failed => task_counts.failed += 1,
            TaskState::Attention => task_counts.attention += 1,
            TaskState::Skipped => task_counts.skipped += 1,
            TaskState::Cancelled => task_counts.cancelled += 1,
        }
    }
    RunSummary {
        id: snapshot.run.id,
        name: snapshot.run.template.name,
        state: snapshot.run.state,
        revision: snapshot.run.revision,
        item_count: snapshot.items.len() as u32,
        task_counts,
        updated_at: snapshot.run.updated_at,
    }
}

fn encode_json<T: serde::Serialize>(
    value: &T,
    label: &str,
    max_bytes: usize,
) -> Result<String, String> {
    let encoded = serde_json::to_string(value)
        .map_err(|error| format!("Failed to encode {label}: {error}"))?;
    if encoded.len() > max_bytes {
        return Err(format!(
            "{label} exceeds its {} KiB limit",
            max_bytes / 1024
        ));
    }
    Ok(encoded)
}

fn decode_json<T: serde::de::DeserializeOwned>(encoded: &str, label: &str) -> Result<T, String> {
    if encoded.len() > MAX_STORED_JSON_BYTES {
        return Err(format!(
            "Stored {label} exceeds its {} KiB safety limit",
            MAX_STORED_JSON_BYTES / 1024
        ));
    }
    serde_json::from_str(encoded)
        .map_err(|error| format!("Invalid {label} in workflow store: {error}"))
}

fn as_u64(value: i64, label: &str) -> Result<u64, String> {
    u64::try_from(value).map_err(|_| format!("Invalid negative {label} in workflow store"))
}

fn as_u32(value: i64, label: &str) -> Result<u32, String> {
    u32::try_from(value).map_err(|_| format!("Invalid {label} in workflow store"))
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn database_error(error: rusqlite::Error) -> String {
    format!("Workflow database error: {error}")
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path).map_err(|error| {
        format!(
            "Failed to create workflow storage {}: {error}",
            path.display()
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            format!(
                "Failed to protect workflow storage {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn ensure_private_file(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            format!(
                "Failed to protect workflow database {}: {error}",
                path.display()
            )
        })?;
    }
    Ok(())
}

fn ensure_run_output_budget(
    transaction: &Transaction<'_>,
    run_id: &str,
    additional_bytes: usize,
) -> Result<(), String> {
    let accepted_bytes: i64 = transaction
        .query_row(
            "SELECT COALESCE(SUM(LENGTH(output_json)), 0) FROM tasks WHERE run_id = ?1",
            params![run_id],
            |row| row.get(0),
        )
        .map_err(database_error)?;
    let accepted_bytes = usize::try_from(accepted_bytes)
        .map_err(|_| "Invalid accepted output size in workflow store".to_string())?;
    if accepted_bytes.saturating_add(additional_bytes) > MAX_TOTAL_RUN_OUTPUT_BYTES {
        return Err(format!(
            "Workflow run outputs exceed their {} MiB total limit",
            MAX_TOTAL_RUN_OUTPUT_BYTES / (1024 * 1024)
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workflow::model::{fixture_items, fixture_template};

    fn test_store() -> (WorkflowStore, PathBuf) {
        let root = std::env::temp_dir().join(format!("coffee-workflow-test-{}", Uuid::new_v4()));
        let store = WorkflowStore::open_at(root.join("workflows.db")).unwrap();
        (store, root)
    }

    #[test]
    fn create_run_expands_each_item_and_only_first_stage_is_ready() {
        let (store, root) = test_store();
        let snapshot = store
            .create_run(fixture_template(), fixture_items())
            .unwrap();
        assert_eq!(snapshot.items.len(), 2);
        assert_eq!(snapshot.tasks.len(), 4);
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .filter(|task| task.state == TaskState::Ready)
                .count(),
            2
        );
        assert_eq!(snapshot.events.len(), 1);
        assert_eq!(snapshot.events[0].kind, "run_created");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn successful_report_advances_only_the_same_item() {
        let (store, root) = test_store();
        let run = store
            .create_run(fixture_template(), fixture_items())
            .unwrap();
        let claim = store.claim_next_ready_task("worker-1").unwrap().unwrap();
        store
            .mark_attempt_running(&claim.attempt_id, "worker-1")
            .unwrap();
        store
            .submit_report(
                &claim.attempt_id,
                "worker-1",
                AttemptReport::Succeeded {
                    output: json!({"notes": ["ok"]}),
                },
            )
            .unwrap();
        let snapshot = store.get_run(&run.run.id).unwrap();
        let completed = snapshot
            .tasks
            .iter()
            .find(|task| task.id == claim.task_id)
            .unwrap();
        assert_eq!(completed.state, TaskState::Succeeded);
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .filter(|task| task.state == TaskState::Ready)
                .count(),
            2
        );
        let advanced = snapshot
            .tasks
            .iter()
            .find(|task| task.item_id == claim.item_id && task.stage_index == 1)
            .unwrap();
        assert_eq!(advanced.state, TaskState::Ready);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn invalid_report_does_not_advance_state() {
        let (store, root) = test_store();
        let run = store
            .create_run(fixture_template(), fixture_items())
            .unwrap();
        let claim = store.claim_next_ready_task("worker-1").unwrap().unwrap();
        store
            .mark_attempt_running(&claim.attempt_id, "worker-1")
            .unwrap();
        assert!(store
            .submit_report(
                &claim.attempt_id,
                "worker-1",
                AttemptReport::Succeeded { output: json!({}) },
            )
            .unwrap_err()
            .contains("missing required key"));
        let snapshot = store.get_run(&run.run.id).unwrap();
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == claim.task_id)
                .unwrap()
                .state,
            TaskState::Running
        );
        assert_eq!(snapshot.attempts[0].state, AttemptState::Running);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn claim_keeps_a_single_global_active_attempt() {
        let (store, root) = test_store();
        store
            .create_run(fixture_template(), fixture_items())
            .unwrap();
        store
            .create_run(fixture_template(), fixture_items())
            .unwrap();
        let first = store.claim_next_ready_task("worker-1").unwrap().unwrap();
        assert!(store.claim_next_ready_task("worker-2").unwrap().is_none());
        store
            .mark_attempt_interrupted(&first.attempt_id, "worker-1", "Worker stopped")
            .unwrap();
        assert!(store.claim_next_ready_task("worker-2").unwrap().is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn only_the_current_worker_can_change_an_attempt() {
        let (store, root) = test_store();
        let run = store
            .create_run(fixture_template(), fixture_items())
            .unwrap();
        let claim = store.claim_next_ready_task("worker-1").unwrap().unwrap();
        assert!(store
            .mark_attempt_running(&claim.attempt_id, "worker-2")
            .unwrap_err()
            .contains("does not belong"));
        store
            .mark_attempt_running(&claim.attempt_id, "worker-1")
            .unwrap();
        assert!(store
            .submit_report(
                &claim.attempt_id,
                "worker-2",
                AttemptReport::Succeeded {
                    output: json!({"notes": []}),
                },
            )
            .unwrap_err()
            .contains("does not belong"));
        store
            .mark_attempt_interrupted(&claim.attempt_id, "worker-1", "Worker stopped")
            .unwrap();
        assert!(store
            .submit_report(
                &claim.attempt_id,
                "worker-1",
                AttemptReport::Succeeded {
                    output: json!({"notes": []}),
                },
            )
            .is_err());
        assert_eq!(
            store
                .get_run(&run.run.id)
                .unwrap()
                .tasks
                .into_iter()
                .find(|task| task.id == claim.task_id)
                .unwrap()
                .state,
            TaskState::Attention
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn interrupted_attempt_requires_explicit_retry() {
        let (store, root) = test_store();
        let run = store
            .create_run(fixture_template(), fixture_items())
            .unwrap();
        let claim = store.claim_next_ready_task("worker-1").unwrap().unwrap();
        store
            .mark_attempt_interrupted(&claim.attempt_id, "worker-1", "Worker exited")
            .unwrap();
        let snapshot = store.get_run(&run.run.id).unwrap();
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == claim.task_id)
                .unwrap()
                .state,
            TaskState::Attention
        );
        let other_item = store.claim_next_ready_task("worker-2").unwrap().unwrap();
        store
            .mark_attempt_interrupted(&other_item.attempt_id, "worker-2", "Worker exited")
            .unwrap();
        store.retry_task(&claim.task_id, &claim.attempt_id).unwrap();
        let retry = store.claim_next_ready_task("worker-3").unwrap().unwrap();
        assert_eq!(retry.task_id, claim.task_id);
        let after_retry = store.get_run(&run.run.id).unwrap();
        assert_eq!(
            after_retry
                .attempts
                .iter()
                .filter(|attempt| attempt.task_id == claim.task_id)
                .count(),
            2
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recovery_marks_active_attempts_attention_without_retrying() {
        let (store, root) = test_store();
        let run = store
            .create_run(fixture_template(), fixture_items())
            .unwrap();
        let claim = store.claim_next_ready_task("worker-1").unwrap().unwrap();
        store
            .mark_attempt_running(&claim.attempt_id, "worker-1")
            .unwrap();
        assert_eq!(
            store
                .recover_interrupted_attempts("Coffee restarted")
                .unwrap(),
            1
        );
        let snapshot = store.get_run(&run.run.id).unwrap();
        assert_eq!(snapshot.run.state, RunState::Running);
        assert_eq!(
            snapshot
                .tasks
                .iter()
                .find(|task| task.id == claim.task_id)
                .unwrap()
                .state,
            TaskState::Attention
        );
        assert_eq!(snapshot.attempts[0].state, AttemptState::Interrupted);
        assert!(store
            .submit_report(
                &claim.attempt_id,
                "worker-1",
                AttemptReport::Succeeded {
                    output: json!({"notes": []})
                },
            )
            .is_err());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn store_rejects_invalid_input_without_partial_run() {
        let (store, root) = test_store();
        let mut invalid_items = fixture_items();
        invalid_items[1].client_key = invalid_items[0].client_key.clone();
        assert!(store.create_run(fixture_template(), invalid_items).is_err());
        assert!(store.list_runs().unwrap().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reopening_store_keeps_snapshot_and_events() {
        let (store, root) = test_store();
        let run = store
            .create_run(fixture_template(), fixture_items())
            .unwrap();
        let path = store.path().to_path_buf();
        drop(store);
        let reopened = WorkflowStore::open_at(path).unwrap();
        let restored = reopened.get_run(&run.run.id).unwrap();
        assert_eq!(restored.tasks.len(), 4);
        assert_eq!(restored.events.len(), 1);
        let _ = fs::remove_dir_all(root);
    }
}
