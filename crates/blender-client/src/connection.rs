//! Accepting connections from the Blender add-on and pumping frames.
//!
//! The server listens; the add-on dials in. That direction is deliberate. The
//! MCP server is started and stopped by whatever MCP client is in use, often
//! several times an hour, while Blender stays open for hours. Putting the
//! retry loop in the long-lived process means a server restart is invisible to
//! the user, and Blender needs no knowledge of when the server will appear.

use std::{sync::Arc, time::Duration};

use blender_protocol::{
    envelope::{Envelope, SessionId},
    error::{BlenderError, ErrorCode},
    handshake::{Hello, HelloAck},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::{broadcast, mpsc},
};

use crate::{
    Config,
    framing::{self, FrameError},
    pending::{self, PendingRequests},
    session::{SessionSlot, session_from_ack},
};

/// Everything a connection needs to service requests.
pub struct Runtime {
    pub config: Config,
    pub slot: Arc<SessionSlot>,
    pub pending: PendingRequests,
    pub events: broadcast::Sender<blender_protocol::event::Event>,
}

/// Accept connections forever, servicing one at a time.
///
/// A second connection replaces the first: when Blender crashes, its old socket
/// can linger half-open for minutes, and refusing the new connection would make
/// the tool unusable until the OS noticed.
pub async fn serve(runtime: Arc<Runtime>, listener: TcpListener) {
    let local = listener
        .local_addr()
        .map(|a| a.to_string())
        .unwrap_or_else(|_| runtime.config.bind.to_string());
    tracing::info!(address = %local, "waiting for the Blender add-on to connect");

    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(error) => {
                // Per-connection accept errors (a peer that vanished during the
                // handshake, a transient resource limit) must not kill the
                // listener.
                tracing::warn!(%error, "accept failed");
                runtime.slot.record_error(format!("accept failed: {error}"));
                tokio::time::sleep(Duration::from_millis(200)).await;
                continue;
            }
        };

        // Nagle batches small writes, which is exactly wrong for a
        // request/response protocol on loopback.
        if let Err(error) = stream.set_nodelay(true) {
            tracing::debug!(%error, "could not disable Nagle");
        }

        let runtime = Arc::clone(&runtime);
        tokio::spawn(async move {
            let peer = peer.to_string();
            if let Err(error) = handle_connection(runtime.clone(), stream, peer.clone()).await {
                tracing::warn!(%peer, error = %error.message, code = %error.code, "connection ended");
                runtime.slot.record_error(error.message);
            }
        });
    }
}

/// Run one connection from handshake to close.
async fn handle_connection(
    runtime: Arc<Runtime>,
    stream: TcpStream,
    peer: String,
) -> Result<(), BlenderError> {
    let (reader, writer) = stream.into_split();
    let mut reader = tokio::io::BufReader::new(reader);
    let mut writer = tokio::io::BufWriter::new(writer);

    let session_id = SessionId::new();
    let hello = Hello::new(
        runtime.config.client_name.clone(),
        runtime.config.client_version.clone(),
        session_id,
    );

    let ack = handshake(&runtime, &mut reader, &mut writer, &hello).await?;

    let (outbound_tx, outbound_rx) = mpsc::channel::<Envelope>(runtime.config.outbound_queue);
    let session = session_from_ack(ack, outbound_tx, peer.clone());
    let session_id = session.id;

    if let Some(previous) = runtime.slot.replace(session) {
        tracing::info!(
            previous = %previous.id,
            current = %session_id,
            "a new Blender connection replaced the previous one"
        );
        runtime.pending.fail_session(
            previous.id,
            &pending::connection_lost("Replaced by a new Blender connection."),
        );
    }
    // The version and add-on build are what a bug report needs first, so they
    // go in the line that says a connection happened rather than waiting for
    // someone to call `blender.capabilities`.
    if let Some(current) = runtime.slot.get() {
        let identity = &current.identity;
        tracing::info!(
            session = %session_id,
            %peer,
            blender = %identity.blender_version,
            python = %identity.python_version,
            addon = %identity.addon_version,
            platform = %identity.platform,
            background = identity.background,
            "Blender connected"
        );
    }

    let write_task = tokio::spawn(write_loop(
        writer,
        outbound_rx,
        runtime.config.max_frame_bytes,
    ));

    let outcome = read_loop(Arc::clone(&runtime), &mut reader, session_id).await;

    // Dropping the session drops the sender, which ends the write loop.
    let ended = runtime.slot.clear_if(
        session_id,
        outcome
            .as_ref()
            .err()
            .map_or_else(|| "Blender disconnected".to_string(), |e| e.message.clone()),
    );
    if ended {
        runtime.pending.fail_session(
            session_id,
            &pending::connection_lost("Blender disconnected before the request completed."),
        );
    }
    write_task.abort();
    tracing::info!(session = %session_id, "Blender disconnected");
    outcome
}

