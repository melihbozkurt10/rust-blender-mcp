//! Import and export tools.
//!
//! Paths are always a managed root plus a relative path. The server resolves
//! and canonicalises before Blender is told anything, so `../../etc/passwd` is
//! a validation error rather than a filesystem event.

use std::sync::Arc;

use blender_protocol::command::{Category, OpKind};
use blender_protocol::{
    BlenderError, ErrorCode,
    capabilities::CapabilityKind,
    io::{Export, FileFormat, Import, ManagedRoot},
};
use serde_json::{Value, json};

use super::NoParams;
use crate::{
    artifacts,
    config::{Root, resolve_managed_path},
    registry::ToolSpec,
    state::AppState,
};

const IO: Category = Category::ImportExport;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<NoParams>(
            "io.capabilities",
            IO,
            OpKind::Read,
            "Import and export capabilities",
            "Which formats this Blender build can actually import and export, and which operator \
             each one uses. Check here before assuming a format is available.",
        ),
        ToolSpec::custom::<Import, _, _>(
            "io.import",
            IO,
            OpKind::ExternalSideEffect,
            "Import a file",
            "Import FBX, OBJ, glTF, USD, STL, PLY, Alembic, Collada or SVG. The path is relative \
             to a managed root -- project, downloads or temp -- never absolute.",
            |state: Arc<AppState>, params: Import| async move { import(state, params).await },
        ),
        ToolSpec::custom::<Export, _, _>(
            "io.export",
            IO,
            OpKind::ExternalSideEffect,
            "Export to a file",
            "Export the scene, the selection, named objects or a collection. Format-specific \
             options that this build does not have are dropped rather than failing the export, \
             and options the format cannot carry are refused up front.",
            |state: Arc<AppState>, params: Export| async move { export(state, params).await },
        ),
        ToolSpec::custom::<SaveFile, _, _>(
            "file.save",
            IO,
            OpKind::ExternalSideEffect,
            "Save the .blend file",
            "Save the current file, either over itself or to a new path inside a managed root.",
            |state: Arc<AppState>, params: SaveFile| async move { save(state, params).await },
        ),
        ToolSpec::forward::<NoParams>(
            "file.info",
            IO,
            OpKind::Read,
            "File information",
            "Where the current .blend lives, whether it has unsaved changes, and which Blender is \
             running.",
        ),
    ]
}

/// `file.save`
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct SaveFile {
    /// Where to save, relative to a managed root. Omit to save over the file
    /// that is already open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<blender_protocol::io::ManagedPath>,
    /// Compress the .blend file.
    #[serde(default)]
    pub compress: bool,
}

impl blender_protocol::Validate for SaveFile {
    fn validate(&self) -> blender_protocol::Result<()> {
        match &self.destination {
            Some(path) => path.validate(),
            None => Ok(()),
        }
    }
}

/// Map the protocol's managed root onto the server's directory layout.
fn root_of(root: ManagedRoot) -> Root {
    match root {
        ManagedRoot::Project => Root::Project,
        ManagedRoot::Downloads => Root::Downloads,
        ManagedRoot::Renders => Root::Renders,
        ManagedRoot::Exports => Root::Exports,
        ManagedRoot::Temp => Root::Temp,
    }
}

async fn import(state: Arc<AppState>, params: Import) -> Result<Value, BlenderError> {
    let format = params.resolved_format()?;
    state.require_capability(CapabilityKind::ImportFormat, &format_name(format))?;

    let root = root_of(params.source.root);
    let absolute = resolve_managed_path(&state.config.root_path(root), &params.source.path)?;
    if !absolute.exists() {
        return Err(BlenderError::new(
            ErrorCode::InvalidPath,
            format!(
                "`{}` does not exist under the {} root.",
                params.source.path,
                params.source.root.id()
            ),
        )
        .with_detail("path", params.source.path.clone())
        .with_detail("root", params.source.root.id()));
    }

    let mut args =
        serde_json::to_value(&params).map_err(|e| BlenderError::internal(e.to_string()))?;
    args["source_path"] = json!(absolute.display().to_string());
    args["format"] = json!(format_name(format));

    let mut result = state.client.call("io.import", args).await?;
    if let Some(object) = result.as_object_mut() {
        object.insert("source".into(), json!(params.source));
    }
    Ok(result)
}

