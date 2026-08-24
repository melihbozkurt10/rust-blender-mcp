//! Multi-step production workflows.
//!
//! A workflow is not a large function full of side effects. It is a sequence of
//! named steps run through a [`Run`], which records what each step produced and
//! which entities it created. That record is what makes two things possible:
//! a structured report a model can read, and a rollback that deletes exactly
//! what the workflow made and nothing else.
//!
//! Workflows talk to Blender through the [`Executor`] trait rather than
//! directly, so this crate has no dependency on the MCP server and its logic
//! can be tested against a recording executor with no Blender in sight.

#![forbid(unsafe_code)]

pub mod executor;
pub mod rollback;
pub mod run;
pub mod step;
pub mod workflows;

pub use executor::{BoxFuture, Executor};
pub use rollback::{Compensation, RollbackReport};
pub use run::Run;
pub use step::{Step, StepStatus};
