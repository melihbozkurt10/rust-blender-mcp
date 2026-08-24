//! The public handle onto the Blender connection.

use std::{sync::Arc, time::Duration};

use blender_protocol::{
    capabilities::Capabilities,
    command::{Command, OpKind},
    envelope::{Envelope, Request, RequestId},
    error::{BlenderError, ErrorCode},
    event::Event,
};
use serde_json::Value;
use tokio::{net::TcpListener, sync::broadcast, task::JoinHandle};

use crate::{
    ClientError, Config, connection,
    pending::{self, PendingRequests},
    reconnect::{Backoff, bind_with_retry},
    session::{Session, SessionSlot, Status},
};

/// A handle onto the Blender bridge.
///
/// Cloning is cheap and every clone talks to the same connection.
#[derive(Clone)]
pub struct BlenderClient {
    runtime: Arc<connection::Runtime>,
    listen_address: Arc<String>,
    _accept_task: Arc<AbortOnDrop>,
}

/// Aborts the accept loop when the last handle goes away, so a dropped client
/// does not leave a listener holding the port.
struct AbortOnDrop(JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl BlenderClient {
    /// Bind the listener and start accepting connections from the add-on.
    pub async fn start(config: Config) -> Result<Self, ClientError> {
        let listener = bind_with_retry(&config, Backoff::default()).await?;
        Ok(Self::from_listener(config, listener))
    }

    /// Start on an already-bound listener. Used by tests, and by anything that
    /// needs to hand the port over from outside.
    pub fn from_listener(config: Config, listener: TcpListener) -> Self {
        let listen_address = listener
            .local_addr()
            .map(|a| a.to_string())
            .unwrap_or_else(|_| config.bind.to_string());
        let (events, _) = broadcast::channel(config.event_buffer);
        let runtime = Arc::new(connection::Runtime {
            config,
            slot: Arc::new(SessionSlot::new()),
            pending: PendingRequests::new(),
            events,
        });
        let accept = tokio::spawn(connection::serve(Arc::clone(&runtime), listener));
        Self {
            runtime,
            listen_address: Arc::new(listen_address),
            _accept_task: Arc::new(AbortOnDrop(accept)),
        }
    }

    /// Address the add-on should connect to.
    pub fn listen_address(&self) -> &str {
        &self.listen_address
    }

    /// Whether Blender is connected right now.
    pub fn is_connected(&self) -> bool {
        self.runtime.slot.get().is_some()
    }

    /// The active session, or a `BLENDER_NOT_CONNECTED` error.
    pub fn session(&self) -> Result<Session, BlenderError> {
        self.runtime.slot.require()
    }

    /// Capabilities of the connected build, or an error when nothing is
    /// connected.
    pub fn capabilities(&self) -> Result<Arc<Capabilities>, BlenderError> {
        Ok(self.session()?.capabilities)
    }

    /// Connection status, safe to call at any time.
    pub fn status(&self) -> Status {
        self.runtime
            .slot
            .status(&self.listen_address, self.runtime.pending.len())
    }

    /// The event sender, for injecting synthetic events.
    ///
    /// Used by tests and by any host that wants to drive the cache without a
    /// live Blender. Nothing in the normal path publishes through it.
    pub fn event_sender(&self) -> broadcast::Sender<Event> {
        self.runtime.events.clone()
    }

    /// Subscribe to scene-change events.
    ///
    /// A subscriber that falls behind is lagged by the broadcast channel rather
    /// than blocking the socket reader; the cache treats a lag as a signal to
    /// resynchronise from scratch.
    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.runtime.events.subscribe()
    }

    /// Wait until Blender connects, or the deadline passes.
    pub async fn wait_connected(&self, within: Duration) -> Result<Session, BlenderError> {
        let deadline = tokio::time::Instant::now() + within;
        // Polling is the honest implementation here: connection arrival is not
        // something the session slot notifies on, and the poll costs a lock
        // acquisition every 50 ms while a human starts Blender.
        loop {
            if let Some(session) = self.runtime.slot.get() {
                return Ok(session);
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(BlenderError::not_connected()
                    .with_detail("waited_ms", within.as_millis() as u64)
                    .with_detail("listen_address", self.listen_address.to_string()));
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// Send a command and wait for its response, using the default timeout for
    /// the operation.
    pub async fn call(&self, op: &str, args: Value) -> Result<Value, BlenderError> {
        let command = to_command(op, args)?;
        self.execute(command, None).await
    }

    /// Send a command with an explicit deadline.
    pub async fn call_with_timeout(
        &self,
        op: &str,
        args: Value,
        timeout: Duration,
    ) -> Result<Value, BlenderError> {
        let command = to_command(op, args)?;
        self.execute(command, Some(timeout)).await
    }

    /// Send a typed command.
    pub async fn execute(
        &self,
        command: Command,
        timeout: Option<Duration>,
    ) -> Result<Value, BlenderError> {
        let session = self.session()?;
        let timeout = timeout.unwrap_or_else(|| crate::default_timeout_for(&command.op));
        let request_id = RequestId::new();
        let op = command.op.clone();

        let receiver = self
            .runtime
            .pending
            .register(request_id, session.id, op.clone());
        let envelope = Envelope::Request(Request {
            request_id,
            command,
            timeout_ms: Some(timeout.as_millis() as u64),
        });

        if let Err(error) = session.send(envelope).await {
            self.runtime.pending.cancel(request_id);
            return Err(error);
        }

        match tokio::time::timeout(timeout, receiver).await {
            Ok(Ok(Ok(response))) => response.into_result(),
            Ok(Ok(Err(error))) => Err(error),
            // The sender was dropped without answering: the connection died
            // between registration and completion.
            Ok(Err(_)) => Err(BlenderError::new(
                ErrorCode::ConnectionLost,
                "The connection to Blender ended before the response arrived.",
            )
            .with_detail("op", op)),
            Err(_) => {
                self.runtime.pending.cancel(request_id);
                Err(pending::timed_out(&op, timeout.as_millis() as u64))
            }
        }
    }

    /// Send a command only if Blender is connected, returning `None` otherwise.
    ///
    /// Used by background maintenance that should never surface a connection
    /// error to the caller.
    pub async fn try_call(&self, op: &str, args: Value) -> Option<Result<Value, BlenderError>> {
        if !self.is_connected() {
            return None;
        }
        Some(self.call(op, args).await)
    }

    /// Whether a read may be retried after a transport failure.
    ///
    /// Writes are never retried automatically: the bridge may have applied the
    /// mutation before the connection dropped, and applying it twice is worse
    /// than reporting the failure.
    pub fn may_retry(kind: OpKind, error: &BlenderError) -> bool {
        kind.retry_safe() && error.retryable
    }
}

fn to_command(op: &str, args: Value) -> Result<Command, BlenderError> {
    match args {
        Value::Object(map) => Ok(Command::with_args(op, map)),
        Value::Null => Ok(Command::new(op)),
        other => Err(BlenderError::invalid_argument(format!(
            "Command arguments must be a JSON object, got {}.",
            match other {
                Value::Array(_) => "an array",
                Value::String(_) => "a string",
                Value::Number(_) => "a number",
                Value::Bool(_) => "a boolean",
                _ => "another type",
            }
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::net::Ipv4Addr;

    use blender_protocol::{
        capabilities::{BlenderIdentity, Capabilities},
        envelope::Response,
        handshake::{Hello, HelloAck},
        version::{BlenderVersion, PROTOCOL_VERSION},
    };
    use tokio::net::TcpStream;

    use super::*;
    use crate::framing;

    /// Stand in for the add-on: complete the handshake, then answer requests
    /// with `responder`.
    async fn fake_addon(
        address: String,
        responder: impl Fn(&Request) -> Option<Envelope> + Send + 'static,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            let stream = TcpStream::connect(address).await.unwrap();
            let (reader, writer) = stream.into_split();
            let mut reader = tokio::io::BufReader::new(reader);
            let mut writer = tokio::io::BufWriter::new(writer);
            let max = crate::DEFAULT_MAX_FRAME_BYTES;

            let hello = match framing::read_frame(&mut reader, max).await.unwrap() {
                Envelope::Hello(hello) => hello,
                other => panic!("expected hello, got {}", other.type_name()),
            };
            framing::write_frame(&mut writer, &Envelope::HelloAck(ack_for(&hello)), max)
                .await
                .unwrap();

            while let Ok(frame) = framing::read_frame(&mut reader, max).await {
                if let Envelope::Request(request) = frame
                    && let Some(reply) = responder(&request)
                {
                    framing::write_frame(&mut writer, &reply, max)
                        .await
                        .unwrap();
                }
            }
        })
    }

    fn ack_for(hello: &Hello) -> HelloAck {
        HelloAck {
            protocol_version: PROTOCOL_VERSION,
            session_id: hello.session_id,
            identity: BlenderIdentity {
                blender_version: BlenderVersion::new(5, 1, 0),
                python_version: "3.13.0".into(),
                addon_version: "0.1.0".into(),
                platform: "windows".into(),
                background: true,
            },
            capabilities: Capabilities::default(),
            revision: 1,
        }
    }

    async fn client() -> BlenderClient {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        BlenderClient::from_listener(Config::default(), listener)
    }

    #[tokio::test]
    async fn reports_disconnected_before_anything_connects() {
        let client = client().await;
        let status = client.status();
        assert!(!status.connected);
        assert_eq!(
            client
                .call("scene.summary", serde_json::json!({}))
                .await
                .unwrap_err()
                .code,
            ErrorCode::BlenderNotConnected
        );
    }

    #[tokio::test]
    async fn round_trips_a_request() {
        let client = client().await;
        let addon = fake_addon(client.listen_address().to_string(), |request| {
            Some(Envelope::Response(Response::success(
                request.request_id,
                serde_json::json!({"echo": request.command.op}),
            )))
        })
        .await;

        client.wait_connected(Duration::from_secs(5)).await.unwrap();
        let result = client
            .call("scene.summary", serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(result["echo"], "scene.summary");

        let status = client.status();
        assert!(status.connected);
        assert_eq!(status.blender_version.as_deref(), Some("5.1.0"));
        assert!(status.background);
        addon.abort();
    }

    #[tokio::test]
    async fn structured_errors_survive_the_round_trip() {
        let client = client().await;
        let addon = fake_addon(client.listen_address().to_string(), |request| {
            Some(Envelope::Response(Response::failure(
                request.request_id,
                BlenderError::not_found("object", "Cube"),
            )))
        })
        .await;

        client.wait_connected(Duration::from_secs(5)).await.unwrap();
        let error = client
            .call("object.get", serde_json::json!({}))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::ObjectNotFound);
        assert_eq!(error.details["reference"], "Cube");
        addon.abort();
    }

    #[tokio::test]
    async fn a_silent_addon_times_out_without_leaking_the_request() {
        let client = client().await;
        let addon = fake_addon(client.listen_address().to_string(), |_| None).await;

        client.wait_connected(Duration::from_secs(5)).await.unwrap();
        let error = client
            .call_with_timeout(
                "object.get",
                serde_json::json!({}),
                Duration::from_millis(150),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Timeout);
        assert!(error.retryable);
        assert_eq!(
            client.status().pending_requests,
            0,
            "the timed-out request must be dropped"
        );
        addon.abort();
    }

    #[tokio::test]
    async fn a_dropped_connection_fails_waiters_promptly() {
        let client = client().await;
        let addon = fake_addon(client.listen_address().to_string(), |_| None).await;
        client.wait_connected(Duration::from_secs(5)).await.unwrap();

        let pending = {
            let client = client.clone();
            tokio::spawn(async move {
                client
                    .call_with_timeout(
                        "render.execute",
                        serde_json::json!({}),
                        Duration::from_secs(30),
                    )
                    .await
            })
        };
        // Give the request time to reach the add-on before killing it.
        tokio::time::sleep(Duration::from_millis(100)).await;
        addon.abort();

        let error = pending.await.unwrap().unwrap_err();
        assert_eq!(error.code, ErrorCode::ConnectionLost);
        assert!(!client.is_connected());
    }

    #[tokio::test]
    async fn non_object_arguments_are_rejected_before_sending() {
        let client = client().await;
        let error = client
            .call("object.create", serde_json::json!([1, 2, 3]))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn writes_are_never_auto_retried() {
        let transient = BlenderError::new(ErrorCode::ConnectionLost, "gone");
        assert!(BlenderClient::may_retry(OpKind::Read, &transient));
        assert!(!BlenderClient::may_retry(OpKind::Write, &transient));
        assert!(!BlenderClient::may_retry(
            OpKind::ExternalSideEffect,
            &transient
        ));
    }
}
