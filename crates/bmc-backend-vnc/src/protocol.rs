use des::cipher::{BlockEncrypt, KeyInit};
use des::Des;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

pub const MAX_FRAME_WIDTH: u16 = 8192;
pub const MAX_FRAME_HEIGHT: u16 = 8192;
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_RECTANGLES: usize = 4096;
pub const MAX_SERVER_TEXT_BYTES: usize = 1024 * 1024;

const SECURITY_NONE: u8 = 1;
const SECURITY_VNC_AUTH: u8 = 2;
const ENCODING_RAW: i32 = 0;
const ENCODING_DESKTOP_SIZE: i32 = -223;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VncSecurity {
    VncAuth,
    None,
}

impl VncSecurity {
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        match value.unwrap_or("vnc-auth") {
            "vnc-auth" => Ok(Self::VncAuth),
            "none" => Ok(Self::None),
            other => Err(format!("unsupported VNC security mode '{other}'")),
        }
    }

    fn wire_type(self) -> u8 {
        match self {
            Self::None => SECURITY_NONE,
            Self::VncAuth => SECURITY_VNC_AUTH,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::VncAuth => "vnc-auth",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Debug)]
pub struct HandshakeInfo {
    pub version: String,
    pub security: VncSecurity,
    pub width: u16,
    pub height: u16,
    pub desktop_name: String,
}

#[derive(Clone, Debug)]
pub struct Rectangle {
    pub x: u16,
    pub y: u16,
    pub width: u16,
    pub height: u16,
    pub encoding: i32,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug)]
pub enum ServerMessage {
    FramebufferUpdate(Vec<Rectangle>),
    Bell,
    ServerCutText,
}

pub async fn connect_and_handshake(
    host: &str,
    port: u16,
    security: VncSecurity,
    password: Option<&str>,
) -> Result<(TcpStream, HandshakeInfo), String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        handshake_inner(host, port, security, password),
    )
    .await
    .map_err(|_| "VNC negotiation timed out".to_string())?
}

