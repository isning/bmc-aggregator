//! Byte-exact AVMP record codecs used by Dell's legacy Virtual Media client.
//!
//! Evidence: `libVMAPI_DLL.so` from Dell's AVCT VM client:
//!
//! * `AVMPPacket::encodeHeader` (`0xda08a`) writes `AVMP`, a big-endian
//!   u32 length at offset 4, and big-endian u16 type at offset 8.
//! * `VDiskRequest` (`0xd877c`, `0xd8aa8`, `0xd8ae8`) has type `0x0210`.
//! * `ReadRequest` (`0xdc458`, `0xdc606`) has type `0x8300`.
//! * `ReadResponse` (`0xcb468`, `0xcb776`) has type `0x0300`.
//!
//! This file deliberately contains no login/APCP constructor.  Those records
//! are only enabled once their full state transition has been recovered from
//! the same official client, rather than emitting a plausible-looking login.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

pub const AVMP_MAGIC: [u8; 4] = *b"AVMP";
/// Dell's native `PacketReceiver::receive` accepts both framing magics after
/// TLS (native strings at `0x1f2566` = APCP and `0x1f256b` = AVMP).
pub const APCP_MAGIC: [u8; 4] = *b"APCP";
pub const AVMP_HEADER_LEN: usize = 12;
pub const MAX_AVMP_PACKET_LEN: usize = 1024 * 1024;

pub const VDISK_MAP_REQUEST: u16 = 0x0210;
pub const VDISK_RELEASE: u16 = 0x0220;
pub const SET_DEVICE_COUNT: u16 = 0x0240;
pub const SET_DEVICE_COUNT_RESPONSE: u16 = 0x8241;
pub const LOGIN_REQUEST: u16 = 0x0100;
pub const EXTENDED_LOGIN_REQUEST: u16 = 0x0104;
pub const RANDOM_LOGIN_REQUEST: u16 = 0x0102;
pub const CAPABILITY_8_LOGIN_REQUEST: u16 = 0x0101;
pub const LOGIN_RESPONSE: u16 = 0x8100;
pub const GET_VDISK_INFO: u16 = 0x0200;
pub const VDISK_FLOPPY_MAP_REQUEST: u16 = 0x0211;
pub const VDISK_INFO: u16 = 0x8200;
pub const HEARTBEAT_RESPONSE: u16 = 0x8400;
pub const DEVICE_STATUS: u16 = 0x8410;
pub const READ_REQUEST: u16 = 0x8300;
pub const READ_RESPONSE: u16 = 0x0300;
pub const VDISK_WRITE: u16 = 0x8310;
pub const CLIENT_STATUS: u16 = 0x0410;
pub const SESSION_CLOSE: u16 = 0x01ff;
pub const SERVER_IMAGE_INFO_REQUEST: u16 = 0x0500;
pub const SERVER_IMAGE_CREATE_REQUEST: u16 = 0x0540;
pub const SERVER_IMAGE_SPACE_REQUEST: u16 = 0x0530;
pub const SERVER_IMAGE_DELETE_REQUEST: u16 = 0x0550;
pub const SERVER_IMAGE_DATA_REQUEST: u16 = 0x0570;
pub const SERVER_IMAGE_INFO_RESPONSE: u16 = 0x8500;
pub const SERVER_IMAGE_SPACE_RESPONSE: u16 = 0x8530;
pub const SERVER_IMAGE_CREATE_RESPONSE: u16 = 0x8540;
pub const SERVER_IMAGE_DATA_RESPONSE: u16 = 0x8570;
pub const DEVICE_CONFIGURATION_OPTION: u16 = 0x8430;
pub const VCARD_REQUEST: u16 = 0x0213;
pub const VCARD_XFER_BLOCK: u16 = 0x8301;
pub const VCARD_DATA_BLOCK: u16 = 0x0301;
pub const MAX_MEDIA_READ_BYTES: usize = 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvmpGenerationProfile {
    Idrac6Java,
    Idrac8Native,
}

/// iDRAC6 Java launcher's mutually exclusive login selection.  This is kept
/// separate from the iDRAC8 native credential-type selector: identical AVMP
/// record geometry does not make their launcher predicates interchangeable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Idrac6LoginMode {
    DirectAccount,
    Random {
        client_random: u32,
        appliance_random: u32,
        preempt: bool,
    },
    Capability8,
}

pub fn idrac6_login_for_mode(
    mode: Idrac6LoginMode,
    username: &str,
    password: &str,
) -> Result<AvmpPacket, String> {
    match mode {
        Idrac6LoginMode::DirectAccount => direct_login(username, password),
        Idrac6LoginMode::Random {
            client_random,
            appliance_random,
            preempt,
        } => random_login(client_random, appliance_random, preempt),
        Idrac6LoginMode::Capability8 => capability_8_login(),
    }
}

/// Exact `VirtualMedia#login(int,int,byte,byte)` / native
/// `RandomLoginMessage` layout: two u32 values, a raw zero byte, and preempt.
pub fn random_login(
    client_random: u32,
    appliance_random: u32,
    preempt: bool,
) -> Result<AvmpPacket, String> {
    let mut payload = [0u8; 10];
    payload[..4].copy_from_slice(&client_random.to_be_bytes());
    payload[4..8].copy_from_slice(&appliance_random.to_be_bytes());
    // byte 8 is the source-observed raw zero; it has no recovered product
    // name and must not be reused as a port/channel/mode field.
    payload[9] = u8::from(preempt);
    AvmpPacket::new(RANDOM_LOGIN_REQUEST, payload)
}

/// The iDRAC6 setup-capability-8 branch sends this empty AVMP marker.  Its
/// AVMP magic distinguishes it from APCP's unrelated `0x0101` request.
pub fn capability_8_login() -> Result<AvmpPacket, String> {
    AvmpPacket::new(CAPABILITY_8_LOGIN_REQUEST, [])
}

/// Native `ServerImageInfo`: AVMP `0x0500`, total 12, zero extension.
pub fn server_image_info_request() -> AvmpPacket {
    AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: SERVER_IMAGE_INFO_REQUEST,
        header_extension: 0,
        payload: Vec::new(),
    }
}

/// Native `ServerImageSpace(store)`: AVMP `0x0530`, total 12.  The supplied
/// u16 is serialized at outer byte 10, i.e. AVMP's header extension.
pub fn server_image_space_request(store: u16) -> AvmpPacket {
    AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: SERVER_IMAGE_SPACE_REQUEST,
        header_extension: store,
        payload: Vec::new(),
    }
}

/// Native `ServerImageDelete(index)`: AVMP `0x0550`, total 12, with the
/// image index in the AVMP header extension.
pub fn server_image_delete_request(index: u16) -> AvmpPacket {
    AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: SERVER_IMAGE_DELETE_REQUEST,
        header_extension: index,
        payload: Vec::new(),
    }
}