/// Exchange hello frames and validate the peer.
async fn handshake<R, W>(
    runtime: &Runtime,
    reader: &mut R,
    writer: &mut W,
    hello: &Hello,
) -> Result<HelloAck, BlenderError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    framing::write_frame(
        writer,
        &Envelope::Hello(hello.clone()),
        runtime.config.max_frame_bytes,
    )
    .await?;

    let frame = tokio::time::timeout(
        runtime.config.handshake_timeout,
        framing::read_frame(reader, runtime.config.max_frame_bytes),
    )
    .await
    .map_err(|_| {
        BlenderError::new(
            ErrorCode::Timeout,
            format!(
                "The add-on did not answer the handshake within {:?}.",
                runtime.config.handshake_timeout
            ),
        )
    })??;

    let ack = match frame {
        Envelope::HelloAck(ack) => ack,
        Envelope::Fatal { error, .. } => return Err(error),
        other => {
            return Err(BlenderError::new(
                ErrorCode::ProtocolMismatch,
                format!(
                    "Expected a `hello_ack` frame, got `{}`. The add-on may be a different version.",
                    other.type_name()
                ),
            )
            .with_detail("received", other.type_name()));
        }
    };

    ack.validate(hello)?;
    Ok(ack)
}

/// Read frames until the connection ends.
async fn read_loop<R>(
    runtime: Arc<Runtime>,
    reader: &mut R,
    session_id: SessionId,
) -> Result<(), BlenderError>
where
    R: AsyncRead + Unpin,
{
    loop {
        let frame = match framing::read_frame(reader, runtime.config.max_frame_bytes).await {
            Ok(frame) => frame,
            Err(FrameError::Closed) => return Ok(()),
            Err(error) if !error.is_fatal() => {
                // One undecodable message. Log it and keep the connection: the
                // next frame is independent of this one.
                tracing::warn!(%error, session = %session_id, "ignoring an undecodable frame");
                continue;
            }
            Err(error) => return Err(error.into()),
        };

        match frame {
            Envelope::Response(response) => {
                if !runtime.pending.complete(session_id, response) {
                    tracing::debug!(session = %session_id, "response had no waiter");
                }
            }
            Envelope::Event(event) => {
                if event.session_id != session_id {
                    tracing::debug!("dropping an event from a superseded session");
                    continue;
                }
                // A send error only means nobody is subscribed right now.
                let _ = runtime.events.send(event);
            }
            Envelope::Ping { nonce } => {
                if let Some(session) = runtime.slot.get()
                    && session.id == session_id
                {
                    let _ = session.send(Envelope::Pong { nonce }).await;
                }
            }
            Envelope::Pong { .. } => {}
            Envelope::Fatal { error, details } => {
                tracing::error!(
                    code = %error.code,
                    message = %error.message,
                    ?details,
                    "the add-on reported a fatal error"
                );
                return Err(error);
            }
            Envelope::Hello(_) | Envelope::HelloAck(_) => {
                tracing::warn!("ignoring a second handshake frame on an established connection");
            }
            Envelope::Request(request) => {
                // The add-on does not initiate requests in protocol v1.
                tracing::warn!(
                    op = %request.command.op,
                    "ignoring an unsolicited request from the add-on"
                );
            }
        }
    }
}

