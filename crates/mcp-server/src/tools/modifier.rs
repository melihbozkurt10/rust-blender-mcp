//! Modifier tools.

use std::sync::Arc;

use blender_protocol::{
    capabilities::CapabilityKind,
    command::{Category, OpKind},
    modifier::{
        AddModifier, ApplyModifier, CopyModifiers, ListModifiers, ModifierRefParams, MoveModifier,
        UpdateModifier,
    },
};

use crate::{registry::ToolSpec, state::AppState};

const MODIFIERS: Category = Category::Modifiers;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<ListModifiers>(
            "modifier.list",
            MODIFIERS,
            OpKind::Read,
            "List modifiers",
            "The modifier stack on one object, in evaluation order, optionally with every \
             type-specific property.",
        ),
        ToolSpec::forward::<ModifierRefParams>(
            "modifier.get",
            MODIFIERS,
            OpKind::Read,
            "Get a modifier",
            "One modifier with all of its properties and its position in the stack.",
        ),
        // The modifier type is checked against the connected build first: the
        // available set genuinely differs between releases and between object
        // types, and a rejected type should say which ones exist.
        ToolSpec::custom::<AddModifier, _, _>(
            "modifier.add",
            MODIFIERS,
            OpKind::Write,
            "Add a modifier",
            "Add a modifier and configure it in one call. Type-specific settings go in \
             `properties` as typed name/value pairs; the names are checked against the modifier \
             type, so a wrong one is reported with the real list rather than ignored.",
            |state: Arc<AppState>, params: AddModifier| async move {
                let identifier = params.modifier_type.blender_id();
                state.require_capability(CapabilityKind::Modifier, &identifier)?;
                state.call_typed("modifier.add", &params).await
            },
        ),
        ToolSpec::forward::<UpdateModifier>(
            "modifier.update",
            MODIFIERS,
            OpKind::Write,
            "Update a modifier",
            "Rename a modifier, toggle its viewport, render and edit-mode visibility, change its \
             target object, or set its type-specific properties.",
        ),
        ToolSpec::forward::<MoveModifier>(
            "modifier.move",
            MODIFIERS,
            OpKind::Write,
            "Reorder a modifier",
            "Move a modifier up, down, to the start or end of the stack, or to an absolute index. \
             Order matters: a bevel before a subdivision looks nothing like one after it.",
        ),
        ToolSpec::forward::<ModifierRefParams>(
            "modifier.remove",
            MODIFIERS,
            OpKind::Write,
            "Remove a modifier",
            "Remove a modifier from the stack without applying it.",
        ),
        ToolSpec::forward::<ApplyModifier>(
            "modifier.apply",
            MODIFIERS,
            OpKind::Write,
            "Apply a modifier",
            "Bake a modifier into the mesh data. Refused on objects that share their mesh with \
             others, because applying would change all of them.",
        ),
        ToolSpec::forward::<CopyModifiers>(
            "modifier.copy",
            MODIFIERS,
            OpKind::Write,
            "Copy modifiers",
            "Copy some or all of one object modifier stack onto others, optionally replacing what \
             is already there.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_is_marked_destructive() {
        let apply = tools()
            .into_iter()
            .find(|t| t.name == "modifier.apply")
            .unwrap();
        let annotations = apply.to_tool().annotations.unwrap();
        assert_eq!(annotations.destructive_hint, Some(true));
    }

    #[test]
    fn reordering_explains_why_order_matters() {
        let move_tool = tools()
            .into_iter()
            .find(|t| t.name == "modifier.move")
            .unwrap();
        assert!(move_tool.description.contains("Order matters"));
    }
}
