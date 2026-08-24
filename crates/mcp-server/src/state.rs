//! Shared server state.

use std::{
    sync::{Arc, RwLock},
    time::Duration,
};

use blender_client::BlenderClient;
use blender_protocol::{
    BlenderError, ErrorCode,
    capabilities::{Capabilities, CapabilityKind},
    command::Command,
};
use serde::Serialize;
use serde_json::{Map, Value};

use asset_providers::AssetProviders;
use scene_cache::SceneCache;

use crate::{artifacts::ArtifactStore, config::Config, registry::Registry};

/// Everything a tool handler can reach.
pub struct AppState {
    pub config: Config,
    pub client: BlenderClient,
    pub registry: Arc<Registry>,
    /// Files this server produced and owns.
    pub artifacts: Arc<ArtifactStore>,
    /// What has changed in the scene, and when.
    pub cache: Arc<SceneCache>,
    /// External asset libraries, when any could be built.
    pub assets: Option<Arc<AssetProviders>>,
    /// Set once the MCP session is live, so category changes can notify the
    /// client that the tool list moved.
    notifier: RwLock<Option<ToolListNotifier>>,
}

/// How the server tells a client its tool list changed.
///
/// Boxed rather than holding an `rmcp` peer directly, so the registry and state
/// do not have to know about the transport.
pub type ToolListNotifier = Arc<dyn Fn() + Send + Sync>;

impl AppState {
    pub fn new(config: Config, client: BlenderClient, registry: Arc<Registry>) -> Arc<Self> {
        let revision_history = config.revision_history;
        // A provider set that cannot be built is not a reason to refuse to
        // start: everything that does not touch an asset library still works,
        // and `asset.providers` reports the absence.
        let assets = match AssetProviders::new(&config.asset_config()) {
            Ok(assets) => Some(Arc::new(assets)),
            Err(error) => {
                tracing::warn!(%error, "asset providers are unavailable");
                None
            }
        };
        Arc::new(Self {
            config,
            client,
            registry,
            artifacts: ArtifactStore::new(500),
            cache: Arc::new(SceneCache::new(revision_history)),
            assets,
            notifier: RwLock::new(None),
        })
    }

    /// Install the tool-list-changed notifier once a client has connected.
    pub fn set_notifier(&self, notifier: ToolListNotifier) {
        *self.notifier.write().unwrap_or_else(|e| e.into_inner()) = Some(notifier);
    }

    /// Whether a notifier is already installed.
    pub fn has_notifier(&self) -> bool {
        self.notifier
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    /// Tell the client the tool list changed, if one is listening.
    pub fn notify_tool_list_changed(&self) {
        let notifier = self
            .notifier
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(notify) = notifier {
            notify();
        }
    }

    /// Send a typed, already-validated payload to Blender.
    pub async fn call_typed<P: Serialize>(
        &self,
        op: &str,
        params: &P,
    ) -> Result<Value, BlenderError> {
        let command = Command::from_params(op, params).map_err(|error| {
            BlenderError::internal(format!("could not encode `{op}` arguments: {error}"))
        })?;
        self.client.execute(command, None).await
    }

    /// Send a typed payload with an explicit deadline.
    pub async fn call_typed_with_timeout<P: Serialize>(
        &self,
        op: &str,
        params: &P,
        timeout: Duration,
    ) -> Result<Value, BlenderError> {
        let command = Command::from_params(op, params).map_err(|error| {
            BlenderError::internal(format!("could not encode `{op}` arguments: {error}"))
        })?;
        self.client.execute(command, Some(timeout)).await
    }

    /// Send raw arguments. Used by batch execution, where the arguments were
    /// validated against the target tool's schema rather than this call site's.
    pub async fn call_raw(
        &self,
        op: &str,
        args: Map<String, Value>,
    ) -> Result<Value, BlenderError> {
        self.client
            .execute(Command::with_args(op, args), None)
            .await
    }

    /// Capabilities of the connected Blender, or a `BLENDER_NOT_CONNECTED`
    /// error.
    pub fn capabilities(&self) -> Result<Arc<Capabilities>, BlenderError> {
        self.client.capabilities()
    }

    /// Check a value against the connected build's capabilities.
    ///
    /// Returns `Ok(())` when nothing is connected: the request is about to fail
    /// with a connection error anyway, and a capability error would be
    /// misleading.
    pub fn require_capability(
        &self,
        kind: CapabilityKind,
        value: &str,
    ) -> Result<(), BlenderError> {
        match self.client.capabilities() {
            Ok(capabilities) => capabilities.require(kind, value),
            Err(error) if error.code == ErrorCode::BlenderNotConnected => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Refuse an operation that needs a UI when Blender is running headless.
    pub fn require_interactive(&self, what: &str) -> Result<(), BlenderError> {
        let session = self.client.session()?;
        if session.identity.background {
            return Err(BlenderError::new(
                ErrorCode::UnsupportedOperation,
                format!(
                    "{what} needs a 3D viewport, and this Blender is running in background mode."
                ),
            )
            .with_detail("operation", what));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use blender_protocol::command::Category;
    use tokio::net::TcpListener;

    use super::*;
    use crate::registry::{Activation, Registry};

    async fn state() -> Arc<AppState> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let client = BlenderClient::from_listener(blender_client::Config::default(), listener);
        let registry = Arc::new(Registry::new(vec![], Activation::lazy(&[Category::Core])));
        AppState::new(Config::default(), client, registry)
    }

    #[tokio::test]
    async fn capability_checks_pass_through_when_disconnected() {
        let state = state().await;
        // Nothing is connected, so a capability check must not invent a
        // capability failure on top of the connection failure.
        assert!(
            state
                .require_capability(CapabilityKind::Modifier, "ANYTHING")
                .is_ok()
        );
    }

    #[tokio::test]
    async fn calls_fail_cleanly_when_disconnected() {
        let state = state().await;
        let error = state
            .call_raw("scene.summary", Map::new())
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::BlenderNotConnected);
    }

    #[tokio::test]
    async fn notifier_is_optional() {
        let state = state().await;
        // Must not panic when no client has connected yet.
        state.notify_tool_list_changed();

        let hits = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&hits);
        state.set_notifier(Arc::new(move || {
            counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        state.notify_tool_list_changed();
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}