/// Hardened compatible replacement for the official `ServerImageCreate`
/// serializer. The native writer leaves payload bytes 0..3 uninitialised and
/// uses an unbounded `strcpy` at byte 15. This writer preserves every proven
/// field offset but zero-fills the complete 268-byte payload and bounds the
/// C-string region. It intentionally remains a codec only until a product
/// upload policy and hardware interoperability test are enabled.
pub fn server_image_create_request(
    index: u16,
    size_blocks: u32,
    flags: u32,
    image_type: u8,
    image_format: u8,
    store: u8,
    name: &[u8],
) -> Result<AvmpPacket, String> {
    const PAYLOAD_LEN: usize = 268;
    const NAME_OFFSET: usize = 15;
    const NAME_CAPACITY: usize = PAYLOAD_LEN - NAME_OFFSET - 1;
    if name.contains(&0) {
        return Err("AVMP server-image name must not contain NUL".to_string());
    }
    if name.len() > NAME_CAPACITY {
        return Err(format!(
            "AVMP server-image name exceeds the {NAME_CAPACITY}-byte wire limit"
        ));
    }
    let mut payload = vec![0; PAYLOAD_LEN];
    payload[4..8].copy_from_slice(&size_blocks.to_be_bytes());
    payload[8..12].copy_from_slice(&flags.to_be_bytes());
    payload[12] = image_type;
    payload[13] = image_format;
    payload[14] = store;
    payload[NAME_OFFSET..NAME_OFFSET + name.len()].copy_from_slice(name);
    Ok(AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: SERVER_IMAGE_CREATE_REQUEST,
        header_extension: index,
        payload,
    })
}

/// Native `ServerImageData(index, offset, data)`: the only server-image
/// upload request whose serializer supplies a complete deterministic payload.
/// The caller owns the image-create policy; this codec only preserves the
/// source-confirmed wire contract and rejects values that cannot fit its u16
/// length field.
pub fn server_image_data_request(
    index: u16,
    offset_blocks: u32,
    data: impl AsRef<[u8]>,
) -> Result<AvmpPacket, String> {
    let data = data.as_ref();
    let length = u16::try_from(data.len())
        .map_err(|_| "AVMP server-image data exceeds its u16 length field".to_string())?;
    let mut payload = Vec::with_capacity(6 + data.len());
    payload.extend_from_slice(&offset_blocks.to_be_bytes());
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(data);
    Ok(AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: SERVER_IMAGE_DATA_REQUEST,
        header_extension: index,
        payload,
    })
}

/// One bounded entry from native `ServerImageInfoResponse`.  Names are a
/// fixed C-string region on the wire; retain them as bytes rather than
/// assuming UTF-8 or allowing a missing NUL to escape the 253-byte region.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ServerImageInfoEntry {
    pub index: u16,
    pub remote_disk_id: u16,
    pub size_blocks: u32,
    pub flags: u32,
    pub image_type: u8,
    pub image_format: u8,
    pub store: u8,
    pub name: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerImageSpace {
    pub store: u16,
    pub total_blocks: u32,
    pub free_blocks: u32,
}

/// Both create and data responses use their header extension as the image
/// index and carry one raw status byte.  The native waiter does not validate
/// status for data uploads; a safe runtime must do so before reporting
/// success.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ServerImageStatus {
    pub index: u16,
    pub status: u8,
}

pub fn parse_server_image_info(packet: &AvmpPacket) -> Result<Vec<ServerImageInfoEntry>, String> {
    if packet.packet_type != SERVER_IMAGE_INFO_RESPONSE {
        return Err(format!(
            "expected AVMP ServerImageInfoResponse, got 0x{:04x}",
            packet.packet_type
        ));
    }
    const ENTRY_BYTES: usize = 268;
    let count = usize::from(packet.header_extension);
    let expected = count
        .checked_mul(ENTRY_BYTES)
        .ok_or_else(|| "AVMP server-image entry count overflows".to_string())?;
    if packet.payload.len() != expected {
        return Err(format!(
            "AVMP server-image info has {} bytes, expected {expected}",
            packet.payload.len()
        ));
    }
    packet
        .payload
        .chunks_exact(ENTRY_BYTES)
        .map(|entry| {
            let name_region = &entry[15..268];
            let name_end = name_region
                .iter()
                .position(|byte| *byte == 0)
                .unwrap_or(name_region.len());
            Ok(ServerImageInfoEntry {
                index: u16::from_be_bytes(entry[0..2].try_into().expect("bounded")),
                remote_disk_id: u16::from_be_bytes(entry[2..4].try_into().expect("bounded")),
                size_blocks: u32::from_be_bytes(entry[4..8].try_into().expect("bounded")),
                flags: u32::from_be_bytes(entry[8..12].try_into().expect("bounded")),
                image_type: entry[12],
                image_format: entry[13],
                store: entry[14],
                name: name_region[..name_end].to_vec(),
            })
        })
        .collect()
}

pub fn parse_server_image_space(packet: &AvmpPacket) -> Result<ServerImageSpace, String> {
    if packet.packet_type != SERVER_IMAGE_SPACE_RESPONSE || packet.payload.len() != 8 {
        return Err("AVMP server-image space response has an invalid record shape".to_string());
    }
    Ok(ServerImageSpace {
        store: packet.header_extension,
        total_blocks: u32::from_be_bytes(packet.payload[0..4].try_into().expect("bounded")),
        free_blocks: u32::from_be_bytes(packet.payload[4..8].try_into().expect("bounded")),
    })
}

pub fn parse_server_image_status(
    packet: &AvmpPacket,
    expected_type: u16,
) -> Result<ServerImageStatus, String> {
    if !matches!(
        expected_type,
        SERVER_IMAGE_CREATE_RESPONSE | SERVER_IMAGE_DATA_RESPONSE
    ) {
        return Err(format!(
            "AVMP 0x{expected_type:04x} is not a server-image status response"
        ));
    }
    if packet.packet_type != expected_type || packet.payload.is_empty() {
        return Err(format!(
            "AVMP server-image status response 0x{expected_type:04x} has an invalid record shape (extension={}, payload={})",
            packet.header_extension,
            packet.payload.len()
        ));
    }
    Ok(ServerImageStatus {
        index: packet.header_extension,
        status: packet.payload[0],
    })
}

