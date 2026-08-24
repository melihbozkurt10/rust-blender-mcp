//! Light tools.

use blender_protocol::{
    command::{Category, OpKind},
    light::{CreateLight, LightRefParams, ListLights, LookAt, UpdateLight},
};

use crate::registry::ToolSpec;

const LIGHTS: Category = Category::Lights;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<ListLights>(
            "light.list",
            LIGHTS,
            OpKind::Read,
            "List lights",
            "List lights, optionally filtered by name or by type. Paginated.",
        ),
        ToolSpec::forward::<LightRefParams>(
            "light.get",
            LIGHTS,
            OpKind::Read,
            "Get a light",
            "One light in detail: type, transform, energy, colour, shadow settings and the \
             type-specific shaping parameters.",
        ),
        ToolSpec::forward::<CreateLight>(
            "light.create",
            LIGHTS,
            OpKind::Write,
            "Create a light",
            "Create a point, sun, spot or area light. Aim it with `look_at` or `target` instead \
             of working out a rotation, and set colour by temperature in Kelvin if that is easier \
             than an RGB triple.",
        ),
        ToolSpec::forward::<UpdateLight>(
            "light.update",
            LIGHTS,
            OpKind::Write,
            "Update a light",
            "Change a light energy, colour, shaping or even its type in place. Settings that do \
             not apply to the current type are ignored, so one settings block can drive several \
             lights.",
        ),
        ToolSpec::forward::<LightRefParams>(
            "light.delete",
            LIGHTS,
            OpKind::Write,
            "Delete a light",
            "Delete a light object and its unused light data.",
        ),
        ToolSpec::forward::<LookAt>(
            "light.look_at",
            LIGHTS,
            OpKind::Write,
            "Aim a light",
            "Point a light at a world position or at an object bounding-box centre, optionally \
             moving it to a given distance along that direction first.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_and_delete_share_one_reference_shape() {
        let get = tools().into_iter().find(|t| t.name == "light.get").unwrap();
        let delete = tools()
            .into_iter()
            .find(|t| t.name == "light.delete")
            .unwrap();
        assert_eq!(
            serde_json::to_string(&*get.schema).unwrap(),
            serde_json::to_string(&*delete.schema).unwrap()
        );
        assert_eq!(get.kind, OpKind::Read);
        assert_eq!(delete.kind, OpKind::Write);
    }
}
