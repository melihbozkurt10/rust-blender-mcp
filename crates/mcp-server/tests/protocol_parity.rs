//! The Rust tool surface and the Python bridge must agree.
//!
//! Two halves of one protocol live in two languages, and nothing in the type
//! system connects them. A tool that forwards to an operation the add-on does
//! not implement fails at runtime, in front of a user, with a confusing error;
//! this test turns that into a compile-and-test-time failure instead.
//!
//! It also enforces the rule the whole design rests on: there is no tool that
//! executes code.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use blender_mcp_server::tools;
use blender_protocol::command::OpKind;

/// Tools implemented entirely in Rust, which deliberately have no bridge
/// operation behind them.
///
/// Every name here does its work in the server: planning, batching, category
/// activation, or talking to an external asset library. Anything that appears
/// in this list by accident is a typo in a forwarding tool's name, which is
/// exactly what this test exists to catch.
const RUST_ONLY: &[&str] = &[
    "asset.download",
    "asset.get",
    "asset.import",
    "asset.providers",
    "asset.search",
    "batch.execute",
    "geometry_nodes.array_along_curve",
    "geometry_nodes.scatter",
    "blender.capabilities",
    "blender.status",
    "render.artifacts.list",
    "scene.diff",
    "tools.categories.disable",
    "tools.categories.enable",
    "tools.categories.list",
    "workflow.export.prepare",
    "workflow.lighting.three_point",
    "workflow.material.emissive",
    "workflow.material.glass",
    "workflow.material.pbr",
    "workflow.model.create_wall",
    "workflow.model.create_wall_run",
    "workflow.product_turntable",
    "workflow.render.studio",
];

/// Words that would betray a code-execution endpoint.
///
/// Matched against whole words rather than substrings: `render.execute` starts
/// a render and is fine, while `execute_python` is the thing that must never
/// exist. A substring match would either miss the second or ban the first.
const FORBIDDEN_WORDS: &[&str] = &[
    "exec",
    "eval",
    "shell",
    "subprocess",
    "bash",
    "sh",
    "cmd",
    "python",
    "script",
    "install",
    "spawn",
];

/// Split a dotted, underscored operation name into its words.
fn words(name: &str) -> Vec<String> {
    name.split(['.', '_'])
        .map(|word| word.to_ascii_lowercase())
        .collect()
}

fn execution_word(name: &str) -> Option<String> {
    words(name)
        .into_iter()
        .find(|word| FORBIDDEN_WORDS.contains(&word.as_str()))
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("the crate lives two directories below the repository root")
        .to_path_buf()
}

/// Every operation the add-on registers, and the side-effect class it declares.
///
/// Scanned from the source rather than imported, because importing the add-on
/// needs `bpy`, which only exists inside Blender.
fn python_handlers() -> BTreeMap<String, OpKind> {
    let mut handlers = BTreeMap::new();
    let mut files = Vec::new();
    collect_python(&repo_root().join("blender_extension"), &mut files);
    assert!(
        !files.is_empty(),
        "no Python sources found; the add-on directory has moved"
    );

    for file in files {
        let source = std::fs::read_to_string(&file).expect("readable source");
        for (marker, kind) in [
            ("@op(\"", OpKind::Write),
            ("@read(\"", OpKind::Read),
            ("@external(\"", OpKind::ExternalSideEffect),
        ] {
            let mut rest = source.as_str();
            while let Some(start) = rest.find(marker) {
                rest = &rest[start + marker.len()..];
                let Some(end) = rest.find('"') else { break };
                let name = rest[..end].to_string();
                if let Some(previous) = handlers.insert(name.clone(), kind) {
                    assert_eq!(
                        previous, kind,
                        "`{name}` is registered twice with different side-effect classes"
                    );
                }
            }
        }
    }
    handlers
}

fn collect_python(directory: &Path, into: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|name| name == "__pycache__") {
                continue;
            }
            collect_python(&path, into);
        } else if path.extension().is_some_and(|extension| extension == "py") {
            into.push(path);
        }
    }
}

#[test]
fn every_forwarding_tool_has_a_handler() {
    let handlers = python_handlers();
    let rust_only: BTreeSet<&str> = RUST_ONLY.iter().copied().collect();

    let mut missing = Vec::new();
    for tool in tools::all() {
        if handlers.contains_key(tool.name) || rust_only.contains(tool.name) {
            continue;
        }
        missing.push(tool.name);
    }

    assert!(
        missing.is_empty(),
        "these tools forward to operations the add-on does not implement: {missing:?}"
    );
}