/// Drain the outbound queue onto the socket.
async fn write_loop<W>(mut writer: W, mut outbound: mpsc::Receiver<Envelope>, max_frame_bytes: u32)
where
    W: AsyncWrite + Unpin,
{
    while let Some(envelope) = outbound.recv().await {
        if let Err(error) = framing::write_frame(&mut writer, &envelope, max_frame_bytes).await {
            tracing::warn!(%error, kind = envelope.type_name(), "failed to write a frame");
            if error.is_fatal() {
                break;
            }
        }
    }
    let _ = writer.shutdown().await;
}

#[cfg(test)]
mod tests {
    use blender_protocol::{
        capabilities::{BlenderIdentity, Capabilities},
        version::{BlenderVersion, PROTOCOL_VERSION},
    };
    use tokio::io::{AsyncReadExt, duplex};

    use super::*;

    fn runtime() -> Arc<Runtime> {
        let (events, _) = broadcast::channel(16);
        Arc::new(Runtime {
            config: Config::default(),
            slot: Arc::new(SessionSlot::new()),
            pending: PendingRequests::new(),
            events,
        })
    }

    fn ack(session_id: SessionId, blender: BlenderVersion) -> HelloAck {
        HelloAck {
            protocol_version: PROTOCOL_VERSION,
            session_id,
            identity: BlenderIdentity {
                blender_version: blender,
                python_version: "3.13.0".into(),
                addon_version: "0.1.0".into(),
                platform: "windows".into(),
                background: false,
            },
            capabilities: Capabilities::default(),
            revision: 0,
        }
    }

    /// Drive the handshake against a scripted peer.
    async fn run_handshake(
        reply: impl FnOnce(SessionId) -> Envelope + Send + 'static,
    ) -> Result<HelloAck, BlenderError> {
        let runtime = runtime();
        let (mut peer, local) = duplex(64 * 1024);
        let (local_reader, local_writer) = tokio::io::split(local);
        let mut local_reader = tokio::io::BufReader::new(local_reader);
        let mut local_writer = tokio::io::BufWriter::new(local_writer);
        let hello = Hello::new("test", "0.0.0", SessionId::new());
        let offered = hello.session_id;

        tokio::spawn(async move {
            // Read the server's hello, then answer.
            let mut header = [0u8; 4];
            peer.read_exact(&mut header).await.unwrap();
            let len = u32::from_be_bytes(header) as usize;
            let mut body = vec![0u8; len];
            peer.read_exact(&mut body).await.unwrap();
            framing::write_frame(&mut peer, &reply(offered), 1024 * 1024)
                .await
                .unwrap();
        });

        handshake(&runtime, &mut local_reader, &mut local_writer, &hello).await
    }

    #[tokio::test]
    async fn handshake_accepts_a_matching_peer() {
        let ack = run_handshake(|id| Envelope::HelloAck(ack(id, BlenderVersion::new(5, 1, 0))))
            .await
            .unwrap();
        assert_eq!(ack.identity.blender_version, BlenderVersion::new(5, 1, 0));
    }

    #[tokio::test]
    async fn handshake_rejects_an_old_blender() {
        let err = run_handshake(|id| Envelope::HelloAck(ack(id, BlenderVersion::new(3, 6, 0))))
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::UnsupportedBlenderVersion);
    }

    #[tokio::test]
    async fn handshake_rejects_a_mismatched_session_id() {
        let err = run_handshake(|_| {
            Envelope::HelloAck(ack(SessionId::new(), BlenderVersion::new(5, 1, 0)))
        })
        .await
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProtocolMismatch);
    }

    #[tokio::test]
    async fn handshake_rejects_the_wrong_frame_type() {
        let err = run_handshake(|_| Envelope::Ping { nonce: 1 })
            .await
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::ProtocolMismatch);
        assert_eq!(err.details["received"], "ping");
    }

    #[tokio::test]
    async fn write_loop_drains_the_queue() {
        let (mut peer, local) = duplex(64 * 1024);
        let (tx, rx) = mpsc::channel(4);
        let task = tokio::spawn(write_loop(local, rx, 1024 * 1024));

        tx.send(Envelope::Ping { nonce: 7 }).await.unwrap();
        let frame = framing::read_frame(&mut peer, 1024 * 1024).await.unwrap();
        assert!(matches!(frame, Envelope::Ping { nonce: 7 }));

        drop(tx);
        task.await.unwrap();
    }
}
