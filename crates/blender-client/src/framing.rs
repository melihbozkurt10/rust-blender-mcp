//! Length-prefixed JSON framing.
//!
//! Each frame is a 4-byte big-endian unsigned length followed by exactly that
//! many bytes of UTF-8 JSON. The reader is written against the three things
//! that actually go wrong on a stream socket: a frame split across reads,
//! several frames arriving in one read, and a peer that announces a length it
//! never sends.

use std::io;

use blender_protocol::{
    envelope::Envelope,
    error::{BlenderError, ErrorCode},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Default cap on a single frame. Generous enough for a large node graph or a
/// mesh analysis, small enough that a corrupt length prefix cannot make the
/// server allocate its way out of memory.
pub const DEFAULT_MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Bytes of length prefix.
pub const HEADER_LEN: usize = 4;

/// What can go wrong reading or writing a frame.
#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    /// The peer closed the connection cleanly between frames.
    #[error("connection closed by peer")]
    Closed,
    /// The peer closed mid-frame, so the stream is not recoverable.
    #[error("connection closed mid-frame after {read} of {expected} bytes")]
    Truncated { read: usize, expected: usize },
    #[error("frame of {size} bytes exceeds the {limit} byte limit")]
    TooLarge { size: u32, limit: u32 },
    #[error("frame was not valid UTF-8: {0}")]
    NotUtf8(#[from] std::string::FromUtf8Error),
    #[error("frame was not a valid protocol message: {0}")]
    BadJson(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl FrameError {
    /// Whether the connection can keep being used after this error.
    ///
    /// A bad message is one message's problem; a truncated stream or an I/O
    /// failure means the socket is finished.
    pub const fn is_fatal(&self) -> bool {
        !matches!(self, FrameError::BadJson(_) | FrameError::NotUtf8(_))
    }
}

impl From<FrameError> for BlenderError {
    fn from(error: FrameError) -> Self {
        let code = match &error {
            FrameError::Closed | FrameError::Truncated { .. } => ErrorCode::ConnectionLost,
            FrameError::TooLarge { .. } => ErrorCode::MessageTooLarge,
            FrameError::NotUtf8(_) | FrameError::BadJson(_) => ErrorCode::BlenderInternalError,
            FrameError::Io(_) => ErrorCode::ConnectionLost,
        };
        BlenderError::new(code, error.to_string())
    }
}

/// Read exactly one frame.
///
/// `read_exact` handles both partial reads and multiple frames per underlying
/// read, because the caller is buffered: anything past this frame stays in the
/// stream for the next call.
pub async fn read_frame<R>(reader: &mut R, max_bytes: u32) -> Result<Envelope, FrameError>
where
    R: AsyncRead + Unpin,
{
    let bytes = read_frame_bytes(reader, max_bytes).await?;
    let text = String::from_utf8(bytes)?;
    Ok(serde_json::from_str(&text)?)
}

/// Read one frame's payload without decoding it. Used by tests and by the
/// diagnostic path that needs to log a frame it could not parse.
pub async fn read_frame_bytes<R>(reader: &mut R, max_bytes: u32) -> Result<Vec<u8>, FrameError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0u8; HEADER_LEN];
    match reader.read_exact(&mut header).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Err(FrameError::Closed),
        Err(e) => return Err(FrameError::Io(e)),
    }

    let size = u32::from_be_bytes(header);
    if size > max_bytes {
        // The length is not trustworthy, so the stream cannot be resynchronised
        // -- the caller must drop the connection.
        return Err(FrameError::TooLarge {
            size,
            limit: max_bytes,
        });
    }
    if size == 0 {
        return Ok(Vec::new());
    }

    let mut payload = vec![0u8; size as usize];
    match reader.read_exact(&mut payload).await {
        Ok(_) => Ok(payload),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Err(FrameError::Truncated {
            read: 0,
            expected: size as usize,
        }),
        Err(e) => Err(FrameError::Io(e)),
    }
}

/// Write one frame.
pub async fn write_frame<W>(
    writer: &mut W,
    envelope: &Envelope,
    max_bytes: u32,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let payload = serde_json::to_vec(envelope)?;
    write_frame_bytes(writer, &payload, max_bytes).await
}