/// Direct Web-account login used by the default VM credential mode.
/// `LoginMessage::LoginMessage` starts as type `0x0100` and its encoder at
/// `0xc773d` writes this fixed 153-byte payload.  This is separate from the
/// AVCT-console `0x0102` login packet.
pub fn direct_login(username: &str, password: &str) -> Result<AvmpPacket, String> {
    if username.is_empty() || password.is_empty() {
        return Err("iDRAC virtual-media username and password are required".to_string());
    }
    if !username.is_ascii() || !password.is_ascii() {
        return Err("iDRAC virtual-media credentials must be ASCII".to_string());
    }
    if username.len() > 95 || password.len() > 32 {
        return Err("iDRAC virtual-media credentials exceed the legacy field size".to_string());
    }
    // The official record is 153 bytes *including* the 12-byte header.
    let mut payload = vec![0; 141];
    payload[0] = username.len() as u8;
    payload[1..1 + username.len()].copy_from_slice(username.as_bytes());
    // `LoginMessage::setCredentials` stores this into a fixed 32-byte field;
    // direct credentials have no separate password-length byte.
    payload[97..97 + password.len()].copy_from_slice(password.as_bytes());
    // 129..137 is the zeroed direct-login token. Channel, port, selector and
    // preemption fields at 137..141 stay zero for a direct iDRAC session.
    AvmpPacket::new(LOGIN_REQUEST, payload)
}

/// `AVMPSession::sendLoginUser` selects `LoginMessage(major * 100 + minor)`.
/// `LoginMessage` changes its type to `0x0104` above version 1.13.
pub fn login_for_version(
    username: &str,
    password: &str,
    avmp_major: u8,
    avmp_minor: u8,
) -> Result<AvmpPacket, String> {
    if u16::from(avmp_major) * 100 + u16::from(avmp_minor) <= 113 {
        return direct_login(username, password);
    }
    if username.is_empty() || password.is_empty() || !username.is_ascii() || !password.is_ascii() {
        return Err("iDRAC virtual-media credentials must be non-empty ASCII".to_string());
    }
    if username.len() > 95 || password.len() > 32 {
        return Err("iDRAC virtual-media credentials exceed the legacy field size".to_string());
    }
    // Exact `LoginMessage::encodePayload` 0x0104 layout: total is
    // 64 + username + password, therefore payload is 52 + username + password.
    let mut payload = vec![0; 52 + username.len() + password.len()];
    payload[0..2].copy_from_slice(&(username.len() as u16).to_be_bytes());
    payload[2..4].copy_from_slice(&64u16.to_be_bytes());
    payload[4..6].copy_from_slice(&(password.len() as u16).to_be_bytes());
    payload[50..50 + username.len()].copy_from_slice(username.as_bytes());
    let password_offset = 50 + username.len();
    payload[password_offset..password_offset + password.len()].copy_from_slice(password.as_bytes());
    AvmpPacket::new(EXTENDED_LOGIN_REQUEST, payload)
}

/// Minimal login outcome decoded by `LoginResponse::decode` (`0xdad2e`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LoginResponse {
    pub status: u8,
    pub flags: u8,
    pub username: String,
}

pub fn parse_login_response(
    packet: &AvmpPacket,
    profile: AvmpGenerationProfile,
) -> Result<LoginResponse, String> {
    if packet.packet_type != LOGIN_RESPONSE || packet.payload.len() < 3 {
        return Err("peer did not send an AVMP login response".to_string());
    }
    let name_len = usize::from(packet.payload[2]);
    let username_region = match profile {
        // `VirtualMedia::receiveLoginStatus` consumes status, flags,
        // name_len and the fixed 96-byte username region regardless of the
        // advertised record total.
        AvmpGenerationProfile::Idrac6Java => {
            if name_len > 96 || packet.payload.len() != 99 {
                return Err(
                    "iDRAC6 AVMP login response has an invalid fixed username region".to_string(),
                );
            }
            &packet.payload[3..3 + name_len]
        }
        AvmpGenerationProfile::Idrac8Native => {
            // Native `LoginResponse::decode` passes the declared byte count
            // to `decodeString` and does not require it to consume the whole
            // record. Some iDRAC8 firmware returns a fixed padded response
            // (observed 104-byte payload with name_len=0), so reject only a
            // truncated declared region.
            if packet.payload.len() < 3 + name_len {
                return Err(format!(
                    "iDRAC8 AVMP login response has a truncated username (payload={}, declared={name_len})",
                    packet.payload.len()
                ));
            }
            &packet.payload[3..3 + name_len]
        }
    };
    let username = std::str::from_utf8(username_region)
        .map_err(|_| "AVMP login response username is not UTF-8".to_string())?
        .to_string();
    Ok(LoginResponse {
        status: packet.payload[0],
        flags: packet.payload[1],
        username,
    })
}

/// A remotely advertised virtual-media slot.  The official request processor
/// uses the index in this list as `VMRemoteDrive::driveId`, so it is retained
/// explicitly rather than assuming a CD slot number.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
pub struct RemoteDrive {
    pub id: u16,
    pub drive_type: u8,
    pub status: u8,
    pub capabilities: u16,
}

/// Inbound native `VDiskWrite` (`0x8310`). The remote disk id is AVMP's
/// outer header extension; the payload starts with offset and block count.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WriteRequest {
    pub remote_disk_id: u16,
    pub offset_blocks: u32,
    pub block_count: u32,
    pub data: Vec<u8>,
}

/// BMC-to-client `DeviceConfigurationOption` (`0x8430`).  Only option id 1
/// has a recovered interpretation (non-zero remote-readonly); all option IDs
/// remain raw here so protocol parsing does not invent product semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceConfigurationOption {
    pub remote_disk_id: u16,
    pub option_id: u32,
    pub value_width: u32,
    pub value: u32,
}

/// iDRAC6-only smart-card record body.  The AVMP header extension is always
/// the remote smart-card drive id and the body is a u16 byte count plus raw
/// ATR/APDU bytes.  This codec intentionally does not provide a host-card
/// adapter; that requires a separately audited PC/SC integration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VCardBlock {
    pub remote_disk_id: u16,
    pub bytes: Vec<u8>,
}

fn vcard_packet(
    packet_type: u16,
    remote_disk_id: u16,
    bytes: impl AsRef<[u8]>,
) -> Result<AvmpPacket, String> {
    let bytes = bytes.as_ref();
    let length = u16::try_from(bytes.len())
        .map_err(|_| "AVMP smart-card block exceeds its u16 length field".to_string())?;
    let mut payload = Vec::with_capacity(2 + bytes.len());
    payload.extend_from_slice(&length.to_be_bytes());
    payload.extend_from_slice(bytes);
    Ok(AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type,
        header_extension: remote_disk_id,
        payload,
    })
}

pub fn vcard_request(remote_disk_id: u16, atr: impl AsRef<[u8]>) -> Result<AvmpPacket, String> {
    vcard_packet(VCARD_REQUEST, remote_disk_id, atr)
}

pub fn vcard_data_block(
    remote_disk_id: u16,
    response: impl AsRef<[u8]>,
) -> Result<AvmpPacket, String> {
    vcard_packet(VCARD_DATA_BLOCK, remote_disk_id, response)
}