async fn handshake_inner(
    host: &str,
    port: u16,
    security: VncSecurity,
    password: Option<&str>,
) -> Result<(TcpStream, HandshakeInfo), String> {
    use bmc_backend_model::negotiation::{rfb_minor, Event, State};
    let mut negotiation = State::Greeting;
    let address = bmc_backend_support::endpoint::authority(host, port)?;
    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        TcpStream::connect(&address),
    )
    .await
    .map_err(|_| format!("VNC connection to {address} timed out"))?
    .map_err(|error| format!("VNC connection to {address} failed: {error}"))?;

    let mut banner = [0u8; 12];
    stream
        .read_exact(&mut banner)
        .await
        .map_err(|error| format!("VNC version read failed: {error}"))?;
    let (major, minor) = parse_version_banner(&banner)?;
    let selected_minor = rfb_minor(major, minor)
        .ok_or_else(|| format!("unsupported RFB version {major}.{minor:03}"))?;
    negotiation = negotiation.step(Event::GreetingAccepted);
    let selected_banner = format!("RFB 003.{selected_minor:03}\n");
    stream
        .write_all(selected_banner.as_bytes())
        .await
        .map_err(|error| format!("VNC version write failed: {error}"))?;

    if selected_minor == 3 {
        authenticate_v33(&mut stream, security, password).await?;
    } else {
        authenticate_v37_or_v38(&mut stream, security, password, selected_minor).await?;
    }

    negotiation = negotiation.step(Event::Authenticated);

    stream
        .write_all(&[1])
        .await
        .map_err(|error| format!("VNC ClientInit write failed: {error}"))?;

    let mut server_init = [0u8; 24];
    stream
        .read_exact(&mut server_init)
        .await
        .map_err(|error| format!("VNC ServerInit read failed: {error}"))?;
    let width = u16::from_be_bytes([server_init[0], server_init[1]]);
    let height = u16::from_be_bytes([server_init[2], server_init[3]]);
    validate_dimensions(width, height)?;
    let name_length = u32::from_be_bytes([
        server_init[20],
        server_init[21],
        server_init[22],
        server_init[23],
    ]) as usize;
    if name_length > MAX_SERVER_TEXT_BYTES {
        return Err("VNC desktop name exceeds limit".to_string());
    }
    let mut desktop_name = vec![0u8; name_length];
    stream
        .read_exact(&mut desktop_name)
        .await
        .map_err(|error| format!("VNC desktop name read failed: {error}"))?;

    stream
        .write_all(&set_pixel_format_rgba())
        .await
        .map_err(|error| format!("VNC SetPixelFormat write failed: {error}"))?;
    stream
        .write_all(&set_encodings())
        .await
        .map_err(|error| format!("VNC SetEncodings write failed: {error}"))?;
    stream
        .write_all(&framebuffer_update_request(false, 0, 0, width, height))
        .await
        .map_err(|error| format!("VNC initial framebuffer request failed: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("VNC handshake flush failed: {error}"))?;

    negotiation = negotiation.step(Event::Initialized);
    if negotiation != State::Online {
        return Err("invalid VNC negotiation state".into());
    }
    Ok((
        stream,
        HandshakeInfo {
            version: format!("3.{selected_minor}"),
            security,
            width,
            height,
            desktop_name: String::from_utf8_lossy(&desktop_name).into_owned(),
        },
    ))
}

async fn authenticate_v33(
    stream: &mut TcpStream,
    security: VncSecurity,
    password: Option<&str>,
) -> Result<(), String> {
    let security_type = read_u32(stream).await?;
    if security_type == 0 {
        return Err(read_failure_reason(stream).await);
    }
    match security_type {
        1 if security == VncSecurity::None => Ok(()),
        2 if security == VncSecurity::VncAuth => authenticate_vnc_password(stream, password).await,
        offered => Err(format!(
            "VNC server requires security type {offered}, which does not match configured mode {}",
            security.as_str()
        )),
    }
}

async fn authenticate_v37_or_v38(
    stream: &mut TcpStream,
    security: VncSecurity,
    password: Option<&str>,
    minor: u16,
) -> Result<(), String> {
    let mut count = [0u8; 1];
    stream
        .read_exact(&mut count)
        .await
        .map_err(|error| format!("VNC security types read failed: {error}"))?;
    if count[0] == 0 {
        return Err(read_failure_reason(stream).await);
    }
    let mut types = vec![0u8; count[0] as usize];
    stream
        .read_exact(&mut types)
        .await
        .map_err(|error| format!("VNC security types read failed: {error}"))?;
    let selected = security.wire_type();
    if !types.contains(&selected) {
        return Err(format!(
            "VNC server does not offer configured security mode {}",
            security.as_str()
        ));
    }
    stream
        .write_all(&[selected])
        .await
        .map_err(|error| format!("VNC security selection write failed: {error}"))?;
    if security == VncSecurity::VncAuth {
        authenticate_vnc_password(stream, password).await
    } else if bmc_backend_model::negotiation::needs_security_result(minor, false) {
        read_security_result(stream).await
    } else {
        Ok(())
    }
}

async fn authenticate_vnc_password(
    stream: &mut TcpStream,
    password: Option<&str>,
) -> Result<(), String> {
    let password = password.ok_or_else(|| "VNC password is required for vnc-auth".to_string())?;
    let mut challenge = [0u8; 16];
    stream
        .read_exact(&mut challenge)
        .await
        .map_err(|error| format!("VNC authentication challenge read failed: {error}"))?;
    let response = vnc_challenge_response(password, &challenge)?;
    stream
        .write_all(&response)
        .await
        .map_err(|error| format!("VNC authentication response write failed: {error}"))?;
    read_security_result(stream).await
}

async fn read_security_result(stream: &mut TcpStream) -> Result<(), String> {
    match read_u32(stream).await? {
        0 => Ok(()),
        _ => Err(read_failure_reason(stream).await),
    }
}

async fn read_u32(stream: &mut TcpStream) -> Result<u32, String> {
    let mut bytes = [0u8; 4];
    stream
        .read_exact(&mut bytes)
        .await
        .map_err(|error| format!("VNC read failed: {error}"))?;
    Ok(u32::from_be_bytes(bytes))
}

async fn read_failure_reason(stream: &mut TcpStream) -> String {
    let length = match read_u32(stream).await {
        Ok(length) => length as usize,
        Err(error) => return format!("VNC authentication failed ({error})"),
    };
    if length > MAX_SERVER_TEXT_BYTES {
        return "VNC authentication failed (reason exceeds limit)".to_string();
    }
    let mut reason = vec![0u8; length];
    match stream.read_exact(&mut reason).await {
        Ok(_) => format!(
            "VNC authentication failed: {}",
            String::from_utf8_lossy(&reason)
        ),
        Err(error) => format!("VNC authentication failed ({error})"),
    }
}

pub fn vnc_challenge_response(password: &str, challenge: &[u8; 16]) -> Result<[u8; 16], String> {
    let mut key = [0u8; 8];
    for (index, byte) in password.as_bytes().iter().take(8).enumerate() {
        key[index] = byte.reverse_bits();
    }
    let cipher = Des::new_from_slice(&key).map_err(|_| "invalid VNC DES key".to_string())?;
    let mut response = *challenge;
    for block in response.chunks_exact_mut(8) {
        cipher.encrypt_block(block.into());
    }
    Ok(response)
}

pub fn try_parse_server_message(buffer: &[u8]) -> Result<Option<(usize, ServerMessage)>, String> {
    let Some(kind) = buffer.first().copied() else {
        return Ok(None);
    };
    match kind {
        0 => parse_framebuffer_update(buffer),
        2 => Ok(Some((1, ServerMessage::Bell))),
        3 => parse_server_cut_text(buffer),
        1 => Err("VNC server sent unsupported colour-map update".to_string()),
        other => Err(format!("VNC server sent unsupported message type {other}")),
    }
}

fn parse_framebuffer_update(buffer: &[u8]) -> Result<Option<(usize, ServerMessage)>, String> {
    let Some((consumed, views)) = framebuffer_update_views(buffer)? else {
        return Ok(None);
    };
    let rectangles = views
        .into_iter()
        .map(|view| {
            let mut rectangle = view.metadata;
            rectangle.data = view.payload.to_vec();
            rectangle
        })
        .collect();
    Ok(Some((
        consumed,
        ServerMessage::FramebufferUpdate(rectangles),
    )))
}

struct BorrowedRectangle<'a> {
    metadata: Rectangle,
    payload: &'a [u8],
}