async fn export(state: Arc<AppState>, params: Export) -> Result<Value, BlenderError> {
    let format = params.resolved_format()?;
    state.require_capability(CapabilityKind::ExportFormat, &format_name(format))?;

    let root = root_of(params.destination.root);
    let root_path = state.config.root_path(root);
    std::fs::create_dir_all(&root_path).map_err(|error| {
        BlenderError::new(
            ErrorCode::PermissionDenied,
            format!("Could not prepare `{}`: {error}", root_path.display()),
        )
    })?;
    let absolute = resolve_managed_path(&root_path, &params.destination.path)?;
    if let Some(parent) = absolute.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            BlenderError::new(
                ErrorCode::PermissionDenied,
                format!("Could not create `{}`: {error}", parent.display()),
            )
        })?;
    }

    let mut args =
        serde_json::to_value(&params).map_err(|e| BlenderError::internal(e.to_string()))?;
    args["destination_path"] = json!(absolute.display().to_string());
    args["format"] = json!(format_name(format));

    let result = state.client.call("io.export", args).await?;

    let artifact = state.artifacts.register(
        &state.config,
        root,
        &absolute,
        artifacts::mime_for(format.extension()),
    )?;

    Ok(json!({
        "artifact": artifact,
        "format": format_name(format),
        "objects": result.get("objects").cloned().unwrap_or(Value::Null),
        "operator": result.get("operator").cloned().unwrap_or(Value::Null),
    }))
}

async fn save(state: Arc<AppState>, params: SaveFile) -> Result<Value, BlenderError> {
    let mut args = serde_json::Map::new();
    args.insert("compress".into(), json!(params.compress));

    let root = params.destination.as_ref().map(|d| root_of(d.root));
    if let Some(destination) = &params.destination {
        let root_path = state.config.root_path(root_of(destination.root));
        std::fs::create_dir_all(&root_path).map_err(|error| {
            BlenderError::new(
                ErrorCode::PermissionDenied,
                format!("Could not prepare `{}`: {error}", root_path.display()),
            )
        })?;
        let absolute = resolve_managed_path(&root_path, &destination.path)?;
        if let Some(parent) = absolute.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        args.insert(
            "destination_path".into(),
            json!(absolute.display().to_string()),
        );
    }

    let result = state.call_raw("file.save", args).await?;

    // Only a save into a managed root produces an artifact; saving over an
    // existing file elsewhere is the user's own file, not ours to index.
    if let (Some(root), Some(path)) = (root, result.get("path").and_then(Value::as_str))
        && let Ok(artifact) = state.artifacts.register(
            &state.config,
            root,
            std::path::Path::new(path),
            "application/x-blender",
        )
    {
        return Ok(json!({"artifact": artifact, "path": path}));
    }
    Ok(result)
}

/// The protocol's name for a format, which is what the bridge expects.
fn format_name(format: FileFormat) -> String {
    serde_json::to_value(format)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_else(|| format.extension().to_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_names_match_the_bridge_table() {
        assert_eq!(format_name(FileFormat::Fbx), "FBX");
        assert_eq!(format_name(FileFormat::Glb), "GLB");
        assert_eq!(format_name(FileFormat::Usdz), "USDZ");
    }

    #[test]
    fn roots_map_one_to_one() {
        assert_eq!(root_of(ManagedRoot::Exports), Root::Exports);
        assert_eq!(root_of(ManagedRoot::Project), Root::Project);
    }

    #[test]
    fn import_and_export_are_external_side_effects() {
        for tool in tools() {
            let expected = match tool.name {
                "io.capabilities" | "file.info" => OpKind::Read,
                _ => OpKind::ExternalSideEffect,
            };
            assert_eq!(tool.kind, expected, "{}", tool.name);
        }
    }

    #[test]
    fn no_io_tool_takes_an_absolute_path() {
        for tool in tools() {
            let schema = serde_json::to_string(&*tool.schema).unwrap();
            assert!(
                !schema.contains("source_path") && !schema.contains("destination_path"),
                "`{}` exposes a raw path argument",
                tool.name
            );
        }
    }
}
