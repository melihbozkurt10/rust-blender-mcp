//! The tool registry.
//!
//! Every tool is a name, a category, a side-effect class, a JSON schema
//! derived from a Rust type, and a handler. There is no dynamic dispatch on
//! caller-supplied strings anywhere: `call_tool` looks a name up in this map
//! and gets a function pointer or nothing.

pub mod activation;
pub mod category;

use std::{future::Future, pin::Pin, sync::Arc};

use blender_protocol::{
    BlenderError, Validate,
    command::{Category, OpKind},
};
use rmcp::model::{Tool, ToolAnnotations};
use schemars::JsonSchema;
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Map, Value};

use crate::state::AppState;

pub use activation::Activation;
pub use category::{CategoryInfo, CategorySet};

/// A boxed future, which is what a heterogeneous handler table needs.
pub type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// What every tool handler looks like once erased.
pub type Handler =
    Arc<dyn Fn(Arc<AppState>, Value) -> BoxFuture<Result<Value, BlenderError>> + Send + Sync>;

/// Decode and validate a tool's arguments without running it, returning the
/// canonical form to put on the wire.
///
/// Only a plain forward has one: a tool that does work in Rust has no
/// arguments to hand anyone else.
pub type Prepare = Arc<dyn Fn(Value) -> Result<Value, BlenderError> + Send + Sync>;

/// One registered tool.
#[derive(Clone)]
pub struct ToolSpec {
    pub name: &'static str,
    pub category: Category,
    pub kind: OpKind,
    pub title: &'static str,
    pub description: &'static str,
    pub schema: Arc<Map<String, Value>>,
    pub handler: Handler,
    /// The bridge operation this tool forwards to unchanged, if it does no work
    /// of its own. `None` for anything with server-side logic.
    ///
    /// Batching uses this to send a run of operations in one frame instead of
    /// one frame each, which is worth roughly an order of magnitude because a
    /// round trip costs a main-thread pump tick and the operations themselves
    /// usually do not.
    pub forwards_to: Option<&'static str>,
    /// Argument validation for a forward, separated from execution so a batch
    /// can validate a whole run before sending any of it.
    pub prepare: Option<Prepare>,
}

impl std::fmt::Debug for ToolSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToolSpec")
            .field("name", &self.name)
            .field("category", &self.category.id())
            .field("kind", &self.kind.as_str())
            .finish_non_exhaustive()
    }
}

impl ToolSpec {
    /// A tool that validates its arguments and forwards them to Blender
    /// unchanged, under the same operation name.
    ///
    /// This is the shape of most tools: the interesting work is the typed
    /// schema and the validation, not the plumbing.
    pub fn forward<P>(
        name: &'static str,
        category: Category,
        kind: OpKind,
        title: &'static str,
        description: &'static str,
    ) -> Self
    where
        // `Sync` because the payload is borrowed across the await that sends
        // it. Every parameter type is plain data, so this costs nothing.
        P: DeserializeOwned + Serialize + JsonSchema + Validate + Send + Sync + 'static,
    {
        let mut spec = Self::custom::<P, _, _>(
            name,
            category,
            kind,
            title,
            description,
            move |state, params| {
                let op = name;
                async move { state.call_typed(op, &params).await }
            },
        );
        spec.forwards_to = Some(name);
        spec.prepare = Some(Arc::new(move |raw: Value| {
            let params = decode::<P>(name, raw)?;
            params.validate()?;
            serde_json::to_value(&params).map_err(|error| {
                BlenderError::internal(format!("could not encode `{name}` arguments: {error}"))
            })
        }));
        spec
    }

