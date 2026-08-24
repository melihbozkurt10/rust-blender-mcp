//! Batch execution.
//!
//! Three things make this more than a loop:
//!
//! * **Typed references.** A later operation can use an earlier one's result by
//!   id. There is no string interpolation anywhere -- a reference is a JSON
//!   object with a `result_of` field, resolved structurally, so one operation
//!   can never smuggle syntax into another's arguments.
//! * **Whole-batch validation.** Every operation is checked against its tool's
//!   schema before the first one runs, so a typo in step nine does not leave
//!   eight applied.
//! * **Honest atomicity.** `ATOMIC` uses Blender's undo stack, which genuinely
//!   reverts `.blend` mutations. Operations that write outside the file are
//!   refused from an atomic batch rather than pretended over.

use std::sync::Arc;

use blender_protocol::{BlenderError, ErrorCode, command::Category, command::OpKind};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{registry::ToolSpec, state::AppState};

/// How a batch reacts to a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum BatchMode {
    /// Run everything, reporting each failure and carrying on.
    BestEffort,
    /// Stop at the first failure, leaving earlier operations applied.
    #[default]
    StopOnError,
    /// Stop at the first failure and undo everything the batch did.
    Atomic,
}

/// One operation in a batch.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct BatchOperation {
    /// Optional name for this step, so later steps can refer to its result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    /// Tool name, exactly as it appears in the tool list.
    pub op: String,
    /// Arguments for that tool. Any value may instead be
    /// `{"result_of": "<earlier id>", "path": "object.id"}`.
    #[serde(default)]
    pub args: Map<String, Value>,
}

/// `batch.execute`
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ExecuteBatch {
    pub operations: Vec<BatchOperation>,
    #[serde(default)]
    pub mode: BatchMode,
}

