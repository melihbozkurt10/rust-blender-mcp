//! Commands: the finite, named operation set the bridge understands.
//!
//! A command is an `op` string plus a JSON argument object. The `op` set is
//! closed -- it is the union of the handlers registered in the Python
//! dispatcher -- and every argument object is produced by serialising a typed
//! Rust struct that has already been validated. Network input never becomes
//! Python source.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Which tool category an operation belongs to. Categories are the unit of
/// lazy loading in the MCP layer.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Core,
    Scene,
    Materials,
    ShaderNodes,
    Lights,
    Modifiers,
    Mesh,
    Animation,
    GeometryNodes,
    Camera,
    Render,
    ImportExport,
    UvTexture,
    Assets,
    Rigging,
    RigDiagnostics,
    Utilities,
    Workflows,
}

impl Category {
    pub const ALL: [Category; 18] = [
        Category::Core,
        Category::Scene,
        Category::Materials,
        Category::ShaderNodes,
        Category::Lights,
        Category::Modifiers,
        Category::Mesh,
        Category::Animation,
        Category::GeometryNodes,
        Category::Camera,
        Category::Render,
        Category::ImportExport,
        Category::UvTexture,
        Category::Assets,
        Category::Rigging,
        Category::RigDiagnostics,
        Category::Utilities,
        Category::Workflows,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Category::Core => "core",
            Category::Scene => "scene",
            Category::Materials => "materials",
            Category::ShaderNodes => "shader_nodes",
            Category::Lights => "lights",
            Category::Modifiers => "modifiers",
            Category::Mesh => "mesh",
            Category::Animation => "animation",
            Category::GeometryNodes => "geometry_nodes",
            Category::Camera => "camera",
            Category::Render => "render",
            Category::ImportExport => "import_export",
            Category::UvTexture => "uv_texture",
            Category::Assets => "assets",
            Category::Rigging => "rigging",
            Category::RigDiagnostics => "rig_diagnostics",
            Category::Utilities => "utilities",
            Category::Workflows => "workflows",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Category::ALL.into_iter().find(|c| c.id() == s)
    }

    /// One-line summary shown by `list_tool_categories`.
    pub const fn description(self) -> &'static str {
        match self {
            Category::Core => {
                "Connection status, capability negotiation and tool-category control. Always enabled."
            }
            Category::Scene => "Scenes, objects, collections, selection, parenting and transforms.",
            Category::Materials => {
                "Material data-blocks, Principled BSDF properties and slot assignment."
            }
            Category::ShaderNodes => {
                "Generic shader node graph editing: nodes, links, sockets and defaults."
            }
            Category::Lights => "Point/sun/spot/area lights, their properties and aiming.",
            Category::Modifiers => "Add, configure, reorder, apply and copy object modifiers.",
            Category::Mesh => {
                "Mesh editing: extrude, inset, bevel, subdivide, cleanup and analysis."
            }
            Category::Animation => "Keyframes, F-curves, actions, interpolation and NLA strips.",
            Category::GeometryNodes => {
                "Geometry node groups, graphs, interfaces and modifier attachment."
            }
            Category::Camera => "Cameras, lenses, depth of field, tracking and automatic framing.",
            Category::Render => {
                "Render settings, engine selection, stills and viewport screenshots."
            }
            Category::ImportExport => {
                "Import and export of FBX, OBJ, glTF, USD, STL, PLY, Alembic and more."
            }
            Category::UvTexture => {
                "UV maps, unwrapping, packing, seams, images and texture baking."
            }
            Category::Assets => "External asset providers: search, metadata, download and import.",
            Category::Rigging => "Armatures, bones, vertex groups, weights and constraints.",
            Category::RigDiagnostics => {
                "Rig health, naming, weight and symmetry analysis, plus guided fixes."
            }
            Category::Utilities => {
                "Scene cleanup, batch rename, statistics, duplicate and missing-texture hunts."
            }
            Category::Workflows => {
                "Deterministic multi-step production workflows composed server-side."
            }
        }
    }
}

