//! Render tools.
//!
//! Render output never goes where a caller says. The server allocates a path
//! inside its managed renders directory, tells Blender to write there, and
//! returns artifact references. A caller that wants the file elsewhere copies
//! it themselves, which is a decision they get to make explicitly.

use std::sync::Arc;

use blender_protocol::{
    BlenderError, ErrorCode,
    command::{Category, OpKind},
    render::{ExecuteRender, ImageFormat, RenderScope, RenderSettings, ViewportScreenshot},
};
use serde_json::{Value, json};

use super::NoParams;
use crate::{
    artifacts::{self, Artifact},
    config::Root,
    registry::ToolSpec,
    state::AppState,
};

const RENDER: Category = Category::Render;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<NoParams>(
            "render.settings.get",
            RENDER,
            OpKind::Read,
            "Get render settings",
            "Current engine, resolution, sampling, colour management and output format, plus the \
             engines this build actually has.",
        ),
        ToolSpec::custom::<RenderSettings, _, _>(
            "render.settings.update",
            RENDER,
            OpKind::Write,
            "Update render settings",
            "Change engine, resolution, samples, denoising, colour management or output format. \
             Only the fields provided are touched.",
            |state: Arc<AppState>, params: RenderSettings| async move {
                check_engine(&state, &params)?;
                state.call_typed("render.settings.update", &params).await
            },
        ),
        ToolSpec::custom::<SetEngine, _, _>(
            "render.engine.set",
            RENDER,
            OpKind::Write,
            "Set the render engine",
            "Switch between Cycles, EEVEE and Workbench. The build-specific identifier is resolved \
             for you, and an engine this build lacks is refused with the list of ones it has.",
            |state: Arc<AppState>, params: SetEngine| async move {
                state.call_typed("render.engine.set", &params).await
            },
        ),
        ToolSpec::custom::<ExecuteRender, _, _>(
            "render.execute",
            RENDER,
            OpKind::ExternalSideEffect,
            "Render",
            "Render a frame, a range, or the current frame, and return artifact references to the \
             files produced. Output lands in the managed renders directory; the caller chooses a \
             base name, not a path.",
            |state: Arc<AppState>, params: ExecuteRender| async move {
                execute_render(state, params).await
            },
        ),
        ToolSpec::custom::<ViewportScreenshot, _, _>(
            "render.viewport_screenshot",
            RENDER,
            OpKind::ExternalSideEffect,
            "Capture the viewport",
            "Grab the 3D viewport, or the camera view, without a full render. Needs a running \
             Blender UI: headless instances have no viewport, and say so.",
            |state: Arc<AppState>, params: ViewportScreenshot| async move {
                state.require_interactive("Viewport capture")?;

                let name = params
                    .name
                    .clone()
                    .unwrap_or_else(|| "viewport".to_string());
                let path = state
                    .artifacts
                    .allocate(&state.config, Root::Renders, &name, "png")?;

                let mut args = serde_json::to_value(&params)
                    .map_err(|e| BlenderError::internal(e.to_string()))?;
                args["output_path"] = json!(path.display().to_string());

                let result = state
                    .client
                    .call("render.viewport_screenshot", args)
                    .await?;
                Ok(collect_artifacts(&state, &result, ImageFormat::Png, None))
            },
        ),
        ToolSpec::custom::<ArtifactQuery, _, _>(
            "render.artifacts.list",
            RENDER,
            OpKind::Read,
            "List produced files",
            "The files this server has produced this session -- renders, bakes and exports -- with \
             their paths, sizes and ids.",
            |state: Arc<AppState>, params: ArtifactQuery| async move {
                let limit = params.limit.unwrap_or(20).clamp(1, 200) as usize;
                Ok(json!({
                    "artifacts": state.artifacts.recent(limit),
                    "total": state.artifacts.len(),
                }))
            },
        ),
    ]
}

/// `render.engine.set`
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SetEngine {
    /// `CYCLES`, `EEVEE` or `WORKBENCH`.
    pub engine: blender_protocol::render::RenderEngine,
}

impl blender_protocol::Validate for SetEngine {}

