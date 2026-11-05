//! AVCT packet framing.
//!
//! Both BEEF control packets and display packets use an eight-byte header:
//! four bytes of channel magic, a big-endian type and a big-endian total
//! length.  Display payloads remain `Bytes` until the display decoder needs
//! them, avoiding an application-level copy of compressed frames.

use bytes::{Bytes, BytesMut};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const BEEF_MAGIC: [u8; 4] = *b"BEEF";
pub const AVCT_HEADER_BYTES: usize = 8;
pub const IDRAC6_MAX_AVCT_PACKET_BYTES: usize = 15_000;
pub const IDRAC8_MAX_AVCT_PACKET_BYTES: usize = 32_768;

#[derive(Clone, Debug)]
pub struct AvctPacket {
    pub magic: [u8; 4],
    pub message_type: u16,
    pub payload: Bytes,
}

impl AvctPacket {
    pub fn is_beef(&self) -> bool {
        self.magic == BEEF_MAGIC
    }

    pub fn total_len(&self) -> usize {
        AVCT_HEADER_BYTES + self.payload.len()
    }

    pub fn beef(message_type: u16, payload: impl Into<Bytes>) -> Self {
        Self {
            magic: BEEF_MAGIC,
            message_type,
            payload: payload.into(),
        }
    }
}

pub async fn read_packet<S>(stream: &mut S, max_packet_bytes: usize) -> Result<AvctPacket, String>
where
    S: AsyncRead + Unpin,
{
    let mut header = [0u8; AVCT_HEADER_BYTES];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|error| format!("AVCT packet header read failed: {error}"))?;
    let total_len = u16::from_be_bytes([header[6], header[7]]) as usize;
    if !(AVCT_HEADER_BYTES..=max_packet_bytes).contains(&total_len) {
        return Err(format!(
            "AVCT packet length {total_len} exceeds the negotiated AVCT limit {max_packet_bytes}"
        ));
    }
    let payload_len = total_len - AVCT_HEADER_BYTES;
    let mut payload = BytesMut::zeroed(payload_len);
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|error| format!("AVCT packet body read failed: {error}"))?;
    Ok(AvctPacket {
        magic: header[..4].try_into().unwrap(),
        message_type: u16::from_be_bytes([header[4], header[5]]),
        payload: payload.freeze(),
    })
}

pub async fn write_packet<S>(stream: &mut S, packet: &AvctPacket) -> Result<(), String>
where
    S: AsyncWrite + Unpin,
{
    let total_len = packet.total_len();
    if !(AVCT_HEADER_BYTES..=u16::MAX as usize).contains(&total_len) {
        return Err(format!("AVCT packet length {total_len} cannot be encoded"));
    }
    let mut header = [0u8; AVCT_HEADER_BYTES];
    header[..4].copy_from_slice(&packet.magic);
    header[4..6].copy_from_slice(&packet.message_type.to_be_bytes());
    header[6..8].copy_from_slice(&(total_len as u16).to_be_bytes());
    stream
        .write_all(&header)
        .await
        .map_err(|error| format!("AVCT packet header write failed: {error}"))?;
    stream
        .write_all(&packet.payload)
        .await
        .map_err(|error| format!("AVCT packet body write failed: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("AVCT packet flush failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trips_a_beef_packet() {
        let packet = AvctPacket::beef(0x0102, Bytes::from_static(&[1, 2, 3]));
        let mut wire = Vec::new();
        write_packet(&mut wire, &packet).await.unwrap();
        assert_eq!(&wire[..4], b"BEEF");
        assert_eq!(u16::from_be_bytes([wire[4], wire[5]]), 0x0102);
        assert_eq!(u16::from_be_bytes([wire[6], wire[7]]), 11);
        let mut input = wire.as_slice();
        let decoded = read_packet(&mut input, IDRAC8_MAX_AVCT_PACKET_BYTES)
            .await
            .unwrap();
        assert_eq!(decoded.message_type, packet.message_type);
        assert_eq!(decoded.payload, packet.payload);
    }

    #[tokio::test]
    async fn negotiated_generation_limit_is_enforced_before_allocating_payload() {
        let mut wire = [0u8; AVCT_HEADER_BYTES];
        wire[..4].copy_from_slice(b"BEEF");
        wire[6..8].copy_from_slice(&15_001u16.to_be_bytes());
        let mut input = wire.as_slice();
        assert!(read_packet(&mut input, IDRAC6_MAX_AVCT_PACKET_BYTES)
            .await
            .unwrap_err()
            .contains("negotiated AVCT limit"));
    }
}
