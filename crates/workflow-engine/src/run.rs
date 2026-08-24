//! Running a workflow and reporting what happened.

use blender_protocol::BlenderError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::{
    executor::Executor,
    rollback::{Compensation, RollbackReport},
    step::Step,
};

/// A workflow in progress.
pub struct Run<'a> {
    name: &'static str,
    executor: &'a dyn Executor,
    steps: Vec<Step>,
    compensations: Vec<Compensation>,
    created: Map<String, Value>,
    /// Set once a step has failed; later steps are recorded as skipped.
    failure: Option<BlenderError>,
}

/// What a finished workflow reports.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkflowReport {
    pub workflow: String,
    pub success: bool,
    pub steps: Vec<Step>,
    /// Entities the workflow created, keyed by role: `camera`, `key_light`, and
    /// so on. Every one carries a stable id, so a caller can carry on working
    /// with them.
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub created: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::step::StepError>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback: Option<RollbackReport>,
}

impl<'a> Run<'a> {
    pub fn new(name: &'static str, executor: &'a dyn Executor) -> Self {
        Self {
            name,
            executor,
            steps: Vec::new(),
            compensations: Vec::new(),
            created: Map::new(),
            failure: None,
        }
    }

    /// Whether the workflow is still going.
    pub fn is_ok(&self) -> bool {
        self.failure.is_none()
    }

    /// Run one operation as a named step.
    ///
    /// After a failure this records the step as skipped and does nothing, so a
    /// workflow body can be written as a straight line without a check between
    /// every call.
    pub async fn step(&mut self, name: &str, op: &str, args: Value) -> Option<Value> {
        if self.failure.is_some() {
            self.steps.push(Step::skipped(name));
            return None;
        }
        match self.executor.call(op, args).await {
            Ok(value) => {
                self.steps
                    .push(Step::completed(name, Some(op.to_string()), value.clone()));
                Some(value)
            }
            Err(error) => {
                self.steps
                    .push(Step::failed(name, Some(op.to_string()), &error));
                self.failure = Some(error);
                None
            }
        }
    }

    /// Run a step whose failure is not fatal.
    ///
    /// For the parts of a workflow that are nice to have: setting a viewport
    /// colour, say, where a failure should be reported but should not throw
    /// away the work already done.
    pub async fn optional_step(&mut self, name: &str, op: &str, args: Value) -> Option<Value> {
        if self.failure.is_some() {
            self.steps.push(Step::skipped(name));
            return None;
        }
        match self.executor.call(op, args).await {
            Ok(value) => {
                self.steps
                    .push(Step::completed(name, Some(op.to_string()), value.clone()));
                Some(value)
            }
            Err(error) => {
                self.steps
                    .push(Step::failed(name, Some(op.to_string()), &error));
                None
            }
        }
    }

    /// Record a step that did work in Rust rather than in Blender.
    pub fn note(&mut self, name: &str, result: Value) {
        if self.failure.is_some() {
            self.steps.push(Step::skipped(name));
            return;
        }
        self.steps.push(Step::completed(name, None, result));
    }

    /// Fail the run from Rust, without an operation.
    pub fn fail(&mut self, name: &str, error: BlenderError) {
        self.steps.push(Step::failed(name, None, &error));
        self.failure = Some(error);
    }

    /// Register something the workflow created, under a role name.
    pub fn created(&mut self, role: &str, value: Value) {
        self.created.insert(role.to_string(), value);
    }

    /// Register how to undo something the workflow created.
    pub fn compensate(&mut self, compensation: Compensation) {
        self.compensations.push(compensation);
    }

    /// Convenience: record a created object and how to remove it.
    pub fn created_object(&mut self, role: &str, result: &Value) -> Option<String> {
        let id = result
            .get("object")
            .and_then(|o| o.get("id"))
            .and_then(Value::as_str)?;
        self.created(role, result.get("object").cloned().unwrap_or(Value::Null));
        self.compensate(Compensation::delete_object(id));
        Some(id.to_string())
    }

    /// Convenience: record a created material and how to remove it.
    pub fn created_material(&mut self, role: &str, result: &Value) -> Option<String> {
        let id = result
            .get("material")
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)?;
        self.created(role, result.get("material").cloned().unwrap_or(Value::Null));
        self.compensate(Compensation::delete_material(id));
        Some(id.to_string())
    }

    /// Finish, rolling back on failure when asked.
    pub async fn finish(mut self, rollback_on_failure: bool) -> WorkflowReport {
        let error = self.failure.take();
        let rollback = match (&error, rollback_on_failure) {
            (Some(_), true) => Some(self.roll_back().await),
            _ => None,
        };

        WorkflowReport {
            workflow: self.name.to_string(),
            success: error.is_none(),
            steps: self.steps,
            created: if error.is_some() && rollback.as_ref().is_some_and(|r| r.complete) {
                // Nothing survives a clean rollback, and reporting ids that no
                // longer exist would be worse than reporting none.
                Map::new()
            } else {
                self.created
            },
            error: error.as_ref().map(crate::step::StepError::from),
            rollback,
        }
    }

    /// Run the compensations in reverse order.
    async fn roll_back(&mut self) -> RollbackReport {
        let mut report = RollbackReport::default();
        while let Some(compensation) = self.compensations.pop() {
            let args = Value::Object(compensation.args.clone());
            match self.executor.call(&compensation.op, args).await {
                Ok(_) => report.record_success(compensation.describes),
                Err(error) => report.record_failure(compensation.describes, &error),
            }
        }
        report.finish()
    }
}

