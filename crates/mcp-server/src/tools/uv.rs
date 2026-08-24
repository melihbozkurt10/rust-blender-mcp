//! UV, image and texture-baking tools.

use std::sync::Arc;

use blender_protocol::{
    BlenderError,
    capabilities::CapabilityKind,
    command::{Category, OpKind},
    uv::{
        Bake, ImageRefParams, ListImages, LoadImage, MarkSeam, PackIslands, RepathImages, Unwrap,
        UnwrapMethod, UvMapOperation, UvObjectParams,
    },
};
use serde_json::{Value, json};

use crate::{
    artifacts,
    config::{Root, resolve_managed_path},
    registry::ToolSpec,
    state::AppState,
};

const UV: Category = Category::UvTexture;

pub fn tools() -> Vec<ToolSpec> {
    let mut tools = vec![
        ToolSpec::forward::<UvObjectParams>(
            "uv.maps.list",
            UV,
            OpKind::Read,
            "List UV maps",
            "The UV maps on a mesh, and which is active for editing and for rendering.",
        ),
        ToolSpec::forward::<UvMapOperation>(
            "uv.map.create",
            UV,
            OpKind::Write,
            "Create a UV map",
            "Add a UV map, optionally copying the active one. Blender allows at most eight.",
        ),
        ToolSpec::forward::<UvMapOperation>(
            "uv.map.delete",
            UV,
            OpKind::Write,
            "Delete a UV map",
            "Remove a UV map from a mesh.",
        ),
        ToolSpec::forward::<UvMapOperation>(
            "uv.map.set_active",
            UV,
            OpKind::Write,
            "Set the active UV map",
            "Choose which UV map editing and rendering use.",
        ),
        ToolSpec::forward::<PackIslands>(
            "uv.pack_islands",
            UV,
            OpKind::Write,
            "Pack UV islands",
            "Fit UV islands into the 0-1 square with a margin, optionally packing several objects \
             into one shared space for atlasing.",
        ),
        ToolSpec::forward::<UvObjectParams>(
            "uv.average_island_scale",
            UV,
            OpKind::Write,
            "Average island scale",
            "Give every UV island a consistent texel density.",
        ),
        ToolSpec::forward::<MarkSeam>(
            "uv.mark_seam",
            UV,
            OpKind::Write,
            "Mark UV seams",
            "Mark edges as seams, which is what the angle-based and conformal unwrappers cut along.",
        ),
        ToolSpec::forward::<MarkSeam>(
            "uv.clear_seam",
            UV,
            OpKind::Write,
            "Clear UV seams",
            "Unmark edges as seams.",
        ),
        ToolSpec::forward::<ListImages>(
            "image.list",
            UV,
            OpKind::Read,
            "List images",
            "Loaded images, optionally only the ones whose file is missing or that nothing uses.",
        ),
        ToolSpec::forward::<ImageRefParams>(
            "image.get",
            UV,
            OpKind::Read,
            "Get an image",
            "One image: size, channels, colour space, whether it is packed and whether its file is \
             still there.",
        ),
        ToolSpec::custom::<LoadImage, _, _>(
            "image.load",
            UV,
            OpKind::Write,
            "Load an image",
            "Load an image from a managed root. Set `colorspace` to `Non-Color` for normal, \
             roughness and metallic maps -- treating a data map as sRGB is the single most common \
             texturing mistake.",
            |state: Arc<AppState>, params: LoadImage| async move {
                let root = match params.source.root {
                    blender_protocol::io::ManagedRoot::Project => Root::Project,
                    blender_protocol::io::ManagedRoot::Downloads => Root::Downloads,
                    blender_protocol::io::ManagedRoot::Renders => Root::Renders,
                    blender_protocol::io::ManagedRoot::Exports => Root::Exports,
                    blender_protocol::io::ManagedRoot::Temp => Root::Temp,
                };
                let absolute =
                    resolve_managed_path(&state.config.root_path(root), &params.source.path)?;
                let mut args = serde_json::to_value(&params)
                    .map_err(|e| BlenderError::internal(e.to_string()))?;
                args["source_path"] = json!(absolute.display().to_string());
                state.client.call("image.load", args).await
            },
        ),
        ToolSpec::custom::<RepathImages, _, _>(
            "image.repath",
            UV,
            OpKind::Write,
            "Point images at a folder",
            "Aim images that have lost their file at a folder inside a managed root, matching by              file name, and optionally pack the pixels into the .blend so the link cannot break              again. This is what an asset imported from elsewhere needs: it carries the texture              paths of wherever it came from, and saving the file to a new place rewrites those              paths to keep pointing at the old one.",
            |state: Arc<AppState>, params: RepathImages| async move {
                let root = match params.directory.root {
                    blender_protocol::io::ManagedRoot::Project => Root::Project,
                    blender_protocol::io::ManagedRoot::Downloads => Root::Downloads,
                    blender_protocol::io::ManagedRoot::Renders => Root::Renders,
                    blender_protocol::io::ManagedRoot::Exports => Root::Exports,
                    blender_protocol::io::ManagedRoot::Temp => Root::Temp,
                };
                let absolute =
                    resolve_managed_path(&state.config.root_path(root), &params.directory.path)?;
                let mut args = serde_json::to_value(&params)
                    .map_err(|e| BlenderError::internal(e.to_string()))?;
                args["directory"] = json!(absolute.display().to_string());
                state.client.call("image.repath", args).await
            },
        ),
        ToolSpec::forward::<ImageRefParams>(
            "image.reload",
            UV,
            OpKind::Write,
            "Reload an image",
            "Re-read an image from disk, after it has been changed outside Blender.",
        ),
        ToolSpec::forward::<ImageRefParams>(
            "image.remove",
            UV,
            OpKind::Write,
            "Remove an image",
            "Delete an image data-block.",
        ),
        ToolSpec::custom::<Bake, _, _>(
            "texture.bake",
            UV,
            OpKind::ExternalSideEffect,
            "Bake a texture",
            "Bake a pass -- normal, ambient occlusion, diffuse, roughness, emission and the rest -- \
             to an image, optionally from high-poly sources onto a low-poly target. Needs Cycles \
             and a UV map. The result is written to the managed renders directory.",
            |state: Arc<AppState>, params: Bake| async move { bake(state, params).await },
        ),
    ];

    // One tool per unwrap method rather than a method enum: they take
    // genuinely different arguments, and a model choosing between named tools
    // makes fewer mistakes than one filling in a mode field.
    for (name, method, description) in UNWRAP_TOOLS {
        tools.push(ToolSpec::custom::<Unwrap, _, _>(
            name,
            UV,
            OpKind::Write,
            "Unwrap UVs",
            description,
            move |state: Arc<AppState>, mut params: Unwrap| async move {
                params.method = Some(method);
                state.call_typed(name, &params).await
            },
        ));
    }
    tools
}