    /// A tool with server-side logic: workflows, batching, cache queries.
    pub fn custom<P, F, Fut>(
        name: &'static str,
        category: Category,
        kind: OpKind,
        title: &'static str,
        description: &'static str,
        handler: F,
    ) -> Self
    where
        P: DeserializeOwned + JsonSchema + Validate + Send + 'static,
        F: Fn(Arc<AppState>, P) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Value, BlenderError>> + Send + 'static,
    {
        let handler = Arc::new(handler);
        let erased: Handler = Arc::new(move |state: Arc<AppState>, raw: Value| {
            let handler = Arc::clone(&handler);
            Box::pin(async move {
                let params = decode::<P>(name, raw)?;
                params.validate()?;
                handler(state, params).await
            })
        });

        Self {
            name,
            category,
            kind,
            title,
            description,
            schema: Arc::new(schema_for::<P>()),
            handler: erased,
            forwards_to: None,
            prepare: None,
        }
    }

    /// The MCP tool definition sent to clients.
    pub fn to_tool(&self) -> Tool {
        let annotations = ToolAnnotations::with_title(self.title)
            .read_only(self.kind == OpKind::Read)
            .destructive(self.is_destructive())
            .idempotent(self.kind == OpKind::Read)
            // Only the asset tools reach outside the local machine.
            .open_world(self.category == Category::Assets);

        Tool::new(self.name, self.description, Arc::clone(&self.schema))
            .annotate(annotations)
            .with_title(self.title)
    }

    /// Whether this tool can remove or overwrite existing work.
    ///
    /// Creating a cube is a write but not destructive; deleting one is. The
    /// distinction is what an MCP client uses to decide whether to ask the user
    /// first, so guessing it from `OpKind` alone would be wrong in both
    /// directions.
    fn is_destructive(&self) -> bool {
        if self.kind == OpKind::Read {
            return false;
        }
        const DESTRUCTIVE_MARKERS: [&str; 9] = [
            ".delete",
            ".remove",
            ".clear",
            ".apply",
            ".purge",
            ".cleanup",
            ".join",
            ".dissolve",
            ".merge",
        ];
        DESTRUCTIVE_MARKERS
            .iter()
            .any(|marker| self.name.contains(marker))
    }
}

/// Decode a tool's arguments, reporting the failure in the tool's own terms.
fn decode<P: DeserializeOwned>(tool: &str, raw: Value) -> Result<P, BlenderError> {
    let raw = match raw {
        Value::Null => Value::Object(Map::new()),
        other => other,
    };
    serde_json::from_value(raw).map_err(|error| {
        BlenderError::invalid_argument(format!(
            "`{tool}` arguments did not match its schema: {error}"
        ))
        .with_detail("tool", tool)
    })
}

/// Build the JSON schema for a parameter type.
///
/// Subschemas are inlined rather than referenced through `$defs`. MCP clients
/// vary in how well they resolve `$ref`, and a tool whose schema a client
/// cannot read is a tool the model will not call correctly.
pub fn schema_for<P: JsonSchema>() -> Map<String, Value> {
    let settings = schemars::generate::SchemaSettings::draft2020_12().with(|settings| {
        settings.inline_subschemas = true;
        settings.meta_schema = None;
    });
    let schema = settings.into_generator().into_root_schema_for::<P>();
    match schema.to_value() {
        Value::Object(mut map) => {
            // `title` on the root schema is the Rust type name, which is noise
            // in a tool listing.
            map.remove("title");
            map.remove("$schema");
            if !map.contains_key("type") {
                map.insert("type".into(), Value::String("object".into()));
            }
            map
        }
        other => {
            let mut map = Map::new();
            map.insert("type".into(), Value::String("object".into()));
            map.insert("x-schema".into(), other);
            map
        }
    }
}

/// Every tool the build knows about, plus which categories are live.
pub struct Registry {
    tools: Vec<ToolSpec>,
    activation: Activation,
}

impl Registry {
    pub fn new(tools: Vec<ToolSpec>, activation: Activation) -> Self {
        debug_assert!(
            {
                let mut names: Vec<&str> = tools.iter().map(|t| t.name).collect();
                names.sort_unstable();
                let before = names.len();
                names.dedup();
                names.len() == before
            },
            "duplicate tool names in the registry"
        );
        Self { tools, activation }
    }