impl TryFrom<&AvmpPacket> for VCardBlock {
    type Error = String;

    fn try_from(packet: &AvmpPacket) -> Result<Self, Self::Error> {
        if packet.packet_type != VCARD_XFER_BLOCK || packet.payload.len() < 2 {
            return Err("AVMP smart-card transfer has an invalid record shape".to_string());
        }
        let length = usize::from(u16::from_be_bytes(
            packet.payload[..2].try_into().expect("bounded"),
        ));
        if packet.payload.len() != 2 + length {
            return Err("AVMP smart-card transfer has a truncated or trailing APDU".to_string());
        }
        Ok(Self {
            remote_disk_id: packet.header_extension,
            bytes: packet.payload[2..].to_vec(),
        })
    }
}

impl TryFrom<&AvmpPacket> for DeviceConfigurationOption {
    type Error = String;

    fn try_from(packet: &AvmpPacket) -> Result<Self, Self::Error> {
        if packet.packet_type != DEVICE_CONFIGURATION_OPTION || packet.payload.len() < 8 {
            return Err("AVMP device configuration option has an invalid record shape".to_string());
        }
        let option_id = u32::from_be_bytes(packet.payload[..4].try_into().expect("bounded"));
        let value_width = u32::from_be_bytes(packet.payload[4..8].try_into().expect("bounded"));
        let value_len = match value_width {
            1 | 2 | 4 => value_width as usize,
            _ => {
                return Err(format!(
                    "AVMP device configuration option has unsupported value width {value_width}"
                ))
            }
        };
        if packet.payload.len() != 8 + value_len {
            return Err(
                "AVMP device configuration option has a truncated or trailing value".to_string(),
            );
        }
        let value = match value_len {
            1 => u32::from(packet.payload[8]),
            2 => u32::from(u16::from_be_bytes(
                packet.payload[8..10].try_into().expect("bounded"),
            )),
            4 => u32::from_be_bytes(packet.payload[8..12].try_into().expect("bounded")),
            _ => unreachable!(),
        };
        Ok(Self {
            remote_disk_id: packet.header_extension,
            option_id,
            value_width,
            value,
        })
    }
}

impl TryFrom<&AvmpPacket> for WriteRequest {
    type Error = String;

    fn try_from(packet: &AvmpPacket) -> Result<Self, Self::Error> {
        if packet.packet_type != VDISK_WRITE || packet.payload.len() < 8 {
            return Err("AVMP virtual-disk write has an invalid record shape".to_string());
        }
        Ok(Self {
            remote_disk_id: packet.header_extension,
            offset_blocks: u32::from_be_bytes(packet.payload[..4].try_into().expect("bounded")),
            block_count: u32::from_be_bytes(packet.payload[4..8].try_into().expect("bounded")),
            data: packet.payload[8..].to_vec(),
        })
    }
}

/// Request the remote slot table. `GetVDiskInfo::GetVDiskInfo` constructs
/// exactly this zero-payload `0x0200` request (`0xc5118`).
pub fn get_vdisk_info(profile: AvmpGenerationProfile) -> AvmpPacket {
    AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: GET_VDISK_INFO,
        header_extension: match profile {
            AvmpGenerationProfile::Idrac6Java => 0,
            AvmpGenerationProfile::Idrac8Native => 0xffff,
        },
        payload: Vec::new(),
    }
}

/// Requested remote device layout. These are semantic counts from the native
/// `sendSetDeviceCount(numFloppy, numCd, numFlash)` API, not OEM drive types.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct VirtualDeviceCounts {
    pub floppy: u8,
    pub optical: u8,
    pub flash: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeviceStatus {
    pub remote_disk_id: u16,
    pub status: u32,
}

impl TryFrom<&AvmpPacket> for DeviceStatus {
    type Error = String;

    fn try_from(packet: &AvmpPacket) -> Result<Self, Self::Error> {
        if packet.packet_type != DEVICE_STATUS || packet.payload.len() != 4 {
            return Err("AVMP DeviceStatus has an invalid record shape".to_string());
        }
        Ok(Self {
            remote_disk_id: packet.header_extension,
            status: u32::from_be_bytes(packet.payload[..4].try_into().expect("bounded")),
        })
    }
}

/// Native `SetDeviceCount(0x0240)` has two source-confirmed layouts. AVMP
/// versions newer than 1.1 use a four-byte payload; the official serializer
/// leaves byte zero uninitialised, so the hardened encoder deterministically
/// clears that reserved byte. Older versions pack optical in the high nibble
/// and the combined floppy/flash count in the low nibble of header byte 10.
pub fn set_device_count_request(
    extended: bool,
    counts: VirtualDeviceCounts,
) -> Result<AvmpPacket, String> {
    let removable = counts
        .floppy
        .checked_add(counts.flash)
        .ok_or_else(|| "AVMP removable device count overflows".to_string())?;
    if counts.optical > 0x0f || removable > 0x0f {
        return Err("AVMP device counts exceed the official header limits".to_string());
    }
    if extended {
        AvmpPacket::new(
            SET_DEVICE_COUNT,
            [0, counts.flash, counts.optical, counts.floppy],
        )
    } else {
        Ok(AvmpPacket {
            magic: AVMP_MAGIC,
            packet_type: SET_DEVICE_COUNT,
            header_extension: u16::from_be_bytes([(counts.optical << 4) | removable, 0]),
            payload: Vec::new(),
        })
    }
}

pub fn parse_set_device_count_response(packet: &AvmpPacket) -> Result<VirtualDeviceCounts, String> {
    if packet.packet_type != SET_DEVICE_COUNT_RESPONSE || packet.payload.len() != 4 {
        return Err("AVMP SetDeviceCount response has an invalid record shape".to_string());
    }
    Ok(VirtualDeviceCounts {
        floppy: packet.payload[3],
        optical: packet.payload[2],
        flash: packet.payload[1],
    })
}

