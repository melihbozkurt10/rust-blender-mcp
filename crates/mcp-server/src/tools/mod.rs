//! Tool definitions, one module per category.
//!
//! Tool *names* are dotted and match the bridge operation they invoke
//! one-for-one wherever a tool is a straight forward. Where a tool does work in
//! Rust before touching Blender -- workflows, batching, framing maths -- the
//! name still follows the same scheme, so a reader can tell what a call does
//! without a lookup table.

pub mod animation;
pub mod assets;
pub mod batch;
pub mod camera;
pub mod collection;
pub mod geometry_nodes;
pub mod io;
pub mod light;
pub mod material;
pub mod mesh;
pub mod modifier;
pub mod object;
pub mod render;
pub mod rigging;
pub mod scene;
pub mod selection;
pub mod shader;
pub mod surface;
pub mod system;
pub mod utilities;
pub mod uv;
pub mod workflows;

use blender_protocol::Validate;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::registry::ToolSpec;

/// For tools that take no arguments.
///
/// An explicit empty struct rather than `()`, so the generated schema is an
/// object with no properties -- which is what MCP clients expect -- instead of
/// `null`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoParams {}

impl Validate for NoParams {}

/// Every tool this build provides.
pub fn all() -> Vec<ToolSpec> {
    let mut tools = Vec::new();
    tools.extend(system::tools());
    tools.extend(scene::tools());
    tools.extend(object::tools());
    tools.extend(collection::tools());
    tools.extend(selection::tools());
    tools.extend(material::tools());
    tools.extend(shader::tools());
    tools.extend(light::tools());
    tools.extend(modifier::tools());
    tools.extend(mesh::tools());
    tools.extend(animation::tools());
    tools.extend(geometry_nodes::tools());
    tools.extend(camera::tools());
    tools.extend(render::tools());
    tools.extend(io::tools());
    tools.extend(uv::tools());
    tools.extend(rigging::tools());
    tools.extend(utilities::tools());
    tools.extend(assets::tools());
    tools.extend(batch::tools());
    tools.extend(workflows::tools());
    tools.extend(surface::tools());
    tools
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn tool_names_are_unique() {
        let tools = all();
        let unique: BTreeSet<&str> = tools.iter().map(|t| t.name).collect();
        assert_eq!(
            unique.len(),
            tools.len(),
            "duplicate tool name in the registry"
        );
    }

    #[test]
    fn tool_names_are_valid_for_mcp() {
        for tool in all() {
            assert!(!tool.name.is_empty());
            assert!(tool.name.len() <= 128, "{} is too long", tool.name);
            assert!(
                tool.name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')),
                "`{}` contains a character MCP tool names may not use",
                tool.name
            );
            assert!(
                !tool.name.starts_with('.') && !tool.name.ends_with('.'),
                "{}",
                tool.name
            );
        }
    }

    #[test]
    fn every_tool_is_described() {
        for tool in all() {
            assert!(
                !tool.description.is_empty(),
                "{} has no description",
                tool.name
            );
            assert!(!tool.title.is_empty(), "{} has no title", tool.name);
            // A description that does not end in a full stop is usually a
            // truncated sentence.
            assert!(
                tool.description.ends_with('.') || tool.description.ends_with(')'),
                "`{}` description looks unfinished: {}",
                tool.name,
                tool.description
            );
        }
    }

    #[test]
    fn no_tool_offers_code_execution() {
        // The one invariant the whole architecture rests on.
        const FORBIDDEN: [&str; 12] = [
            "execute_python",
            "run_python",
            "eval_python",
            "execute_script",
            "run_script",
            "python",
            "shell",
            "exec",
            "eval",
            "command",
            "subprocess",
            "system",
        ];
        for tool in all() {
            let name = tool.name.to_ascii_lowercase();
            for forbidden in FORBIDDEN {
                // `system.` as a namespace is fine; a tool *called* `system` is not.
                let is_namespace = name.starts_with(&format!("{forbidden}."));
                assert!(
                    is_namespace || !name.split(['.', '_']).any(|part| part == forbidden),
                    "`{}` looks like a code-execution tool",
                    tool.name
                );
            }
        }
    }
}