/// The unwrap tools, their method, and what each is for.
const UNWRAP_TOOLS: [(&str, UnwrapMethod, &str); 7] = [
    (
        "uv.unwrap.angle_based",
        UnwrapMethod::AngleBased,
        "Unwrap along marked seams using angle-based flattening. Blender's default, and the right \
         starting point for hard-surface models with seams already marked.",
    ),
    (
        "uv.unwrap.conformal",
        UnwrapMethod::Conformal,
        "Unwrap along marked seams using least-squares conformal maps. Better than angle-based on \
         organic shapes, worse on flat panels.",
    ),
    (
        "uv.smart_project",
        UnwrapMethod::SmartProject,
        "Unwrap by splitting on face angle, with no seams required. The fastest way to get usable \
         UVs onto a model that has none.",
    ),
    (
        "uv.cube_project",
        UnwrapMethod::CubeProject,
        "Project UVs from six axis-aligned directions at once. Ideal for boxy, hard-surface \n         geometry where every face is roughly axis-aligned; it produces overlapping islands \n         on anything curved.",
    ),
    (
        "uv.cylinder_project",
        UnwrapMethod::CylinderProject,
        "Project UVs cylindrically around the object Z axis. The right choice for pipes, \n         bottles, tree trunks and limbs; it leaves a seam where the projection wraps.",
    ),
    (
        "uv.sphere_project",
        UnwrapMethod::SphereProject,
        "Project UVs spherically from the object centre. Suits balls, domes and planets, and \n         pinches at the poles the way any spherical map does.",
    ),
    (
        "uv.project_from_view",
        UnwrapMethod::ProjectFromView,
        "Project UVs from the current viewport view. Needs a running Blender UI.",
    ),
];

async fn bake(state: Arc<AppState>, params: Bake) -> Result<Value, BlenderError> {
    let bake_type = serde_json::to_value(params.bake_type)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| params.bake_type.blender_id().to_string());
    state.require_capability(CapabilityKind::BakeType, params.bake_type.blender_id())?;

    let format = params
        .format
        .unwrap_or(blender_protocol::render::ImageFormat::Png);
    let base = params
        .name
        .clone()
        .unwrap_or_else(|| format!("bake_{}", bake_type.to_lowercase()));
    let path = state
        .artifacts
        .allocate(&state.config, Root::Renders, &base, format.extension())?;

    let mut args =
        serde_json::to_value(&params).map_err(|e| BlenderError::internal(e.to_string()))?;
    args["output_path"] = json!(path.display().to_string());
    args["type"] = json!(params.bake_type.blender_id());
    args["format"] = json!(format.blender_id());

    let result = state.client.call("texture.bake", args).await?;

    let artifact = state.artifacts.register(
        &state.config,
        Root::Renders,
        &path,
        artifacts::mime_for(format.extension()),
    )?;

    Ok(json!({
        "artifact": artifact,
        "type": params.bake_type.blender_id(),
        "is_data": params.bake_type.is_data(),
        "width": params.width,
        "height": params.height,
        "image": result.get("image").cloned().unwrap_or(Value::Null),
        "duration_ms": result.get("duration_ms").cloned().unwrap_or(Value::Null),
        "connected": result.get("connected").cloned().unwrap_or(Value::Null),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_unwrap_method_has_its_own_tool() {
        let names: Vec<&str> = tools().iter().map(|t| t.name).collect();
        for (name, _, _) in UNWRAP_TOOLS {
            assert!(names.contains(&name), "{name} is missing");
        }
    }

    #[test]
    fn unwrap_descriptions_say_when_to_use_each() {
        for (name, _, description) in UNWRAP_TOOLS {
            assert!(
                description.len() > 60,
                "`{name}` needs a description that helps a caller choose"
            );
        }
    }

    #[test]
    fn baking_is_an_external_side_effect_and_takes_no_path() {
        let bake = tools()
            .into_iter()
            .find(|t| t.name == "texture.bake")
            .unwrap();
        assert_eq!(bake.kind, OpKind::ExternalSideEffect);
        let schema = serde_json::to_string(&*bake.schema).unwrap();
        assert!(!schema.contains("output_path"));
    }
}
