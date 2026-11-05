use tracing::{debug, warn};

use crate::msd as msd_proto;

/// Reject peer-controlled message lengths before a reader waits forever for a
/// fabricated framebuffer payload.  Real KVM updates are normally tiled;
/// 32 MiB leaves headroom for large compressed updates without permitting an
/// unbounded allocation/buffer growth attack.
pub const MAX_SERVER_MESSAGE_BYTES: usize = 32 * 1024 * 1024;

// ── Protocol message types ─────────────────────────────────────────

#[allow(dead_code)]
pub mod msg {
    pub const NOP: u8 = 0x00;
    pub const SERVER_HANDSHAKE: u8 = 0xC8;
    pub const FIRMWARE_VERSION: u8 = 0xC9;
    pub const INFORM_KEY_INDICATORS: u8 = 0xD5;
    pub const INFORM_VESA_MODE: u8 = 0xE1;
    pub const BITBLT: u8 = 0xE2;
    pub const ENHANCE_BITBLT: u8 = 0xE3;
    pub const STANDBY_POWER: u8 = 0xE4;
    pub const SET_PALETTE: u8 = 0xE6;
    pub const SET_TEXT_CURSOR: u8 = 0xEA;
    pub const SPECIAL_GRAPHICS_BIT: u8 = 0xEB;
    pub const MATROX_CURSOR: u8 = 0xEC;
    pub const GRAPHICS_REGISTER: u8 = 0xEE;
    pub const MULTI_USER_STATE: u8 = 0xC5;
    pub const SERVER_DISCONNECT: u8 = 0xC6;

    pub const CLIENT_HANDSHAKE: u8 = 0xDD;
    pub const MOUSE_MOVE: u8 = 0xB5;
    pub const KEY_STATE_CHANGE: u8 = 0xD1;
    pub const CLIENT_NOP: u8 = 0xD2;
    pub const REQUEST_PRIMARY: u8 = 0xD3;
    pub const DISCONNECT_USER: u8 = 0xD6;
    pub const RELINQUISH_CONTROL: u8 = 0xD7;
    pub const CLIENT_DISCONNECT: u8 = 0xD8;
    pub const CLIENT_ABSOLUTE_MODE: u8 = 0xB1;
    pub const CLIENT_RELATIVE_MODE: u8 = 0xB2;
    pub const KEYBOARD_SCREEN_ON: u8 = 0xD9;
    pub const KEYBOARD_SCREEN_OFF: u8 = 0xDA;
    pub const INFORM_HLEVEL_COMPR: u8 = 0xF3;
    pub const REQUEST_VESA_MODE: u8 = 0xF1;
    pub const INVALIDATE: u8 = 0xF2;

    pub const BUTTON_STATE_AT_ABSOLUTE: u8 = 0xB3;
    pub const BUTTON_STATE_AT_RELATIVE: u8 = 0xB4;

    pub const STORAGE_STATUS: u8 = 0x89;
    pub const STORAGE_CLIENT_CONNECT: u8 = 0x99;
    pub const STORAGE_CLIENT_DISCONNECT: u8 = 0x9A;
}

// ── HTTP helpers ─────────────────────────────────────────────────

pub fn extract_httpdata(html: &str) -> Option<String> {
    let pattern = r#"PARAM NAME="httpdata" VALUE=""#;
    let start = html.find(pattern)?;
    let after = &html[start + pattern.len()..];
    let end = after.find('"')?;
    Some(after[..end].to_string())
}

pub fn extract_kvm_port(html: &str) -> Option<u16> {
    for pattern in &[
        r#"NonSecure_KVMPort" VALUE=""#,
        r#"NonSecure_KVMPort" value=""#,
    ] {
        if let Some(start) = html.find(pattern) {
            let after = &html[start + pattern.len()..];
            if let Some(end) = after.find('"') {
                if let Ok(port) = after[..end].parse::<u16>() {
                    return Some(port);
                }
            }
        }
    }
    None
}

// ── Protocol builders ─────────────────────────────────────────────

fn trunc_pad(s: &str, len: usize) -> Vec<u8> {
    let bytes = s.as_bytes();
    let mut out = vec![0u8; len];
    let copy_len = bytes.len().min(len);
    out[..copy_len].copy_from_slice(&bytes[..copy_len]);
    out
}

pub fn build_client_handshake(username: &str, password: &str) -> Vec<u8> {
    let sig: u32 = 0x5A5A5A5A;
    let config: u32 = 0x1F;

    let username_padded = trunc_pad(username, 16);
    let password_padded = trunc_pad(password, 20);
    let password_full = trunc_pad(password, 128);

    let mut buf = Vec::with_capacity(189);
    buf.push(msg::CLIENT_HANDSHAKE);
    buf.extend_from_slice(&sig.to_le_bytes());
    buf.extend_from_slice(&16u32.to_le_bytes());
    buf.extend_from_slice(&20u32.to_le_bytes());
    buf.extend_from_slice(&4u32.to_le_bytes());
    buf.extend_from_slice(&0u32.to_le_bytes());
    buf.extend_from_slice(&username_padded);
    buf.extend_from_slice(&password_padded);
    buf.extend_from_slice(&config.to_le_bytes());
    buf.extend_from_slice(&password_full);
    buf
}

pub fn build_request_primary_control() -> Vec<u8> {
    vec![msg::REQUEST_PRIMARY, 0x00]
}

pub fn build_relinquish_control() -> Vec<u8> {
    vec![msg::RELINQUISH_CONTROL, 0x00]
}

