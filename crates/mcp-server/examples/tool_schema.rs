//! Print one tool's JSON schema.
//!
//!     cargo run -p blender-mcp-server --example tool_schema -- object.create

use blender_mcp_server::tools;

fn main() {
    let wanted: Vec<String> = std::env::args().skip(1).collect();
    for tool in tools::all() {
        if !wanted.iter().any(|name| name == tool.name) {
            continue;
        }
        println!("== {} ==", tool.name);
        println!("{}", serde_json::to_string_pretty(&*tool.schema).unwrap());
    }
}