/// Decode the `0x8200` slot table. `VDiskInfo::decode` (`0xd8134`) reads a
/// big-endian u16 count, then `{type:u8,status:u8,capabilities:u16}` entries.
pub fn parse_vdisk_info(packet: &AvmpPacket) -> Result<Vec<RemoteDrive>, String> {
    if packet.packet_type != VDISK_INFO || packet.payload.len() < 2 {
        return Err("peer did not send AVMP virtual-disk information".to_string());
    }
    let count = u16::from_be_bytes(packet.payload[..2].try_into().unwrap()) as usize;
    let expected = 2usize
        .checked_add(
            count
                .checked_mul(4)
                .ok_or_else(|| "AVMP virtual-disk count overflows".to_string())?,
        )
        .ok_or_else(|| "AVMP virtual-disk record length overflows".to_string())?;
    if packet.payload.len() != expected {
        return Err("AVMP virtual-disk information has an invalid record length".to_string());
    }
    Ok((0..count)
        .map(|id| {
            let offset = 2 + id * 4;
            RemoteDrive {
                id: id as u16,
                drive_type: packet.payload[offset],
                status: packet.payload[offset + 1],
                capabilities: u16::from_be_bytes(
                    packet.payload[offset + 2..offset + 4].try_into().unwrap(),
                ),
            }
        })
        .collect())
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AvmpPacket {
    /// The record framing received from the appliance.  Locally generated
    /// virtual-media requests always use AVMP.
    pub magic: [u8; 4],
    pub packet_type: u16,
    /// Bytes 10..12 of an AVMP frame.  The generic base leaves them zero;
    /// `VDiskRequest` and `ReadRequest` use them as the remote disk id.
    pub header_extension: u16,
    pub payload: Vec<u8>,
}

impl AvmpPacket {
    pub fn new(packet_type: u16, payload: impl Into<Vec<u8>>) -> Result<Self, String> {
        let payload = payload.into();
        let total = AVMP_HEADER_LEN
            .checked_add(payload.len())
            .ok_or_else(|| "AVMP packet length overflow".to_string())?;
        if total > MAX_AVMP_PACKET_LEN {
            return Err(format!(
                "AVMP packet exceeds {} byte safety limit",
                MAX_AVMP_PACKET_LEN
            ));
        }
        Ok(Self {
            magic: AVMP_MAGIC,
            packet_type,
            header_extension: 0,
            payload,
        })
    }

    pub fn encoded_len(&self) -> usize {
        AVMP_HEADER_LEN + self.payload.len()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.encoded_len());
        bytes.extend_from_slice(&self.magic);
        bytes.extend_from_slice(&(self.encoded_len() as u32).to_be_bytes());
        bytes.extend_from_slice(&self.packet_type.to_be_bytes());
        bytes.extend_from_slice(&self.header_extension.to_be_bytes());
        bytes.extend_from_slice(&self.payload);
        bytes
    }
}

pub async fn write_packet<S>(stream: &mut S, packet: &AvmpPacket) -> Result<(), String>
where
    S: AsyncWrite + Unpin,
{
    stream
        .write_all(&packet.encode())
        .await
        .map_err(|error| format!("AVMP write failed: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("AVMP flush failed: {error}"))
}

pub async fn read_packet<S>(stream: &mut S) -> Result<AvmpPacket, String>
where
    S: AsyncRead + Unpin,
{
    let mut header = [0u8; AVMP_HEADER_LEN];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => "AVMP peer closed the record stream".to_string(),
            _ => format!("AVMP header read failed: {error}"),
        })?;
    let magic: [u8; 4] = header[..4].try_into().unwrap();
    if !matches!(magic, AVMP_MAGIC | APCP_MAGIC) {
        return Err("peer did not send an AVMP/APCP record".to_string());
    }
    let total = u32::from_be_bytes(header[4..8].try_into().unwrap()) as usize;
    if !(AVMP_HEADER_LEN..=MAX_AVMP_PACKET_LEN).contains(&total) {
        return Err(format!(
            "AVMP record length {total} is outside the safe limit"
        ));
    }
    let mut payload = vec![0; total - AVMP_HEADER_LEN];
    stream
        .read_exact(&mut payload)
        .await
        .map_err(|error| match error.kind() {
            io::ErrorKind::UnexpectedEof => "AVMP record is truncated".to_string(),
            _ => format!("AVMP payload read failed: {error}"),
        })?;
    Ok(AvmpPacket {
        magic,
        packet_type: u16::from_be_bytes(header[8..10].try_into().unwrap()),
        header_extension: u16::from_be_bytes(header[10..12].try_into().unwrap()),
        payload,
    })
}

/// The BMC's request for local-media blocks.  The native `ReadRequest`
/// decoder reads these three u32 values from the payload at offsets 0, 4, 8.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReadRequest {
    pub remote_disk_id: u16,
    pub offset_blocks: u32,
    pub block_count: u32,
    /// Native `RequestProcessor` uses this third scalar as the maximum number
    /// of blocks per response, not as an LBA/start offset.
    pub max_blocks_per_packet: u32,
}

impl TryFrom<&AvmpPacket> for ReadRequest {
    type Error = String;

    fn try_from(packet: &AvmpPacket) -> Result<Self, Self::Error> {
        if packet.packet_type != READ_REQUEST {
            return Err(format!(
                "expected AVMP ReadRequest, got 0x{:04x}",
                packet.packet_type
            ));
        }
        if packet.payload.len() != 12 {
            return Err(format!(
                "AVMP ReadRequest has {} bytes, expected 12",
                packet.payload.len()
            ));
        }
        Ok(Self {
            // `ReadRequest::decode` reads this from header bytes 10..12.
            remote_disk_id: packet.header_extension,
            offset_blocks: u32::from_be_bytes(packet.payload[0..4].try_into().unwrap()),
            block_count: u32::from_be_bytes(packet.payload[4..8].try_into().unwrap()),
            max_blocks_per_packet: u32::from_be_bytes(packet.payload[8..12].try_into().unwrap()),
        })
    }
}

/// Exact map request written by `AVMPSession::sendMapRequest` for a local
/// image. The optional TOC is only needed for physical optical drives; ISO
/// images use an empty TOC and therefore a fixed 12-byte payload.
pub fn vdisk_map_request(
    remote_disk_id: u16,
    block_size: u32,
    capacity_blocks: u32,
    drive_flags: u8,
    request_block_limit: u8,
    toc: &[u8],
) -> Result<AvmpPacket, String> {
    let toc_len: u16 = toc
        .len()
        .try_into()
        .map_err(|_| "AVMP virtual-media TOC exceeds u16 length".to_string())?;
    let mut payload = Vec::with_capacity(12 + toc.len());
    payload.extend_from_slice(&block_size.to_be_bytes());
    payload.extend_from_slice(&capacity_blocks.to_be_bytes());
    payload.push(drive_flags);
    payload.push(request_block_limit);
    payload.extend_from_slice(&toc_len.to_be_bytes());
    payload.extend_from_slice(toc);
    let mut packet = AvmpPacket::new(VDISK_MAP_REQUEST, payload)?;
    // `VDiskRequest::encodeHeader` writes `m_remoteDiskId` here.
    packet.header_extension = remote_disk_id;
    Ok(packet)
}

