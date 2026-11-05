//! APCP negotiation records for Dell's legacy AVMP virtual-media channel.
//!
//! Unlike the console backend, AVMP carries APCP negotiation in an AVMP
//! `SessionRequest` record.  The layout here is taken from
//! `SessionRequest::encodeHeader/encodePayload` and
//! `ProtocolAPCP::sendSessionRequest` in Dell's `libVMAPI_DLL.so`.

use rand::RngCore;

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::protocol::AvmpGenerationProfile;
use super::protocol::{AvmpPacket, APCP_MAGIC, AVMP_HEADER_LEN, MAX_AVMP_PACKET_LEN};

pub const SESSION_REQUEST: u16 = 0x0100;
pub const SESSION_REQUEST_EX: u16 = 0x0101;
pub const SESSION_SETUP: u16 = 0x8100;
pub const SESSION_ID_REQUEST: u16 = 0x0102;
pub const OTP_REQUEST: u16 = 0x0103;
pub const SESSION_ID_RESPONSE: u16 = 0x8102;
pub const OTP_RESPONSE: u16 = 0x8103;
const SESSION_PAYLOAD_LEN: usize = 56;
const APCP_VERSION: u16 = 0x0103;
const AVMP_VERSION_MAJOR: u8 = 1;
const AVMP_VERSION_MINOR: u8 = 14;
const VIRTUAL_MEDIA_PROTOCOL_TYPE: u8 = 2;
const TLS_CAPABILITY: u32 = 0x0000_060d;

/// The native launcher's `VMConnectionInfo.reconnectMethod` values.  The
/// initial and recovery `getSocket*` calls convert only value one to setup
/// selector one; every other supplied native value uses selector two.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeReconnectMethod {
    Firmware = 0,
    Version1 = 1,
    Version2 = 2,
}

impl NativeReconnectMethod {
    pub fn from_config(value: Option<u8>) -> Result<Self, String> {
        match value.unwrap_or(Self::Firmware as u8) {
            0 => Ok(Self::Firmware),
            1 => Ok(Self::Version1),
            2 => Ok(Self::Version2),
            other => Err(format!(
                "unsupported iDRAC native reconnect method {other}; official launcher values are 0, 1, or 2"
            )),
        }
    }

    pub const fn setup_selector(self) -> u8 {
        match self {
            Self::Version1 => 1,
            Self::Firmware | Self::Version2 => 2,
        }
    }
}

/// Server-side result of the VM APCP negotiation.  The response has the same
/// outer type as the later login response, so callers must consume it before
/// beginning AVMP login and validate its fixed 56-byte payload.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionSetup {
    pub marker: u16,
    pub apcp_version: u16,
    pub avmp_major: u8,
    pub avmp_minor: u8,
    pub capabilities: u32,
    pub redirect_port: u16,
    /// Source-visible tail values. The native parser gives these no product
    /// field names, so they must not be presented as session/reconnect IDs.
    pub tail_primary: u16,
    pub tail_secondary: Option<u16>,
    pub tail_branch: u8,
    /// Exactly `ProtocolAPCP::isReconnectCapable()` after the initial setup
    /// parser's marker/capability/version branch. It is not derived from an
    /// arbitrary tail scalar.
    pub reconnect_available: bool,
}

/// Select the native VM transport exactly as the supplied official clients:
/// first capability bit 1, then 8, then 4. This backend implements only the
/// TLS wrapper selected by value 4, so callers must fault rather than retrying
/// TLS over a peer that selected another transport.
pub fn selected_transport(
    profile: AvmpGenerationProfile,
    capabilities: u32,
) -> Result<u32, String> {
    match profile {
        // iDRAC6 Java dispatches a scalar connection capability in its
        // `switch`, rather than iterating a native bit-vector.
        AvmpGenerationProfile::Idrac6Java => match capabilities {
            // Dell's Java UI rejects this branch, but the same iDRAC6 has
            // been hardware-verified to accept the AVMP login and VDiskInfo
            // sequence directly over this raw TCP transport.
            1 | 4 | 8 => Ok(capabilities),
            _ => Err(format!(
                "iDRAC6 virtual-media APCP setup selected unsupported capability {capabilities}"
            )),
        },
        AvmpGenerationProfile::Idrac8Native => {
            for transport in [0x0000_0001, 0x0000_0008, 0x0000_0004] {
                if capabilities & transport != 0 {
                    return Ok(transport);
                }
            }
            Err(format!(
                "iDRAC8 virtual-media APCP setup has no source-supported transport (capabilities=0x{capabilities:08x})"
            ))
        }
    }
}

