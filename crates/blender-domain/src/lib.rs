//! The domain layer: everything the server works out before Blender is asked
//! to do anything.
//!
//! This is where the architecture earns its keep. A wall between two points, a
//! camera that frames a subject, a PBR graph wired from a set of texture maps,
//! a scatter setup -- all of these are *computed here*, as plain functions over
//! plain data, and handed to Blender as a finished plan.
//!
//! The alternative, and the thing this project exists to avoid, is generating
//! Python that does the reasoning inside Blender where it cannot be tested.
//! Every function in this crate has tests that run in milliseconds without
//! Blender being installed.

#![forbid(unsafe_code)]

pub mod camera;
pub mod graph;
pub mod lighting;
pub mod material;
pub mod modeling;
pub mod validation;

pub use blender_protocol::{
    BlenderError, Result,
    math::{Aabb, Axis, Color4, Vec2, Vec3},
};