/// `render.artifacts.list`
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ArtifactQuery {
    /// How many to return, newest first. Default 20.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

impl blender_protocol::Validate for ArtifactQuery {}

/// Refuse an engine this build does not have, before Blender is asked.
fn check_engine(state: &AppState, settings: &RenderSettings) -> Result<(), BlenderError> {
    let Some(engine) = settings.engine else {
        return Ok(());
    };
    let Ok(capabilities) = state.capabilities() else {
        return Ok(());
    };
    if capabilities.render_engines.is_empty() {
        return Ok(());
    }
    if engine
        .candidates()
        .iter()
        .any(|candidate| capabilities.render_engines.contains(*candidate))
    {
        return Ok(());
    }
    Err(BlenderError::new(
        ErrorCode::CapabilityUnavailable,
        format!(
            "This Blender build has no {} engine.",
            engine.candidates().first().copied().unwrap_or("requested")
        ),
    )
    .with_detail_json("tried", &engine.candidates())
    .with_detail_json("available", &capabilities.render_engines))
}

async fn execute_render(
    state: Arc<AppState>,
    params: ExecuteRender,
) -> Result<Value, BlenderError> {
    check_engine(&state, &params.settings)?;

    let format = params.settings.format.unwrap_or(ImageFormat::Png);
    let base = params
        .name
        .clone()
        .unwrap_or_else(|| default_render_name(&params.scope));
    let path = state
        .artifacts
        .allocate(&state.config, Root::Renders, &base, format.extension())?;

    let mut args =
        serde_json::to_value(&params).map_err(|e| BlenderError::internal(e.to_string()))?;
    args["output_path"] = json!(path.display().to_string());
    // The bridge needs the format identifier Blender uses, not the protocol's.
    args["format"] = json!(format.blender_id());

    let result = state.client.call("render.execute", args).await?;
    Ok(collect_artifacts(&state, &result, format, Some(base)))
}

fn default_render_name(scope: &RenderScope) -> String {
    match scope {
        RenderScope::Frame(frame) => format!("render_{frame:04}"),
        RenderScope::Range { start, end, .. } => format!("render_{start:04}_{end:04}"),
        RenderScope::Current => "render".to_string(),
    }
}

/// Turn the bridge's file list into registered artifacts.
///
/// A file the bridge claims to have written but that is not on disk is
/// reported rather than silently dropped -- that is the failure mode that
/// wastes the most time when it goes unnoticed.
fn collect_artifacts(
    state: &AppState,
    result: &Value,
    format: ImageFormat,
    base: Option<String>,
) -> Value {
    let mut artifacts: Vec<Artifact> = Vec::new();
    let mut problems: Vec<Value> = Vec::new();

    if let Some(files) = result.get("files").and_then(Value::as_array) {
        for entry in files {
            let Some(path) = entry.get("path").and_then(Value::as_str) else {
                continue;
            };
            match state.artifacts.register(
                &state.config,
                Root::Renders,
                std::path::Path::new(path),
                artifacts::mime_for(format.extension()),
            ) {
                Ok(mut artifact) => {
                    artifact.frame = entry.get("frame").and_then(Value::as_i64).map(|f| f as i32);
                    artifact.width = result
                        .get("width")
                        .and_then(Value::as_u64)
                        .map(|v| v as u32);
                    artifact.height = result
                        .get("height")
                        .and_then(Value::as_u64)
                        .map(|v| v as u32);
                    artifact.engine = result
                        .get("engine")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    artifact.duration_ms = result.get("duration_ms").and_then(Value::as_u64);
                    artifacts.push(artifact);
                }
                Err(error) => problems.push(json!({
                    "path": path,
                    "error": error.message,
                })),
            }
        }
    }

    let mut payload = json!({
        "artifacts": artifacts,
        "count": artifacts.len(),
        "format": format.blender_id(),
        "mime_type": format.mime_type(),
    });
    if let Some(base) = base {
        payload["name"] = json!(base);
    }
    if let Some(engine) = result.get("engine") {
        payload["engine"] = engine.clone();
    }
    if let Some(duration) = result.get("duration_ms") {
        payload["duration_ms"] = duration.clone();
    }
    if !problems.is_empty() {
        payload["missing_outputs"] = Value::Array(problems);
    }
    payload
}

#[cfg(test)]
mod tests {
    use blender_protocol::render::RenderEngine;

    use super::*;

    #[test]
    fn rendering_is_an_external_side_effect() {
        let render = tools()
            .into_iter()
            .find(|t| t.name == "render.execute")
            .unwrap();
        assert_eq!(render.kind, OpKind::ExternalSideEffect);
        assert!(
            !render.kind.transactional(),
            "renders cannot be rolled back"
        );
    }

    #[test]
    fn default_names_describe_the_scope() {
        assert_eq!(default_render_name(&RenderScope::Current), "render");
        assert_eq!(default_render_name(&RenderScope::Frame(7)), "render_0007");
        assert_eq!(
            default_render_name(&RenderScope::Range {
                start: 1,
                end: 24,
                step: 1
            }),
            "render_0001_0024"
        );
    }

    #[test]
    fn no_render_tool_accepts_a_path() {
        // The whole point: a caller supplies a name, never a destination.
        for tool in tools() {
            let schema = serde_json::to_string(&*tool.schema).unwrap();
            assert!(
                !schema.contains("output_path") && !schema.contains("filepath"),
                "`{}` exposes a path argument",
                tool.name
            );
        }
    }

    #[test]
    fn engine_candidates_are_ordered_newest_first() {
        assert_eq!(RenderEngine::Eevee.candidates()[0], "BLENDER_EEVEE_NEXT");
    }
}
