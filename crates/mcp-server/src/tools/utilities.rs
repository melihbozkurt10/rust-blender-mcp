//! Scene housekeeping tools.

use blender_protocol::{
    command::{Category, OpKind},
    scene::{
        ApplySceneTransforms, BatchRename, CleanupOptions, FindDuplicates, PurgeOrphans,
        SceneMeshAnalysis,
    },
};

use super::NoParams;
use crate::registry::ToolSpec;

const UTILITIES: Category = Category::Utilities;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<CleanupOptions>(
            "scene.cleanup",
            UTILITIES,
            OpKind::Write,
            "Clean up the scene",
            "Run named cleanup passes: purge orphans, remove empty collections, delete loose \
             geometry, drop unused material slots, merge duplicate materials, recalculate normals \
             and remove misconfigured modifiers. Every pass is opt-in, and `dry_run` reports what \
             each would do without doing it.",
        ),
        ToolSpec::forward::<PurgeOrphans>(
            "scene.purge_orphans",
            UTILITIES,
            OpKind::Write,
            "Purge unused data",
            "Delete data-blocks nothing references. Supports a dry run that lists what would go.",
        ),
        ToolSpec::forward::<BatchRename>(
            "scene.batch_rename",
            UTILITIES,
            OpKind::Write,
            "Rename in bulk",
            "Rename objects, materials, collections, meshes, actions or images with find/replace, \
             a regular expression, prefixes, suffixes, case conversion and numbering. Returns the \
             full rename map, and defaults to reporting rather than applying when `dry_run` is set.",
        ),
        ToolSpec::forward::<ApplySceneTransforms>(
            "scene.apply_transforms",
            UTILITIES,
            OpKind::Write,
            "Apply transforms in bulk",
            "Bake location, rotation or scale into mesh data across many objects. Objects that \
             share their data, or come from a linked library, are skipped and listed rather than \
             silently changed.",
        ),
        ToolSpec::forward::<SceneMeshAnalysis>(
            "scene.mesh_analysis",
            UTILITIES,
            OpKind::Read,
            "Analyse every mesh",
            "Run the mesh diagnostics across the scene and list the objects with problems: \
             non-manifold edges, degenerate faces, loose geometry, missing UVs and unapplied scale.",
        ),
        ToolSpec::forward::<FindDuplicates>(
            "scene.find_duplicates",
            UTILITIES,
            OpKind::Read,
            "Find duplicates",
            "Find objects stacked in the same place with the same geometry, materials that differ \
             only by a `.001` suffix, and meshes shared between objects.",
        ),
        ToolSpec::forward::<NoParams>(
            "scene.find_missing_textures",
            UTILITIES,
            OpKind::Read,
            "Find missing textures",
            "Images whose file is no longer on disk, and which materials use them.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_destructive_utility_offers_a_dry_run() {
        for tool in tools().into_iter().filter(|t| t.kind == OpKind::Write) {
            let schema = serde_json::to_string(&*tool.schema).unwrap();
            assert!(
                schema.contains("dry_run") || tool.name == "scene.apply_transforms",
                "`{}` changes the scene but offers no dry run",
                tool.name
            );
        }
    }

    #[test]
    fn cleanup_is_flagged_destructive() {
        let cleanup = tools()
            .into_iter()
            .find(|t| t.name == "scene.cleanup")
            .unwrap();
        assert_eq!(
            cleanup.to_tool().annotations.unwrap().destructive_hint,
            Some(true)
        );
    }
}
