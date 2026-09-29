//! TCP transport — ordered, reliable streaming (spec §5.3).
//!
//! Each batch is sent as a single length-prefixed frame
//! (`u32` big-endian length + compressed batch payload). A dropped
//! connection triggers a lazy reconnect on the next attempt.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::time::Duration;

use tpt_log_core::{Config, Transport, TransportError};

/// TCP streaming transport.
pub struct TcpTransport {
    target: SocketAddr,
    connect_timeout: Duration,
    stream: std::sync::Mutex<Option<TcpStream>>,
}

impl std::fmt::Debug for TcpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TcpTransport")
            .field("target", &self.target)
            .finish()
    }
}

impl TcpTransport {
    /// Aim at `host:port`. The first connection is established lazily on the
    /// first `send`, so a receiver need not be listening at construction time.
    pub fn new(host: &str, port: u16) -> Result<Self, TransportError> {
        let target = ToSocketAddrs::to_socket_addrs(&(host, port))
            .map_err(|e| TransportError::Config(format!("tcp resolve failed: {e}")))?
            .next()
            .ok_or_else(|| {
                TransportError::Config(format!("tcp target {host}:{port} did not resolve"))
            })?;
        Ok(Self {
            target,
            connect_timeout: Duration::from_secs(5),
            stream: std::sync::Mutex::new(None),
        })
    }

    /// Build from core config (`net_endpoint` as `host:port`).
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let endpoint = config
            .net_endpoint
            .clone()
            .ok_or_else(|| TransportError::Config("TCP backend needs `net_endpoint`".into()))?;
        let (host, port) = split_endpoint(&endpoint)?;
        Self::new(&host, port)
    }

    fn ensure_stream(&self) -> Result<TcpStream, TransportError> {
        let mut guard = self.stream.lock().expect("tcp mutex poisoned");
        if let Some(stream) = guard.as_ref() {
            return Ok(stream.try_clone().expect("clone tcp stream"));
        }
        let stream = TcpStream::connect_timeout(&self.target, self.connect_timeout)
            .map_err(|e| TransportError::Transient(format!("tcp connect failed: {e}")))?;
        stream
            .set_nodelay(true)
            .map_err(|e| TransportError::Transient(format!("tcp setup failed: {e}")))?;
        let cloned = stream.try_clone().expect("clone tcp stream");
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
        let mut stream = self.ensure_stream()?;
        let mut frame = Vec::with_capacity(4 + payload.bytes.len());
        frame.extend_from_slice(&(payload.bytes.len() as u32).to_be_bytes());
        frame.extend_from_slice(&payload.bytes);
        if let Err(_e) = stream.write_all(&frame) {
            self.drop_stream();
            // One reconnect attempt per send.
            stream = self.ensure_stream()?;
            stream.write_all(&frame).map_err(|e| {
                self.drop_stream();
                TransportError::Transient(format!("tcp write failed: {e}"))
            })?;
        }
        let _ = stream.flush();
        Ok(())
    }

    fn name(&self) -> &str {
        "tcp"
    }
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

/// Keep `module` referenced so this helper is not flagged as dead code when the
/// crate is used only as a library by consumers that never parse frames.
#[allow(dead_code)]
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_frame_parses_len_prefix() {
        let payload: Vec<u8> = (0..200u8).collect();
        let mut framed = (payload.len() as u32).to_be_bytes().to_vec();
        framed.extend_from_slice(&payload);
        let mut cursor = std::io::Cursor::new(framed);
        assert_eq!(read_frame(&mut cursor).unwrap(), Some(payload));
        assert_eq!(read_frame(&mut cursor).unwrap(), None);
    }

    #[test]
    fn read_frame_detects_truncation() {
        let short = vec![0u8, 0, 0, 5, 1, 2];
        let mut cursor = std::io::Cursor::new(short);
        assert!(read_frame(&mut cursor).is_err());
    }
}
