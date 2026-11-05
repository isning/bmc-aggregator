//! Source-backed APCP negotiation for Dell/Avocent AVCT.
//!
//! APCP is a pre-TLS protocol.  Its request form is selected by the launcher
//! profile, not guessed from a packet type: the official iDRAC8 client has
//! three writers with different tails.  Keep that decision typed here so a
//! reconnect record cannot accidentally become the normal initial handshake.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const APCP_MAGIC: [u8; 4] = *b"APCP";
pub const APCP_BASE_BYTES: usize = 53;
pub const APCP_EXTENDED_BYTES: usize = 68;
const APCP_VERSION_MODERN: u16 = 0x0104;
const APCP_VERSION_RECONNECT_V0: u16 = 0;
const RANDOM_BYTES: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum ApcpChannel {
    #[allow(dead_code)] // Reserved by the official APCP channel enum; AVMP owns media transport.
    VirtualMedia = 2,
    Control = 3,
    Video = 4,
}

/// Launcher values consumed by `ProtocolAPCP`.  They are configuration, not
/// negotiated response values.  `selector` is deliberately explicit: while
/// the supplied iDRAC8 KVM client defaults it to 5, other official call paths
/// construct the helper with a different value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub struct ApcpLauncherProfile {
    pub selector: u32,
    pub reconnect_capability: bool,
    pub reconnect_version: i16,
    pub reconnect_preference: u16,
}

impl Default for ApcpLauncherProfile {
    fn default() -> Self {
        Self {
            // `AVSPKVMSession.fb` defaults to 5 in the supplied iDRAC8
            // client. This is a source value, not a name-derived guess.
            selector: 5,
            reconnect_capability: false,
            reconnect_version: -1,
            reconnect_preference: 0,
        }
    }
}

/// The only APCP request writers present in the supplied official client.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApcpRequestKind {
    /// iDRAC6 `com.avocent.kvm.b.k`: the launcher-selected `apcp=1`
    /// branch.  It is a 53-byte `0x0100` request with zero protocol version
    /// and zero major/minor fields, not the iDRAC8 2.41 request.
    Idrac6Initial53,
    InitialNoReconnect53,
    InitialReconnectVersion0,
    InitialReconnectModernPreference,
    ReconnectWithRetainedSession {
        session: u32,
    },
}

impl ApcpRequestKind {
    pub fn initial(profile: ApcpLauncherProfile) -> Result<Self, String> {
        if !profile.reconnect_capability {
            return Ok(Self::InitialNoReconnect53);
        }
        match profile.reconnect_version {
            0 => Ok(Self::InitialReconnectVersion0),
            -1 => Err("iDRAC AVCT launcher profile enables reconnect but has unsupported RECONNECT_VERSION=-1".to_string()),
            _ => Ok(Self::InitialReconnectModernPreference),
        }
    }

