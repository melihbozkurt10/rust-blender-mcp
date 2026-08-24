//! Print the registered tool surface, with the schema cost of each category.
//!
//! The documentation quotes tool counts and schema sizes, and a number in a
//! README that nobody regenerates is wrong within a week. This prints the real
//! registry, so the docs can be checked against the binary rather than against
//! somebody's memory.
//!
//!     cargo run -p blender-mcp-server --example tool_inventory
//!     cargo run -p blender-mcp-server --example tool_inventory -- --names

use std::collections::BTreeMap;

use blender_mcp_server::tools;

fn main() {
    let names = std::env::args().any(|argument| argument == "--names");
    let all = tools::all();

    let mut by_category: BTreeMap<&str, Vec<(&str, usize)>> = BTreeMap::new();
    for tool in &all {
        // The serialised schema is what actually reaches a client, so measure
        // that rather than guessing from the number of fields.
        let bytes = serde_json::to_vec(&*tool.schema)
            .expect("a schema that cannot be serialised would never reach a client")
            .len();
        by_category
            .entry(tool.category.id())
            .or_default()
            .push((tool.name, bytes));
    }

    let mut total_bytes = 0;
    println!("{:<16} {:>5} {:>9}", "category", "tools", "schema");
    for (category, mut tools) in by_category {
        tools.sort_unstable();
        let bytes: usize = tools.iter().map(|(_, bytes)| bytes).sum();
        total_bytes += bytes;
        println!(
            "{category:<16} {:>5} {:>8.1}K",
            tools.len(),
            bytes as f64 / 1024.0
        );
        if names {
            for (name, bytes) in tools {
                println!("  {name:<44} {:>6.1}K", bytes as f64 / 1024.0);
            }
        }
    }
    println!(
        "{:<16} {:>5} {:>8.1}K",
        "total",
        all.len(),
        total_bytes as f64 / 1024.0
    );
}