/// Pull an id out of a result, whatever shape it came back in.
pub fn id_of(result: &Value, key: &str) -> Option<String> {
    result
        .get(key)
        .and_then(|entry| entry.get("id"))
        .or_else(|| result.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned)
}

/// Build an argument object without ceremony.
pub fn args(pairs: Value) -> Value {
    match pairs {
        Value::Object(_) => pairs,
        other => json!({ "value": other }),
    }
}

#[cfg(test)]
mod tests {
    use blender_protocol::ErrorCode;

    use super::*;
    use crate::executor::recording::{Recorder, Reply};

    fn object_result(id: &str) -> Value {
        json!({"object": {"id": id, "name": "Thing"}})
    }

    #[tokio::test]
    async fn a_clean_run_reports_every_step() {
        let recorder = Recorder::new(object_result("a"));
        let mut run = Run::new("test", &recorder);
        run.step("first", "object.create", json!({})).await;
        run.step("second", "object.create", json!({})).await;
        let report = run.finish(true).await;

        assert!(report.success);
        assert_eq!(report.steps.len(), 2);
        assert!(report.rollback.is_none());
        assert!(report.error.is_none());
    }

    #[tokio::test]
    async fn a_failure_skips_the_rest_and_reports_where() {
        let recorder = Recorder::new(object_result("a")).expect(
            "object.transform",
            Reply::Fail(BlenderError::not_found("object", "Ghost")),
        );
        let mut run = Run::new("test", &recorder);
        run.step("create", "object.create", json!({})).await;
        run.step("move", "object.transform", json!({})).await;
        run.step("colour", "material.create", json!({})).await;
        let report = run.finish(false).await;

        assert!(!report.success);
        assert_eq!(report.steps[0].status, crate::StepStatus::Completed);
        assert_eq!(report.steps[1].status, crate::StepStatus::Failed);
        assert_eq!(report.steps[2].status, crate::StepStatus::Skipped);
        assert_eq!(report.error.unwrap().code, "OBJECT_NOT_FOUND");
        assert!(
            !recorder.called("material.create"),
            "the skipped step must not run"
        );
    }

    #[tokio::test]
    async fn rollback_removes_what_the_workflow_created_in_reverse() {
        let recorder = Recorder::new(object_result("a")).expect(
            "object.transform",
            Reply::Fail(BlenderError::invalid_argument("no")),
        );
        let mut run = Run::new("test", &recorder);

        let first = run
            .step("create one", "object.create", json!({}))
            .await
            .unwrap();
        run.created_object("one", &first);
        let second = run
            .step("create two", "object.create", json!({}))
            .await
            .unwrap();
        run.created_object("two", &second);
        run.step("move", "object.transform", json!({})).await;

        let report = run.finish(true).await;
        assert!(!report.success);
        let rollback = report.rollback.expect("a rollback report");
        assert!(rollback.complete, "{rollback:?}");
        assert_eq!(rollback.undone.len(), 2);
        assert!(
            report.created.is_empty(),
            "a clean rollback leaves nothing to report as created"
        );

        let deletes: Vec<_> = recorder
            .ops()
            .into_iter()
            .filter(|op| op == "object.delete")
            .collect();
        assert_eq!(deletes.len(), 2);
    }

    #[tokio::test]
    async fn a_failed_rollback_is_reported_loudly() {
        let recorder = Recorder::new(object_result("a"))
            .expect(
                "object.transform",
                Reply::Fail(BlenderError::invalid_argument("no")),
            )
            .expect(
                "object.delete",
                Reply::Fail(BlenderError::new(ErrorCode::PermissionDenied, "linked")),
            );
        let mut run = Run::new("test", &recorder);
        let created = run
            .step("create", "object.create", json!({}))
            .await
            .unwrap();
        run.created_object("one", &created);
        run.step("move", "object.transform", json!({})).await;

        let report = run.finish(true).await;
        let rollback = report.rollback.unwrap();
        assert!(!rollback.complete);
        assert_eq!(rollback.failed.len(), 1);
        assert!(
            !report.created.is_empty(),
            "what survived a failed rollback must still be reported"
        );
    }

    #[tokio::test]
    async fn an_optional_step_does_not_stop_the_run() {
        let recorder = Recorder::new(object_result("a")).expect(
            "scene.world.update",
            Reply::Fail(BlenderError::invalid_argument("no world")),
        );
        let mut run = Run::new("test", &recorder);
        run.optional_step("set the world", "scene.world.update", json!({}))
            .await;
        run.step("create", "object.create", json!({})).await;
        let report = run.finish(true).await;

        assert!(
            report.success,
            "an optional failure must not fail the workflow"
        );
        assert!(report.steps[0].is_failure());
        assert_eq!(report.steps[1].status, crate::StepStatus::Completed);
    }

    #[tokio::test]
    async fn failing_from_rust_records_a_step_with_no_operation() {
        let recorder = Recorder::new(Value::Null);
        let mut run = Run::new("test", &recorder);
        run.fail(
            "check the input",
            BlenderError::invalid_argument("bad radius"),
        );
        let report = run.finish(false).await;
        assert!(!report.success);
        assert!(report.steps[0].op.is_none());
        assert!(
            recorder.ops().is_empty(),
            "nothing should have been sent to Blender"
        );
    }

    #[test]
    fn ids_are_found_wherever_they_sit() {
        assert_eq!(id_of(&object_result("x"), "object").as_deref(), Some("x"));
        assert_eq!(id_of(&json!({"id": "y"}), "object").as_deref(), Some("y"));
        assert_eq!(id_of(&json!({"count": 1}), "object"), None);
    }
}
