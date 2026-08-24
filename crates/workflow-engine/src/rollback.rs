//! Undoing what a workflow made.
//!
//! Workflows do not use Blender's undo stack: they may run headless, where
//! there is none, and they often span operations a user would not want folded
//! into one undo step. Instead each creating step registers a compensation --
//! "delete this object", "delete this material" -- and a failed workflow runs
//! them in reverse.
//!
//! This is honest about its limits. A compensation removes something the
//! workflow created; it cannot restore something the workflow changed, and it
//! cannot unwrite a file. Steps of that kind register no compensation and the
//! report says so.

use blender_protocol::BlenderError;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// One thing to undo.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Compensation {
    /// What it undoes, for the report.
    pub describes: String,
    /// The operation that undoes it.
    pub op: String,
    /// Arguments for that operation.
    pub args: serde_json::Map<String, Value>,
}

impl Compensation {
    /// Delete an object the workflow created.
    pub fn delete_object(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            describes: format!("object {id}"),
            op: "object.delete".into(),
            args: json!({"objects": [id], "delete_data": true})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Delete a material the workflow created.
    pub fn delete_material(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            describes: format!("material {id}"),
            op: "material.delete".into(),
            args: json!({"material": id, "force": true})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Delete a collection the workflow created.
    pub fn delete_collection(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            describes: format!("collection {id}"),
            op: "collection.delete".into(),
            args: json!({"collection": id, "delete_objects": false})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Delete a geometry node group the workflow created.
    pub fn delete_node_group(id: impl Into<String>) -> Self {
        let id = id.into();
        Self {
            describes: format!("node group {id}"),
            op: "geometry_nodes.group.delete".into(),
            args: json!({"group": id, "force": true})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Remove a modifier the workflow added.
    pub fn remove_modifier(object: impl Into<String>, modifier: impl Into<String>) -> Self {
        let object = object.into();
        let modifier = modifier.into();
        Self {
            describes: format!("modifier {modifier} on {object}"),
            op: "modifier.remove".into(),
            args: json!({"object": object, "modifier": modifier})
                .as_object()
                .cloned()
                .unwrap_or_default(),
        }
    }
}

/// What happened when a workflow tried to clean up after itself.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RollbackReport {
    /// Whether every compensation succeeded.
    pub complete: bool,
    /// What was undone.
    pub undone: Vec<String>,
    /// What could not be undone, and why. Anything listed here is still in the
    /// scene and needs a person to look at it.
    pub failed: Vec<FailedCompensation>,
}

/// A compensation that did not work.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct FailedCompensation {
    pub describes: String,
    pub code: String,
    pub message: String,
}

impl RollbackReport {
    pub fn record_success(&mut self, describes: String) {
        self.undone.push(describes);
    }

    pub fn record_failure(&mut self, describes: String, error: &BlenderError) {
        self.failed.push(FailedCompensation {
            describes,
            code: error.code.as_str().to_string(),
            message: error.message.clone(),
        });
    }

    pub fn finish(mut self) -> Self {
        self.complete = self.failed.is_empty();
        self
    }

    /// A sentence a person can act on.
    pub fn summary(&self) -> String {
        if self.complete {
            format!("Rolled back {} item(s).", self.undone.len())
        } else {
            format!(
                "Rolled back {} item(s); {} could not be removed and are still in the scene.",
                self.undone.len(),
                self.failed.len()
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use blender_protocol::ErrorCode;

    use super::*;

    #[test]
    fn compensations_name_what_they_undo() {
        let compensation = Compensation::delete_object("abc");
        assert_eq!(compensation.op, "object.delete");
        assert_eq!(compensation.args["objects"][0], "abc");
        assert!(compensation.describes.contains("abc"));
    }

    #[test]
    fn material_compensation_forces_the_delete() {
        // The workflow just assigned the material, so it has users; without
        // `force` the cleanup would refuse and leave it behind.
        let compensation = Compensation::delete_material("m1");
        assert_eq!(compensation.args["force"], true);
    }

    #[test]
    fn collection_compensation_keeps_the_objects() {
        // Deleting a collection the workflow made must not take objects that
        // existed before it with it.
        let compensation = Compensation::delete_collection("c1");
        assert_eq!(compensation.args["delete_objects"], false);
    }

    #[test]
    fn a_report_is_complete_only_when_nothing_failed() {
        let mut report = RollbackReport::default();
        report.record_success("object a".into());
        let report = report.finish();
        assert!(report.complete);
        assert!(report.summary().contains("Rolled back 1"));

        let mut report = RollbackReport::default();
        report.record_success("object a".into());
        report.record_failure(
            "material m".into(),
            &BlenderError::new(ErrorCode::PermissionDenied, "in use"),
        );
        let report = report.finish();
        assert!(!report.complete);
        assert!(report.summary().contains("still in the scene"));
    }
}
