//! TCP transport — ordered, reliable streaming (spec §5.3).
//!
//! Each batch is sent as a single length-prefixed frame:
//!
//! ```text
//! u32 BE length | u8 flags | payload
//! ```
//!
//! where `flags` bit 0 marks the payload as zstd-compressed. With
//! `tcp_ack = true` the transport waits (bounded) for a one-byte `K`
//! acknowledgement from the receiver and treats anything else as a
//! transient failure. A dropped connection triggers a lazy reconnect on the
//! next attempt; no call panics on I/O errors.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use tpt_log_core::{Config, Transport, TransportError};

/// Bound on a single frame write so a stalled receiver cannot hang `send`.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Bound on the ack read.
const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// `flags` bit meaning the payload is zstd-compressed.
pub const FLAG_COMPRESSED: u8 = 0x1;
/// Ack byte a receiver writes after persisting a frame.
pub const ACK_BYTE: u8 = b'K';

/// TCP streaming transport.
pub struct TcpTransport {
    target: SocketAddr,
    connect_timeout: Duration,
    want_ack: bool,
    stream: std::sync::Mutex<Option<TcpStream>>,
}

impl std::fmt::Debug for TcpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpTransport")
            .field("target", &self.target)
            .field("want_ack", &self.want_ack)
            .finish()
    }
}

impl TcpTransport {
    /// Aim at `host:port`. The first connection is established lazily on the
    /// first `send`, so a receiver need not be listening at construction time.
    pub fn new(host: &str, port: u16) -> Result<Self, TransportError> {
        Self::with_options(host, port, false)
    }

    /// Like [`TcpTransport::new`], with the receiver-ack handshake enabled.
    pub fn with_options(host: &str, port: u16, want_ack: bool) -> Result<Self, TransportError> {
        let target = ToSocketAddrs::to_socket_addrs(&(host, port))
            .map_err(|e| TransportError::Config(format!("tcp resolve failed: {e}")))?
            .next()
            .ok_or_else(|| {
                TransportError::Config(format!("tcp target {host}:{port} did not resolve"))
            })?;
        Ok(Self {
            target,
            connect_timeout: Duration::from_secs(5),
            want_ack,
            stream: std::sync::Mutex::new(None),
        })
    }

    /// Build from core config (`net_endpoint` as `host:port`, `tcp_ack`).
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let endpoint = config
            .net_endpoint
            .clone()
            .ok_or_else(|| TransportError::Config("TCP backend needs `net_endpoint`".into()))?;
        let (host, port) = split_endpoint(&endpoint)?;
        Self::with_options(&host, port, config.tcp_ack)
    }

    fn ensure_stream(&self) -> Result<TcpStream, TransportError> {
        let mut guard = self.stream.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(stream) = guard.as_ref() {
            return stream
                .try_clone()
                .map_err(|e| TransportError::Transient(format!("tcp stream clone failed: {e}")));
        }
        let stream = TcpStream::connect_timeout(&self.target, self.connect_timeout)
            .map_err(|e| TransportError::Transient(format!("tcp connect failed: {e}")))?;
        stream
            .set_nodelay(true)
            .map_err(|e| TransportError::Transient(format!("tcp setup failed: {e}")))?;
        stream
            .set_write_timeout(Some(WRITE_TIMEOUT))
            .map_err(|e| TransportError::Transient(format!("tcp setup failed: {e}")))?;
        stream
            .set_read_timeout(Some(ACK_TIMEOUT))
            .map_err(|e| TransportError::Transient(format!("tcp setup failed: {e}")))?;
        let cloned = stream
            .try_clone()
            .map_err(|e| TransportError::Transient(format!("tcp stream clone failed: {e}")))?;
        *guard = Some(stream);
        Ok(cloned)
    }

    fn drop_stream(&self) {
        if let Ok(mut guard) = self.stream.lock() {
            *guard = None;
        }
    }
}