/// Exact `VDiskRequest2` (`0x0211`) encoder used by the iDRAC6 Java VM
/// client's `cmdMapFloppyDrive`. The BMC slot is still selected by the
/// backend; callers supply generic floppy geometry, never a wire device type.
pub fn vdisk_floppy_map_request(
    remote_disk_id: u16,
    block_size: u32,
    capacity_blocks: u32,
    read_only: bool,
    cylinders: u16,
    heads: u8,
    sectors: u8,
) -> Result<AvmpPacket, String> {
    if block_size == 0 || capacity_blocks == 0 || heads == 0 || sectors == 0 {
        return Err("iDRAC floppy geometry must be non-zero".to_string());
    }
    let request_block_limit = u8::try_from(capacity_blocks).unwrap_or(u8::MAX);
    let mut payload = Vec::with_capacity(14);
    payload.extend_from_slice(&block_size.to_be_bytes());
    payload.extend_from_slice(&capacity_blocks.to_be_bytes());
    payload.push(u8::from(read_only));
    payload.push(request_block_limit);
    payload.extend_from_slice(&cylinders.to_be_bytes());
    payload.push(heads);
    payload.push(sectors);
    Ok(AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: VDISK_FLOPPY_MAP_REQUEST,
        header_extension: remote_disk_id,
        payload,
    })
}

/// Build a ReadResponse for a BMC `ReadRequest`.
///
/// `ReadResponse::encodePayload` proves payload layout `offset, block_count, data`;
/// response bytes are supplied without copying again by accepting a `Vec`.
pub fn read_response(
    remote_disk_id: u16,
    offset_blocks: u32,
    block_count: u32,
    data: Vec<u8>,
) -> Result<AvmpPacket, String> {
    let mut payload = Vec::with_capacity(8 + data.len());
    payload.extend_from_slice(&offset_blocks.to_be_bytes());
    payload.extend_from_slice(&block_count.to_be_bytes());
    payload.extend_from_slice(&data);
    let mut packet = AvmpPacket::new(READ_RESPONSE, payload)?;
    packet.header_extension = remote_disk_id;
    Ok(packet)
}

/// Release an attached local image.  `VDiskRelease::encodeHeader`
/// (`0xd8608`) writes the target disk id in header bytes 10..12 and has no
/// payload.
pub fn vdisk_release(remote_disk_id: u16) -> AvmpPacket {
    AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: VDISK_RELEASE,
        header_extension: remote_disk_id,
        payload: Vec::new(),
    }
}

pub fn client_status(remote_disk_id: u16, status: u32) -> AvmpPacket {
    AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: CLIENT_STATUS,
        header_extension: remote_disk_id,
        payload: status.to_be_bytes().to_vec(),
    }
}

pub fn heartbeat() -> AvmpPacket {
    AvmpPacket {
        magic: AVMP_MAGIC,
        packet_type: 0x0400,
        header_extension: 0,
        payload: Vec::new(),
    }
}

/// Native iDRAC8 SessionClose is APCP-framed, with an all-zero transmitted
/// payload despite the object's internal session-id setter.
pub fn native_session_close() -> AvmpPacket {
    AvmpPacket {
        magic: APCP_MAGIC,
        packet_type: SESSION_CLOSE,
        header_extension: 0,
        payload: vec![0; 4],
    }
}

