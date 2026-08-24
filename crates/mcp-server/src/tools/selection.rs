//! Selection tools.
//!
//! Selection is exposed because Blender users think in terms of it and some
//! operators genuinely need it. Nothing else in this server depends on it:
//! every other tool addresses objects explicitly, so a model never has to
//! select something first and hope the state survives.

use blender_protocol::{
    command::{Category, OpKind},
    object::GetObject,
    scene::SelectionUpdate,
};

use super::NoParams;
use crate::registry::ToolSpec;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<NoParams>(
            "selection.get",
            Category::Core,
            OpKind::Read,
            "Get the selection",
            "Which objects are selected, which is active, and the current interaction mode.",
        ),
        ToolSpec::forward::<SelectionUpdate>(
            "selection.set",
            Category::Scene,
            OpKind::Write,
            "Set the selection",
            "Replace the selection with the given objects, optionally making one of them active.",
        ),
        ToolSpec::forward::<SelectionUpdate>(
            "selection.add",
            Category::Scene,
            OpKind::Write,
            "Add to the selection",
            "Add objects to the current selection.",
        ),
        ToolSpec::forward::<SelectionUpdate>(
            "selection.remove",
            Category::Scene,
            OpKind::Write,
            "Remove from the selection",
            "Remove objects from the current selection.",
        ),
        ToolSpec::forward::<NoParams>(
            "selection.clear",
            Category::Scene,
            OpKind::Write,
            "Clear the selection",
            "Deselect everything and clear the active object.",
        ),
        ToolSpec::forward::<GetObject>(
            "selection.set_active",
            Category::Scene,
            OpKind::Write,
            "Set the active object",
            "Make one object active, selecting it as well. The active object is what mode changes \
             and several operators act on.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reading_the_selection_is_a_core_tool() {
        let get = tools()
            .into_iter()
            .find(|t| t.name == "selection.get")
            .unwrap();
        assert_eq!(get.category, Category::Core);
    }

    #[test]
    fn mutating_the_selection_is_never_a_read() {
        for tool in tools() {
            if tool.name != "selection.get" {
                assert_eq!(tool.kind, OpKind::Write, "{}", tool.name);
            }
        }
    }
}