/// Validate the complete message before allocating owned pixel buffers. A
/// fragmented later rectangle must not repeatedly copy all preceding pixels.
fn framebuffer_update_views(
    buffer: &[u8],
) -> Result<Option<(usize, Vec<BorrowedRectangle<'_>>)>, String> {
    if buffer.len() < 4 {
        return Ok(None);
    }
    let count = u16::from_be_bytes([buffer[2], buffer[3]]) as usize;
    if count > MAX_RECTANGLES {
        return Err("VNC framebuffer update has too many rectangles".to_string());
    }
    let mut offset = 4;
    let mut rectangles = Vec::with_capacity(count);
    let mut total_bytes = 0usize;
    for _ in 0..count {
        if buffer.len() < offset + 12 {
            return Ok(None);
        }
        let x = u16::from_be_bytes([buffer[offset], buffer[offset + 1]]);
        let y = u16::from_be_bytes([buffer[offset + 2], buffer[offset + 3]]);
        let width = u16::from_be_bytes([buffer[offset + 4], buffer[offset + 5]]);
        let height = u16::from_be_bytes([buffer[offset + 6], buffer[offset + 7]]);
        let encoding = i32::from_be_bytes([
            buffer[offset + 8],
            buffer[offset + 9],
            buffer[offset + 10],
            buffer[offset + 11],
        ]);
        offset += 12;
        let payload_len = match encoding {
            ENCODING_RAW => checked_pixel_bytes(width, height)?,
            ENCODING_DESKTOP_SIZE => 0,
            other => return Err(format!("VNC framebuffer encoding {other} is not supported")),
        };
        total_bytes = total_bytes
            .checked_add(payload_len)
            .ok_or_else(|| "VNC framebuffer update size overflow".to_string())?;
        if total_bytes > MAX_FRAME_BYTES {
            return Err("VNC framebuffer update exceeds byte limit".to_string());
        }
        if buffer.len() < offset + payload_len {
            return Ok(None);
        }
        let payload = &buffer[offset..offset + payload_len];
        offset += payload_len;
        rectangles.push(BorrowedRectangle {
            metadata: Rectangle {
                x,
                y,
                width,
                height,
                encoding,
                data: Vec::new(),
            },
            payload,
        });
    }
    Ok(Some((offset, rectangles)))
}

fn parse_server_cut_text(buffer: &[u8]) -> Result<Option<(usize, ServerMessage)>, String> {
    if buffer.len() < 8 {
        return Ok(None);
    }
    let length = u32::from_be_bytes([buffer[4], buffer[5], buffer[6], buffer[7]]) as usize;
    if length > MAX_SERVER_TEXT_BYTES {
        return Err("VNC server cut text exceeds limit".to_string());
    }
    let total = 8usize
        .checked_add(length)
        .ok_or_else(|| "VNC server cut text length overflow".to_string())?;
    if buffer.len() < total {
        return Ok(None);
    }
    Ok(Some((total, ServerMessage::ServerCutText)))
}