    fn response_reader(self) -> ApcpResponseReader {
        match self {
            Self::InitialReconnectModernPreference => {
                ApcpResponseReader::Alternate { argument: true }
            }
            Self::Idrac6Initial53
            | Self::InitialNoReconnect53
            | Self::InitialReconnectVersion0
            | Self::ReconnectWithRetainedSession { .. } => ApcpResponseReader::Ordinary,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ApcpResponseReader {
    Ordinary,
    Alternate { argument: bool },
}

/// Build a complete request with caller-provided randomness, for both the
/// runtime and byte-oracle tests.  The 32-byte random field is fixed-width;
/// no untrusted or secret field is accepted here.
pub fn build_request(
    kind: ApcpRequestKind,
    channel: ApcpChannel,
    profile: ApcpLauncherProfile,
    random: [u8; RANDOM_BYTES],
) -> Vec<u8> {
    let extended = !matches!(
        kind,
        ApcpRequestKind::Idrac6Initial53 | ApcpRequestKind::InitialNoReconnect53
    );
    let length = if extended {
        APCP_EXTENDED_BYTES
    } else {
        APCP_BASE_BYTES
    };
    let mut out = vec![0u8; length];
    out[..4].copy_from_slice(&APCP_MAGIC);
    out[4..8].copy_from_slice(&(length as u32).to_be_bytes());
    let (record_type, version): (u16, u16) = match kind {
        ApcpRequestKind::Idrac6Initial53 => (0x0100, 0),
        ApcpRequestKind::InitialNoReconnect53
        | ApcpRequestKind::InitialReconnectModernPreference => (0x0100, APCP_VERSION_MODERN),
        ApcpRequestKind::InitialReconnectVersion0 => (0x0101, APCP_VERSION_RECONNECT_V0),
        ApcpRequestKind::ReconnectWithRetainedSession { .. } => (0x0101, APCP_VERSION_MODERN),
    };
    out[8..10].copy_from_slice(&record_type.to_be_bytes());
    out[10..12].copy_from_slice(&version.to_be_bytes());
    out[12] = channel as u8;
    // The iDRAC8 AVSP client starts at protocol 2.41; iDRAC6 `k.a()` leaves
    // both bytes at the zero-initialised defaults.
    if !matches!(kind, ApcpRequestKind::Idrac6Initial53) {
        out[13] = 2;
        out[14] = 41;
    }
    out[16..20].copy_from_slice(&profile.selector.to_be_bytes());
    out[20] = RANDOM_BYTES as u8;
    out[21..53].copy_from_slice(&random);
    match kind {
        ApcpRequestKind::Idrac6Initial53 | ApcpRequestKind::InitialNoReconnect53 => {}
        ApcpRequestKind::InitialReconnectModernPreference => {
            out[53..55].copy_from_slice(&profile.reconnect_preference.to_be_bytes());
            out[56..58].copy_from_slice(&180u16.to_be_bytes());
            out[58..60].copy_from_slice(&30u16.to_be_bytes());
        }
        ApcpRequestKind::InitialReconnectVersion0
        | ApcpRequestKind::ReconnectWithRetainedSession { .. } => {
            let session = match kind {
                ApcpRequestKind::ReconnectWithRetainedSession { session } => session,
                _ => 0,
            };
            out[61..65].copy_from_slice(&session.to_be_bytes());
        }
    }
    out
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApcpServerTail {
    None,
    /// Source-visible scalar ordering of the ordinary reader's profile-zero
    /// branch. Meanings are not assigned because the official class does not
    /// give them product names.
    OrdinaryProfileZero {
        raw: [u8; 15],
        required_nonzero: u32,
    },
    OrdinaryExtended {
        raw: [u8; 15],
        required_nonzero: u8,
    },
    AlternateExtended {
        raw: [u8; 15],
        required_nonzero: u8,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ApcpSetup {
    pub server_marker: u16,
    pub protocol_version: u16,
    pub generation_major: u8,
    pub generation_minor: u8,
    pub transports: u32,
    pub redirect_port: u16,
    pub server_random: Vec<u8>,
    pub tail: ApcpServerTail,
}

pub async fn write_request<S>(
    stream: &mut S,
    kind: ApcpRequestKind,
    channel: ApcpChannel,
    profile: ApcpLauncherProfile,
    random: [u8; RANDOM_BYTES],
) -> Result<(), String>
where
    S: AsyncWrite + Unpin,
{
    stream
        .write_all(&build_request(kind, channel, profile, random))
        .await
        .map_err(|error| format!("AVCT APCP setup write failed: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("AVCT APCP setup flush failed: {error}"))
}

pub async fn read_server_setup<S>(
    stream: &mut S,
    kind: ApcpRequestKind,
) -> Result<ApcpSetup, String>
where
    S: AsyncRead + Unpin,
{
    let mut header = [0u8; 8];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::UnexpectedEof => {
                "AVCT APCP setup was rejected or closed before the server response".to_string()
            }
            _ => format!("AVCT APCP setup header read failed: {error}"),
        })?;
    if header[..4] != APCP_MAGIC {
        return Err("AVCT peer did not send an APCP setup record".to_string());
    }
    let length = u32::from_be_bytes(header[4..8].try_into().expect("fixed header")) as usize;
    if !matches!(length, APCP_BASE_BYTES | APCP_EXTENDED_BYTES) {
        return Err(format!(
            "AVCT APCP server setup length {length} is not an official 53- or 68-byte record"
        ));
    }
    let mut bytes = vec![0u8; length];
    bytes[..8].copy_from_slice(&header);
    stream
        .read_exact(&mut bytes[8..])
        .await
        .map_err(|error| format!("AVCT APCP setup body read failed: {error}"))?;
    let marker = u16::from_be_bytes(bytes[8..10].try_into().expect("fixed record"));
    if !matches!(marker, 0x8100 | 0x8101) {
        return Err(format!(
            "AVCT peer returned unsupported APCP marker 0x{marker:04x}"
        ));
    }
    let protocol_version = u16::from_be_bytes(bytes[10..12].try_into().expect("fixed record"));
    let transports = u32::from_be_bytes(bytes[14..18].try_into().expect("fixed record"));
    let random_len = usize::from(bytes[20]);
    if random_len > RANDOM_BYTES {
        return Err("AVCT APCP setup has an invalid random field length".to_string());
    }
    let tail_bytes: [u8; 15] = if length == APCP_EXTENDED_BYTES {
        bytes[53..68].try_into().expect("fixed tail")
    } else {
        [0; 15]
    };
    let tail = parse_tail(
        kind.response_reader(),
        marker,
        protocol_version,
        transports,
        length,
        tail_bytes,
    )?;
    Ok(ApcpSetup {
        server_marker: marker,
        protocol_version,
        generation_major: bytes[12],
        generation_minor: bytes[13],
        transports,
        redirect_port: u16::from_be_bytes(bytes[18..20].try_into().expect("fixed record")),
        server_random: bytes[21..21 + random_len].to_vec(),
        tail,
    })
}

fn parse_tail(
    reader: ApcpResponseReader,
    marker: u16,
    version: u16,
    transports: u32,
    length: usize,
    raw: [u8; 15],
) -> Result<ApcpServerTail, String> {
    let capability_200 = transports & 0x200 != 0;
    match reader {
        // The ordinary official reader intentionally consumes no suffix for
        // this branch.  We still consume the advertised record atomically;
        // both source-confirmed record lengths are valid here.
        ApcpResponseReader::Ordinary if marker == 0x8100 && !capability_200 && matches!(length, APCP_BASE_BYTES | APCP_EXTENDED_BYTES) => Ok(ApcpServerTail::None),
        ApcpResponseReader::Ordinary if marker == 0x8101 && version == 0 && length == APCP_EXTENDED_BYTES => {
            let required_nonzero = u32::from_be_bytes(raw[3..7].try_into().expect("tail"));
            if required_nonzero == 0 { return Err("AVCT APCP profile-zero response has a zero required session scalar".to_string()); }
            Ok(ApcpServerTail::OrdinaryProfileZero { raw, required_nonzero })
        }
        ApcpResponseReader::Ordinary if marker == 0x8101 && length == APCP_EXTENDED_BYTES && ((version >= 0x0102 && capability_200) || (version > 100 && !capability_200)) => {
            let required_nonzero = raw[6];
            if required_nonzero == 0 { return Err("AVCT APCP extended response has a zero required scalar".to_string()); }
            Ok(ApcpServerTail::OrdinaryExtended { raw, required_nonzero })
        }
        ApcpResponseReader::Alternate { argument: true } if marker == 0x8100 && version >= 0x0102 && length == APCP_EXTENDED_BYTES => {
            let required_nonzero = raw[7];
            if required_nonzero == 0 { return Err("AVCT APCP alternate response has a zero required scalar".to_string()); }
            Ok(ApcpServerTail::AlternateExtended { raw, required_nonzero })
        }
        _ => Err(format!("AVCT APCP response marker=0x{marker:04x}, version={version}, transports=0x{transports:08x}, length={length} does not match the selected official parser branch")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn profile() -> ApcpLauncherProfile {
        ApcpLauncherProfile {
            selector: 5,
            reconnect_capability: true,
            reconnect_version: 7,
            reconnect_preference: 0x1234,
        }
    }
    #[test]
    fn every_official_writer_has_an_exact_oracle() {
        let random = [0x5a; 32];
        let cases = [
            (ApcpRequestKind::InitialNoReconnect53, 53, 0x0100, 0x0104),
            (ApcpRequestKind::InitialReconnectVersion0, 68, 0x0101, 0),
            (
                ApcpRequestKind::InitialReconnectModernPreference,
                68,
                0x0100,
                0x0104,
            ),
            (
                ApcpRequestKind::ReconnectWithRetainedSession {
                    session: 0x01020304,
                },
                68,
                0x0101,
                0x0104,
            ),
        ];
        for (kind, size, typ, version) in cases {
            let wire = build_request(kind, ApcpChannel::Control, profile(), random);
            assert_eq!(wire.len(), size);
            assert_eq!(u16::from_be_bytes(wire[8..10].try_into().unwrap()), typ);
            assert_eq!(
                u16::from_be_bytes(wire[10..12].try_into().unwrap()),
                version
            );
            assert_eq!(&wire[21..53], &random);
            assert_eq!(u32::from_be_bytes(wire[16..20].try_into().unwrap()), 5);
        }
    }

    #[test]
    fn idrac6_apcp_launcher_writer_keeps_its_zero_version_and_protocol_bytes() {
        let wire = build_request(
            ApcpRequestKind::Idrac6Initial53,
            ApcpChannel::Control,
            ApcpLauncherProfile::default(),
            [0x5a; RANDOM_BYTES],
        );
        assert_eq!(wire.len(), APCP_BASE_BYTES);
        assert_eq!(&wire[..4], b"APCP");
        assert_eq!(u16::from_be_bytes(wire[8..10].try_into().unwrap()), 0x0100);
        assert_eq!(u16::from_be_bytes(wire[10..12].try_into().unwrap()), 0);
        assert_eq!(&wire[13..15], &[0, 0]);
        assert_eq!(u32::from_be_bytes(wire[16..20].try_into().unwrap()), 5);
    }
    #[test]
    fn initial_writer_selection_is_not_inferred_from_a_packet_type() {
        assert_eq!(
            ApcpRequestKind::initial(ApcpLauncherProfile::default()).unwrap(),
            ApcpRequestKind::InitialNoReconnect53
        );
        let mut p = profile();
        p.reconnect_version = 0;
        assert_eq!(
            ApcpRequestKind::initial(p).unwrap(),
            ApcpRequestKind::InitialReconnectVersion0
        );
        p.reconnect_version = -1;
        assert!(ApcpRequestKind::initial(p).is_err());
    }
    #[tokio::test]
    async fn parser_rejects_a_response_for_the_wrong_official_branch() {
        let mut wire = vec![0u8; 68];
        wire[..4].copy_from_slice(b"APCP");
        wire[4..8].copy_from_slice(&68u32.to_be_bytes());
        wire[8..10].copy_from_slice(&0x8101u16.to_be_bytes());
        wire[10..12].copy_from_slice(&0x0104u16.to_be_bytes());
        wire[14..18].copy_from_slice(&4u32.to_be_bytes());
        wire[20] = 32;
        let mut input = wire.as_slice();
        assert!(
            read_server_setup(&mut input, ApcpRequestKind::InitialNoReconnect53)
                .await
                .is_err()
        );
    }
}
