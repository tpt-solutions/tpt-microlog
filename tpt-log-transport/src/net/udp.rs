//! UDP transport — fire-and-forget datagrams (spec §5.3).
//!
//! The batch payload is split into MTU-safe chunks (~1.3 KB, comfortably
//! under the ~1.5 KB Ethernet MTU so datagrams are never fragmented), each
//! prefixed by a 13-byte framing header so a receiver can group chunks by
//! batch, reorder, and reassemble payloads that exceed a single datagram.

use std::sync::atomic::{AtomicU32, Ordering};

use tpt_log_core::{Config, Transport, TransportError};

/// Magic bytes identifying tpt-microlog UDP frames.
pub const FRAME_MAGIC: &[u8; 4] = b"TPT1";
/// Max bytes shipped in a single datagram (MTU-safe: fits the ~1500 B
/// Ethernet MTU with IP/UDP headers to spare).
pub const MAX_DATAGRAM_BYTES: usize = 1_300;
/// Header bytes: magic (4) + batch id u32 BE + chunk index u16 BE +
/// chunk count u16 BE + flags u8.
pub const HEADER_LEN: usize = 13;
/// `flags` bit meaning the payload chunk is zstd-compressed.
pub const FLAG_COMPRESSED: u8 = 0x1;

/// UDP fire-and-forget transport.
pub struct UdpTransport {
    socket: std::net::UdpSocket,
    target: std::net::SocketAddr,
    next_batch_id: AtomicU32,
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
        Ok(Self {
            socket,
            target,
            next_batch_id: AtomicU32::new(1),
        })
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
        let flag = if batch.payload.compressed {
            FLAG_COMPRESSED
        } else {
            0
        };
        let batch_id = self.next_batch_id.fetch_add(1, Ordering::Relaxed);
        let max_chunk = MAX_DATAGRAM_BYTES - HEADER_LEN;
        let chunks: Vec<&[u8]> = if payload.is_empty() {
            vec![&[]]
        } else {
            payload.chunks(max_chunk).collect()
        };
        let chunk_count = chunks.len();
        for (index, chunk) in chunks.iter().enumerate() {
            let mut frame = Vec::with_capacity(HEADER_LEN + chunk.len());
            frame.extend_from_slice(FRAME_MAGIC);
            frame.extend_from_slice(&batch_id.to_be_bytes());
            frame.extend_from_slice(&(index as u16).to_be_bytes());
            frame.extend_from_slice(&(chunk_count as u16).to_be_bytes());
            frame.push(flag);
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
/// All frames must belong to the same batch (identical batch id and chunk
/// count). Returns an error when a chunk is missing or the headers are
/// inconsistent.
pub fn reassemble(frames: &[&[u8]]) -> Result<Vec<u8>, String> {
    if frames.is_empty() {
        return Ok(Vec::new());
    }
    let first = frames[0];
    if first.len() < HEADER_LEN || &first[..4] != FRAME_MAGIC {
        return Err("not a tpt-microlog frame".to_string());
    }
    let batch_id = u32::from_be_bytes([first[4], first[5], first[6], first[7]]);
    let chunk_count = u16::from_be_bytes([first[10], first[11]]) as usize;
    let mut slots: Vec<Option<&[u8]>> = vec![None; chunk_count];
    for frame in frames {
        if frame.len() < HEADER_LEN || &frame[..4] != FRAME_MAGIC {
            return Err("not a tpt-microlog frame".to_string());
        }
        let id = u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]);
        if id != batch_id {
            return Err("frames from mixed batches".to_string());
        }
        let index = u16::from_be_bytes([frame[8], frame[9]]) as usize;
        let count = u16::from_be_bytes([frame[10], frame[11]]) as usize;
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

    fn frame(batch_id: u32, index: usize, chunk_count: usize, flag: u8, chunk: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(HEADER_LEN + chunk.len());
        frame.extend_from_slice(FRAME_MAGIC);
        frame.extend_from_slice(&batch_id.to_be_bytes());
        frame.extend_from_slice(&(index as u16).to_be_bytes());
        frame.extend_from_slice(&(chunk_count as u16).to_be_bytes());
        frame.push(flag);
        frame.extend_from_slice(chunk);
        frame
    }

    #[test]
    fn reassemble_round_trips_multi_chunk() {
        let payload: Vec<u8> = (0..200_000u32).flat_map(|i| i.to_le_bytes()).collect();
        let max_chunk = MAX_DATAGRAM_BYTES - HEADER_LEN;
        let chunk_count = payload.len().div_ceil(max_chunk);
        assert!(chunk_count > 1);
        let frames: Vec<Vec<u8>> = payload
            .chunks(max_chunk)
            .enumerate()
            .map(|(index, chunk)| frame(7, index, chunk_count, FLAG_COMPRESSED, chunk))
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
            .map(|(index, chunk)| frame(9, index, chunk_count, 0, chunk))
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
            .map(|(index, chunk)| frame(9, index, chunk_count, 0, chunk))
            .collect();
        frames.pop();
        let frame_refs: Vec<&[u8]> = frames.iter().map(|f| f.as_slice()).collect();
        assert!(reassemble(&frame_refs).is_err());
    }

    #[test]
    fn reassemble_rejects_mixed_batch_ids() {
        let a = frame(1, 0, 1, 0, b"batch-a");
        let b = frame(2, 0, 1, 0, b"batch-b");
        let frames = [a.as_slice(), b.as_slice()];
        assert!(reassemble(&frames).is_err());
    }

    #[test]
    fn datagram_size_is_mtu_safe() {
        // A full chunk plus the header must fit a standard MTU datagram.
        const { assert!(MAX_DATAGRAM_BYTES <= 1_400) };
    }
}