/// Exact VM session request used by `ProtocolAPCP::getSocket(2, 2000, 2)`.
/// The native implementation sets a 32-byte random value, a 180-second
/// reconnect timeout, zero keepalive/session id, and the `0x060d` capability
/// set before serialising its 68-byte AVMP packet.
/// Serialize the regular native APCP request with the complete state used by
/// `ProtocolAPCP::sendSessionRequest`.  Initial attach uses `(180, 0)`;
/// `getSocketReconnect` can use the same writer with the retained session id
/// when the launcher's selector is not one.  Keeping that distinction in the
/// writer avoids accidentally treating every recovery as `SessionRequestEx`.
pub fn session_request_with_state(
    random: [u8; 32],
    reconnect_timeout_seconds: u16,
    session_id: u32,
) -> Vec<u8> {
    let mut payload = [0u8; SESSION_PAYLOAD_LEN];
    payload[0] = VIRTUAL_MEDIA_PROTOCOL_TYPE;
    payload[1] = AVMP_VERSION_MAJOR;
    payload[2] = AVMP_VERSION_MINOR;
    payload[4..8].copy_from_slice(&TLS_CAPABILITY.to_be_bytes());
    payload[8] = random.len() as u8;
    payload[9..41].copy_from_slice(&random);
    payload[44..46].copy_from_slice(&reconnect_timeout_seconds.to_be_bytes());
    payload[48..52].copy_from_slice(&session_id.to_be_bytes());
    // Base `AVMPPacket::encodeHeader` writes AVMP first, then
    // `SessionRequest::encodeHeader` overwrites bytes 0..4 with the literal
    // APCP (native address 0xd6957).  This is deliberately not an AVMP frame.
    let mut wire = Vec::with_capacity(AVMP_HEADER_LEN + payload.len());
    wire.extend_from_slice(b"APCP");
    wire.extend_from_slice(&((AVMP_HEADER_LEN + payload.len()) as u32).to_be_bytes());
    wire.extend_from_slice(&SESSION_REQUEST.to_be_bytes());
    wire.extend_from_slice(&APCP_VERSION.to_be_bytes());
    wire.extend_from_slice(&payload);
    wire
}

pub fn session_request_with_random(random: [u8; 32]) -> Vec<u8> {
    session_request_with_state(random, 180, 0)
}

/// Exact native `SessionRequestEx` used only by
/// `ProtocolAPCP::getSocketReconnect` when its setup selector is one.  Its
/// prefix matches the native initial writer through byte 52, but its tail is
/// a distinct serializer: five zero bytes, timeout 20, retained session id,
/// then four zero bytes.  It must never be selected for an initial session.
pub fn session_request_reconnect_with_random(session_id: u32, random: [u8; 32]) -> Vec<u8> {
    let mut wire = session_request_with_random(random);
    wire[8..10].copy_from_slice(&SESSION_REQUEST_EX.to_be_bytes());
    wire[10..12].fill(0);
    wire[53..58].fill(0);
    wire[58..60].copy_from_slice(&20u16.to_be_bytes());
    wire[60..64].copy_from_slice(&session_id.to_be_bytes());
    wire[64..68].fill(0);
    wire
}

