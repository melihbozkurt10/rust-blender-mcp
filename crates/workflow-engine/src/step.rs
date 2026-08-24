//! What a workflow records about each step it takes.

use blender_protocol::BlenderError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How a step ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Completed,
    Failed,
    /// Not attempted, because an earlier step failed.
    Skipped,
}

/// One step of a workflow.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Step {
    /// What this step was for, in words a person can read.
    pub name: String,
    /// The bridge operation it ran, when it ran one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    pub status: StepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<StepError>,
    /// How long the step took, in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// A failure, flattened for the report.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct StepError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub details: serde_json::Map<String, Value>,
}

impl From<&BlenderError> for StepError {
    fn from(error: &BlenderError) -> Self {
        Self {
            code: error.code.as_str().to_string(),
            message: error.message.clone(),
            details: error.details.clone(),
        }
    }
}

impl Step {
    pub fn completed(name: impl Into<String>, op: Option<String>, result: Value) -> Self {
        Self {
            name: name.into(),
            op,
            status: StepStatus::Completed,
            result: Some(result),
            error: None,
            duration_ms: None,
        }
    }

    pub fn failed(name: impl Into<String>, op: Option<String>, error: &BlenderError) -> Self {
        Self {
            name: name.into(),
            op,
            status: StepStatus::Failed,
            result: None,
            error: Some(StepError::from(error)),
            duration_ms: None,
        }
    }

    pub fn skipped(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            op: None,
            status: StepStatus::Skipped,
            result: None,
            error: None,
            duration_ms: None,
        }
    }

    pub fn with_duration(mut self, millis: u64) -> Self {
        self.duration_ms = Some(millis);
        self
    }

    pub fn is_failure(&self) -> bool {
        self.status == StepStatus::Failed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failure_carries_the_code_and_details() {
        let error = BlenderError::not_found("object", "Cube");
        let step = Step::failed("create the wall", Some("object.create".into()), &error);
        assert!(step.is_failure());
        let recorded = step.error.unwrap();
        assert_eq!(recorded.code, "OBJECT_NOT_FOUND");
        assert_eq!(recorded.details["reference"], "Cube");
    }

    #[test]
    fn a_skipped_step_records_no_operation() {
        let step = Step::skipped("render the turntable");
        assert_eq!(step.status, StepStatus::Skipped);
        assert!(step.op.is_none() && step.result.is_none() && step.error.is_none());
    }
}