/// Write a pre-encoded payload as one frame.
pub async fn write_frame_bytes<W>(
    writer: &mut W,
    payload: &[u8],
    max_bytes: u32,
) -> Result<(), FrameError>
where
    W: AsyncWrite + Unpin,
{
    let size = u32::try_from(payload.len()).map_err(|_| FrameError::TooLarge {
        size: u32::MAX,
        limit: max_bytes,
    })?;
    if size > max_bytes {
        return Err(FrameError::TooLarge {
            size,
            limit: max_bytes,
        });
    }
    // One write call for header and payload together: two writes on a TCP
    // socket invite a partial flush between them under load.
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.extend_from_slice(&size.to_be_bytes());
    frame.extend_from_slice(payload);
    writer.write_all(&frame).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use blender_protocol::{
        command::Command,
        envelope::{Request, RequestId},
    };

    use super::*;

    fn request() -> Envelope {
        Envelope::Request(Request {
            request_id: RequestId::new(),
            command: Command::new("object.list"),
            timeout_ms: None,
        })
    }

    #[tokio::test]
    async fn round_trips_a_frame() {
        let mut buffer = Vec::new();
        write_frame(&mut buffer, &request(), DEFAULT_MAX_FRAME_BYTES)
            .await
            .unwrap();
        let mut cursor = std::io::Cursor::new(buffer);
        let back = read_frame(&mut cursor, DEFAULT_MAX_FRAME_BYTES)
            .await
            .unwrap();
        assert_eq!(back.type_name(), "request");
    }

    #[tokio::test]
    async fn reads_several_frames_from_one_buffer() {
        let mut buffer = Vec::new();
        for _ in 0..3 {
            write_frame(&mut buffer, &request(), DEFAULT_MAX_FRAME_BYTES)
                .await
                .unwrap();
        }
        let mut cursor = std::io::Cursor::new(buffer);
        for _ in 0..3 {
            assert_eq!(
                read_frame(&mut cursor, DEFAULT_MAX_FRAME_BYTES)
                    .await
                    .unwrap()
                    .type_name(),
                "request"
            );
        }
        assert!(matches!(
            read_frame(&mut cursor, DEFAULT_MAX_FRAME_BYTES).await,
            Err(FrameError::Closed)
        ));
    }

    #[tokio::test]
    async fn handles_a_frame_split_across_reads() {
        // A duplex pipe delivers exactly what is written, when it is written,
        // so writing the header first proves the reader waits for the rest.
        let (mut client, mut server) = tokio::io::duplex(64);
        let payload = serde_json::to_vec(&request()).unwrap();
        let size = payload.len() as u32;

        let writer = tokio::spawn(async move {
            client.write_all(&size.to_be_bytes()).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            client.write_all(&payload[..4]).await.unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            client.write_all(&payload[4..]).await.unwrap();
        });

        let frame = read_frame(&mut server, DEFAULT_MAX_FRAME_BYTES)
            .await
            .unwrap();
        assert_eq!(frame.type_name(), "request");
        writer.await.unwrap();
    }

    #[tokio::test]
    async fn rejects_an_oversized_declared_length() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&u32::MAX.to_be_bytes());
        let mut cursor = std::io::Cursor::new(buffer);
        let err = read_frame(&mut cursor, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::TooLarge { limit: 1024, .. }));
        assert!(err.is_fatal());
    }

    #[tokio::test]
    async fn refuses_to_write_an_oversized_frame() {
        let big = Envelope::Fatal {
            error: BlenderError::internal("x".repeat(5000)),
            details: Default::default(),
        };
        let mut buffer = Vec::new();
        let err = write_frame(&mut buffer, &big, 1024).await.unwrap_err();
        assert!(matches!(err, FrameError::TooLarge { .. }));
        assert!(
            buffer.is_empty(),
            "nothing should be written when the frame is refused"
        );
    }

    #[tokio::test]
    async fn truncated_payload_is_reported_as_such() {
        let mut buffer = Vec::new();
        buffer.extend_from_slice(&100u32.to_be_bytes());
        buffer.extend_from_slice(b"only ten b");
        let mut cursor = std::io::Cursor::new(buffer);
        assert!(matches!(
            read_frame(&mut cursor, DEFAULT_MAX_FRAME_BYTES).await,
            Err(FrameError::Truncated { .. })
        ));
    }

    #[tokio::test]
    async fn malformed_json_is_not_fatal() {
        let mut buffer = Vec::new();
        write_frame_bytes(&mut buffer, b"{not json", DEFAULT_MAX_FRAME_BYTES)
            .await
            .unwrap();
        let mut cursor = std::io::Cursor::new(buffer);
        let err = read_frame(&mut cursor, DEFAULT_MAX_FRAME_BYTES)
            .await
            .unwrap_err();
        assert!(matches!(err, FrameError::BadJson(_)));
        assert!(
            !err.is_fatal(),
            "one bad message must not kill the connection"
        );
    }
}