/// Source-confirmed APCP `SessionIdRequest`, emitted before an OTP request.
/// The native serializer's first constructor parameter is not serialized;
/// only the second selector chooses the 12- or 16-byte form.
pub fn session_id_request(extended: bool) -> AvmpPacket {
    AvmpPacket {
        magic: APCP_MAGIC,
        packet_type: SESSION_ID_REQUEST,
        header_extension: 0,
        payload: if extended {
            1u32.to_be_bytes().to_vec()
        } else {
            Vec::new()
        },
    }
}

/// Source-confirmed APCP OTP request.  The observed raw u32 remains zero and
/// the purpose byte remains two; neither has a recovered product-facing name.
pub fn otp_request() -> AvmpPacket {
    let mut payload = vec![0; 36];
    payload[4] = 2;
    AvmpPacket {
        magic: APCP_MAGIC,
        packet_type: OTP_REQUEST,
        header_extension: 0,
        payload,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionIdResponse {
    pub session_id: u32,
    /// The native decoder defaults this to sixty seconds when absent.
    pub reconnect_timeout_seconds: u16,
}

pub fn parse_session_id_response(packet: &AvmpPacket) -> Result<SessionIdResponse, String> {
    if packet.magic != APCP_MAGIC || packet.packet_type != SESSION_ID_RESPONSE {
        return Err("expected APCP SessionIdResponse".to_string());
    }
    if !matches!(packet.payload.len(), 4 | 6) {
        return Err("APCP SessionIdResponse has an invalid record length".to_string());
    }
    Ok(SessionIdResponse {
        session_id: u32::from_be_bytes(packet.payload[..4].try_into().expect("bounded")),
        reconnect_timeout_seconds: if packet.payload.len() == 6 {
            u16::from_be_bytes(packet.payload[4..6].try_into().expect("bounded"))
        } else {
            60
        },
    })
}

/// OTP response credentials are opaque byte fields until the selected
/// launcher state authorizes their use.  Keeping bytes avoids asserting an
/// encoding that the native decoder does not establish.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OtpResponse {
    pub username: Vec<u8>,
    pub password: Vec<u8>,
}

pub fn parse_otp_response(packet: &AvmpPacket) -> Result<OtpResponse, String> {
    if packet.magic != APCP_MAGIC || packet.packet_type != OTP_RESPONSE || packet.payload.len() < 2
    {
        return Err("APCP OTPResponse has an invalid record shape".to_string());
    }
    let username_len = usize::from(packet.payload[0]);
    let password_len = usize::from(packet.payload[1]);
    let expected = 2usize
        .checked_add(username_len)
        .and_then(|length| length.checked_add(password_len))
        .ok_or_else(|| "APCP OTPResponse lengths overflow".to_string())?;
    if packet.payload.len() != expected {
        return Err("APCP OTPResponse has truncated or trailing credentials".to_string());
    }
    Ok(OtpResponse {
        username: packet.payload[2..2 + username_len].to_vec(),
        password: packet.payload[2 + username_len..].to_vec(),
    })
}

/// iDRAC6 Java `VirtualMedia::sendSessionRequest`: this is not an AVMP 1.14
/// request. Its launcher passes protocol type 2, version bytes 0/0 and the
/// raw connection capability selector 4.
pub fn idrac6_session_request_with_random(random: [u8; 32]) -> Vec<u8> {
    let mut wire = vec![0u8; 53];
    wire[..4].copy_from_slice(b"APCP");
    wire[4..8].copy_from_slice(&53u32.to_be_bytes());
    wire[8..10].copy_from_slice(&0x0100u16.to_be_bytes());
    wire[12] = 2;
    wire[16..20].copy_from_slice(&4u32.to_be_bytes());
    wire[20] = 32;
    wire[21..53].copy_from_slice(&random);
    wire
}

pub fn session_request_for_profile_with_random(
    profile: AvmpGenerationProfile,
    random: [u8; 32],
) -> Vec<u8> {
    match profile {
        AvmpGenerationProfile::Idrac6Java => idrac6_session_request_with_random(random),
        AvmpGenerationProfile::Idrac8Native => session_request_with_random(random),
    }
}

pub fn session_request() -> Vec<u8> {
    let mut random = [0; 32];
    rand::thread_rng().fill_bytes(&mut random);
    session_request_with_random(random)
}

pub async fn write_session_request<S>(
    stream: &mut S,
    profile: AvmpGenerationProfile,
) -> Result<(), String>
where
    S: AsyncWrite + Unpin,
{
    stream
        .write_all(&session_request_for_profile_with_random(profile, {
            let mut random = [0; 32];
            rand::thread_rng().fill_bytes(&mut random);
            random
        }))
        .await
        .map_err(|error| format!("iDRAC virtual-media APCP setup write failed: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("iDRAC virtual-media APCP setup flush failed: {error}"))
}

/// Write exactly the request selected by native `getSocketReconnect`.
///
/// The caller must pass the session id retained from a validated
/// `SessionIdResponse`; this function deliberately does not manufacture one.
/// Selector one uses `0x0101` with its fixed 20-second field. Selectors zero
/// and two use the ordinary `0x0100` serializer with its retained id and the
/// native 180-second reconnect field.
pub async fn write_reconnect_session_request<S>(
    stream: &mut S,
    method: NativeReconnectMethod,
    session_id: u32,
) -> Result<(), String>
where
    S: AsyncWrite + Unpin,
{
    let mut random = [0; 32];
    rand::thread_rng().fill_bytes(&mut random);
    let wire = if method.setup_selector() == 1 {
        session_request_reconnect_with_random(session_id, random)
    } else {
        session_request_with_state(random, 180, session_id)
    };
    stream
        .write_all(&wire)
        .await
        .map_err(|error| format!("iDRAC virtual-media reconnect APCP write failed: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("iDRAC virtual-media reconnect APCP flush failed: {error}"))
}

pub async fn read_session_setup<S>(
    stream: &mut S,
    profile: AvmpGenerationProfile,
    reconnect_method: NativeReconnectMethod,
) -> Result<SessionSetup, String>
where
    S: AsyncRead + Unpin,
{
    let mut header = [0; AVMP_HEADER_LEN];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => {
                "iDRAC virtual-media APCP setup was rejected or closed".to_string()
            }
            _ => format!("iDRAC virtual-media APCP setup header read failed: {error}"),
        })?;
    if header[..4] != *b"APCP" {
        return Err("iDRAC virtual-media peer did not return an APCP setup record".to_string());
    }
    let total = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
    if !(AVMP_HEADER_LEN..=MAX_AVMP_PACKET_LEN).contains(&total) {
        return Err("iDRAC virtual-media APCP setup has an invalid length".to_string());
    }
    let mut payload = vec![0; total - AVMP_HEADER_LEN];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => {
                "iDRAC virtual-media APCP setup is truncated".to_string()
            }
            _ => format!("iDRAC virtual-media APCP setup body read failed: {error}"),
        })?;
    parse_session_setup(
        &AvmpPacket {
            magic: APCP_MAGIC,
            packet_type: u16::from_be_bytes(header[8..10].try_into().unwrap()),
            header_extension: u16::from_be_bytes(header[10..12].try_into().unwrap()),
            payload,
        },
        profile,
        reconnect_method,
    )
}

pub fn parse_session_setup(
    packet: &AvmpPacket,
    profile: AvmpGenerationProfile,
    reconnect_method: NativeReconnectMethod,
) -> Result<SessionSetup, String> {
    if !matches!(packet.packet_type, 0x8100 | 0x8101) {
        return Err("iDRAC virtual-media APCP setup has an invalid type or length".to_string());
    }
    if profile == AvmpGenerationProfile::Idrac6Java {
        if packet.packet_type != SESSION_SETUP
            || packet.header_extension != 0
            || packet.payload.len() != 41
        {
            return Err(
                "iDRAC6 virtual-media APCP setup has an invalid type, extension, or length"
                    .to_string(),
            );
        }
        let random_len = usize::from(packet.payload[8]);
        if random_len > 32 || 9 + random_len > 41 {
            return Err("iDRAC6 virtual-media APCP setup has an invalid random field".to_string());
        }
        return Ok(SessionSetup {
            marker: packet.packet_type,
            apcp_version: 0,
            avmp_major: packet.payload[0],
            avmp_minor: packet.payload[1],
            capabilities: u32::from_be_bytes(packet.payload[2..6].try_into().unwrap()),
            redirect_port: u16::from_be_bytes(packet.payload[6..8].try_into().unwrap()),
            tail_primary: 0,
            tail_secondary: None,
            tail_branch: 0,
            reconnect_available: false,
        });
    }
    if packet.payload.len() != SESSION_PAYLOAD_LEN {
        return Err("iDRAC8 virtual-media APCP setup has an invalid length".to_string());
    }
    // `ProtocolAPCP::receiveSessionSetup` reads ten bytes first, then treats
    // the two-byte AVMP extension (the APCP version) as body offsets 0..2.
    // Our normalised record keeps that extension separately, so every
    // remaining body field is shifted two bytes left.
    let random_len = packet.payload[8] as usize;
    if random_len > 32 || 9 + random_len > 42 {
        return Err("iDRAC virtual-media APCP setup has an invalid random field".to_string());
    }
    let capabilities = u32::from_be_bytes(packet.payload[2..6].try_into().expect("bounded"));
    // `ProtocolAPCP::receiveSessionSetup` resets this state first. An 0x8101
    // setup sets it only when the caller supplied selector one or two. An
    // 0x8100 setup requires both native capability bits 0x400 and 0x200 and
    // an APCP version strictly greater than 0x0101.
    let reconnect_available = match packet.packet_type {
        0x8101 => matches!(reconnect_method.setup_selector(), 1 | 2),
        SESSION_SETUP => {
            capabilities & 0x0000_0600 == 0x0000_0600 && packet.header_extension > 0x0101
        }
        _ => false,
    };
    Ok(SessionSetup {
        marker: packet.packet_type,
        apcp_version: packet.header_extension,
        avmp_major: packet.payload[0],
        avmp_minor: packet.payload[1],
        capabilities,
        redirect_port: u16::from_be_bytes(packet.payload[6..8].try_into().unwrap()),
        tail_primary: u16::from_be_bytes(packet.payload[42..44].try_into().unwrap()),
        tail_secondary: if packet.packet_type == SESSION_SETUP {
            Some(u16::from_be_bytes(
                packet.payload[44..46].try_into().unwrap(),
            ))
        } else {
            None
        },
        tail_branch: packet.payload[47],
        reconnect_available,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_session_request_matches_the_native_encoder_layout() {
        let packet = session_request_with_random([0x5a; 32]);
        assert_eq!(&packet[..4], b"APCP");
        assert_eq!(u32::from_be_bytes(packet[4..8].try_into().unwrap()), 68);
        assert_eq!(
            u16::from_be_bytes(packet[8..10].try_into().unwrap()),
            SESSION_REQUEST
        );
        assert_eq!(
            u16::from_be_bytes(packet[10..12].try_into().unwrap()),
            0x0103
        );
        assert_eq!(&packet[12..21], &[2, 1, 14, 0, 0, 0, 6, 13, 32]);
        assert_eq!(&packet[21..53], &[0x5a; 32]);
        assert_eq!(&packet[56..64], &[0, 180, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn regular_reconnect_keeps_the_native_writer_and_retained_id() {
        let packet = session_request_with_state([0x5a; 32], 180, 0x0102_0304);
        assert_eq!(packet.len(), 68);
        assert_eq!(
            u16::from_be_bytes(packet[8..10].try_into().unwrap()),
            SESSION_REQUEST
        );
        assert_eq!(u16::from_be_bytes(packet[56..58].try_into().unwrap()), 180);
        assert_eq!(
            u32::from_be_bytes(packet[60..64].try_into().unwrap()),
            0x0102_0304
        );
    }

    #[test]
    fn reconnect_writer_is_not_a_mutated_initial_apcp_tail() {
        let packet = session_request_reconnect_with_random(0x0102_0304, [0x5a; 32]);
        assert_eq!(packet.len(), 68);
        assert_eq!(&packet[..4], b"APCP");
        assert_eq!(
            u16::from_be_bytes(packet[8..10].try_into().unwrap()),
            SESSION_REQUEST_EX
        );
        assert_eq!(&packet[10..12], &[0, 0]);
        assert_eq!(&packet[53..58], &[0; 5]);
        assert_eq!(&packet[58..60], &20u16.to_be_bytes());
        assert_eq!(&packet[60..64], &0x0102_0304u32.to_be_bytes());
        assert_eq!(&packet[64..68], &[0; 4]);
    }

    #[test]
    fn otp_records_and_responses_are_strictly_bounded() {
        assert_eq!(
            session_id_request(false).encode(),
            b"APCP\0\0\0\x0c\x01\x02\0\0"
        );
        assert_eq!(
            session_id_request(true).encode(),
            b"APCP\0\0\0\x10\x01\x02\0\0\0\0\0\x01"
        );
        let otp = otp_request();
        assert_eq!(otp.encoded_len(), 48);
        assert_eq!(otp.magic, APCP_MAGIC);
        assert_eq!(otp.payload[4], 2);
        assert!(otp
            .payload
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || *byte == 0));

        let session = AvmpPacket {
            magic: APCP_MAGIC,
            packet_type: SESSION_ID_RESPONSE,
            header_extension: 0,
            payload: [0x01, 0x02, 0x03, 0x04, 0, 30].to_vec(),
        };
        assert_eq!(
            parse_session_id_response(&session).unwrap(),
            SessionIdResponse {
                session_id: 0x0102_0304,
                reconnect_timeout_seconds: 30,
            }
        );
        let otp_response = AvmpPacket {
            magic: APCP_MAGIC,
            packet_type: OTP_RESPONSE,
            header_extension: 0,
            payload: b"\x04\x02rootpw".to_vec(),
        };
        assert_eq!(
            parse_otp_response(&otp_response).unwrap(),
            OtpResponse {
                username: b"root".to_vec(),
                password: b"pw".to_vec(),
            }
        );
        assert!(parse_otp_response(&AvmpPacket {
            magic: APCP_MAGIC,
            packet_type: OTP_RESPONSE,
            header_extension: 0,
            payload: b"\x04\x02root".to_vec(),
        })
        .is_err());
    }

    #[test]
    fn session_setup_decodes_the_official_fixed_fields() {
        let mut payload = [0u8; SESSION_PAYLOAD_LEN];
        payload[..2].copy_from_slice(&[1, 14]);
        payload[2..6].copy_from_slice(&0x600u32.to_be_bytes());
        payload[6..8].copy_from_slice(&5901u16.to_be_bytes());
        payload[8] = 32;
        payload[42..44].copy_from_slice(&15u16.to_be_bytes());
        payload[44..46].copy_from_slice(&16u16.to_be_bytes());
        payload[47] = 1;
        let setup = parse_session_setup(
            &AvmpPacket {
                magic: APCP_MAGIC,
                packet_type: SESSION_SETUP,
                header_extension: 0x0103,
                payload: payload.to_vec(),
            },
            AvmpGenerationProfile::Idrac8Native,
            NativeReconnectMethod::Firmware,
        )
        .unwrap();
        assert_eq!(setup.apcp_version, 0x0103);
        assert_eq!(setup.redirect_port, 5901);
        assert_eq!(setup.tail_primary, 15);
        assert_eq!(setup.tail_secondary, Some(16));
        assert_eq!(setup.tail_branch, 1);
    }

    #[test]
    fn native_8101_setup_preserves_its_distinct_tail_shape() {
        let mut payload = [0u8; SESSION_PAYLOAD_LEN];
        payload[..2].copy_from_slice(&[1, 14]);
        payload[2..6].copy_from_slice(&4u32.to_be_bytes());
        payload[8] = 32;
        payload[42..44].copy_from_slice(&9u16.to_be_bytes());
        payload[47] = 2;
        let setup = parse_session_setup(
            &AvmpPacket {
                magic: APCP_MAGIC,
                packet_type: 0x8101,
                header_extension: 0x0103,
                payload: payload.to_vec(),
            },
            AvmpGenerationProfile::Idrac8Native,
            NativeReconnectMethod::Firmware,
        )
        .unwrap();
        assert_eq!(setup.marker, 0x8101);
        assert_eq!(setup.tail_primary, 9);
        assert_eq!(setup.tail_secondary, None);
        assert_eq!(setup.tail_branch, 2);
        assert!(setup.reconnect_available);
    }

    #[test]
    fn native_reconnect_capability_uses_the_official_marker_and_bit_predicates() {
        let mut payload = [0u8; SESSION_PAYLOAD_LEN];
        payload[..2].copy_from_slice(&[1, 14]);
        payload[2..6].copy_from_slice(&0x0600u32.to_be_bytes());
        payload[8] = 32;
        let packet = AvmpPacket {
            magic: APCP_MAGIC,
            packet_type: SESSION_SETUP,
            header_extension: 0x0102,
            payload: payload.to_vec(),
        };
        assert!(
            parse_session_setup(
                &packet,
                AvmpGenerationProfile::Idrac8Native,
                NativeReconnectMethod::Firmware,
            )
            .unwrap()
            .reconnect_available
        );

        let mut old_version = packet.clone();
        old_version.header_extension = 0x0101;
        assert!(
            !parse_session_setup(
                &old_version,
                AvmpGenerationProfile::Idrac8Native,
                NativeReconnectMethod::Firmware,
            )
            .unwrap()
            .reconnect_available
        );
        assert_eq!(NativeReconnectMethod::Version1.setup_selector(), 1);
        assert_eq!(NativeReconnectMethod::Firmware.setup_selector(), 2);
        assert_eq!(NativeReconnectMethod::Version2.setup_selector(), 2);
        assert!(NativeReconnectMethod::from_config(Some(3)).is_err());
    }

    #[test]
    fn idrac6_rejects_the_native_only_8101_marker() {
        let packet = AvmpPacket {
            magic: APCP_MAGIC,
            packet_type: 0x8101,
            header_extension: 0,
            payload: vec![0; 41],
        };
        assert!(parse_session_setup(
            &packet,
            AvmpGenerationProfile::Idrac6Java,
            NativeReconnectMethod::Firmware,
        )
        .is_err());
    }

    #[test]
    fn idrac6_request_is_the_distinct_53_byte_java_writer() {
        let wire = idrac6_session_request_with_random([0x5a; 32]);
        assert_eq!(wire.len(), 53);
        assert_eq!(
            &wire[..21],
            b"APCP\0\0\0\x35\x01\0\0\0\x02\0\0\0\0\0\0\x04\x20"
        );
        assert_eq!(&wire[21..], &[0x5a; 32]);
    }

    #[test]
    fn transport_priority_does_not_force_tls_when_an_earlier_mode_is_selected() {
        assert_eq!(
            selected_transport(AvmpGenerationProfile::Idrac8Native, 0x0000_0004).unwrap(),
            0x0000_0004
        );
        assert_eq!(
            selected_transport(AvmpGenerationProfile::Idrac8Native, 0x0000_0005).unwrap(),
            0x0000_0001
        );
        assert_eq!(
            selected_transport(AvmpGenerationProfile::Idrac8Native, 0x0000_000c).unwrap(),
            0x0000_0008
        );
        assert!(selected_transport(AvmpGenerationProfile::Idrac8Native, 0).is_err());
        assert_eq!(
            selected_transport(AvmpGenerationProfile::Idrac6Java, 4).unwrap(),
            4
        );
        assert_eq!(
            selected_transport(AvmpGenerationProfile::Idrac6Java, 1).unwrap(),
            1
        );
        assert!(selected_transport(AvmpGenerationProfile::Idrac6Java, 5).is_err());
    }
}