pub fn framebuffer_update_request(
    incremental: bool,
    x: u16,
    y: u16,
    width: u16,
    height: u16,
) -> Vec<u8> {
    let mut message = Vec::with_capacity(10);
    message.push(3);
    message.push(u8::from(incremental));
    message.extend_from_slice(&x.to_be_bytes());
    message.extend_from_slice(&y.to_be_bytes());
    message.extend_from_slice(&width.to_be_bytes());
    message.extend_from_slice(&height.to_be_bytes());
    message
}

pub fn key_event(keysym: u32, pressed: bool) -> [u8; 8] {
    let mut message = [0u8; 8];
    message[0] = 4;
    message[1] = u8::from(pressed);
    message[4..8].copy_from_slice(&keysym.to_be_bytes());
    message
}

pub fn pointer_event(x: u16, y: u16, buttons: u8) -> [u8; 6] {
    let mut message = [0u8; 6];
    message[0] = 5;
    message[1] = buttons;
    message[2..4].copy_from_slice(&x.to_be_bytes());
    message[4..6].copy_from_slice(&y.to_be_bytes());
    message
}

pub fn validate_dimensions(width: u16, height: u16) -> Result<(), String> {
    if width == 0 || height == 0 || width > MAX_FRAME_WIDTH || height > MAX_FRAME_HEIGHT {
        return Err(format!(
            "unsupported VNC framebuffer dimensions {width}x{height}"
        ));
    }
    let _ = checked_pixel_bytes(width, height)?;
    Ok(())
}

fn checked_pixel_bytes(width: u16, height: u16) -> Result<usize, String> {
    let bytes = usize::from(width)
        .checked_mul(usize::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .ok_or_else(|| "VNC framebuffer size overflow".to_string())?;
    if bytes > MAX_FRAME_BYTES {
        return Err("VNC framebuffer exceeds byte limit".to_string());
    }
    Ok(bytes)
}

fn parse_version_banner(banner: &[u8; 12]) -> Result<(u16, u16), String> {
    if &banner[0..4] != b"RFB " || banner[7] != b'.' || banner[11] != b'\n' {
        return Err("invalid VNC version banner".to_string());
    }
    let major = parse_decimal(&banner[4..7])?;
    let minor = parse_decimal(&banner[8..11])?;
    Ok((major, minor))
}

fn parse_decimal(bytes: &[u8]) -> Result<u16, String> {
    let mut value = 0u16;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return Err("invalid VNC version banner".to_string());
        }
        value = value * 10 + u16::from(byte - b'0');
    }
    Ok(value)
}

fn set_pixel_format_rgba() -> [u8; 20] {
    [
        0, 0, 0, 0, // type + padding
        32, 24, 0, 1, // 32-bit, 24-bit depth, little endian, true colour
        0, 255, // red max
        0, 255, // green max
        0, 255, // blue max
        16, 8, 0, // RGB shifts
        0, 0, 0,
    ]
}