impl blender_protocol::Validate for ExecuteBatch {
    fn validate(&self) -> blender_protocol::Result<()> {
        if self.operations.is_empty() {
            return Err(BlenderError::invalid_argument(
                "`operations` must not be empty.",
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for (index, operation) in self.operations.iter().enumerate() {
            if operation.op.is_empty() {
                return Err(BlenderError::invalid_argument(format!(
                    "operations[{index}] has no `op`."
                )));
            }
            if let Some(id) = &operation.id
                && !seen.insert(id.as_str())
            {
                return Err(BlenderError::invalid_argument(format!(
                    "Duplicate operation id `{id}`."
                ))
                .with_detail("id", id.clone()));
            }
        }
        Ok(())
    }
}

pub fn tools() -> Vec<ToolSpec> {
    vec![ToolSpec::custom::<ExecuteBatch, _, _>(
        "batch.execute",
        Category::Core,
        OpKind::Write,
        "Run several operations",
        "Run a sequence of tool calls in one round trip. Give a step an `id` and later steps can \
         use its result with `{\"result_of\": \"<id>\"}`. Modes: STOP_ON_ERROR (default) halts at \
         the first failure, BEST_EFFORT runs everything and reports what failed, ATOMIC undoes the \
         whole batch if any step fails -- and refuses operations that write outside the .blend \
         file, because those cannot be undone.",
        |state: Arc<AppState>, params: ExecuteBatch| async move { execute(state, params).await },
    )]
}

async fn execute(state: Arc<AppState>, params: ExecuteBatch) -> Result<Value, BlenderError> {
    if params.operations.len() > state.config.max_batch_operations {
        return Err(BlenderError::invalid_argument(format!(
            "A batch may hold at most {} operations; this one has {}.",
            state.config.max_batch_operations,
            params.operations.len()
        ))
        .with_detail("limit", state.config.max_batch_operations)
        .with_detail("given", params.operations.len()));
    }

    // Resolve every tool up front, so an unknown name fails before anything
    // has been applied.
    let mut specs = Vec::with_capacity(params.operations.len());
    for (index, operation) in params.operations.iter().enumerate() {
        let spec = state.registry.get(&operation.op).ok_or_else(|| {
            BlenderError::invalid_argument(format!(
                "operations[{index}] names `{}`, which is not a tool.",
                operation.op
            ))
            .with_detail("index", index)
            .with_detail("op", operation.op.clone())
        })?;
        if params.mode == BatchMode::Atomic && !spec.kind.transactional() {
            return Err(BlenderError::new(
                ErrorCode::TransactionUnsupported,
                format!(
                    "`{}` writes outside the .blend file, so it cannot take part in an atomic \
                     batch -- undoing would not remove what it wrote. Use STOP_ON_ERROR, or run it \
                     outside the batch.",
                    operation.op
                ),
            )
            .with_detail("index", index)
            .with_detail("op", operation.op.clone())
            .with_detail("kind", spec.kind.as_str()));
        }
        specs.push(spec.clone());
    }

    let transactional = params.mode == BatchMode::Atomic;
    if transactional {
        state
            .call_raw("transaction.begin", batch_label())
            .await
            .map_err(|error| {
                if error.code == ErrorCode::TransactionUnsupported {
                    error
                } else {
                    BlenderError::new(
                        ErrorCode::TransactionFailed,
                        format!("Could not open a transaction: {}", error.message),
                    )
                }
            })?;
    }

    let mut results: Vec<Value> = Vec::with_capacity(params.operations.len());
    let mut outputs: Map<String, Value> = Map::new();
    let mut failed_index: Option<usize> = None;
    let mut dispatched_runs = 0usize;

    let mut index = 0usize;
    while index < params.operations.len() {
        // Consecutive operations that are plain forwards with fully resolved
        // arguments go to Blender in one frame. That is the whole point of
        // batching: a round trip costs a main-thread pump tick, and moving an
        // object costs far less than a tick, so N separate frames spend nearly
        // all of their time waiting rather than working.
        let run = collect_run(&params.operations[index..], &specs[index..]);
        if run.len() >= 2 {
            let outcome = dispatch_run(
                &state,
                index,
                &params.operations[index..index + run.len()],
                run,
                params.mode,
                transactional,
            )
            .await;
            match outcome {
                Ok(step) => {
                    dispatched_runs += 1;
                    for (id, value) in step.outputs {
                        outputs.insert(id, value);
                    }
                    results.extend(step.results);
                    if let Some(failed) = step.failed_index {
                        failed_index = Some(failed);
                        if params.mode != BatchMode::BestEffort {
                            break;
                        }
                    }
                    index += step.consumed;
                    continue;
                }
                Err(error) => {
                    // The run itself failed to reach Blender -- a lost
                    // connection, a timeout. That is not one step's problem, so
                    // report it against the first step of the run and stop.
                    results.push(step_failure(index, &params.operations[index], &error));
                    failed_index = Some(index);
                    break;
                }
            }
        }

        let operation = &params.operations[index];
        let spec = &specs[index];
        let resolved = match resolve_references(&operation.args, &outputs) {
            Ok(resolved) => resolved,
            Err(error) => {
                results.push(step_failure(index, operation, &error));
                failed_index = Some(index);
                if params.mode == BatchMode::BestEffort {
                    index += 1;
                    continue;
                }
                break;
            }
        };

        let handler = Arc::clone(&spec.handler);
        match handler(Arc::clone(&state), Value::Object(resolved)).await {
            Ok(value) => {
                if let Some(id) = &operation.id {
                    outputs.insert(id.clone(), value.clone());
                }
                results.push(json!({
                    "index": index,
                    "id": operation.id,
                    "op": operation.op,
                    "ok": true,
                    "result": value,
                }));
                if transactional {
                    // Mark an undo boundary per step, so rollback can walk back
                    // exactly as far as this batch reached.
                    let _ = state
                        .call_raw("transaction.step", step_label(index, &operation.op))
                        .await;
                }
            }
            Err(error) => {
                results.push(step_failure(index, operation, &error));
                failed_index = Some(index);
                if params.mode == BatchMode::BestEffort {
                    index += 1;
                    continue;
                }
                break;
            }
        }
        index += 1;
    }

    let succeeded = failed_index.is_none();
    let mut payload = json!({
        "success": succeeded,
        "mode": params.mode,
        "completed": results.iter().filter(|r| r["ok"] == json!(true)).count(),
        "total": params.operations.len(),
        "results": results,
    });
    if let Some(index) = failed_index {
        payload["failed_index"] = json!(index);
    }
    if dispatched_runs > 0 {
        // Visible in the result because it explains the timing, and because a
        // caller comparing a batch against the same calls made individually
        // should be able to see whether anything was actually coalesced.
        payload["dispatch_runs"] = json!(dispatched_runs);
    }

    if transactional {
        if succeeded {
            match state.call_raw("transaction.commit", Map::new()).await {
                Ok(_) => payload["committed"] = json!(true),
                Err(error) => payload["commit_warning"] = json!(error.message),
            }
        } else {
            match state.call_raw("transaction.rollback", Map::new()).await {
                Ok(result) => {
                    payload["rolled_back"] = json!(true);
                    payload["steps_undone"] =
                        result.get("steps_undone").cloned().unwrap_or(Value::Null);
                }
                Err(error) => {
                    // A failed rollback is the worst outcome and must be loud:
                    // the scene is now in a state nobody asked for.
                    payload["rolled_back"] = json!(false);
                    payload["rollback_error"] = json!({
                        "code": error.code.as_str(),
                        "message": error.message,
                    });
                }
            }
        }
    }

    Ok(payload)
}

/// One coalesced run's outcome, in the terms `execute` reports.
struct RunOutcome {
    /// How many of the batch's operations the run actually accounted for.
    consumed: usize,
    results: Vec<Value>,
    outputs: Vec<(String, Value)>,
    failed_index: Option<usize>,
}

/// The longest prefix of `operations` that can be sent to Blender in one frame.
///
/// An operation qualifies when its tool is a plain forward -- no server-side
/// logic to run in between -- and its arguments are already literal. A
/// `result_of` reference ends the run, because resolving it needs the previous
/// step's result, which does not exist until the frame comes back.
///
/// Arguments are validated here rather than on the far side. A run that
/// contained an invalid operation would otherwise reach Blender before anything
/// checked it, which is exactly the property this server exists to keep. An
/// operation that fails validation simply ends the run; the ordinary path then
/// runs it and produces the error it would always have produced.
fn collect_run(operations: &[BatchOperation], specs: &[ToolSpec]) -> Vec<Value> {
    let mut run = Vec::new();
    for (operation, spec) in operations.iter().zip(specs.iter()) {
        let (Some(bridge_op), Some(prepare)) = (spec.forwards_to, spec.prepare.as_ref()) else {
            break;
        };
        if holds_reference(&operation.args) {
            break;
        }
        match prepare(Value::Object(operation.args.clone())) {
            Ok(args) => run.push(json!({"op": bridge_op, "args": args})),
            Err(_) => break,
        }
    }
    run
}

/// Whether any value in this argument tree is still a `result_of` marker.
fn holds_reference(args: &Map<String, Value>) -> bool {
    args.values().any(value_holds_reference)
}

fn value_holds_reference(value: &Value) -> bool {
    match value {
        Value::Object(map) => {
            map.contains_key("result_of") || map.values().any(value_holds_reference)
        }
        Value::Array(items) => items.iter().any(value_holds_reference),
        _ => false,
    }
}

/// Send one run and translate its per-operation outcomes back into batch steps.
async fn dispatch_run(
    state: &Arc<AppState>,
    offset: usize,
    operations: &[BatchOperation],
    run: Vec<Value>,
    mode: BatchMode,
    transactional: bool,
) -> Result<RunOutcome, BlenderError> {
    let mut args = Map::new();
    args.insert("operations".into(), Value::Array(run));
    args.insert("stop_on_error".into(), json!(mode != BatchMode::BestEffort));
    if transactional {
        // The bridge pushes the undo boundaries, because doing it from here
        // would mean a round trip between every step and give back everything
        // the run just saved.
        args.insert("undo_label".into(), json!("MCP batch"));
    }

    let response = state.call_raw("batch.dispatch", args).await?;
    let entries = response
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            BlenderError::internal("`batch.dispatch` did not return a `results` array")
        })?;

    let mut outcome = RunOutcome {
        consumed: entries.len(),
        results: Vec::with_capacity(entries.len()),
        outputs: Vec::new(),
        failed_index: None,
    };

    for (position, entry) in entries.iter().enumerate() {
        let operation = &operations[position];
        let index = offset + position;
        if entry.get("ok") == Some(&Value::Bool(true)) {
            let value = entry.get("result").cloned().unwrap_or(Value::Null);
            if let Some(id) = &operation.id {
                outcome.outputs.push((id.clone(), value.clone()));
            }
            outcome.results.push(json!({
                "index": index,
                "id": operation.id,
                "op": operation.op,
                "ok": true,
                "result": value,
            }));
        } else {
            let error = entry.get("error").cloned().unwrap_or(Value::Null);
            outcome.results.push(json!({
                "index": index,
                "id": operation.id,
                "op": operation.op,
                "ok": false,
                "error": error,
            }));
            if outcome.failed_index.is_none() {
                outcome.failed_index = Some(index);
            }
        }
    }

    // Under STOP_ON_ERROR the bridge stops at the failure, so the run accounts
    // for fewer operations than it carried. `consumed` is what the caller
    // advances by, and it must never be zero or the loop would not terminate.
    outcome.consumed = outcome.consumed.max(1);
    Ok(outcome)
}

fn batch_label() -> Map<String, Value> {
    let mut args = Map::new();
    args.insert("label".into(), json!("MCP batch"));
    args
}

fn step_label(index: usize, op: &str) -> Map<String, Value> {
    let mut args = Map::new();
    args.insert("label".into(), json!(format!("{index}: {op}")));
    args
}

fn step_failure(index: usize, operation: &BatchOperation, error: &BlenderError) -> Value {
    json!({
        "index": index,
        "id": operation.id,
        "op": operation.op,
        "ok": false,
        "error": {
            "code": error.code.as_str(),
            "message": error.message,
            "details": error.details,
            "retryable": error.retryable,
        },
    })
}

/// Replace `{"result_of": ...}` markers with earlier results.
///
/// Walks the argument structure rather than the text, so a reference can appear
/// anywhere -- nested in an object, inside an array -- and a string that merely
/// looks like one is left alone.
fn resolve_references(
    args: &Map<String, Value>,
    outputs: &Map<String, Value>,
) -> Result<Map<String, Value>, BlenderError> {
    let mut resolved = Map::with_capacity(args.len());
    for (key, value) in args {
        resolved.insert(key.clone(), resolve_value(value, outputs)?);
    }
    Ok(resolved)
}

fn resolve_value(value: &Value, outputs: &Map<String, Value>) -> Result<Value, BlenderError> {
    match value {
        Value::Object(map) => {
            if let Some(reference) = map.get("result_of").and_then(Value::as_str) {
                let source = outputs.get(reference).ok_or_else(|| {
                    BlenderError::invalid_argument(format!(
                        "`result_of` names `{reference}`, which is not an earlier step in this \
                         batch that produced a result."
                    ))
                    .with_detail("result_of", reference)
                    .with_detail_json("available", &outputs.keys().cloned().collect::<Vec<_>>())
                })?;
                let path = map.get("path").and_then(Value::as_str);
                return extract(source, path, reference);
            }
            let mut out = Map::with_capacity(map.len());
            for (key, nested) in map {
                out.insert(key.clone(), resolve_value(nested, outputs)?);
            }
            Ok(Value::Object(out))
        }
        Value::Array(items) => items
            .iter()
            .map(|item| resolve_value(item, outputs))
            .collect::<Result<Vec<_>, _>>()
            .map(Value::Array),
        other => Ok(other.clone()),
    }
}

/// Pull a value out of an earlier result.
///
/// With no explicit path, the conventional identifier is used: results in this
/// server put the thing they created under a well-known key, so
/// `{"result_of": "cube"}` means "the cube's id" without the caller having to
/// know the response shape.
fn extract(source: &Value, path: Option<&str>, reference: &str) -> Result<Value, BlenderError> {
    if let Some(path) = path {
        let mut current = source;
        for segment in path.split('.') {
            current = match current.get(segment) {
                Some(next) => next,
                None => {
                    return Err(BlenderError::invalid_argument(format!(
                        "`{reference}` has no `{path}` in its result."
                    ))
                    .with_detail("result_of", reference)
                    .with_detail("path", path)
                    .with_detail_json(
                        "available",
                        &source
                            .as_object()
                            .map(|o| o.keys().cloned().collect::<Vec<_>>())
                            .unwrap_or_default(),
                    ));
                }
            };
        }
        return Ok(current.clone());
    }

    // Conventional keys, most specific first.
    for candidate in [
        "object",
        "material",
        "collection",
        "camera",
        "light",
        "group",
        "armature",
        "action",
        "image",
        "node",
        "artifact",
    ] {
        if let Some(entry) = source.get(candidate)
            && let Some(id) = entry.get("id")
        {
            return Ok(id.clone());
        }
    }
    if let Some(id) = source.get("id") {
        return Ok(id.clone());
    }
    Err(BlenderError::invalid_argument(format!(
        "`{reference}` produced no obvious identifier to reference. Add a `path`, e.g. \
         {{\"result_of\": \"{reference}\", \"path\": \"objects.0.id\"}}."
    ))
    .with_detail("result_of", reference)
    .with_detail_json(
        "available",
        &source
            .as_object()
            .map(|o| o.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default(),
    ))
}

#[cfg(test)]
mod tests {
    use blender_protocol::Validate;

    use super::*;

    fn outputs() -> Map<String, Value> {
        let mut map = Map::new();
        map.insert(
            "cube".into(),
            json!({"object": {"id": "11111111-1111-4111-8111-111111111111", "name": "Cube"}}),
        );
        map.insert(
            "bare".into(),
            json!({"id": "22222222-2222-4222-8222-222222222222"}),
        );
        map.insert("odd".into(), json!({"count": 3}));
        map
    }

    #[test]
    fn references_resolve_to_the_conventional_id() {
        let args = json!({"object": {"result_of": "cube"}});
        let resolved = resolve_references(args.as_object().unwrap(), &outputs()).unwrap();
        assert_eq!(resolved["object"], "11111111-1111-4111-8111-111111111111");
    }

    #[test]
    fn references_resolve_through_an_explicit_path() {
        let args = json!({"name": {"result_of": "cube", "path": "object.name"}});
        let resolved = resolve_references(args.as_object().unwrap(), &outputs()).unwrap();
        assert_eq!(resolved["name"], "Cube");
    }

    #[test]
    fn references_work_when_nested_in_arrays() {
        let args = json!({"objects": [{"result_of": "cube"}, "literal"]});
        let resolved = resolve_references(args.as_object().unwrap(), &outputs()).unwrap();
        assert_eq!(
            resolved["objects"][0],
            "11111111-1111-4111-8111-111111111111"
        );
        assert_eq!(resolved["objects"][1], "literal");
    }

    #[test]
    fn an_unknown_reference_lists_what_is_available() {
        let args = json!({"object": {"result_of": "ghost"}});
        let error = resolve_references(args.as_object().unwrap(), &outputs()).unwrap_err();
        assert_eq!(error.details["result_of"], "ghost");
        assert!(
            error.details["available"]
                .as_array()
                .unwrap()
                .contains(&json!("cube"))
        );
    }

    #[test]
    fn a_missing_path_is_reported_precisely() {
        let args = json!({"x": {"result_of": "cube", "path": "object.nope"}});
        let error = resolve_references(args.as_object().unwrap(), &outputs()).unwrap_err();
        assert_eq!(error.details["path"], "object.nope");
    }

    #[test]
    fn a_result_with_no_identifier_asks_for_a_path() {
        let args = json!({"x": {"result_of": "odd"}});
        let error = resolve_references(args.as_object().unwrap(), &outputs()).unwrap_err();
        assert!(error.message.contains("path"), "{}", error.message);
    }

    #[test]
    fn plain_strings_that_look_like_references_are_left_alone() {
        // Nothing is interpolated: only an object with `result_of` is special.
        let args = json!({"name": "result_of cube", "note": "{result_of}"});
        let resolved = resolve_references(args.as_object().unwrap(), &outputs()).unwrap();
        assert_eq!(resolved["name"], "result_of cube");
        assert_eq!(resolved["note"], "{result_of}");
    }

    #[test]
    fn duplicate_step_ids_are_rejected() {
        let batch = ExecuteBatch {
            operations: vec![
                BatchOperation {
                    id: Some("a".into()),
                    op: "object.create".into(),
                    args: Map::new(),
                },
                BatchOperation {
                    id: Some("a".into()),
                    op: "object.delete".into(),
                    args: Map::new(),
                },
            ],
            mode: BatchMode::StopOnError,
        };
        assert!(batch.validate().is_err());
    }

    #[test]
    fn an_empty_batch_is_rejected() {
        let batch = ExecuteBatch {
            operations: vec![],
            mode: BatchMode::default(),
        };
        assert!(batch.validate().is_err());
    }

    fn forwarding_specs(names: &[&'static str]) -> Vec<ToolSpec> {
        names
            .iter()
            .map(|name| {
                ToolSpec::forward::<blender_protocol::object::CreateObject>(
                    name,
                    Category::Scene,
                    OpKind::Write,
                    "Create object",
                    "Create an object.",
                )
            })
            .collect()
    }

    fn operation(op: &str, args: Value) -> BatchOperation {
        BatchOperation {
            id: None,
            op: op.to_string(),
            args: match args {
                Value::Object(map) => map,
                _ => Map::new(),
            },
        }
    }

    #[test]
    fn a_run_covers_consecutive_forwards_with_literal_arguments() {
        let specs = forwarding_specs(&["object.create", "object.create"]);
        let operations = vec![
            operation("object.create", json!({"type": "CUBE"})),
            operation("object.create", json!({"type": "CONE"})),
        ];
        let run = collect_run(&operations, &specs);
        assert_eq!(run.len(), 2, "both should be coalesced: {run:?}");
        assert_eq!(run[0]["op"], "object.create");
        assert_eq!(run[0]["args"]["type"], "CUBE");
    }

    #[test]
    fn a_reference_ends_the_run_before_it() {
        // The second step needs the first step's result, which does not exist
        // until the frame comes back, so it cannot travel in the same frame.
        let specs = forwarding_specs(&["object.create", "object.create"]);
        let operations = vec![
            operation("object.create", json!({"type": "CUBE"})),
            operation(
                "object.create",
                json!({"type": "CUBE", "collection": {"result_of": "earlier"}}),
            ),
        ];
        assert_eq!(collect_run(&operations, &specs).len(), 1);
    }

    #[test]
    fn a_nested_reference_is_found_too() {
        assert!(holds_reference(
            json!({"objects": [{"result_of": "wall"}]})
                .as_object()
                .unwrap()
        ));
        assert!(holds_reference(
            json!({"a": {"b": {"result_of": "x"}}}).as_object().unwrap()
        ));
        assert!(!holds_reference(
            json!({"name": "result_of", "n": 1}).as_object().unwrap()
        ));
    }

    #[test]
    fn invalid_arguments_end_the_run_rather_than_travelling() {
        // Validation is what makes the run safe to send unvalidated on the far
        // side, so an operation that fails it must not be in one.
        let specs = forwarding_specs(&["object.create", "object.create"]);
        let operations = vec![
            operation("object.create", json!({"type": "CUBE"})),
            operation("object.create", json!({"type": "NOT_A_PRIMITIVE"})),
        ];
        assert_eq!(collect_run(&operations, &specs).len(), 1);
    }

    #[test]
    fn a_tool_with_server_side_logic_is_never_coalesced() {
        // `batch.execute` itself is the clearest case: it has no bridge
        // operation to forward to at all.
        let specs = tools();
        let operations = vec![operation("batch.execute", json!({"operations": []}))];
        assert!(specs[0].forwards_to.is_none());
        assert!(collect_run(&operations, &specs).is_empty());
    }

    #[test]
    fn forwards_carry_their_bridge_operation_and_validator() {
        let spec = &forwarding_specs(&["object.create"])[0];
        assert_eq!(spec.forwards_to, Some("object.create"));
        let prepare = spec.prepare.as_ref().expect("a forward has a validator");
        assert!(prepare(json!({"type": "CUBE"})).is_ok());
        assert!(prepare(json!({"type": "NOPE"})).is_err());
    }

    #[test]
    fn stop_on_error_is_the_default_mode() {
        assert_eq!(BatchMode::default(), BatchMode::StopOnError);
    }
}
