//! The workflows themselves.
//!
//! Each one is a plain async function over an [`crate::Executor`], so it can be
//! tested end to end against a recording executor. What they have in common is
//! that all the thinking -- which nodes, where the camera goes, how bright the
//! key light is -- happens in Rust before Blender is asked for anything.

pub mod export;
pub mod geometry;
pub mod lighting;
pub mod material;
pub mod modelling;
pub mod render;

pub use export::prepare_export;
pub use geometry::{array_along_curve, scatter};
pub use lighting::three_point;
pub use material::{emissive_material, glass_material, pbr_material};
pub use modelling::create_wall;
pub use render::{product_turntable, studio_render};