    /// Every tool, enabled or not.
    pub fn all(&self) -> &[ToolSpec] {
        &self.tools
    }

    /// Tools currently visible to the client.
    pub fn visible(&self) -> Vec<&ToolSpec> {
        self.tools
            .iter()
            .filter(|tool| self.activation.is_enabled(tool.category))
            .collect()
    }

    /// Look a tool up by name, whether or not its category is enabled.
    ///
    /// Calling a tool from a disabled category succeeds: a model that
    /// remembers a tool from earlier in the conversation should not be
    /// punished for the category having been turned off, and the security
    /// boundary is the handler table, not the visibility list.
    pub fn get(&self, name: &str) -> Option<&ToolSpec> {
        self.tools.iter().find(|tool| tool.name == name)
    }

    pub fn activation(&self) -> &Activation {
        &self.activation
    }

    /// Category summaries for `list_tool_categories`.
    pub fn categories(&self) -> Vec<CategoryInfo> {
        Category::ALL
            .into_iter()
            .map(|category| CategoryInfo {
                id: category.id(),
                enabled: self.activation.is_enabled(category),
                always_on: category == Category::Core,
                tool_count: self.tools.iter().filter(|t| t.category == category).count(),
                description: category.description(),
            })
            .filter(|info| info.tool_count > 0)
            .collect()
    }

    /// Names of the tools in one category.
    pub fn tools_in(&self, category: Category) -> Vec<&'static str> {
        self.tools
            .iter()
            .filter(|tool| tool.category == category)
            .map(|tool| tool.name)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use blender_protocol::object::CreateObject;

    use super::*;

    fn registry() -> Registry {
        Registry::new(
            vec![
                ToolSpec::forward::<CreateObject>(
                    "object.create",
                    Category::Scene,
                    OpKind::Write,
                    "Create object",
                    "Create an object.",
                ),
                ToolSpec::forward::<CreateObject>(
                    "object.delete",
                    Category::Scene,
                    OpKind::Write,
                    "Delete object",
                    "Delete an object.",
                ),
            ],
            Activation::lazy(&[Category::Core]),
        )
    }

    #[test]
    fn schemas_have_no_dangling_refs() {
        let schema = schema_for::<CreateObject>();
        let text = serde_json::to_string(&schema).unwrap();
        assert!(
            !text.contains("$ref"),
            "subschemas must be inlined so every client can read them"
        );
        assert!(!text.contains("$defs"));
        assert_eq!(schema["type"], "object");
        assert!(schema.contains_key("properties"), "got {schema:?}");
    }

    #[test]
    fn destructive_tools_are_flagged_but_creation_is_not() {
        let registry = registry();
        let create = registry.get("object.create").unwrap().to_tool();
        let delete = registry.get("object.delete").unwrap().to_tool();
        let annotations = |t: &Tool| t.annotations.clone().unwrap();
        assert_eq!(annotations(&create).destructive_hint, Some(false));
        assert_eq!(annotations(&delete).destructive_hint, Some(true));
    }

    #[test]
    fn disabled_categories_are_hidden_but_still_callable() {
        let registry = registry();
        assert!(
            registry.visible().is_empty(),
            "scene is not enabled by default"
        );
        assert!(registry.get("object.create").is_some());
    }

    #[test]
    fn categories_report_their_tool_counts() {
        let registry = registry();
        let scene = registry
            .categories()
            .into_iter()
            .find(|c| c.id == "scene")
            .expect("scene category");
        assert_eq!(scene.tool_count, 2);
        assert!(!scene.enabled);
    }

    #[test]
    fn arguments_that_do_not_match_the_schema_are_rejected() {
        let error =
            decode::<CreateObject>("object.create", serde_json::json!({"type": 42})).unwrap_err();
        assert_eq!(error.code, blender_protocol::ErrorCode::InvalidArgument);
        assert_eq!(error.details["tool"], "object.create");
    }
}