#[test]
fn the_rust_only_list_is_exactly_right() {
    let handlers = python_handlers();
    let names: BTreeSet<&str> = tools::all().into_iter().map(|tool| tool.name).collect();

    let actual: BTreeSet<&str> = names
        .iter()
        .copied()
        .filter(|name| !handlers.contains_key(*name))
        .collect();
    let declared: BTreeSet<&str> = RUST_ONLY.iter().copied().collect();

    assert_eq!(
        actual, declared,
        "the set of tools with no bridge operation has changed; update RUST_ONLY if that is \
         deliberate, or fix the tool name if it is not"
    );

    let stale: Vec<&str> = declared
        .iter()
        .copied()
        .filter(|name| !names.contains(*name))
        .collect();
    assert!(
        stale.is_empty(),
        "RUST_ONLY names tools that do not exist: {stale:?}"
    );
}

#[test]
fn side_effect_classes_agree_across_the_bridge() {
    let handlers = python_handlers();
    let mut disagreements = Vec::new();

    for tool in tools::all() {
        let Some(bridge_kind) = handlers.get(tool.name) else {
            continue;
        };
        if *bridge_kind != tool.kind {
            disagreements.push(format!(
                "{}: Rust says {}, the add-on says {}",
                tool.name,
                tool.kind.as_str(),
                bridge_kind.as_str()
            ));
        }
    }

    // A disagreement here is not cosmetic: the classification drives retry
    // policy and whether an operation may join an undo-backed batch. A read
    // that is really a write would be retried after a dropped connection and
    // applied twice.
    assert!(
        disagreements.is_empty(),
        "side-effect classification differs between Rust and the add-on: {disagreements:#?}"
    );
}

#[test]
fn no_tool_can_execute_code() {
    // The one rule the whole design exists to keep. Checked over the real tool
    // list rather than by reading the source, so a tool added in any module is
    // covered.
    for tool in tools::all() {
        assert!(
            execution_word(tool.name).is_none(),
            "`{}` looks like an arbitrary execution tool, which must never exist",
            tool.name
        );
    }
}

#[test]
fn no_bridge_operation_can_execute_code() {
    // The same rule on the other side of the wire: the add-on's dispatcher must
    // not register anything that runs caller-supplied code either.
    for name in python_handlers().keys() {
        assert!(
            execution_word(name).is_none(),
            "the add-on registers `{name}`, which must never exist"
        );
    }
}

#[test]
fn every_tool_takes_a_typed_object() {
    // A tool whose schema is not an object accepts free-form input, which is
    // how a typed protocol quietly turns into a passthrough.
    for tool in tools::all() {
        let schema = serde_json::to_value(&*tool.schema).expect("serialisable schema");
        assert_eq!(
            schema["type"], "object",
            "{} has a non-object schema",
            tool.name
        );
        assert!(
            schema.get("$ref").is_none() && schema.get("$defs").is_none(),
            "{} leaks a schema reference; MCP clients must see a self-contained schema",
            tool.name
        );
    }
}

#[test]
fn the_add_on_implements_more_than_the_tools_expose() {
    // Not every bridge operation needs a tool of its own -- some exist only to
    // serve a workflow or a batch -- but a large gap in the other direction
    // would mean the tool surface has drifted away from the bridge.
    let handlers = python_handlers();
    let tools = tools::all();
    assert!(
        handlers.len() >= 200,
        "only {} bridge operations were found; the scan is probably broken",
        handlers.len()
    );
    assert!(
        tools.len() >= 200,
        "only {} tools were registered; the tool list is probably broken",
        tools.len()
    );
}

#[test]
fn the_execution_check_tells_a_render_from_an_exec() {
    // The check itself is worth a test: too loose and it lets `execute_python`
    // through, too tight and it bans `render.execute`.
    for allowed in ["render.execute", "batch.execute", "object.transform.set"] {
        assert_eq!(execution_word(allowed), None, "{allowed}");
    }
    for banned in [
        "execute_python",
        "run_python",
        "eval_python",
        "execute_script",
        "run_script",
        "shell",
        "exec",
        "eval",
        "system.shell.run",
        "addon.install",
    ] {
        assert!(execution_word(banned).is_some(), "{banned} was not caught");
    }
}