pub fn build_keyboard_screen(enabled: bool) -> Vec<u8> {
    vec![
        if enabled {
            msg::KEYBOARD_SCREEN_ON
        } else {
            msg::KEYBOARD_SCREEN_OFF
        },
        0x00,
    ]
}

pub fn build_absolute_mode(enabled: bool) -> Vec<u8> {
    vec![msg::CLIENT_ABSOLUTE_MODE, if enabled { 1 } else { 0 }]
}

/// Select Mahogany's relative mouse mode.
///
/// This is a distinct protocol message from `ClientAbsoluteMode(false)`.
/// Bit 0 requests relative coordinates; bit 4 asks the BMC to hide its
/// hardware cursor.  The browser owns cursor presentation, so hiding is not
/// requested here.
pub fn build_relative_mode(relative: bool, mouse_hide: bool) -> Vec<u8> {
    let mut flags = 0u8;
    if relative {
        flags |= 0x01;
    }
    if mouse_hide {
        flags |= 0x10;
    }
    vec![msg::CLIENT_RELATIVE_MODE, flags]
}

pub fn build_inform_hlevel_compression(encrypted: bool) -> Vec<u8> {
    vec![msg::INFORM_HLEVEL_COMPR, if encrypted { 1 } else { 0 }]
}

pub fn build_invalidate_full_frame(width: u16, height: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(11);
    buf.push(msg::INVALIDATE);
    buf.push(0x01);
    buf.extend_from_slice(&[0x00, 0x00, 0x00, 0x00, 0x00]);
    buf.extend_from_slice(&width.to_le_bytes());
    buf.extend_from_slice(&height.to_le_bytes());
    buf
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerPrivileges {
    pub keyboard: bool,
    pub video: bool,
    pub mouse: bool,
    pub storage: bool,
    pub raw: String,
}

pub fn parse_server_privileges(raw: &[u8]) -> ServerPrivileges {
    let mut normalized = String::with_capacity(raw.len());
    for &b in raw {
        let c = b as char;
        if c.is_ascii_alphanumeric() || c == '_' {
            normalized.push(c);
        } else {
            normalized.push(' ');
        }
    }
    let normalized = normalized.trim().to_string();
    let privileges = normalized
        .find(char::is_whitespace)
        .map(|idx| normalized[idx..].trim().to_ascii_lowercase())
        .unwrap_or_default();

    ServerPrivileges {
        keyboard: privileges.contains('k'),
        video: privileges.contains('v'),
        mouse: privileges.contains('m'),
        storage: privileges.contains('s'),
        raw: normalized,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserControlStatus {
    FullControl,
    ViewOnly,
    EphemeralConnection,
    ForcibleDisconnect,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultiUserState {
    pub flags: u32,
    pub user_status: UserControlStatus,
    pub has_full_control: bool,
    pub can_take_full_control: bool,
    pub can_relinquish_full_control: bool,
}

pub fn parse_multi_user_state_payload(data: &[u8]) -> Result<MultiUserState, anyhow::Error> {
    if data.len() < 8 {
        anyhow::bail!("MultiUserState payload too short: {} bytes", data.len());
    }
    let flags = read_u32_le(data, 0);
    let status = match (flags & 0x18) >> 3 {
        0 => UserControlStatus::FullControl,
        1 => UserControlStatus::ViewOnly,
        2 => UserControlStatus::EphemeralConnection,
        _ => UserControlStatus::ForcibleDisconnect,
    };
    Ok(MultiUserState {
        flags,
        user_status: status,
        has_full_control: status == UserControlStatus::FullControl,
        can_take_full_control: flags & 0x01 != 0,
        can_relinquish_full_control: flags & 0x04 != 0,
    })
}

// ── Server Message Types ──────────────────────────────────────────

#[derive(Debug)]
pub enum ServerMessage {
    KeyIndicators {
        num_lock: bool,
        caps_lock: bool,
        scroll_lock: bool,
    },
    DisplayParameters {
        width: u16,
        height: u16,
    },
    FramebufferUpdate {
        sub_type: u16,
        rects: Vec<TileMsg>,
    },
    EnhanceBltRaw {
        bilt_type: u16,
        tile_w: u16,
        tile_h: u16,
        snoop_data: Vec<u8>,
        scrunch_data: Vec<u8>,
        raw_size: u32,
        #[allow(dead_code)]
        scrunch_size: u32,
    },
    MultiUserState(MultiUserState),
    StorageStatus(msd_proto::StorageStatus),
    FirmwareVersion(String),
    Nop,
    Other {
        type_id: u8,
        _data_len: usize,
    },
}

#[derive(Debug)]
pub struct TileMsg {
    pub x: u16,
    pub y: u16,
    pub w: u16,
    pub h: u16,
    pub raw_data: Vec<u8>,
}

// ── Parsing helpers ───────────────────────────────────────────────

fn read_u16_le(buf: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([buf[offset], buf[offset + 1]])
}

fn read_u32_le(buf: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        buf[offset],
        buf[offset + 1],
        buf[offset + 2],
        buf[offset + 3],
    ])
}

fn get_message_length(msg_type: u8, buf: &[u8]) -> Option<usize> {
    match msg_type {
        msg::SERVER_HANDSHAKE => Some(33),
        msg::FIRMWARE_VERSION => {
            if buf.len() < 5 {
                return None;
            }
            let ver_len = read_u32_le(buf, 1) as usize;
            Some(5 + ver_len)
        }
        msg::INFORM_KEY_INDICATORS => Some(4),
        msg::INFORM_VESA_MODE => Some(9),
        msg::BITBLT => {
            if buf.len() < 25 {
                return None;
            }
            let data_size = read_u32_le(buf, 21) as usize;
            Some(25 + data_size)
        }
        msg::ENHANCE_BITBLT => {
            if buf.len() < 21 {
                return None;
            }
            let scrunch_size = read_u32_le(buf, 17) as usize;
            Some(533 + scrunch_size)
        }
        msg::STANDBY_POWER => Some(16),
        msg::SET_PALETTE => {
            if buf.len() < 5 {
                return None;
            }
            let attr_size = read_u16_le(buf, 1) as usize;
            if buf.len() < 3 + attr_size * 2 + 2 {
                return None;
            }
            let pal_size = read_u16_le(buf, 3 + attr_size * 2) as usize;
            Some(1 + 2 + attr_size * 2 + 2 + pal_size * 4)
        }
        msg::SET_TEXT_CURSOR => Some(5),
        msg::SPECIAL_GRAPHICS_BIT => Some(5),
        msg::MATROX_CURSOR => Some(3329),
        msg::GRAPHICS_REGISTER => Some(69),
        msg::MULTI_USER_STATE => {
            if buf.len() < 9 {
                return None;
            }
            let u0_size = buf[5] as usize;
            let u1_size = buf[6] as usize;
            Some(1 + 4 + 1 + 1 + 1 + 1 + u0_size + u1_size)
        }
        msg::SERVER_DISCONNECT => {
            if buf.len() < 7 {
                return None;
            }
            let msg_len = read_u16_le(buf, 5) as usize;
            Some(7 + msg_len)
        }
        msg::STORAGE_STATUS => Some(msd_proto::STORAGE_MSG_WIRE_LEN),
        _ => Some(1),
    }
}

pub fn try_parse_message(buf: &[u8]) -> Result<Option<(ServerMessage, usize)>, anyhow::Error> {
    if buf.is_empty() {
        return Ok(None);
    }
    let msg_type = buf[0];

    let total_len = match get_message_length(msg_type, buf) {
        Some(len) => len,
        None => return Ok(None),
    };

    if total_len > MAX_SERVER_MESSAGE_BYTES {
        anyhow::bail!(
            "server message type 0x{msg_type:02x} declares {total_len} bytes, above {} byte limit",
            MAX_SERVER_MESSAGE_BYTES
        );
    }

    if buf.len() < total_len {
        return Ok(None);
    }

    let payload = &buf[1..total_len];
    let msg = parse_payload(msg_type, payload)?;
    Ok(Some((msg, total_len)))
}

fn parse_payload(msg_type: u8, data: &[u8]) -> Result<ServerMessage, anyhow::Error> {
    match msg_type {
        msg::SERVER_HANDSHAKE => {
            let priv_str =
                String::from_utf8_lossy(data.split(|&b| b == 0).next().unwrap_or(data)).to_string();
            debug!("ServerHandshake privilege: {}", priv_str);
            Ok(ServerMessage::Nop)
        }
        msg::NOP | msg::CLIENT_NOP => Ok(ServerMessage::Nop),
        msg::FIRMWARE_VERSION => {
            if data.len() < 4 {
                anyhow::bail!("FirmwareVersion payload too short: {} bytes", data.len());
            }
            let declared = read_u32_le(data, 0) as usize;
            if data.len() != 4 + declared {
                anyhow::bail!(
                    "FirmwareVersion length mismatch: declared {}, payload {}",
                    declared,
                    data.len().saturating_sub(4)
                );
            }
            let version = std::str::from_utf8(&data[4..])
                .map_err(|error| anyhow::anyhow!("FirmwareVersion is not UTF-8: {error}"))?
                .trim_end_matches('\0')
                .to_string();
            Ok(ServerMessage::FirmwareVersion(version))
        }
        msg::INFORM_KEY_INDICATORS => Ok(ServerMessage::KeyIndicators {
            num_lock: data[0] != 0,
            caps_lock: data[1] != 0,
            scroll_lock: data[2] != 0,
        }),
        msg::MULTI_USER_STATE => Ok(ServerMessage::MultiUserState(
            parse_multi_user_state_payload(data)?,
        )),
        msg::INFORM_VESA_MODE => {
            if data.len() < 7 {
                return Ok(ServerMessage::Other {
                    type_id: msg_type,
                    _data_len: data.len(),
                });
            }
            let width = read_u16_le(data, 2);
            let height = read_u16_le(data, 4);
            Ok(ServerMessage::DisplayParameters { width, height })
        }
        msg::BITBLT => parse_bitblt(data),
        msg::ENHANCE_BITBLT => parse_enhance_bitblt(data),
        msg::STORAGE_STATUS => match msd_proto::parse_storage_status(data) {
            Ok(status) => Ok(ServerMessage::StorageStatus(status)),
            Err(e) => {
                warn!("failed to parse StorageStatus: {}", e);
                Ok(ServerMessage::Other {
                    type_id: msg_type,
                    _data_len: data.len(),
                })
            }
        },
        _ => Ok(ServerMessage::Other {
            type_id: msg_type,
            _data_len: data.len(),
        }),
    }
}

fn parse_bitblt(data: &[u8]) -> Result<ServerMessage, anyhow::Error> {
    if data.len() < 24 {
        anyhow::bail!("BitBlt message too short: {} bytes", data.len());
    }
    let bilt_type = read_u16_le(data, 0);
    let _src_x = read_u16_le(data, 4);
    let _src_y = read_u16_le(data, 6);
    let _src_w = read_u16_le(data, 8);
    let _src_h = read_u16_le(data, 10);
    let dst_x = read_u16_le(data, 12);
    let dst_y = read_u16_le(data, 14);
    let dst_w = read_u16_le(data, 16);
    let dst_h = read_u16_le(data, 18);
    let data_size = read_u32_le(data, 20) as usize;

    let required_bytes = usize::from(dst_w)
        .checked_mul(usize::from(dst_h))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| anyhow::anyhow!("BitBlt rectangle byte length overflow"))?;
    if data_size < required_bytes {
        anyhow::bail!(
            "BitBlt rectangle needs {required_bytes} pixel bytes but declares {data_size}"
        );
    }

    if data.len() < 24 + data_size {
        anyhow::bail!(
            "BitBlt payload too short: need {} have {}",
            24 + data_size,
            data.len()
        );
    }

    let pixel_data = data[24..24 + data_size].to_vec();

    let rects = vec![TileMsg {
        x: dst_x,
        y: dst_y,
        w: dst_w,
        h: dst_h,
        raw_data: pixel_data,
    }];

    Ok(ServerMessage::FramebufferUpdate {
        sub_type: bilt_type,
        rects,
    })
}

fn parse_enhance_bitblt(data: &[u8]) -> Result<ServerMessage, anyhow::Error> {
    if data.len() < 532 {
        anyhow::bail!("EnhanceBitBlt message too short: {} bytes", data.len());
    }
    let bilt_type = read_u16_le(data, 0);
    let tile_w = data[2] as u16;
    let tile_h = data[3] as u16;
    if tile_w == 0 || tile_h == 0 {
        anyhow::bail!("EnhanceBitBlt tile dimensions must be nonzero");
    }
    let _triplet_code = read_u32_le(data, 4);
    let _repeat_code = read_u32_le(data, 8);
    let raw_size = read_u32_le(data, 12);
    let scrunch_size = read_u32_le(data, 16);

    let snoop_offset = 20;
    let data_offset = snoop_offset + 64 * 4 + 64 * 4;

    if data.len() < data_offset + scrunch_size as usize {
        anyhow::bail!(
            "EnhanceBitBlt scrunch data too short: need {} have {}",
            data_offset + scrunch_size as usize,
            data.len()
        );
    }

    let scrunch_data = data[data_offset..data_offset + scrunch_size as usize].to_vec();

    Ok(ServerMessage::EnhanceBltRaw {
        bilt_type,
        tile_w,
        tile_h,
        snoop_data: data[snoop_offset..snoop_offset + 512].to_vec(),
        scrunch_data,
        raw_size,
        scrunch_size,
    })
}

fn parse_snoop_maps(snoop_data: &[u8]) -> Result<([u32; 64], [u32; 64]), anyhow::Error> {
    if snoop_data.len() < 512 {
        anyhow::bail!("snoop data too short: {} bytes, need 512", snoop_data.len());
    }
    let mut low = [0u32; 64];
    let mut high = [0u32; 64];
    for i in 0..64 {
        let off = i * 8;
        low[i] = u32::from_le_bytes([
            snoop_data[off],
            snoop_data[off + 1],
            snoop_data[off + 2],
            snoop_data[off + 3],
        ]);
        high[i] = u32::from_le_bytes([
            snoop_data[off + 4],
            snoop_data[off + 5],
            snoop_data[off + 6],
            snoop_data[off + 7],
        ]);
    }
    Ok((low, high))
}

fn is_tile_active(tx: u16, ty: usize, snoop_low: [u32; 64], snoop_high: [u32; 64]) -> bool {
    if tx < 32 {
        (snoop_low[ty] & (1 << tx)) != 0
    } else {
        (snoop_high[ty] & (1 << (tx - 32))) != 0
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn decode_enhance_bitblt_tiles(
    bilt_type: u16,
    tile_w: u16,
    tile_h: u16,
    snoop_data: &[u8],
    scrunch_data: &[u8],
    _raw_size: u32,
    canvas_w: u16,
    canvas_h: u16,
) -> Result<Vec<TileMsg>, anyhow::Error> {
    if tile_w == 0 || tile_h == 0 {
        anyhow::bail!("EnhanceBitBlt tile dimensions must be nonzero");
    }
    let (snoop_low, snoop_high) = parse_snoop_maps(snoop_data)?;
    let bpp_bytes: usize = 4;

    let mut rects = Vec::new();
    let mut data_off: usize = 0;

    let num_tiles_x = (canvas_w.max(1) as usize).div_ceil(tile_w as usize);
    let num_tiles_x = num_tiles_x.min(64);
    let num_tiles_y = (canvas_h.max(1) as usize).div_ceil(tile_h as usize);
    let num_tiles_y = num_tiles_y.min(64);

    match bilt_type {
        0x01F0 | 0x81F0 => {
            for ty in 0..num_tiles_y {
                for tx in 0..num_tiles_x as u16 {
                    if !is_tile_active(tx, ty, snoop_low, snoop_high) {
                        continue;
                    }
                    let tile_bytes = tile_w as usize * tile_h as usize * bpp_bytes;
                    if data_off + tile_bytes > scrunch_data.len() {
                        anyhow::bail!("Basic Enhance tile ({},{}) at byte {} needs {} bytes but only {} remain",
                              tx, ty, data_off, tile_bytes, scrunch_data.len() - data_off);
                    }
                    rects.push(TileMsg {
                        x: tx * tile_w,
                        y: ty as u16 * tile_h,
                        w: tile_w,
                        h: tile_h,
                        raw_data: scrunch_data[data_off..data_off + tile_bytes].to_vec(),
                    });
                    data_off += tile_bytes;
                }
            }
        }
        0x01F2 | 0x81F2 => {
            let active = (0..num_tiles_y).any(|row| {
                (0..num_tiles_x as u16).any(|col| is_tile_active(col, row, snoop_low, snoop_high))
            });
            if active || !scrunch_data.is_empty() {
                decode_hlc_tiles(
                    &snoop_low,
                    &snoop_high,
                    tile_w,
                    tile_h,
                    scrunch_data,
                    &mut data_off,
                    &mut rects,
                    canvas_w,
                    canvas_h,
                )?;
            }
        }
        _ => anyhow::bail!("unsupported EnhanceBitBlt encoding 0x{bilt_type:04x}"),
    }

    Ok(rects)
}

#[allow(clippy::too_many_arguments)]
fn decode_hlc_tiles(
    snoop_low: &[u32; 64],
    snoop_high: &[u32; 64],
    tile_w: u16,
    tile_h: u16,
    scrunch: &[u8],
    off: &mut usize,
    rects: &mut Vec<TileMsg>,
    canvas_w: u16,
    canvas_h: u16,
) -> Result<(), anyhow::Error> {
    let read_bgra = |data: &[u8], pos: &mut usize| -> Option<[u8; 4]> {
        if *pos + 4 > data.len() {
            return None;
        }
        let p = [data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]];
        *pos += 4;
        Some(p)
    };

    let bg_color = read_bgra(scrunch, off)
        .ok_or_else(|| anyhow::anyhow!("HLC: not enough data for bg_color"))?;
    let fg_color = read_bgra(scrunch, off)
        .ok_or_else(|| anyhow::anyhow!("HLC: not enough data for fg_color"))?;
    let _extra = read_bgra(scrunch, off);
    let _extra = read_bgra(scrunch, off);

    let num_tiles_x = (canvas_w.max(1) as usize).div_ceil(tile_w as usize).min(64);
    let num_tiles_y = (canvas_h.max(1) as usize).div_ceil(tile_h as usize).min(64);
    for row in 0..num_tiles_y {
        for col in 0..num_tiles_x as u16 {
            if !is_tile_active(col, row, *snoop_low, *snoop_high) {
                continue;
            }

            let tile_x = col * tile_w;
            let tile_y = row as u16 * tile_h;
            let tile_bytes = tile_w as usize * tile_h as usize * 4;
            let mut out = vec![0u8; tile_bytes];

            let mut px: u16 = 0;
            let mut py: u16 = 0;

            while py < tile_h && (tile_y + py) < canvas_h {
                if *off >= scrunch.len() {
                    anyhow::bail!(
                        "HLC: data exhausted at tile ({},{}), pixel ({},{})",
                        col,
                        row,
                        px,
                        py
                    );
                }
                let cmd = scrunch[*off];
                *off += 1;

                match cmd {
                    0..=127 => {
                        if *off >= scrunch.len() {
                            anyhow::bail!("HLC: palette index is missing");
                        }
                        let idx = scrunch[*off];
                        *off += 1;
                        let color = if idx == 0 { &bg_color } else { &fg_color };
                        let dst = (py as usize * tile_w as usize + px as usize) * 4;
                        if dst + 4 <= out.len() {
                            out[dst..dst + 4].copy_from_slice(color);
                        }
                        px += 1;
                    }
                    128..=191 => {
                        let count = (cmd & 0x3F) as usize + 1;
                        if *off >= scrunch.len() {
                            anyhow::bail!("HLC: palette index is missing");
                        }
                        let idx = scrunch[*off];
                        *off += 1;
                        let color = if idx == 0 { &bg_color } else { &fg_color };
                        for _ in 0..count {
                            let dst = (py as usize * tile_w as usize + px as usize) * 4;
                            if dst + 4 <= out.len() {
                                out[dst..dst + 4].copy_from_slice(color);
                            }
                            px += 1;
                            if px >= tile_w {
                                px = 0;
                                py += 1;
                                if py >= tile_h || (tile_y + py) >= canvas_h {
                                    break;
                                }
                            }
                        }
                    }
                    _ => {
                        let count = (cmd & 0x3F) as usize + 1;
                        for _ in 0..count {
                            if *off + 3 > scrunch.len() {
                                anyhow::bail!("HLC: raw pixel data exhausted");
                            }
                            let r = scrunch[*off];
                            let g = scrunch[*off + 1];
                            let b_col = scrunch[*off + 2];
                            *off += 3;
                            let dst = (py as usize * tile_w as usize + px as usize) * 4;
                            if dst + 4 <= out.len() {
                                out[dst] = b_col;
                                out[dst + 1] = g;
                                out[dst + 2] = r;
                                out[dst + 3] = 255;
                            }
                            px += 1;
                            if px >= tile_w {
                                px = 0;
                                py += 1;
                                if py >= tile_h || (tile_y + py) >= canvas_h {
                                    break;
                                }
                            }
                        }
                    }
                }

                if px >= tile_w || (tile_x + px) >= canvas_w {
                    px = 0;
                    py += 1;
                }
            }

            rects.push(TileMsg {
                x: tile_x,
                y: tile_y,
                w: tile_w,
                h: tile_h,
                raw_data: out,
            });
        }
    }

    Ok(())
}

// ── Tests ──────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hid;

    #[test]
    fn zero_tile_dimensions_are_rejected_before_decode_arithmetic() {
        for (width, height) in [(0, 1), (1, 0), (0, 0)] {
            let mut message = vec![0u8; 534];
            message[0] = msg::ENHANCE_BITBLT;
            message[1] = 0xf0;
            message[2] = 1;
            message[3] = width;
            message[4] = height;
            message[17] = 1;
            assert!(try_parse_message(&message).is_err());
            assert!(decode_enhance_bitblt_tiles(
                0x01f0,
                width.into(),
                height.into(),
                &[0; 512],
                &[0],
                0,
                640,
                480
            )
            .is_err());
        }
    }

    #[test]
    fn test_build_nop_structure() {
        let buf = hid::encode_nop();
        assert_eq!(buf.len(), 8);
        assert_eq!(buf[0], 0xD2);
        assert!(buf[1..8].iter().all(|&b| b == 0));
    }

    #[test]
    fn test_build_client_handshake_structure() {
        let buf = build_client_handshake("admin", "httpdata123");
        assert_eq!(buf[0], 0xDD);
        assert_eq!(&buf[1..5], &0x5A5A5A5Au32.to_le_bytes());
        assert_eq!(u32::from_le_bytes([buf[5], buf[6], buf[7], buf[8]]), 16);
        assert_eq!(u32::from_le_bytes([buf[9], buf[10], buf[11], buf[12]]), 20);
        assert_eq!(u32::from_le_bytes([buf[13], buf[14], buf[15], buf[16]]), 4);
        assert_eq!(u32::from_le_bytes([buf[17], buf[18], buf[19], buf[20]]), 0);
        assert_eq!(&buf[21..26], b"admin");
    }

    #[test]
    fn test_build_request_primary_control() {
        let buf = build_request_primary_control();
        assert_eq!(buf, vec![0xD3, 0x00]);
    }

    #[test]
    fn test_build_absolute_mode() {
        let enabled = build_absolute_mode(true);
        assert_eq!(enabled, vec![0xB1, 0x01]);
        let disabled = build_absolute_mode(false);
        assert_eq!(disabled, vec![0xB1, 0x00]);
    }

    #[test]
    fn test_build_relative_mode_is_a_distinct_wire_message() {
        assert_eq!(build_relative_mode(true, false), vec![0xB2, 0x01]);
        assert_eq!(build_relative_mode(true, true), vec![0xB2, 0x11]);
        assert_eq!(build_relative_mode(false, false), vec![0xB2, 0x00]);
    }

    #[test]
    fn test_build_inform_hlevel() {
        let enc = build_inform_hlevel_compression(true);
        assert_eq!(enc, vec![0xF3, 0x01]);
        let plain = build_inform_hlevel_compression(false);
        assert_eq!(plain, vec![0xF3, 0x00]);
    }

    #[test]
    fn test_build_invalidate_full_frame() {
        let buf = build_invalidate_full_frame(800, 600);
        assert_eq!(
            buf,
            vec![0xF2, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x20, 0x03, 0x58, 0x02]
        );
    }

    #[test]
    fn test_parse_nop() {
        let result = try_parse_message(&[0x00]).unwrap();
        assert!(matches!(result, Some((ServerMessage::Nop, 1))));
    }

    #[test]
    fn test_parse_server_handshake() {
        let mut data = vec![0xC8];
        let mut priv_bytes = vec![0u8; 32];
        priv_bytes[..5].copy_from_slice(b"admin");
        data.extend_from_slice(&priv_bytes);
        let result = try_parse_message(&data).unwrap();
        assert!(matches!(result, Some((ServerMessage::Nop, 33))));
    }

    #[test]
    fn test_parse_server_handshake_incomplete() {
        let data = vec![0xC8, 0x01, 0x02];
        let result = try_parse_message(&data).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_parse_server_privileges() {
        let privileges = parse_server_privileges(b"admin kvms\0\0");
        assert!(privileges.keyboard);
        assert!(privileges.video);
        assert!(privileges.mouse);
        assert!(privileges.storage);
    }

    #[test]
    fn test_parse_multi_user_state_full_control() {
        let data = [0x01, 0x00, 0x00, 0x00, 0, 0, 0, 0];
        let state = parse_multi_user_state_payload(&data).unwrap();
        assert!(state.has_full_control);
        assert!(state.can_take_full_control);
        assert_eq!(state.user_status, UserControlStatus::FullControl);
    }

    #[test]
    fn test_parse_multi_user_state_view_only() {
        let data = [0x08, 0x00, 0x00, 0x00, 0, 0, 0, 0];
        let state = parse_multi_user_state_payload(&data).unwrap();
        assert!(!state.has_full_control);
        assert_eq!(state.user_status, UserControlStatus::ViewOnly);
    }

    #[test]
    fn test_parse_inform_vesa_mode() {
        let mut buf = vec![0xE1, 0x00, 0x00];
        buf.extend_from_slice(&800u16.to_le_bytes());
        buf.extend_from_slice(&600u16.to_le_bytes());
        buf.extend_from_slice(&32u16.to_le_bytes());
        assert_eq!(buf.len(), 9);

        let (msg, consumed) = try_parse_message(&buf).unwrap().unwrap();
        assert_eq!(consumed, 9);
        match msg {
            ServerMessage::DisplayParameters { width, height } => {
                assert_eq!(width, 800);
                assert_eq!(height, 600);
            }
            _ => panic!("expected DisplayParameters"),
        }
    }

    #[test]
    fn test_parse_bitblt_simple() {
        let dst_x: u16 = 10;
        let dst_y: u16 = 20;
        let dst_w: u16 = 4;
        let dst_h: u16 = 4;
        let pixel_count = dst_w as usize * dst_h as usize;
        let pixel_data: Vec<u8> = (0..pixel_count * 4).map(|i| i as u8).collect();
        let data_size = pixel_data.len() as u32;

        let mut msg: Vec<u8> = Vec::new();
        msg.push(0xE2);
        msg.extend_from_slice(&0u16.to_le_bytes());
        msg.extend_from_slice(&0u8.to_le_bytes());
        msg.extend_from_slice(&0u8.to_le_bytes());
        msg.extend_from_slice(&0u16.to_le_bytes());
        msg.extend_from_slice(&0u16.to_le_bytes());
        msg.extend_from_slice(&0u16.to_le_bytes());
        msg.extend_from_slice(&0u16.to_le_bytes());
        msg.extend_from_slice(&dst_x.to_le_bytes());
        msg.extend_from_slice(&dst_y.to_le_bytes());
        msg.extend_from_slice(&dst_w.to_le_bytes());
        msg.extend_from_slice(&dst_h.to_le_bytes());
        msg.extend_from_slice(&data_size.to_le_bytes());
        msg.extend_from_slice(&pixel_data);

        let result = try_parse_message(&msg).unwrap().unwrap();
        match result {
            (ServerMessage::FramebufferUpdate { ref rects, .. }, _) => {
                assert_eq!(rects.len(), 1);
                assert_eq!(rects[0].x, dst_x);
                assert_eq!(rects[0].y, dst_y);
                assert_eq!(rects[0].w, dst_w);
                assert_eq!(rects[0].h, dst_h);
                assert_eq!(rects[0].raw_data.len(), pixel_data.len());
            }
            _ => panic!("expected FramebufferUpdate"),
        }
    }

    #[test]
    fn test_parse_bitblt_incomplete() {
        let result = try_parse_message(&[0xE2, 0x00]).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn bitblt_declared_payload_must_cover_the_entire_rectangle() {
        for length in [0usize, 1, 4, 7, 15, 16, 17] {
            let mut message = vec![0u8; 25 + length];
            message[0] = msg::BITBLT;
            message[17..19].copy_from_slice(&2u16.to_le_bytes());
            message[19..21].copy_from_slice(&2u16.to_le_bytes());
            message[21..25].copy_from_slice(&(length as u32).to_le_bytes());
            assert_eq!(try_parse_message(&message).is_ok(), length >= 16);
        }
    }

    #[test]
    fn test_parse_enhance_bitblt_basic() {
        let tile_w: u8 = 2;
        let tile_h: u8 = 2;
        let raw_size: u32 = 16;
        let scrunch_size: u32 = 16;
        let mut msg: Vec<u8> = vec![0xE3];
        msg.extend_from_slice(&0x01F0u16.to_le_bytes());
        msg.extend_from_slice(&tile_w.to_le_bytes());
        msg.extend_from_slice(&tile_h.to_le_bytes());
        msg.extend_from_slice(&0u32.to_le_bytes());
        msg.extend_from_slice(&0u32.to_le_bytes());
        msg.extend_from_slice(&raw_size.to_le_bytes());
        msg.extend_from_slice(&scrunch_size.to_le_bytes());
        msg.extend_from_slice(&[0u8; 512]);
        msg.extend_from_slice(&[128u8; 16]);
        let result = try_parse_message(&msg).unwrap().unwrap();
        match result {
            (
                ServerMessage::EnhanceBltRaw {
                    bilt_type,
                    tile_w: tw,
                    tile_h: th,
                    ..
                },
                _,
            ) => {
                assert_eq!(bilt_type, 0x01F0);
                assert_eq!(tw, 2);
                assert_eq!(th, 2);
            }
            _ => panic!("expected EnhanceBltRaw"),
        }
    }

    fn enhance_snoop(bits: u32) -> [u8; 512] {
        let mut snoop = [0; 512];
        snoop[..4].copy_from_slice(&bits.to_le_bytes());
        snoop
    }

    #[test]
    fn unsupported_enhance_encodings_are_rejected_even_with_empty_data() {
        for encoding in [0x01f3, 0x81f3, 0xffff] {
            for data in [vec![], vec![0; 4]] {
                assert!(decode_enhance_bitblt_tiles(
                    encoding,
                    1,
                    1,
                    &enhance_snoop(1),
                    &data,
                    0,
                    1,
                    1
                )
                .is_err());
            }
        }
    }

    #[test]
    fn truncated_basic_enhance_data_rejects_the_entire_tile_batch() {
        for length in [0, 1, 4, 7] {
            assert!(
                decode_enhance_bitblt_tiles(
                    0x01f0,
                    1,
                    1,
                    &enhance_snoop(3),
                    &vec![0; length],
                    0,
                    2,
                    1
                )
                .is_err(),
                "accepted {length} bytes for two active pixels"
            );
        }
        assert_eq!(
            decode_enhance_bitblt_tiles(0x01f0, 1, 1, &enhance_snoop(3), &[0; 8], 0, 2, 1)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn truncated_hlc_commands_never_publish_partially_decoded_tiles() {
        for tail in [vec![], vec![0], vec![128], vec![192, 1, 2], vec![0, 0]] {
            let mut data = vec![0; 16];
            data.extend_from_slice(&tail);
            assert!(
                decode_enhance_bitblt_tiles(0x01f2, 1, 1, &enhance_snoop(3), &data, 0, 2, 1)
                    .is_err(),
                "accepted truncated HLC tail {tail:?}"
            );
        }
    }

    #[test]
    fn empty_enhance_update_without_active_tiles_remains_valid() {
        for encoding in [0x01f0, 0x81f0, 0x01f2, 0x81f2] {
            assert!(
                decode_enhance_bitblt_tiles(encoding, 1, 1, &enhance_snoop(0), &[], 0, 2, 1)
                    .unwrap()
                    .is_empty()
            );
        }
    }

    #[test]
    fn test_decode_hlc_tiles_basic() {
        let tile_w = 2;
        let tile_h = 2;
        let canvas_w = 4u16;
        let canvas_h = 4u16;

        // Build interleaved snoop data: activate all 4 tiles (2x2 grid)
        // low[0]=0x03 (tiles 0,1 active), low[1]=0x03 (tiles 0,1 active), rest=0
        let mut snoop_data = vec![0u8; 512];
        snoop_data[0..4].copy_from_slice(&3u32.to_le_bytes()); // low[0] = 3 → bits 0,1
        snoop_data[8..12].copy_from_slice(&3u32.to_le_bytes()); // low[1] = 3 → bits 0,1

        let mut scrunch = Vec::new();
        let bg: [u8; 4] = [0, 0, 0, 255];
        let fg: [u8; 4] = [255, 255, 255, 255];
        scrunch.extend_from_slice(&bg);
        scrunch.extend_from_slice(&fg);
        scrunch.extend_from_slice(&bg);
        scrunch.extend_from_slice(&fg);

        // Four 2x2 tiles require sixteen complete one-pixel palette commands.
        for _ in 0..16 {
            scrunch.extend_from_slice(&[0x01, 0x00]);
        }

        let raw_size = 0u32;
        let rects = decode_enhance_bitblt_tiles(
            0x01F2,
            tile_w,
            tile_h,
            &snoop_data,
            &scrunch,
            raw_size,
            canvas_w,
            canvas_h,
        )
        .unwrap();

        assert_eq!(rects.len(), 4);
        for (i, rect) in rects.iter().enumerate() {
            assert_eq!(rect.w, 2, "rect {} width", i);
            assert_eq!(rect.h, 2, "rect {} height", i);
            assert_eq!(rect.raw_data.len(), 16);
            assert!(rect.raw_data.chunks_exact(4).all(|pixel| pixel == bg));
        }
    }

    #[test]
    fn test_trunc_pad() {
        let padded = trunc_pad("hello", 8);
        assert_eq!(&padded[..5], b"hello");
        assert_eq!(padded[5..].to_vec(), vec![0u8; 3]);
        assert_eq!(padded.len(), 8);
    }

    #[test]
    fn test_extract_kvm_port() {
        let html = r#"<input type="hidden" name="NonSecure_KVMPort" VALUE="17990">"#;
        assert_eq!(extract_kvm_port(html), Some(17990));
    }

    #[test]
    fn test_is_tile_active() {
        let low = [0u32; 64];
        let high = [0u32; 64];
        let mut low = low;
        low[0] = 1 << 5;
        assert!(is_tile_active(5, 0, low, high));
        assert!(!is_tile_active(6, 0, low, high));
        let mut high = high;
        high[0] = 1 << 0;
        assert!(is_tile_active(32, 0, low, high));
        assert!(!is_tile_active(33, 0, low, high));
    }

    #[test]
    fn test_parse_snoop_maps_basic() {
        let mut snoop = vec![0u8; 512];
        snoop[0..4].copy_from_slice(&1u32.to_le_bytes());
        snoop[4..8].copy_from_slice(&2u32.to_le_bytes());
        let (low, high) = parse_snoop_maps(&snoop).unwrap();
        assert_eq!(low[0], 1);
        assert_eq!(high[0], 2);
        assert_eq!(low[1], 0);
    }

    #[test]
    fn test_extract_httpdata_found() {
        let html = r#"<PARAM NAME="httpdata" VALUE="my-secret-pass">"#;
        assert_eq!(extract_httpdata(html), Some("my-secret-pass".to_string()));
    }

    #[test]
    fn test_extract_httpdata_not_found() {
        let html = "<html><body>no httpdata here</body></html>";
        assert!(extract_httpdata(html).is_none());
    }

    #[test]
    fn firmware_version_uses_declared_utf8_payload() {
        let version = b"2.75\0";
        let mut wire = vec![msg::FIRMWARE_VERSION];
        wire.extend_from_slice(&(version.len() as u32).to_le_bytes());
        wire.extend_from_slice(version);
        let (message, consumed) = try_parse_message(&wire).unwrap().unwrap();
        assert_eq!(consumed, wire.len());
        match message {
            ServerMessage::FirmwareVersion(value) => assert_eq!(value, "2.75"),
            other => panic!("expected FirmwareVersion, got {other:?}"),
        }
    }

    #[test]
    fn firmware_version_waits_for_the_complete_declared_payload() {
        let wire = [msg::FIRMWARE_VERSION, 5, 0, 0, 0, b'1'];
        assert!(try_parse_message(&wire).unwrap().is_none());
    }
}
