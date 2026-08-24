//! Scene tools: summary, settings, world and statistics.

use blender_protocol::{
    command::{Category, OpKind},
    scene::{SceneSettings, WorldSettings},
};

use super::NoParams;
use crate::registry::ToolSpec;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<NoParams>(
            "scene.summary",
            Category::Core,
            OpKind::Read,
            "Scene summary",
            "A compact snapshot of the scene: object counts by type, material and collection \
             totals, selection, active camera, frame range and render engine. Start here.",
        ),
        ToolSpec::forward::<NoParams>(
            "scene.get",
            Category::Scene,
            OpKind::Read,
            "Get scene settings",
            "Frame range, frame rate, units, gravity, 3D cursor, active camera, render resolution \
             and top-level collections.",
        ),
        ToolSpec::forward::<SceneSettings>(
            "scene.settings.update",
            Category::Scene,
            OpKind::Write,
            "Update scene settings",
            "Change frame range, current frame, frame rate, unit system and scale, active camera, \
             3D cursor or gravity. Only the fields provided are touched.",
        ),
        ToolSpec::forward::<NoParams>(
            "scene.world.get",
            Category::Scene,
            OpKind::Read,
            "Get world settings",
            "The world background colour, strength and environment texture, plus whether the film \
             is transparent.",
        ),
        ToolSpec::forward::<WorldSettings>(
            "scene.world.update",
            Category::Scene,
            OpKind::Write,
            "Update world settings",
            "Set the background colour and strength, attach an HDRI environment texture and rotate \
             it, or switch the film to transparent. Creates the world node graph if needed.",
        ),
        ToolSpec::forward::<NoParams>(
            "scene.statistics",
            Category::Scene,
            OpKind::Read,
            "Scene statistics",
            "Geometry and data-block totals for the whole scene: vertices, edges, faces, triangles, \
             materials, images, modifiers, hidden objects and an estimate of texture memory.",
        ),
        ToolSpec::custom::<NoParams, _, _>(
            "scene.snapshot",
            Category::Scene,
            OpKind::Read,
            "Take a scene snapshot",
            "Return the current scene revision, so a later `scene.diff` can report what changed since. \n             Also reports how far back the server can still diff from, so a long-running caller \n             knows when its marker is about to age out.",
            |state: std::sync::Arc<crate::state::AppState>, _params| async move {
                serde_json::to_value(state.cache.snapshot())
                    .map_err(|error| blender_protocol::BlenderError::internal(error.to_string()))
            },
        ),
        ToolSpec::custom::<DiffParams, _, _>(
            "scene.diff",
            Category::Scene,
            OpKind::Read,
            "What changed since a revision",
            "Report what has changed since a revision from `scene.snapshot`: entities created, \n             modified and deleted, meshes whose indices are now stale, node graphs to re-read, and \n             the current selection. Changes are folded, so an object created and then edited five \n             times appears once. A revision that has fallen out of history returns \n             REVISION_EXPIRED rather than a partial answer.",
            |state: std::sync::Arc<crate::state::AppState>, params: DiffParams| async move {
                let diff = state.cache.diff(
                    scene_cache::Revision(params.from_revision),
                    params.to_revision.map(scene_cache::Revision),
                )?;
                serde_json::to_value(diff)
                    .map_err(|error| blender_protocol::BlenderError::internal(error.to_string()))
            },
        ),
    ]
}

/// `scene.diff`
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct DiffParams {
    /// Revision to compare from, as returned by `scene.snapshot`. Exclusive.
    pub from_revision: u64,
    /// Revision to compare to. Defaults to the current one. Inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_revision: Option<u64>,
}

impl blender_protocol::Validate for DiffParams {
    fn validate(&self) -> blender_protocol::Result<()> {
        if let Some(to) = self.to_revision
            && to < self.from_revision
        {
            return Err(blender_protocol::BlenderError::invalid_argument(format!(
                "`to_revision` {to} is before `from_revision` {}.",
                self.from_revision
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_is_a_core_read() {
        let summary = tools()
            .into_iter()
            .find(|t| t.name == "scene.summary")
            .unwrap();
        assert_eq!(summary.category, Category::Core);
        assert_eq!(summary.kind, OpKind::Read);
    }

    #[test]
    fn diff_rejects_an_inverted_range_before_asking_the_cache() {
        use blender_protocol::Validate;
        let params = DiffParams {
            from_revision: 10,
            to_revision: Some(2),
        };
        assert!(params.validate().is_err());
        assert!(
            DiffParams {
                from_revision: 2,
                to_revision: Some(10)
            }
            .validate()
            .is_ok()
        );
        assert!(
            DiffParams {
                from_revision: 2,
                to_revision: None
            }
            .validate()
            .is_ok()
        );
    }

    #[test]
    fn no_scene_tool_takes_a_free_form_payload() {
        // Every scene tool must have a typed schema with declared properties,
        // or `additionalProperties: false` on an empty object.
        for tool in tools() {
            let schema = serde_json::to_value(&*tool.schema).unwrap();
            assert_eq!(schema["type"], "object", "{}", tool.name);
        }
    }
}