/// Side-effect classification. Drives retry policy, transaction eligibility,
/// permission checks and log verbosity.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum OpKind {
    /// Observes scene state without mutating it. Safe to retry.
    Read,
    /// Mutates the .blend data. Undo-able, so eligible for atomic batches.
    Write,
    /// Touches the world outside the .blend file -- writes render output,
    /// exports geometry, downloads assets. Never rolled back automatically.
    ExternalSideEffect,
}

impl OpKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            OpKind::Read => "READ",
            OpKind::Write => "WRITE",
            OpKind::ExternalSideEffect => "EXTERNAL_SIDE_EFFECT",
        }
    }

    /// Reads may be transparently retried after a dropped connection; writes
    /// may not, because the bridge may have applied them before dying.
    pub const fn retry_safe(self) -> bool {
        matches!(self, OpKind::Read)
    }

    /// Only pure `.blend` mutations can participate in an undo-backed
    /// transaction.
    pub const fn transactional(self) -> bool {
        matches!(self, OpKind::Read | OpKind::Write)
    }
}

/// One operation to run inside Blender.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Command {
    /// Dotted operation name, e.g. `object.transform`.
    pub op: String,
    /// Validated, normalised arguments.
    #[serde(default)]
    pub args: Map<String, Value>,
}

impl Command {
    pub fn new(op: impl Into<String>) -> Self {
        Self {
            op: op.into(),
            args: Map::new(),
        }
    }

    pub fn with_args(op: impl Into<String>, args: Map<String, Value>) -> Self {
        Self {
            op: op.into(),
            args,
        }
    }

    /// Build a command from any serialisable, already-validated parameter
    /// struct.
    pub fn from_params<T: Serialize>(
        op: impl Into<String>,
        params: &T,
    ) -> Result<Self, serde_json::Error> {
        let value = serde_json::to_value(params)?;
        let args = match value {
            Value::Object(map) => map,
            Value::Null => Map::new(),
            other => {
                let mut map = Map::new();
                map.insert("value".into(), other);
                map
            }
        };
        Ok(Self {
            op: op.into(),
            args,
        })
    }

    pub fn arg(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.args.insert(key.into(), value.into());
        self
    }
}

/// A batch step's reference to an earlier step's output.
///
/// Typed, not textual: there is no string interpolation anywhere in batch
/// execution, so a step can never smuggle syntax into another step's arguments.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResultRef {
    /// `id` of an earlier operation in the same batch.
    pub result_of: String,
    /// Optional dotted path into that operation's result, e.g. `object.id`.
    /// Defaults to the conventional primary id for the producing operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn category_ids_round_trip() {
        for c in Category::ALL {
            assert_eq!(Category::parse(c.id()), Some(c));
        }
    }

    #[test]
    fn category_all_is_exhaustive() {
        // Guards against adding a variant without adding it to ALL.
        let json = serde_json::to_value(Category::ALL).unwrap();
        assert_eq!(json.as_array().unwrap().len(), Category::ALL.len());
        let unique: std::collections::BTreeSet<_> = Category::ALL.iter().map(|c| c.id()).collect();
        assert_eq!(unique.len(), Category::ALL.len());
    }

    #[test]
    fn external_side_effects_are_not_transactional() {
        assert!(!OpKind::ExternalSideEffect.transactional());
        assert!(OpKind::Write.transactional());
        assert!(!OpKind::Write.retry_safe());
    }

    #[test]
    fn from_params_flattens_struct_into_args() {
        #[derive(Serialize)]
        struct P {
            name: String,
        }
        let cmd = Command::from_params(
            "object.create",
            &P {
                name: "Cube".into(),
            },
        )
        .unwrap();
        assert_eq!(cmd.op, "object.create");
        assert_eq!(cmd.args.get("name").unwrap(), "Cube");
    }
}