fn set_encodings() -> [u8; 12] {
    let mut message = [0u8; 12];
    message[0] = 2;
    message[2..4].copy_from_slice(&2u16.to_be_bytes());
    message[4..8].copy_from_slice(&ENCODING_RAW.to_be_bytes());
    message[8..12].copy_from_slice(&ENCODING_DESKTOP_SIZE.to_be_bytes());
    message
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[test]
    fn parses_rfb_version_banner() {
        assert_eq!(parse_version_banner(b"RFB 003.008\n").unwrap(), (3, 8));
        assert!(parse_version_banner(b"RFB 004.001\n").is_ok());
        assert!(parse_version_banner(b"not a banner").is_err());
    }

    #[test]
    fn encodes_standard_input_messages() {
        assert_eq!(key_event(0xff0d, true), [4, 1, 0, 0, 0, 0, 0xff, 0x0d]);
        assert_eq!(pointer_event(20, 30, 1), [5, 1, 0, 20, 0, 30]);
    }

    #[test]
    fn rejects_oversized_framebuffer() {
        assert!(validate_dimensions(MAX_FRAME_WIDTH + 1, 1).is_err());
        assert!(validate_dimensions(0, 1).is_err());
    }

    #[test]
    fn parses_raw_framebuffer_update() {
        let mut message = vec![0, 0, 0, 1];
        message.extend_from_slice(&0u16.to_be_bytes());
        message.extend_from_slice(&0u16.to_be_bytes());
        message.extend_from_slice(&1u16.to_be_bytes());
        message.extend_from_slice(&1u16.to_be_bytes());
        message.extend_from_slice(&ENCODING_RAW.to_be_bytes());
        message.extend_from_slice(&[1, 2, 3, 4]);
        let Some((consumed, ServerMessage::FramebufferUpdate(rectangles))) =
            try_parse_server_message(&message).unwrap()
        else {
            panic!("expected framebuffer update");
        };
        assert_eq!(consumed, message.len());
        assert_eq!(rectangles[0].data, vec![1, 2, 3, 4]);
    }

    #[test]
    fn fragmented_multi_rectangle_updates_borrow_pixels_until_complete() {
        let mut message = vec![0, 0, 0, 2];
        for pixel in [[1, 2, 3, 4], [5, 6, 7, 8]] {
            message.extend_from_slice(&[0, 0, 0, 0, 0, 1, 0, 1]);
            message.extend_from_slice(&ENCODING_RAW.to_be_bytes());
            message.extend_from_slice(&pixel);
        }
        for length in 0..message.len() {
            assert!(framebuffer_update_views(&message[..length])
                .unwrap()
                .is_none());
            assert!(try_parse_server_message(&message[..length])
                .unwrap()
                .is_none());
        }
        let (consumed, views) = framebuffer_update_views(&message).unwrap().unwrap();
        assert_eq!(consumed, message.len());
        assert!(views.iter().all(|view| view.metadata.data.is_empty()));
        assert_eq!(views[0].payload.as_ptr(), message[16..].as_ptr());
        assert_eq!(views[1].payload.as_ptr(), message[32..].as_ptr());
        let Some((_, ServerMessage::FramebufferUpdate(rectangles))) =
            try_parse_server_message(&message).unwrap()
        else {
            panic!("expected completed framebuffer update");
        };
        assert_eq!(rectangles[0].data, [1, 2, 3, 4]);
        assert_eq!(rectangles[1].data, [5, 6, 7, 8]);
    }

    #[tokio::test]
    async fn performs_rfb38_none_handshake_and_requests_raw_frames() {
        none_handshake(8).await;
    }
    #[tokio::test]
    async fn performs_rfb37_none_handshake_without_security_result() {
        none_handshake(7).await;
    }
    async fn none_handshake(minor: u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let banner = format!("RFB 003.{minor:03}\n");
            stream.write_all(banner.as_bytes()).await.unwrap();
            let mut client_banner = [0u8; 12];
            stream.read_exact(&mut client_banner).await.unwrap();
            assert_eq!(&client_banner, banner.as_bytes());
            stream.write_all(&[1, SECURITY_NONE]).await.unwrap();
            let mut selected = [0u8; 1];
            stream.read_exact(&mut selected).await.unwrap();
            assert_eq!(selected, [SECURITY_NONE]);
            if minor >= 8 {
                stream.write_all(&0u32.to_be_bytes()).await.unwrap();
            }
            let mut client_init = [0u8; 1];
            stream.read_exact(&mut client_init).await.unwrap();
            assert_eq!(client_init, [1]);

            let mut server_init = [0u8; 24];
            server_init[0..2].copy_from_slice(&2u16.to_be_bytes());
            server_init[2..4].copy_from_slice(&1u16.to_be_bytes());
            server_init[20..24].copy_from_slice(&4u32.to_be_bytes());
            stream.write_all(&server_init).await.unwrap();
            stream.write_all(b"mock").await.unwrap();

            let mut pixel_format = [0u8; 20];
            stream.read_exact(&mut pixel_format).await.unwrap();
            assert_eq!(pixel_format[0], 0);
            assert_eq!(pixel_format[4..8], [32, 24, 0, 1]);
            let mut encodings = [0u8; 12];
            stream.read_exact(&mut encodings).await.unwrap();
            assert_eq!(encodings[0], 2);
            assert_eq!(encodings[2..4], 2u16.to_be_bytes());
            let mut request = [0u8; 10];
            stream.read_exact(&mut request).await.unwrap();
            assert_eq!(
                request.as_slice(),
                framebuffer_update_request(false, 0, 0, 2, 1)
            );
        });

        let (_, info) = connect_and_handshake("127.0.0.1", address.port(), VncSecurity::None, None)
            .await
            .unwrap();
        assert_eq!(info.version, format!("3.{minor}"));
        assert_eq!((info.width, info.height), (2, 1));
        server.await.unwrap();
    }
}
