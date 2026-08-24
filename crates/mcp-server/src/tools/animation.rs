//! Animation tools.

use blender_protocol::{
    animation::{
        ActionAssignment, ActionRefParams, CreateAction, CreateNlaStrip, CreateNlaTrack,
        DeleteKeyframes, GetFCurve, InsertKeyframes, ListActions, ListFCurves, ListKeyframes,
        LoopAnimation, MoveMotion, NlaStripRefParams, NlaTrackRefParams, ObjectRefParams,
        RotationMotion, ScaleMotion, SetFrame, SetFrameRange, SetInterpolation, UpdateFCurve,
    },
    command::{Category, OpKind},
};

use super::NoParams;
use crate::registry::ToolSpec;

const ANIMATION: Category = Category::Animation;

pub fn tools() -> Vec<ToolSpec> {
    vec![
        ToolSpec::forward::<NoParams>(
            "animation.frame.get",
            ANIMATION,
            OpKind::Read,
            "Get the current frame",
            "Which frame the scene is on.",
        ),
        ToolSpec::forward::<SetFrame>(
            "animation.frame.set",
            ANIMATION,
            OpKind::Write,
            "Set the current frame",
            "Move the playhead, which also re-evaluates every animated value.",
        ),
        ToolSpec::forward::<NoParams>(
            "animation.range.get",
            ANIMATION,
            OpKind::Read,
            "Get the frame range",
            "Start, end, step and frame rate.",
        ),
        ToolSpec::forward::<SetFrameRange>(
            "animation.range.set",
            ANIMATION,
            OpKind::Write,
            "Set the frame range",
            "Change the playback range. A range that would end before it starts is refused rather \
             than being silently clamped, which is what Blender does on its own.",
        ),
        ToolSpec::forward::<InsertKeyframes>(
            "animation.keyframe.insert",
            ANIMATION,
            OpKind::Write,
            "Insert keyframes",
            "Key location, rotation, scale, visibility, shape keys, custom properties, bone \
             channels or material inputs. Each keyframe can carry its own value, interpolation and \
             easing, so a whole motion goes in one call.",
        ),
        ToolSpec::forward::<DeleteKeyframes>(
            "animation.keyframe.delete",
            ANIMATION,
            OpKind::Write,
            "Delete keyframes",
            "Remove keyframes on named frames or across a range. Requires one or the other, so \
             `delete everything` is never the accidental default.",
        ),
        ToolSpec::forward::<ListKeyframes>(
            "animation.keyframe.list",
            ANIMATION,
            OpKind::Read,
            "List keyframes",
            "Keyframes on an object, with their frames, values, interpolation and easing. \
             Paginated.",
        ),
        ToolSpec::forward::<SetInterpolation>(
            "animation.interpolation.set",
            ANIMATION,
            OpKind::Write,
            "Set interpolation",
            "Change how existing keyframes interpolate: constant for stepped motion, linear for \
             turntables, Bezier or an easing curve for anything that should feel hand-animated.",
        ),
        ToolSpec::forward::<ListFCurves>(
            "animation.fcurve.list",
            ANIMATION,
            OpKind::Read,
            "List F-curves",
            "The animation channels on an object, with their ranges and any modifiers.",
        ),
        ToolSpec::forward::<GetFCurve>(
            "animation.fcurve.get",
            ANIMATION,
            OpKind::Read,
            "Get an F-curve",
            "One channel with every keyframe on it.",
        ),
        ToolSpec::forward::<UpdateFCurve>(
            "animation.fcurve.update",
            ANIMATION,
            OpKind::Write,
            "Update an F-curve",
            "Mute, lock, change extrapolation, or make a channel cycle forever.",
        ),
        ToolSpec::forward::<ListActions>(
            "animation.action.list",
            ANIMATION,
            OpKind::Read,
            "List actions",
            "Every action in the file, with its range, channel count and users.",
        ),
        ToolSpec::forward::<ActionRefParams>(
            "animation.action.get",
            ANIMATION,
            OpKind::Read,
            "Get an action",
            "One action with all of its channels.",
        ),
        ToolSpec::forward::<CreateAction>(
            "animation.action.create",
            ANIMATION,
            OpKind::Write,
            "Create an action",
            "Make a new action, optionally assigning it to an object and giving it a fake user so \
             it survives a save.",
        ),
        ToolSpec::forward::<ActionAssignment>(
            "animation.action.assign",
            ANIMATION,
            OpKind::Write,
            "Assign an action",
            "Put an action on an object, creating it if asked.",
        ),
        ToolSpec::forward::<ActionRefParams>(
            "animation.action.delete",
            ANIMATION,
            OpKind::Write,
            "Delete an action",
            "Remove an action data-block.",
        ),
        ToolSpec::forward::<RotationMotion>(
            "animation.create_rotation",
            ANIMATION,
            OpKind::Write,
            "Animate a rotation",
            "Spin an object by a number of degrees about an axis between two frames, optionally \
             looping forever. Expands to ordinary keyframes, so the result can be edited by hand \
             afterwards.",
        ),
        ToolSpec::forward::<MoveMotion>(
            "animation.create_move",
            ANIMATION,
            OpKind::Write,
            "Animate a move",
            "Move an object to a position, or by an offset, between two frames.",
        ),
        ToolSpec::forward::<ScaleMotion>(
            "animation.create_scale",
            ANIMATION,
            OpKind::Write,
            "Animate a scale",
            "Scale an object to a target size between two frames.",
        ),
        ToolSpec::forward::<LoopAnimation>(
            "animation.loop",
            ANIMATION,
            OpKind::Write,
            "Loop an animation",
            "Make every channel on an object repeat forever, or stop it doing so.",
        ),
        ToolSpec::forward::<ObjectRefParams>(
            "animation.nla.track.list",
            ANIMATION,
            OpKind::Read,
            "List NLA tracks",
            "The non-linear animation tracks on an object and the strips on each.",
        ),
        ToolSpec::forward::<CreateNlaTrack>(
            "animation.nla.track.create",
            ANIMATION,
            OpKind::Write,
            "Create an NLA track",
            "Add a track to layer actions on.",
        ),
        ToolSpec::forward::<NlaTrackRefParams>(
            "animation.nla.track.delete",
            ANIMATION,
            OpKind::Write,
            "Delete an NLA track",
            "Remove a track and its strips.",
        ),
        ToolSpec::forward::<CreateNlaStrip>(
            "animation.nla.strip.create",
            ANIMATION,
            OpKind::Write,
            "Create an NLA strip",
            "Place an action on a track at a frame, with a blend mode, influence and repeat count. \
             Strips on one track may not overlap, and an overlap is reported rather than silently \
             moved.",
        ),
        ToolSpec::forward::<NlaStripRefParams>(
            "animation.nla.strip.update",
            ANIMATION,
            OpKind::Write,
            "Update an NLA strip",
            "Change a strip's end frame, blend mode, influence or repeat count.",
        ),
        ToolSpec::forward::<NlaStripRefParams>(
            "animation.nla.strip.delete",
            ANIMATION,
            OpKind::Write,
            "Delete an NLA strip",
            "Remove a strip from a track.",
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_high_level_helpers_are_present() {
        let names: Vec<&str> = tools().iter().map(|t| t.name).collect();
        for expected in [
            "animation.create_rotation",
            "animation.create_move",
            "animation.create_scale",
            "animation.loop",
        ] {
            assert!(names.contains(&expected), "{expected} is missing");
        }
    }

    #[test]
    fn helpers_say_they_expand_to_keyframes() {
        let rotation = tools()
            .into_iter()
            .find(|t| t.name == "animation.create_rotation")
            .unwrap();
        assert!(
            rotation.description.contains("ordinary keyframes"),
            "a caller should know the result is editable"
        );
    }
}
