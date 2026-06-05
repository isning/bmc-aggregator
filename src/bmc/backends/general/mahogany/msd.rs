use std::sync::Arc;
use tokio::sync::Mutex;
use crate::bmc::backends::general::mahogany::storage_server::MahoganyStorageServer;



/// MSD (Mass Storage Device) backend state.
#[derive(Debug, Clone)]
pub struct MsdBackend {
    inner: Arc<Mutex<MsdInner>>,
    pub storage_server: Arc<MahoganyStorageServer>,
}

#[derive(Debug, Clone)]
pub struct MsdInner {
    pub enabled: bool,
    pub image: String,
    pub image_size: u64,
    pub connected: bool,
}

impl MsdBackend {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(MsdInner {
                enabled: false,
                image: String::new(),
                image_size: 0,
                connected: false,
            })),
            storage_server: Arc::new(MahoganyStorageServer::new()),
        }
    }

    pub async fn set_image(&self, path: String, size: u64) {
        let mut inner = self.inner.lock().await;
        inner.enabled = !path.is_empty();
        inner.image_size = size;
        self.storage_server.set_image(Some(path.clone())).await;
        inner.image = path;
    }

    pub async fn set_connected(&self, connected: bool) {
        let mut inner = self.inner.lock().await;
        inner.connected = connected;
    }

    pub async fn status(&self) -> MsdInner {
        self.inner.lock().await.clone()
    }
}

impl Default for MsdBackend {
    fn default() -> Self {
        Self::new()
    }
}
// ── MSD (Mass Storage Device) Protocol ─────────────────────────────
// Wire formats reverse-engineered from M2.JAR (ServerEngines iLO4 KVM).
//
// Messages flow on the MAIN KVM TCP connection:
//   Client→Server: StorageClientConnect (0x99)
//   Client→Server: StorageClientDisconnect (0x9A)
//   Server→Client: StorageStatus (0x89)

pub const MSG_STORAGE_STATUS: u8 = 0x89;         // Server → Client
pub const MSG_STORAGE_CLIENT_CONNECT: u8 = 0x99;  // Client → Server
pub const MSG_STORAGE_CLIENT_DISCONNECT: u8 = 0x9A; // Client → Server

/// Payload size for both StorageClientConnect and StorageStatus.
pub const STORAGE_MSG_PAYLOAD_LEN: usize = 1056;

pub const STORAGE_MSG_WIRE_LEN: usize = 1 + STORAGE_MSG_PAYLOAD_LEN; // 1057

// ── Status constants (from StorageStatus bytecode) ────────────────
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageShareStatus {
    Connected = 0,
    DisconnectedByRequest = 1,
    ConnectFailed = 2,
    DisconnectError = 3,
    NotConnected = 4,
}

impl StorageShareStatus {
    pub fn from_byte(b: u8) -> Self {
        match b {
            0 => Self::Connected,
            1 => Self::DisconnectedByRequest,
            2 => Self::ConnectFailed,
            3 => Self::DisconnectError,
            _ => Self::NotConnected,
        }
    }
}

pub const DEFAULT_STORAGE_PORT: u16 = 5901;

// ─── StorageClientConnect (0x99) Builder ──────────────────────────
// Wire format (1056 bytes payload):
//   [0..16)   ipAddress          – 16 bytes (client IP, can be zeros)
//   [16]      shareIndex0        – byte  (0xFF = undefined)
//   [17]      shareIndex1        – byte  (0xFF = undefined)
//   [18..20)  port               – u16 LE
//   [20]      sharePath0Len      – byte  (length in chars)
//   [21]      sharePath1Len      – byte  (length in chars)
//   [22]      shareType0         – byte  (0xFF = undefined)
//   [23]      shareType1         – byte  (0xFF = undefined)
//   [24]      ipType             – byte  (0 = unknown, 4 = IPv4, 6 = IPv6)
//   [25..32)  reserved           – 7 bytes (zeros)
//   [32..544) sharePath0         – 512 bytes (UTF-16LE, null-padded)
//   [544..1056) sharePath1       – 512 bytes (UTF-16LE, null-padded)

pub fn build_storage_client_connect(
    ip_address: &[u8; 16],
    port: u16,
    share_path0: &str,
    share_path1: &str,
    share_type0: u8,
    share_type1: u8,
    ip_type: u8,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(1 + STORAGE_MSG_PAYLOAD_LEN);
    buf.push(MSG_STORAGE_CLIENT_CONNECT);
    buf.extend_from_slice(ip_address);

    // shareIndex0, shareIndex1 — 0xFF = undefined
    buf.push(0xFF);
    buf.push(0xFF);

    buf.extend_from_slice(&port.to_le_bytes());

    // path length in chars (as byte, max 255)
    let p0_bytes = encode_utf16le(share_path0);
    let p1_bytes = encode_utf16le(share_path1);
    let p0_chars = (p0_bytes.len() / 2).min(255) as u8;
    let p1_chars = (p1_bytes.len() / 2).min(255) as u8;
    buf.push(p0_chars);
    buf.push(p1_chars);

    buf.push(share_type0);
    buf.push(share_type1);
    buf.push(ip_type);

    // reserved[7] = zeros
    buf.extend_from_slice(&[0u8; 7]);

    // sharePath0 — 512 bytes UTF-16LE, padded
    let mut path0 = vec![0u8; 512];
    let copy0 = p0_bytes.len().min(512);
    path0[..copy0].copy_from_slice(&p0_bytes[..copy0]);
    buf.extend_from_slice(&path0);

    // sharePath1 — 512 bytes UTF-16LE, padded
    let mut path1 = vec![0u8; 512];
    let copy1 = p1_bytes.len().min(512);
    path1[..copy1].copy_from_slice(&p1_bytes[..copy1]);
    buf.extend_from_slice(&path1);

    debug_assert_eq!(buf.len(), 1 + STORAGE_MSG_PAYLOAD_LEN);
    buf
}

