//! UDP transport — fire-and-forget datagrams (spec §5.3).
//!
//! The compressed batch is split into chunked datagrams (each well under the
//! 65 507-byte UDP upper bound), each prefixed by a 9-byte framing header so a
//! receiver can reorder and reassemble batches that exceed a single payload.

use tpt_log_core::{Config, Transport, TransportError};

/// Magic bytes identifying tpt-microlog UDP frames.
pub const FRAME_MAGIC: &[u8; 4] = b"TPT1";
/// Max bytes shipped in a single datagram.
pub const MAX_DATAGRAM_BYTES: usize = 60_000;
/// Header bytes: magic (4) + chunk index u16 BE + chunk count u16 BE + flags u8.
pub const HEADER_LEN: usize = 9;
/// `flags` bit meaning the payload chunk is zstd-compressed.
pub const FLAG_COMPRESSED: u8 = 0x1;

/// UDP fire-and-forget transport.
pub struct UdpTransport {
    socket: std::net::UdpSocket,
    target: std::net::SocketAddr,
}

impl std::fmt::Debug for UdpTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UdpTransport")
            .field("target", &self.target)
            .finish()
    }
}

impl UdpTransport {
    /// Bind to an ephemeral local port and aim at `host:port`.
    pub fn new(host: &str, port: u16) -> Result<Self, TransportError> {
        let socket = std::net::UdpSocket::bind(("0.0.0.0", 0))
            .map_err(|e| TransportError::Config(format!("udp bind failed: {e}")))?;
        let target = std::net::ToSocketAddrs::to_socket_addrs(&(host, port))
            .map_err(|e| TransportError::Config(format!("udp resolve failed: {e}")))?
            .next()
            .ok_or_else(|| {
                TransportError::Config(format!("udp target {host}:{port} did not resolve"))
            })?;
        Ok(Self { socket, target })
    }

    /// Build from core config (`net_endpoint` as `host:port`).
    pub fn from_config(config: &Config) -> Result<Self, TransportError> {
        let endpoint = config
            .net_endpoint
            .clone()
            .ok_or_else(|| TransportError::Config("UDP backend needs `net_endpoint`".into()))?;
        let (host, port) = split_endpoint(&endpoint)?;
        Self::new(&host, port)
    }
}

impl Transport for UdpTransport {
    fn send(&self, batch: &tpt_log_core::CompressedBatch) -> Result<(), TransportError> {
        let payload = &batch.payload.bytes;
        let max_chunk = MAX_DATAGRAM_BYTES - HEADER_LEN;
        let chunk_count = if payload.is_empty() {
            1
        } else {
            payload.len().div_ceil(max_chunk)
        };
        for (index, chunk) in payload.chunks(max_chunk).enumerate() {
            let mut frame = Vec::with_capacity(HEADER_LEN + chunk.len());
            frame.extend_from_slice(FRAME_MAGIC);
            frame.extend_from_slice(&(index as u16).to_be_bytes());
            frame.extend_from_slice(&(chunk_count as u16).to_be_bytes());
            frame.push(FLAG_COMPRESSED);
            frame.extend_from_slice(chunk);
            self.socket
                .send_to(&frame, self.target)
                .map_err(|e| TransportError::Transient(format!("udp send failed: {e}")))?;
        }
        Ok(())
    }

    fn name(&self) -> &str {
        "udp"
    }
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

/// Reassemble received datagrams into the original batch payload.
///
/// Mirrors the framing the transport emits; exposed for receivers and tests.
/// Returns an error when a chunk is missing or indices are inconsistent.
pub fn reassemble(frames: &[&[u8]]) -> Result<Vec<u8>, String> {
    if frames.is_empty() {
        return Ok(Vec::new());
    }
    let first = frames[0];
    if first.len() < HEADER_LEN || &first[..4] != FRAME_MAGIC {
        return Err("not a tpt-microlog frame".to_string());
    }
    let chunk_count = u16::from_be_bytes([first[6], first[7]]) as usize;
    let mut slots: Vec<Option<&[u8]>> = vec![None; chunk_count];
    for frame in frames {
        if frame.len() < HEADER_LEN || &frame[..4] != FRAME_MAGIC {
            return Err("not a tpt-microlog frame".to_string());
        }
        let index = u16::from_be_bytes([frame[4], frame[5]]) as usize;
        let count = u16::from_be_bytes([frame[6], frame[7]]) as usize;
        if count != chunk_count || index >= chunk_count {
            return Err("inconsistent frame indices".to_string());
        }
        slots[index] = Some(&frame[HEADER_LEN..]);
    }
    let mut out = Vec::new();
    for slot in slots {
        out.extend_from_slice(slot.ok_or_else(|| "missing datagram chunk".to_string())?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reassemble_round_trips_multi_chunk() {
        let payload: Vec<u8> = (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let max_chunk = MAX_DATAGRAM_BYTES - HEADER_LEN;
        let chunk_count = payload.len().div_ceil(max_chunk);
        assert!(chunk_count > 1);
        let frames: Vec<Vec<u8>> = payload
            .chunks(max_chunk)
            .enumerate()
            .map(|(index, chunk)| frame(index, chunk_count, chunk))
            .collect();
        let frame_refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        assert_eq!(reassemble(&frame_refs).unwrap(), payload);
    }

    #[test]
    fn reassemble_is_order_insensitive() {
        let payload: Vec<u8> = (0..80_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let max_chunk = MAX_DATAGRAM_BYTES - HEADER_LEN;
        let chunk_count = payload.len().div_ceil(max_chunk);
        let mut frames: Vec<Vec<u8>> = payload
            .chunks(max_chunk)
            .enumerate()
            .map(|(index, chunk)| frame(index, chunk_count, chunk))
            .collect();
        frames.reverse();
        let frame_refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        assert_eq!(reassemble(&frame_refs).unwrap(), payload);
    }

    #[test]
    fn reassemble_rejects_missing_chunk() {
        let payload: Vec<u8> = (0..80_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let max_chunk = MAX_DATAGRAM_BYTES - HEADER_LEN;
        let chunk_count = payload.len().div_ceil(max_chunk);
        let mut frames: Vec<Vec<u8>> = payload
            .chunks(max_chunk)
            .enumerate()
            .map(|(index, chunk)| frame(index, chunk_count, chunk))
            .collect();
        frames.pop();
        let frame_refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        assert!(reassemble(&frame_refs).is_err());
    }

    fn frame(index: usize, chunk_count: usize, chunk: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(HEADER_LEN + chunk.len());
        frame.extend_from_slice(FRAME_MAGIC);
        frame.extend_from_slice(&(index as u16).to_be_bytes());
        frame.extend_from_slice(&(chunk_count as u16).to_be_bytes());
        frame.push(FLAG_COMPRESSED);
        frame.extend_from_slice(chunk);
        frame
    }
}
