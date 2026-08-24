//! How a workflow reaches Blender.

use std::{future::Future, pin::Pin};

use blender_protocol::BlenderError;
use serde_json::Value;

/// A boxed future, so the trait stays object-safe.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Anything that can run one bridge operation.
///
/// The MCP server implements this over its transport; tests implement it over a
/// recording, which is what lets every workflow be tested without Blender.
pub trait Executor: Send + Sync {
    /// Run one operation and return its result.
    fn call<'a>(&'a self, op: &'a str, args: Value) -> BoxFuture<'a, Result<Value, BlenderError>>;
}

/// A recording executor for tests: it answers from a script and remembers every
/// call it was asked to make.
#[cfg(any(test, feature = "testing"))]
pub mod recording {
    use std::sync::Mutex;

    use super::*;

    /// What a scripted executor should do for one operation.
    pub enum Reply {
        /// Answer with this value.
        Ok(Value),
        /// Fail with this error.
        Fail(BlenderError),
    }

    /// An executor driven by a script keyed on operation name.
    pub struct Recorder {
        replies: Mutex<Vec<(String, Reply)>>,
        calls: Mutex<Vec<(String, Value)>>,
        /// What to answer for an operation the script does not mention.
        default: Value,
    }

    impl Recorder {
        pub fn new(default: Value) -> Self {
            Self {
                replies: Mutex::new(Vec::new()),
                calls: Mutex::new(Vec::new()),
                default,
            }
        }

        /// Queue an answer for the next call to `op`.
        pub fn expect(self, op: &str, reply: Reply) -> Self {
            self.replies.lock().unwrap().push((op.to_string(), reply));
            self
        }

        /// Every call made, in order.
        pub fn calls(&self) -> Vec<(String, Value)> {
            self.calls.lock().unwrap().clone()
        }

        /// The operation names called, in order.
        pub fn ops(&self) -> Vec<String> {
            self.calls().into_iter().map(|(op, _)| op).collect()
        }

        /// Whether an operation was called at all.
        pub fn called(&self, op: &str) -> bool {
            self.ops().iter().any(|name| name == op)
        }

        /// The arguments of the first call to an operation.
        pub fn args_for(&self, op: &str) -> Option<Value> {
            self.calls()
                .into_iter()
                .find(|(name, _)| name == op)
                .map(|(_, args)| args)
        }
    }

    impl Executor for Recorder {
        fn call<'a>(
            &'a self,
            op: &'a str,
            args: Value,
        ) -> BoxFuture<'a, Result<Value, BlenderError>> {
            self.calls.lock().unwrap().push((op.to_string(), args));
            let mut replies = self.replies.lock().unwrap();
            let position = replies.iter().position(|(name, _)| name == op);
            let reply = position.map(|index| replies.remove(index).1);
            drop(replies);
            let default = self.default.clone();
            Box::pin(async move {
                match reply {
                    Some(Reply::Ok(value)) => Ok(value),
                    Some(Reply::Fail(error)) => Err(error),
                    None => Ok(default),
                }
            })
        }
    }
}