impl Transport for TcpTransport {
    fn send(&self, batch: &tpt_log_core::CompressedBatch) -> Result<(), TransportError> {
        let payload = &batch.payload;
        let flags = if payload.compressed {
            FLAG_COMPRESSED
        } else {
            0
        };
        let mut frame = Vec::with_capacity(5 + payload.bytes.len());
        // Length prefix covers flags + payload.
        frame.extend_from_slice(&((payload.bytes.len() + 1) as u32).to_be_bytes());
        frame.push(flags);
        frame.extend_from_slice(&payload.bytes);

        let mut stream = self.ensure_stream()?;
        let write = stream.write_all(&frame).and_then(|()| stream.flush());
        if let Err(_e) = write {
            self.drop_stream();
            // One reconnect attempt per send.
            stream = self.ensure_stream()?;
            stream
                .write_all(&frame)
                .and_then(|()| stream.flush())
                .map_err(|e| {
                    self.drop_stream();
                    TransportError::Transient(format!("tcp write failed: {e}"))
                })?;
        }
        if self.want_ack {
            let mut ack = [0u8; 1];
            match stream.read_exact(&mut ack) {
                Ok(()) if ack[0] == ACK_BYTE => {}
                Ok(()) => {
                    self.drop_stream();
                    return Err(TransportError::Transient(
                        "tcp receiver sent an invalid ack".into(),
                    ));
                }
                Err(e) => {
                    self.drop_stream();
                    return Err(TransportError::Transient(format!("tcp ack failed: {e}")));
                }
            }
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "tcp"
    }
}

/// Length of the frame header: u32 BE length + u8 flags.
pub const FRAME_HEADER_LEN: usize = 5;

/// Parse a frame as emitted by [`TcpTransport::send`] (without the length
/// prefix, e.g. as returned by [`read_frame`]): `(compressed, payload)`.
pub fn parse_frame(frame: &[u8]) -> Result<(bool, &[u8]), String> {
    if frame.is_empty() {
        return Err("empty frame".to_string());
    }
    let compressed = frame[0] & FLAG_COMPRESSED != 0;
    Ok((compressed, &frame[1..]))
}

/// Drain a length-prefixed frame from a reader. Returns `Ok(None)` on a clean
/// EOF before any data, `Err(..)` when the stream broke mid-frame. Exposed for
/// receivers/tests to parse frames the transport emits.
pub fn read_frame<R: Read>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_buf = [0u8; 4];
    let mut read = 0;
    while read < 4 {
        match reader.read(&mut len_buf[read..]) {
            Ok(0) => {
                if read == 0 {
                    return Ok(None);
                }
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "truncated length prefix",
                ));
            }
            Ok(n) => read += n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        }
    }
    let len = u32::from_be_bytes(len_buf) as usize;
    let mut payload = vec![0u8; len];
    reader
        .read_exact(&mut payload)
        .map_err(|e| std::io::Error::new(e.kind(), format!("truncated frame: {e}")))?;
    Ok(Some(payload))
}

fn split_endpoint(endpoint: &str) -> Result<(String, u16), TransportError> {
    match endpoint.rsplit_once(':') {
        Some((host, port)) => {
            let port = port
                .parse()
                .map_err(|_| TransportError::Config(format!("invalid port in `{endpoint}`")))?;
            Ok((host.to_string(), port))
        }
        None => Err(TransportError::Config(format!(
            "net_endpoint must be `host:port`, got `{endpoint}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_frame_parses_len_prefix_and_flags_survive() {
        let payload: Vec<u8> = (0..200u8).collect();
        let mut framed = ((payload.len() + 1) as u32).to_be_bytes().to_vec();
        framed.push(FLAG_COMPRESSED);
        framed.extend_from_slice(&payload);
        let mut cursor = std::io::Cursor::new(framed);
        let frame = read_frame(&mut cursor).unwrap().unwrap();
        let (compressed, inner) = parse_frame(&frame).unwrap();
        assert!(compressed);
        assert_eq!(inner, payload.as_slice());
        assert_eq!(read_frame(&mut cursor).unwrap(), None);
    }

    #[test]
    fn read_frame_detects_truncation() {
        let short = vec![0u8, 0, 0, 5, 1, 2];
        let mut cursor = std::io::Cursor::new(short);
        assert!(read_frame(&mut cursor).is_err());
    }

    #[test]
    fn parse_frame_rejects_empty() {
        assert!(parse_frame(&[]).is_err());
        let (compressed, payload) = parse_frame(&[0, 1, 2, 3]).unwrap();
        assert!(!compressed);
        assert_eq!(payload, &[1, 2, 3]);
    }
}
