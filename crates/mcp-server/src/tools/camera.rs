//! Camera tools.

use blender_protocol::{
    camera::{
        AutoFrame, CameraLookAt, CameraRefParams, CreateCamera, ListCameras, TrackObject,
        UpdateCamera, UpdateDepthOfField,
    },
    command::{Category, OpKind},
};

use crate::registry::ToolSpec;

const CAMERA: Category = Category::Camera;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<ListCameras>(
            "camera.list",
            CAMERA,
            OpKind::Read,
            "List cameras",
            "Every camera in the file, with lens, sensor, clipping and which one is active.",
        ),
        ToolSpec::forward::<CameraRefParams>(
            "camera.get",
            CAMERA,
            OpKind::Read,
            "Get a camera",
            "One camera in detail, including its depth of field and any tracking constraints. \
             Omit `camera` to read the scene's active one.",
        ),
        ToolSpec::forward::<CreateCamera>(
            "camera.create",
            CAMERA,
            OpKind::Write,
            "Create a camera",
            "Create a camera and place it. Give it `frame_objects` and it works out where to \
             stand; give it `look_at` and it works out the rotation.",
        ),
        ToolSpec::forward::<UpdateCamera>(
            "camera.update",
            CAMERA,
            OpKind::Write,
            "Update a camera",
            "Change lens (in millimetres or as a field of view), sensor, projection, clipping, \
             shift or depth of field.",
        ),
        ToolSpec::forward::<CameraRefParams>(
            "camera.delete",
            CAMERA,
            OpKind::Write,
            "Delete a camera",
            "Delete a camera object and its unused camera data.",
        ),
        ToolSpec::forward::<CameraRefParams>(
            "camera.set_active",
            CAMERA,
            OpKind::Write,
            "Set the active camera",
            "Make a camera the one the scene renders through.",
        ),
        ToolSpec::forward::<CameraLookAt>(
            "camera.look_at",
            CAMERA,
            OpKind::Write,
            "Aim a camera",
            "Point a camera at a world position or at an object, without moving it.",
        ),
        ToolSpec::forward::<TrackObject>(
            "camera.track_object",
            CAMERA,
            OpKind::Write,
            "Track an object",
            "Add a constraint so the camera keeps aiming at a target as either moves. Replaces any \
             existing tracking constraint rather than stacking a second one that would fight it.",
        ),
        ToolSpec::forward::<CameraRefParams>(
            "camera.clear_tracking",
            CAMERA,
            OpKind::Write,
            "Stop tracking",
            "Remove tracking constraints from a camera, leaving it where it is pointing.",
        ),
        ToolSpec::forward::<UpdateDepthOfField>(
            "camera.depth_of_field.update",
            CAMERA,
            OpKind::Write,
            "Set depth of field",
            "Enable depth of field and set the focus object or distance, aperture, blade count and \
             bokeh shape.",
        ),
        ToolSpec::forward::<AutoFrame>(
            "camera.auto_frame",
            CAMERA,
            OpKind::Write,
            "Frame a subject",
            "Place and aim a camera so the given objects fill the frame with a chosen padding. The \
             distance is computed from the subject's bounds and the camera's field of view, in \
             one pass -- not by rendering and adjusting.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_are_classified_correctly() {
        for tool in tools() {
            let expected = if matches!(tool.name, "camera.list" | "camera.get") {
                OpKind::Read
            } else {
                OpKind::Write
            };
            assert_eq!(tool.kind, expected, "{}", tool.name);
        }
    }

    #[test]
    fn auto_frame_explains_that_it_is_closed_form() {
        let frame = tools()
            .into_iter()
            .find(|t| t.name == "camera.auto_frame")
            .unwrap();
        assert!(frame.description.contains("one pass"));
    }
}