/// Build a default StorageClientConnect with empty/unset fields.
pub fn build_storage_client_connect_default(ip_type: u8) -> Vec<u8> {
    build_storage_client_connect(
        &[0u8; 16],
        DEFAULT_STORAGE_PORT,
        "",
        "",
        0xFF, // shareType0 = undefined
        0xFF, // shareType1 = undefined
        ip_type,
    )
}

// ─── StorageClientDisconnect (0x9A) Builder ───────────────────────
// Wire format: [type=0x9A, 0x00]

pub fn build_storage_client_disconnect() -> Vec<u8> {
    vec![MSG_STORAGE_CLIENT_DISCONNECT, 0x00]
}

// ─── StorageStatus (0x89) Parser ──────────────────────────────────
// Wire format (1056 bytes payload):
//   [0..16)   ipAddress          – 16 bytes
//   [16]      shareIndex0        – byte
//   [17]      shareIndex1        – byte
//   [18..20)  port               – u16 LE
//   [20]      path0Chars         – byte (path length in chars)
//   [21]      path1Chars         – byte
//   [22]      shareType0         – byte
//   [23]      shareType1         – byte
//   [24]      shareStatus0       – byte
//   [25]      shareStatus1       – byte
//   [26]      ipType             – byte
//   [27..32)  reserved           – 5 bytes
//   [32..544) sharePath0         – 512 bytes (UTF-16LE)
//   [544..1056) sharePath1       – 512 bytes (UTF-16LE)

#[derive(Debug, Clone)]
pub struct StorageStatus {
    pub ip_address: [u8; 16],
    pub share_index0: u8,
    pub share_index1: u8,
    pub port: u16,
    pub share_status0: StorageShareStatus,
    pub share_status1: StorageShareStatus,
    pub share_type0: u8,
    pub share_type1: u8,
    pub ip_type: u8,
    pub share_path0: String,
    pub share_path1: String,
}

pub fn parse_storage_status(payload: &[u8]) -> Result<StorageStatus, anyhow::Error> {
    if payload.len() < STORAGE_MSG_PAYLOAD_LEN {
        anyhow::bail!(
            "StorageStatus payload too short: {} < {}",
            payload.len(),
            STORAGE_MSG_PAYLOAD_LEN
        );
    }

    let mut ip_address = [0u8; 16];
    ip_address.copy_from_slice(&payload[0..16]);

    let share_index0 = payload[16];
    let share_index1 = payload[17];
    let port = u16::from_le_bytes([payload[18], payload[19]]);
    let path0_chars = payload[20] as usize;
    let path1_chars = payload[21] as usize;
    let share_type0 = payload[22];
    let share_type1 = payload[23];
    let share_status0 = StorageShareStatus::from_byte(payload[24]);
    let share_status1 = StorageShareStatus::from_byte(payload[25]);
    let ip_type = payload[26];
    // reserved[5] at bytes 27..32

    let share_path0 = decode_utf16le(&payload[32..544], path0_chars);
    let share_path1 = decode_utf16le(&payload[544..1056], path1_chars);

    Ok(StorageStatus {
        ip_address,
        share_index0,
        share_index1,
        port,
        share_status0,
        share_status1,
        share_type0,
        share_type1,
        ip_type,
        share_path0,
        share_path1,
    })
}

// ─── UTF-16LE helpers ─────────────────────────────────────────────

fn encode_utf16le(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len() * 2);
    for code in s.encode_utf16() {
        out.extend_from_slice(&code.to_le_bytes());
    }
    out
}

fn decode_utf16le(data: &[u8], char_count: usize) -> String {
    let byte_len = char_count * 2;
    let end = byte_len.min(data.len());
    let mut chars = Vec::with_capacity(char_count);
    for chunk in data[..end].chunks_exact(2) {
        let code = u16::from_le_bytes([chunk[0], chunk[1]]);
        if code == 0 {
            break;
        }
        chars.push(code);
    }
    String::from_utf16_lossy(&chars)
}


#[cfg(test)]
mod tests {
    use super::*;

    // ── Protocol unit tests ─────────────────────────────────────

    #[test]
    fn test_connect_message_size() {
        let msg = build_storage_client_connect_default(0);
        assert_eq!(msg.len(), 1057);
        assert_eq!(msg[0], MSG_STORAGE_CLIENT_CONNECT);
    }

    #[test]
    fn test_disconnect_message() {
        let msg = build_storage_client_disconnect();
        assert_eq!(msg, vec![MSG_STORAGE_CLIENT_DISCONNECT, 0x00]);
    }

    #[test]
    fn test_parse_storage_connected() {
        let mut raw = vec![0u8; STORAGE_MSG_PAYLOAD_LEN];
        raw[24] = 0;
        let status = parse_storage_status(&raw).unwrap();
        assert_eq!(status.share_status0 as u8, 0);
    }

    #[test]
    fn test_parse_storage_truncated_fails() {
        let raw = vec![0xC0];
        assert!(parse_storage_status(&raw).is_err());
    }

    #[test]
    fn test_utf16_roundtrip() {
        let s = "hello.iso";
        let encoded = encode_utf16le(s);
        let decoded = decode_utf16le(&encoded, s.len());
        assert_eq!(decoded, s);
    }

    #[test]
    fn test_utf16_with_unicode() {
        let s = "имя.iso";
        let encoded = encode_utf16le(s);
        let decoded = decode_utf16le(&encoded, s.chars().count());
        assert_eq!(decoded, s);
    }
}