/// Serve one verified block request from an already-open local image.
///
/// The vendor client calculates disk byte position from `offset_blocks` and
/// `block_size` in `RequestProcessor::process(ReadRequest*)`.  We retain the
/// same block semantics but enforce checked arithmetic and a bounded read
/// before allocating.  EOF is zero-padded because the BMC asked for complete
/// blocks; an image that ends mid-block therefore remains a valid readonly
/// block device.
pub async fn serve_read_request(
    image: &mut tokio::fs::File,
    request: ReadRequest,
    block_size: u32,
) -> Result<AvmpPacket, String> {
    if block_size == 0 || request.block_count == 0 || request.max_blocks_per_packet == 0 {
        return Err("AVMP media read has a zero block size or count".to_string());
    }
    if request.block_count > request.max_blocks_per_packet {
        return Err("AVMP media read must be chunked at the BMC-provided block limit".to_string());
    }
    let byte_offset = u64::from(request.offset_blocks)
        .checked_mul(u64::from(block_size))
        .ok_or_else(|| "AVMP media read offset overflows".to_string())?;
    let byte_count = usize::try_from(
        u64::from(request.block_count)
            .checked_mul(u64::from(block_size))
            .ok_or_else(|| "AVMP media read length overflows".to_string())?,
    )
    .map_err(|_| "AVMP media read length does not fit this platform".to_string())?;
    if byte_count > MAX_MEDIA_READ_BYTES {
        return Err(format!(
            "AVMP media read of {byte_count} bytes exceeds {} byte limit",
            MAX_MEDIA_READ_BYTES
        ));
    }
    image
        .seek(std::io::SeekFrom::Start(byte_offset))
        .await
        .map_err(|error| format!("seek local media image: {error}"))?;
    let mut data = vec![0; byte_count];
    let mut received = 0;
    while received < data.len() {
        match image.read(&mut data[received..]).await {
            Ok(0) => break,
            Ok(count) => received += count,
            Err(error) => return Err(format!("read local media image: {error}")),
        }
    }
    read_response(
        request.remote_disk_id,
        request.offset_blocks,
        request.block_count,
        data,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn avmp_header_matches_the_vendor_codec() {
        let packet = AvmpPacket::new(0x0210, vec![1, 2, 3]).unwrap();
        assert_eq!(packet.encode(), b"AVMP\0\0\0\x0f\x02\x10\0\0\x01\x02\x03");
    }

    #[test]
    fn discovery_extension_is_generation_profiled() {
        assert_eq!(
            get_vdisk_info(AvmpGenerationProfile::Idrac6Java).header_extension,
            0
        );
        assert_eq!(
            get_vdisk_info(AvmpGenerationProfile::Idrac8Native).header_extension,
            0xffff
        );
    }

    #[test]
    fn device_count_layouts_match_the_native_encoder() {
        let counts = VirtualDeviceCounts {
            floppy: 1,
            optical: 1,
            flash: 0,
        };
        assert_eq!(
            set_device_count_request(true, counts).unwrap().encode(),
            b"AVMP\0\0\0\x10\x02\x40\0\0\0\0\x01\x01"
        );
        assert_eq!(
            set_device_count_request(false, counts).unwrap().encode(),
            b"AVMP\0\0\0\x0c\x02\x40\x11\0"
        );

        let response = AvmpPacket::new(SET_DEVICE_COUNT_RESPONSE, [0, 0, 1, 1]).unwrap();
        assert_eq!(parse_set_device_count_response(&response).unwrap(), counts);

        let device_status = AvmpPacket::new(0x8410, 0u32.to_be_bytes()).unwrap();
        assert!(parse_set_device_count_response(&device_status).is_err());
    }

    #[test]
    fn device_status_preserves_remote_disk_identity_and_rejects_bad_shapes() {
        let mut packet = AvmpPacket::new(DEVICE_STATUS, 6u32.to_be_bytes()).unwrap();
        packet.header_extension = 3;
        assert_eq!(
            DeviceStatus::try_from(&packet).unwrap(),
            DeviceStatus {
                remote_disk_id: 3,
                status: 6,
            }
        );

        packet.payload.pop();
        assert!(DeviceStatus::try_from(&packet).is_err());
    }

    #[test]
    fn direct_login_has_the_official_fixed_layout() {
        let login = direct_login("root", "secret").unwrap();
        assert_eq!(login.packet_type, LOGIN_REQUEST);
        assert_eq!(login.encoded_len(), 153);
        assert_eq!(login.payload.len(), 141);
        assert_eq!(&login.payload[..5], b"\x04root");
        assert_eq!(&login.payload[97..103], b"secret");
        assert!(login.payload[103..].iter().all(|byte| *byte == 0));
    }

    #[test]
    fn idrac6_alternate_launcher_logins_have_distinct_wire_forms() {
        let random = idrac6_login_for_mode(
            Idrac6LoginMode::Random {
                client_random: 0x0102_0304,
                appliance_random: 0x0506_0708,
                preempt: true,
            },
            "unused",
            "unused",
        )
        .unwrap();
        assert_eq!(random.packet_type, RANDOM_LOGIN_REQUEST);
        assert_eq!(
            random.encode(),
            b"AVMP\0\0\0\x16\x01\x02\0\0\x01\x02\x03\x04\x05\x06\x07\x08\0\x01"
        );

        let capability_8 = idrac6_login_for_mode(Idrac6LoginMode::Capability8, "", "").unwrap();
        assert_eq!(capability_8.packet_type, CAPABILITY_8_LOGIN_REQUEST);
        assert_eq!(capability_8.encode(), b"AVMP\0\0\0\x0c\x01\x01\0\0");
    }

    #[test]
    fn native_server_image_request_headers_match_the_serializers() {
        assert_eq!(
            server_image_info_request().encode(),
            b"AVMP\0\0\0\x0c\x05\0\0\0"
        );
        assert_eq!(
            server_image_space_request(7).encode(),
            b"AVMP\0\0\0\x0c\x05\x30\0\x07"
        );
        assert_eq!(
            server_image_delete_request(0x1234).encode(),
            b"AVMP\0\0\0\x0c\x05\x50\x12\x34"
        );
    }

    #[test]
    fn hardened_server_image_create_zero_fills_the_officially_skipped_bytes() {
        let packet =
            server_image_create_request(7, 0x0102_0304, 5, 2, 3, 4, b"install.iso").unwrap();
        assert_eq!(packet.packet_type, SERVER_IMAGE_CREATE_REQUEST);
        assert_eq!(packet.header_extension, 7);
        assert_eq!(packet.payload.len(), 268);
        assert_eq!(&packet.payload[..4], &[0; 4]);
        assert_eq!(&packet.payload[4..12], &[1, 2, 3, 4, 0, 0, 0, 5]);
        assert_eq!(&packet.payload[12..15], &[2, 3, 4]);
        assert_eq!(&packet.payload[15..27], b"install.iso\0");
        assert!(packet.payload[27..].iter().all(|byte| *byte == 0));
        assert!(server_image_create_request(0, 0, 0, 0, 0, 0, b"a\0b").is_err());
        assert!(server_image_create_request(0, 0, 0, 0, 0, 0, &[b'a'; 253]).is_err());
    }

    #[test]
    fn native_server_image_data_has_a_complete_deterministic_wire_form() {
        assert_eq!(
            server_image_data_request(0x1234, 0x0102_0304, [9, 8, 7])
                .unwrap()
                .encode(),
            b"AVMP\0\0\0\x15\x05\x70\x12\x34\x01\x02\x03\x04\0\x03\x09\x08\x07"
        );
        assert!(server_image_data_request(0, 0, vec![0; usize::from(u16::MAX) + 1]).is_err());
    }

    #[test]
    fn server_image_response_parsers_are_exactly_length_bounded() {
        let mut info = AvmpPacket::new(SERVER_IMAGE_INFO_RESPONSE, vec![0; 268]).unwrap();
        info.header_extension = 1;
        info.payload[0..2].copy_from_slice(&7u16.to_be_bytes());
        info.payload[2..4].copy_from_slice(&9u16.to_be_bytes());
        info.payload[4..8].copy_from_slice(&123u32.to_be_bytes());
        info.payload[8..12].copy_from_slice(&0x0102_0304u32.to_be_bytes());
        info.payload[12..15].copy_from_slice(&[1, 2, 0x10]);
        info.payload[15..20].copy_from_slice(b"disk\0");
        assert_eq!(
            parse_server_image_info(&info).unwrap(),
            vec![ServerImageInfoEntry {
                index: 7,
                remote_disk_id: 9,
                size_blocks: 123,
                flags: 0x0102_0304,
                image_type: 1,
                image_format: 2,
                store: 0x10,
                name: b"disk".to_vec(),
            }]
        );
        info.payload.pop();
        assert!(parse_server_image_info(&info).is_err());

        let mut space =
            AvmpPacket::new(SERVER_IMAGE_SPACE_RESPONSE, [0, 0, 0, 8, 0, 0, 0, 3]).unwrap();
        space.header_extension = 0x10;
        assert_eq!(
            parse_server_image_space(&space).unwrap(),
            ServerImageSpace {
                store: 0x10,
                total_blocks: 8,
                free_blocks: 3,
            }
        );

        let mut status = AvmpPacket::new(SERVER_IMAGE_DATA_RESPONSE, [0]).unwrap();
        status.header_extension = 9;
        assert_eq!(
            parse_server_image_status(&status, SERVER_IMAGE_DATA_RESPONSE).unwrap(),
            ServerImageStatus {
                index: 9,
                status: 0
            }
        );
        assert!(parse_server_image_status(&status, SERVER_IMAGE_CREATE_RESPONSE).is_err());
    }

    #[test]
    fn idrac8_uses_the_extended_version_selected_login_layout() {
        let login = login_for_version("root", "secret", 2, 59).unwrap();
        assert_eq!(login.packet_type, EXTENDED_LOGIN_REQUEST);
        assert_eq!(login.encoded_len(), 64 + 4 + 6);
        assert_eq!(&login.payload[..6], &[0, 4, 0, 64, 0, 6]);
        assert_eq!(&login.payload[50..60], b"rootsecret");
    }

    #[test]
    fn login_response_parser_preserves_the_idrac6_fixed_shape_and_native_padding() {
        let mut idrac6 = AvmpPacket::new(LOGIN_RESPONSE, vec![0; 99]).unwrap();
        idrac6.payload[..7].copy_from_slice(&[0, 1, 4, b'r', b'o', b'o', b't']);
        assert_eq!(
            parse_login_response(&idrac6, AvmpGenerationProfile::Idrac6Java)
                .unwrap()
                .username,
            "root"
        );
        // Native LoginResponse::decode consumes only the declared name span;
        // a 99-byte iDRAC6-shaped payload is therefore also a valid native
        // padded response. Generation cannot be inferred from this packet.
        assert_eq!(
            parse_login_response(&idrac6, AvmpGenerationProfile::Idrac8Native)
                .unwrap()
                .username,
            "root"
        );

        let native = AvmpPacket::new(LOGIN_RESPONSE, [0, 0, 4, b'r', b'o', b'o', b't']).unwrap();
        assert_eq!(
            parse_login_response(&native, AvmpGenerationProfile::Idrac8Native)
                .unwrap()
                .username,
            "root"
        );
        let padded = AvmpPacket::new(LOGIN_RESPONSE, vec![0; 104]).unwrap();
        assert_eq!(
            parse_login_response(&padded, AvmpGenerationProfile::Idrac8Native)
                .unwrap()
                .username,
            ""
        );
    }

    #[test]
    fn vdisk_map_layout_matches_vdisk_request_encoder() {
        let packet = vdisk_map_request(7, 2048, 1234, 1, 0xff, &[9, 8]).unwrap();
        assert_eq!(packet.packet_type, VDISK_MAP_REQUEST);
        assert_eq!(
            packet.payload,
            [0, 0, 8, 0, 0, 0, 4, 210, 1, 255, 0, 2, 9, 8]
        );
        assert_eq!(&packet.encode()[..12], b"AVMP\0\0\0\x1a\x02\x10\0\x07");
    }

    #[test]
    fn floppy_map_layout_matches_the_official_vdisk_request2_encoder() {
        let packet = vdisk_floppy_map_request(7, 512, 2880, true, 80, 2, 18).unwrap();
        assert_eq!(packet.packet_type, VDISK_FLOPPY_MAP_REQUEST);
        assert_eq!(packet.header_extension, 7);
        assert_eq!(
            packet.encode(),
            b"AVMP\0\0\0\x1a\x02\x11\0\x07\0\0\x02\0\0\0\x0b\x40\x01\xff\0\x50\x02\x12"
        );
    }

    #[test]
    fn read_request_parser_rejects_bad_shape() {
        let bad = AvmpPacket::new(READ_REQUEST, vec![0; 11]).unwrap();
        assert!(ReadRequest::try_from(&bad).is_err());
    }

    #[test]
    fn writable_request_preserves_native_header_extension_and_raw_data() {
        let mut packet = AvmpPacket::new(VDISK_WRITE, [0, 0, 0, 12, 0, 0, 0, 1, 9, 8]).unwrap();
        packet.header_extension = 7;
        assert_eq!(
            WriteRequest::try_from(&packet).unwrap(),
            WriteRequest {
                remote_disk_id: 7,
                offset_blocks: 12,
                block_count: 1,
                data: vec![9, 8],
            }
        );
        assert!(WriteRequest::try_from(&AvmpPacket::new(VDISK_WRITE, [0; 7]).unwrap()).is_err());
    }

    #[test]
    fn configuration_option_validates_the_native_width_tag() {
        let mut packet = AvmpPacket::new(
            DEVICE_CONFIGURATION_OPTION,
            [0, 0, 0, 1, 0, 0, 0, 2, 0x12, 0x34],
        )
        .unwrap();
        packet.header_extension = 7;
        assert_eq!(
            DeviceConfigurationOption::try_from(&packet).unwrap(),
            DeviceConfigurationOption {
                remote_disk_id: 7,
                option_id: 1,
                value_width: 2,
                value: 0x1234,
            }
        );
        let bad = AvmpPacket::new(DEVICE_CONFIGURATION_OPTION, [0; 8]).unwrap();
        assert!(DeviceConfigurationOption::try_from(&bad).is_err());
    }

    #[test]
    fn idrac6_smart_card_blocks_are_length_delimited_and_generation_scoped() {
        assert_eq!(
            vcard_request(7, [0x3b, 0x00]).unwrap().encode(),
            b"AVMP\0\0\0\x10\x02\x13\0\x07\0\x02\x3b\0"
        );
        assert_eq!(
            vcard_data_block(7, [0x90, 0x00]).unwrap().encode(),
            b"AVMP\0\0\0\x10\x03\x01\0\x07\0\x02\x90\0"
        );
        let mut xfer = AvmpPacket::new(VCARD_XFER_BLOCK, [0, 2, 0, 0xa4]).unwrap();
        xfer.header_extension = 7;
        assert_eq!(
            VCardBlock::try_from(&xfer).unwrap(),
            VCardBlock {
                remote_disk_id: 7,
                bytes: vec![0, 0xa4]
            }
        );
        assert!(
            VCardBlock::try_from(&AvmpPacket::new(VCARD_XFER_BLOCK, [0, 2, 0]).unwrap()).is_err()
        );
    }

    #[test]
    fn read_response_preserves_image_bytes_without_reencoding() {
        let data = vec![1, 2, 3, 4];
        let packet = read_response(7, 12, 1, data).unwrap();
        assert_eq!(packet.packet_type, READ_RESPONSE);
        assert_eq!(packet.header_extension, 7);
        assert_eq!(packet.payload, [0, 0, 0, 12, 0, 0, 0, 1, 1, 2, 3, 4]);
    }

    #[test]
    fn release_uses_the_vendor_vdisk_release_header() {
        assert_eq!(vdisk_release(7).encode(), b"AVMP\0\0\0\x0c\x02\x20\0\x07");
    }

    #[test]
    fn native_close_and_client_status_have_official_wire_forms() {
        assert_eq!(
            native_session_close().encode(),
            b"APCP\0\0\0\x10\x01\xff\0\0\0\0\0\0"
        );
        assert_eq!(
            client_status(7, 0).encode(),
            b"AVMP\0\0\0\x10\x04\x10\0\x07\0\0\0\0"
        );
        assert_eq!(heartbeat().encode(), b"AVMP\0\0\0\x0c\x04\0\0\0");
    }

    #[test]
    fn remote_drive_table_uses_wire_indexes_as_drive_ids() {
        let packet = AvmpPacket::new(VDISK_INFO, [0, 2, 1, 3, 0, 4, 4, 5, 0, 6]).unwrap();
        assert_eq!(
            parse_vdisk_info(&packet).unwrap(),
            vec![
                RemoteDrive {
                    id: 0,
                    drive_type: 1,
                    status: 3,
                    capabilities: 4
                },
                RemoteDrive {
                    id: 1,
                    drive_type: 4,
                    status: 5,
                    capabilities: 6
                },
            ]
        );
    }
}
