use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use native_tls::TlsConnector as NativeTlsConnector;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot, watch, Mutex};
use tokio_native_tls::TlsConnector;

use bmc_backend_support::removable_image::detect_removable_geometry as removable_geometry;
use bmc_feature_api::msd::MsdMediaKind;

use super::apcp::{
    parse_session_id_response, read_session_setup, selected_transport,
    write_reconnect_session_request, write_session_request, NativeReconnectMethod,
    SESSION_ID_RESPONSE,
};
use super::protocol::{
    client_status, get_vdisk_info, heartbeat, login_for_version, native_session_close,
    parse_login_response, parse_server_image_info, parse_server_image_space,
    parse_server_image_status, parse_set_device_count_response, parse_vdisk_info,
    serve_read_request, server_image_create_request, server_image_data_request,
    server_image_delete_request, server_image_info_request, server_image_space_request,
    set_device_count_request, vdisk_floppy_map_request, vdisk_map_request, vdisk_release,
    write_packet, AvmpGenerationProfile, DeviceConfigurationOption, DeviceStatus, ReadRequest,
    RemoteDrive, ServerImageInfoEntry, ServerImageSpace, VirtualDeviceCounts, DEVICE_STATUS,
    HEARTBEAT_RESPONSE, SERVER_IMAGE_CREATE_RESPONSE, SERVER_IMAGE_DATA_RESPONSE,
    SERVER_IMAGE_INFO_RESPONSE, SERVER_IMAGE_SPACE_RESPONSE, SET_DEVICE_COUNT_RESPONSE, VDISK_INFO,
};

const CD_DRIVE_TYPE: u8 = 2;
const IDRAC6_FLOPPY_DRIVE_TYPE: u8 = 1;
const IDRAC8_REMOVABLE_DRIVE_TYPE: u8 = 1;
const IDRAC8_FLOPPY_DRIVE_TYPE: u8 = 4;
const DEVICE_STATUS_OK: u32 = 0;
const VDISK_RELEASE_RESPONSE: u16 = 0x8240;
const OPTICAL_BLOCK_SIZE: u32 = 2048;
const FLOPPY_BLOCK_SIZE: u32 = 512;
const DRIVE_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(10);
const IDRAC6_DRIVE_DISCOVERY_RETRY_DELAY: Duration = Duration::from_millis(40);
const IDRAC6_DRIVE_DISCOVERY_RETRIES: u8 = 20;

trait AsyncAvmpIo: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncAvmpIo for T {}
/// The owned framing model survives dropped read futures in the worker select.
struct AvmpIo {
    inner: Box<dyn AsyncAvmpIo>,
    framing: bmc_idrac_model::framing::AvmpFraming,
    write_revision: u64,
    origin: tokio::time::Instant,
    deferred: VecDeque<super::protocol::AvmpPacket>,
}
impl AvmpIo {
    fn new(inner: impl AsyncAvmpIo + 'static) -> Self {
        Self {
            inner: Box::new(inner),
            framing: Default::default(),
            write_revision: 0,
            origin: tokio::time::Instant::now(),
            deferred: VecDeque::new(),
        }
    }
    async fn next_packet(&mut self) -> Result<super::protocol::AvmpPacket, String> {
        if let Some(packet) = self.deferred.pop_front() {
            return Ok(packet);
        }
        self.next_wire_packet().await
    }
    fn defer_packet(&mut self, packet: super::protocol::AvmpPacket) -> Result<(), String> {
        use bmc_idrac_model::media::{response_action, ResponseAction};
        if response_action(packet.packet_type, u16::MAX, self.deferred.len())
            == ResponseAction::Defer
        {
            self.deferred.push_back(packet);
            Ok(())
        } else {
            Err(format!(
                "unsupported or excessive asynchronous AVMP packet 0x{:04x}",
                packet.packet_type
            ))
        }
    }
    async fn next_wire_packet(&mut self) -> Result<super::protocol::AvmpPacket, String> {
        use bmc_idrac_model::framing::{FramingEffect, FramingEvent, FramingState};
        loop {
            if let FramingState::Failed(fault) = self.framing.state() {
                return Err(format!("AVMP framing failed: {fault:?}"));
            }
            let mut chunk = vec![0; self.framing.needed().min(8192)];
            // AsyncReadExt::read is cancellation safe; commit each successful
            // chunk before the next await. No partially consumed packet is lost.
            let read = tokio::io::AsyncReadExt::read(&mut self.inner, &mut chunk);
            let count = match self.framing.deadline() {
                Some(deadline) => tokio::time::timeout_at(self.origin + deadline, read)
                    .await
                    .ok(),
                None => Some(read.await),
            };
            let event = match count {
                None => FramingEvent::Tick {
                    now: self.origin.elapsed(),
                },
                Some(Err(error)) => return Err(format!("AVMP read failed: {error}")),
                Some(Ok(0)) => FramingEvent::Eof,
                Some(Ok(count)) => FramingEvent::Bytes {
                    bytes: &chunk[..count],
                    now: self.origin.elapsed(),
                },
            };
            let transition = std::mem::take(&mut self.framing).step(event);
            self.framing = transition.state;
            match transition.effect {
                FramingEffect::More => {}
                FramingEffect::Packet(bytes) => {
                    return Ok(super::protocol::AvmpPacket {
                        magic: bytes[..4].try_into().expect("validated header"),
                        packet_type: u16::from_be_bytes([bytes[8], bytes[9]]),
                        header_extension: u16::from_be_bytes([bytes[10], bytes[11]]),
                        payload: bytes[12..].to_vec(),
                    })
                }
                FramingEffect::Reject(fault) => {
                    return Err(format!("AVMP framing failed: {fault:?}"))
                }
                FramingEffect::IgnoreTerminal => {
                    return Err("AVMP framing already ended".to_string())
                }
            }
        }
    }
}
impl AsyncWrite for AvmpIo {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let result = std::pin::Pin::new(&mut self.inner).poll_write(cx, bytes);
        if matches!(result, std::task::Poll::Ready(Ok(count)) if count > 0) {
            self.write_revision = self.write_revision.saturating_add(1);
        }
        result
    }
    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[derive(Clone, Debug)]
pub struct IdracLegacyMediaConfig {
    pub host: String,
    pub port: u16,
    username: String,
    password: String,
    profile: AvmpGenerationProfile,
    heartbeat_interval: Option<Duration>,
    reconnect_method: NativeReconnectMethod,
}

impl IdracLegacyMediaConfig {
    pub fn new(
        host: String,
        port: u16,
        username: String,
        password: String,
        generation: u8,
        heartbeat_interval_s: Option<u32>,
        reconnect_method: Option<u8>,
    ) -> Result<Self, String> {
        if username.is_empty() {
            return Err("iDRAC virtual-media requires the device username".to_string());
        }
        if password.is_empty() {
            return Err("iDRAC virtual-media requires the device password".to_string());
        }
        if host.trim().is_empty() {
            return Err("iDRAC virtual-media host is empty".to_string());
        }
        let profile = match generation {
            6 => AvmpGenerationProfile::Idrac6Java,
            8 => AvmpGenerationProfile::Idrac8Native,
            7 => return Err("iDRAC7 virtual-media is unsupported: no official iDRAC7 client artifact has established its profile".to_string()),
            other => return Err(format!("unsupported iDRAC virtual-media generation {other}")),
        };
        let configured_heartbeat_interval = heartbeat_interval_s
            .map(|seconds| Duration::from_secs(u64::from(seconds)))
            .filter(|interval| !interval.is_zero());
        // iDRAC6's supplied Java VM client always starts
        // `ThreadDSSendHeartbeat`: it checks the timestamp of the last
        // successful AVMP write every 100 ms and emits an empty `0x0400`
        // record after ten seconds of output inactivity.  This is not an
        // optional appliance tuning value, so the generation profile owns the
        // default.  A supplied value may only restate the source-confirmed
        // ten-second interval; accepting another value would create a wire
        // behaviour for which we have no official-client evidence.
        let heartbeat_interval = match profile {
            AvmpGenerationProfile::Idrac6Java => match configured_heartbeat_interval {
                Some(interval) if interval != Duration::from_secs(10) => {
                    return Err(
                        "iDRAC6 virtual-media uses the official fixed 10-second AVMP heartbeat"
                            .to_string(),
                    );
                }
                _ => Some(Duration::from_secs(10)),
            },
            AvmpGenerationProfile::Idrac8Native => configured_heartbeat_interval,
        };
        let reconnect_method = match profile {
            AvmpGenerationProfile::Idrac6Java => NativeReconnectMethod::Firmware,
            AvmpGenerationProfile::Idrac8Native => {
                NativeReconnectMethod::from_config(reconnect_method)?
            }
        };
        Ok(Self {
            host,
            // Dell's iDRAC8 JNLP invokes the official client with both
            // `kmport=5900` and `vport=5900`; VM does not use a separate
            // 2068 endpoint on this profile.  Preserve an explicit VM-port
            // override for appliances that advertise a different endpoint.
            port,
            username,
            password,
            profile,
            heartbeat_interval,
            reconnect_method,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct MediaStatus {
    pub connected: bool,
    pub mappings: BTreeMap<String, MediaMappingStatus>,
    pub remote_drives: Vec<RemoteDrive>,
    /// Media kinds proven by a drive table during this authenticated session.
    /// A mapped drive may temporarily disappear from the idle-drive view, but
    /// its stable feature slot must remain selectable after release.
    pub known_media_types: BTreeSet<MsdMediaKind>,
    /// Session-stable topology facts. Idle drive entries may disappear while
    /// mapped, so capability rendering cannot rely on the latest table alone.
    pub removable_target_available: bool,
    pub dedicated_floppy_target_available: bool,
    /// The native device-count API has materialized a type-1 removable target
    /// that the official client can map as flash media. On tested firmware the
    /// same target is also used for floppy emulation; this does not imply a
    /// third independently mappable USB device.
    pub flash_layout_enabled: bool,
    pub flash_activation_available: bool,
    /// Raw native device configuration options keyed by remote drive then
    /// option id. Unknown options remain observable but do not alter behavior.
    pub configuration_options: BTreeMap<u16, BTreeMap<u32, u32>>,
    /// Only option id 1 has a source-confirmed product interpretation.
    pub remote_readonly: BTreeMap<u16, bool>,
    pub reconnect_available: bool,
    pub reconnecting: bool,
    /// A validated native APCP `SessionIdResponse` was received. The id itself
    /// is intentionally never exposed through status or trace output.
    pub reconnect_session_response_received: bool,
    /// The official processor changes its reconnect-check interval to half
    /// the positive timeout supplied by `SessionIdResponse`.
    pub reconnect_check_interval_seconds: Option<u16>,
    /// Observable proof that the BMC consumed the mapped image. These counters
    /// are session-local and reset on a new virtual-media connection.
    pub read_requests: u64,
    pub read_blocks: u64,
    pub server_images: Vec<ServerImageInfoEntry>,
    pub server_image_space: Option<ServerImageSpace>,
    pub server_storage_probed: bool,
}

#[derive(Clone, Debug)]
pub struct MediaMappingStatus {
    pub remote_drive_id: u16,
    pub media_type: MsdMediaKind,
    pub image_size: u64,
    pub read_requests: u64,
    pub read_blocks: u64,
}

/// Private recovery state.  The session id is protocol material, not product
/// status: it must never cross the backend boundary or enter logs.
#[derive(Debug)]
struct NativeReconnectState {
    session_id: Option<u32>,
    reconnect_check_timeout: Duration,
    last_packet_at: Instant,
    lifecycle: bmc_idrac_model::session::Session,
}

struct MappedImage {
    target_id: String,
    path: String,
    size: u64,
    media_type: MsdMediaKind,
    block_size: u32,
    read_requests: u64,
    read_blocks: u64,
    file: tokio::fs::File,
}

impl NativeReconnectState {
    fn initial() -> Self {
        Self {
            session_id: None,
            // The supplied iDRAC8 launcher sets VMConnectionInfo's native
            // reconnect timeout to 180 seconds.
            reconnect_check_timeout: Duration::from_secs(180),
            last_packet_at: Instant::now(),
            lifecycle: bmc_idrac_model::session::Session::default()
                .step(bmc_idrac_model::session::SessionEvent::Established { epoch: 0 })
                .state,
        }
    }

    fn observe_packet(&mut self) {
        self.last_packet_at = Instant::now();
    }

    #[cfg(test)]
    fn due_at(&self, now: Instant) -> bool {
        bmc_idrac_model::media::Reconnect::new(Duration::ZERO, self.reconnect_check_timeout)
            .due_at(now.saturating_duration_since(self.last_packet_at))
    }
}

enum Command {
    RefreshDrives {
        requested_media_type: Option<MsdMediaKind>,
        reply: oneshot::Sender<Result<(), String>>,
    },
    RefreshServerStorage {
        reply: oneshot::Sender<Result<(), String>>,
    },
    UploadServerImage {
        name: String,
        path: std::path::PathBuf,
        size: u64,
        image_type: u8,
        reply: oneshot::Sender<Result<ServerImageInfoEntry, String>>,
    },
    DeleteServerImage {
        index: u16,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Map {
        target_id: String,
        path: String,
        size: u64,
        media_type: MsdMediaKind,
        reply: oneshot::Sender<Result<(), String>>,
    },
    Unmap {
        target_id: String,
        reply: oneshot::Sender<Result<(), String>>,
    },
}

impl Command {
    fn kind(&self) -> bmc_idrac_model::media::CommandKind {
        use bmc_idrac_model::media::CommandKind;
        match self {
            Self::RefreshDrives { .. } => CommandKind::RefreshDrives,
            Self::Map { .. } => CommandKind::Map,
            Self::Unmap { .. } => CommandKind::Unmap,
            Self::RefreshServerStorage { .. }
            | Self::UploadServerImage { .. }
            | Self::DeleteServerImage { .. } => CommandKind::Storage,
        }
    }
    fn reject_busy(self) {
        let error =
            "iDRAC drive refresh owns the response stream; retry after it completes".to_string();
        match self {
            Self::RefreshDrives { reply, .. }
            | Self::Map { reply, .. }
            | Self::Unmap { reply, .. }
            | Self::RefreshServerStorage { reply, .. }
            | Self::DeleteServerImage { reply, .. } => {
                let _ = reply.send(Err(error));
            }
            Self::UploadServerImage { reply, .. } => {
                let _ = reply.send(Err(error));
            }
        }
    }
}

use bmc_idrac_model::media::{
    DriveRefresh, DriveRefreshEffect, DriveRefreshEvent, DriveRefreshState,
};

struct PendingDriveRefresh {
    requested_media_type: Option<MsdMediaKind>,
    model: DriveRefresh,
    origin: Instant,
    reply: oneshot::Sender<Result<(), String>>,
}

pub struct LegacyMediaSession {
    commands: mpsc::Sender<Command>,
    status: Arc<Mutex<MediaStatus>>,
    profile: AvmpGenerationProfile,
    shutdown: watch::Sender<bool>,
    finished: watch::Receiver<bool>,
}

impl LegacyMediaSession {
    pub async fn connect(
        config: IdracLegacyMediaConfig,
        on_disconnect: impl FnOnce() + Send + 'static,
    ) -> Result<Arc<Self>, String> {
        let established = establish_authenticated_transport(&config).await?;
        let extended_device_counts = established.extended_device_counts;
        let mut initial_reconnect = NativeReconnectState::initial();
        let known_media_types = advertised_media_types(config.profile, &established.drives)
            .into_iter()
            .collect();
        let removable_target_available = established
            .drives
            .iter()
            .any(|drive| drive.drive_type == IDRAC8_REMOVABLE_DRIVE_TYPE);
        let dedicated_floppy_target_available = config.profile
            == AvmpGenerationProfile::Idrac8Native
            && established
                .drives
                .iter()
                .any(|drive| drive.drive_type == IDRAC8_FLOPPY_DRIVE_TYPE);
        let mut initial_status = MediaStatus {
            connected: true,
            remote_drives: established.drives,
            known_media_types,
            removable_target_available,
            dedicated_floppy_target_available,
            flash_activation_available: config.profile == AvmpGenerationProfile::Idrac8Native
                && extended_device_counts,
            reconnect_available: established.reconnect_available,
            ..Default::default()
        };
        if let Some(response) = established.session_id_response {
            initial_reconnect.session_id = Some(response.session_id);
            initial_reconnect.reconnect_check_timeout =
                Duration::from_secs(u64::from(response.reconnect_timeout_seconds / 2));
            record_reconnect_session(&mut initial_status, response);
        }
        let status = Arc::new(Mutex::new(initial_status));
        let (commands, receiver) = mpsc::channel(8);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let (finished_tx, finished) = watch::channel(false);
        let session = Arc::new(Self {
            commands,
            status: status.clone(),
            profile: config.profile,
            shutdown,
            finished,
        });
        tokio::spawn(async move {
            let result = run_cancellable(
                established.stream,
                receiver,
                status,
                config,
                initial_reconnect,
                extended_device_counts,
                shutdown_rx,
            )
            .await;
            finished_tx.send_replace(true);
            if let Err(error) = result {
                tracing::debug!("iDRAC legacy virtual-media session ended: {error}");
                on_disconnect();
            }
        });
        Ok(session)
    }

    pub async fn map(
        &self,
        target_id: String,
        path: String,
        size: u64,
        media_type: MsdMediaKind,
    ) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::Map {
                target_id,
                path,
                size,
                media_type,
                reply,
            })
            .await
            .map_err(|_| "iDRAC virtual-media session is disconnected".to_string())?;
        response
            .await
            .map_err(|_| "iDRAC virtual-media session stopped while mapping".to_string())?
    }

    pub async fn refresh_drives(
        &self,
        requested_media_type: Option<MsdMediaKind>,
    ) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::RefreshDrives {
                requested_media_type,
                reply,
            })
            .await
            .map_err(|_| "iDRAC virtual-media session is disconnected".to_string())?;
        response.await.map_err(|_| {
            "iDRAC virtual-media session stopped while refreshing drives".to_string()
        })?
    }

    pub async fn refresh_server_storage(&self) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::RefreshServerStorage { reply })
            .await
            .map_err(|_| "iDRAC virtual-media session is disconnected".to_string())?;
        response.await.map_err(|_| {
            "iDRAC virtual-media session stopped while probing BMC storage".to_string()
        })?
    }

    pub async fn upload_server_image(
        &self,
        name: String,
        path: std::path::PathBuf,
        size: u64,
        image_type: u8,
    ) -> Result<ServerImageInfoEntry, String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::UploadServerImage {
                name,
                path,
                size,
                image_type,
                reply,
            })
            .await
            .map_err(|_| "iDRAC virtual-media session is disconnected".to_string())?;
        response.await.map_err(|_| {
            "iDRAC virtual-media session stopped while uploading BMC storage".to_string()
        })?
    }

    pub async fn delete_server_image(&self, index: u16) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::DeleteServerImage { index, reply })
            .await
            .map_err(|_| "iDRAC virtual-media session is disconnected".to_string())?;
        response.await.map_err(|_| {
            "iDRAC virtual-media session stopped while deleting BMC storage".to_string()
        })?
    }

    pub async fn unmap(&self, target_id: String) -> Result<(), String> {
        let (reply, response) = oneshot::channel();
        self.commands
            .send(Command::Unmap { target_id, reply })
            .await
            .map_err(|_| "iDRAC virtual-media session is disconnected".to_string())?;
        response
            .await
            .map_err(|_| "iDRAC virtual-media session stopped while unmapping".to_string())?
    }

    pub async fn close(&self) {
        self.shutdown.send_replace(true);
        let mut finished = self.finished.clone();
        while !*finished.borrow() {
            if finished.changed().await.is_err() {
                break;
            }
        }
    }

    pub async fn status(&self) -> MediaStatus {
        self.status.lock().await.clone()
    }

    pub const fn profile(&self) -> AvmpGenerationProfile {
        self.profile
    }
}

async fn connect_transport(
    config: &IdracLegacyMediaConfig,
    request: TransportRequest,
) -> Result<(AvmpIo, super::apcp::SessionSetup), String> {
    let address = bmc_backend_support::endpoint::authority(&config.host, config.port)?;
    let mut tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&address))
        .await
        .map_err(|_| format!("iDRAC virtual-media connection to {address} timed out"))?
        .map_err(|error| format!("iDRAC virtual-media connection to {address} failed: {error}"))?;
    tcp.set_nodelay(true)
        .map_err(|error| format!("iDRAC virtual-media TCP setup failed: {error}"))?;
    match request {
        TransportRequest::Initial => write_session_request(&mut tcp, config.profile).await?,
        TransportRequest::Reconnect { session_id } => {
            if config.profile != AvmpGenerationProfile::Idrac8Native {
                return Err(
                    "iDRAC6 virtual-media has no official native reconnect path".to_string()
                );
            }
            write_reconnect_session_request(&mut tcp, config.reconnect_method, session_id).await?;
        }
    }
    let setup = read_session_setup(&mut tcp, config.profile, config.reconnect_method).await?;
    if setup.avmp_major != 1 {
        return Err("iDRAC virtual-media returned an unsupported APCP setup".to_string());
    }
    let transport = selected_transport(config.profile, setup.capabilities)?;
    if setup.redirect_port != 0 {
        let redirect_address =
            bmc_backend_support::endpoint::authority(&config.host, setup.redirect_port)?;
        tcp = tokio::time::timeout(
            Duration::from_secs(10),
            TcpStream::connect(&redirect_address),
        )
        .await
        .map_err(|_| format!("iDRAC virtual-media redirect to {redirect_address} timed out"))?
        .map_err(|error| {
            format!("iDRAC virtual-media redirect to {redirect_address} failed: {error}")
        })?;
        tcp.set_nodelay(true)
            .map_err(|error| format!("iDRAC virtual-media redirected TCP setup failed: {error}"))?;
    }
    if transport == 0x0000_0001 && config.profile == AvmpGenerationProfile::Idrac6Java {
        return Ok((AvmpIo::new(tcp), setup));
    }
    if transport != 0x0000_0004 {
        return Err(format!(
            "iDRAC virtual-media selected unsupported transport capability {transport}"
        ));
    }
    let mut builder = NativeTlsConnector::builder();
    // This exception is narrowly scoped to the legacy OEM socket; iDRAC 6/7/8
    // commonly use appliance/self-signed certificates and old RSA suites.
    builder.danger_accept_invalid_certs(true);
    builder.danger_accept_invalid_hostnames(true);
    let connector = TlsConnector::from(
        builder
            .build()
            .map_err(|error| format!("iDRAC virtual-media TLS setup failed: {error}"))?,
    );
    let tls = tokio::time::timeout(
        Duration::from_secs(10),
        connector.connect(config.host.trim().trim_matches(['[', ']']), tcp),
    )
    .await
    .map_err(|_| "iDRAC virtual-media TLS handshake timed out".to_string())?
    .map_err(|error| format!("iDRAC virtual-media TLS handshake failed: {error}"))?;
    Ok((AvmpIo::new(tls), setup))
}

#[derive(Clone, Copy, Debug)]
enum TransportRequest {
    Initial,
    Reconnect { session_id: u32 },
}

async fn wait_for_drive_info(
    stream: &mut AvmpIo,
    profile: AvmpGenerationProfile,
) -> Result<DriveDiscovery, String> {
    tokio::time::timeout(DRIVE_DISCOVERY_TIMEOUT, async {
        let mut session_id_response = None;
        let mut retries = 0;
        loop {
            let packet = stream.next_wire_packet().await?;
            if packet.packet_type == VDISK_INFO {
                let drives = parse_vdisk_info(&packet)?;
                // Lab iDRAC6 firmware can publish a sequence of partial tables.
                // Use a short, bounded compatibility retry to obtain a useful
                // table. This is deliberately separate from the official JAR,
                // whose getVDiskInfoAll sends once and waits for one response;
                // iDRAC8 must not inherit this generation-specific workaround.
                if should_retry_idrac6_drive_info(profile, &drives, None, retries) {
                    retries += 1;
                    tokio::time::sleep(IDRAC6_DRIVE_DISCOVERY_RETRY_DELAY).await;
                    write_packet(stream, &get_vdisk_info(profile)).await?;
                    continue;
                }
                return Ok(DriveDiscovery {
                    drives,
                    session_id_response,
                });
            }
            if packet.packet_type == SESSION_ID_RESPONSE {
                if profile != AvmpGenerationProfile::Idrac8Native {
                    return Err(
                        "iDRAC6 virtual-media received unsupported native SessionIdResponse"
                            .to_string(),
                    );
                }
                session_id_response = Some(parse_session_id_response(&packet)?);
            } else if packet.packet_type == HEARTBEAT_RESPONSE {
                write_packet(stream, &heartbeat()).await?;
            } else {
                stream.defer_packet(packet)?;
            }
        }
    })
    .await
    .map_err(|_| "iDRAC virtual-media drive discovery timed out".to_string())?
}

/// Retain only the APCP session-id record while the initial VDiskInfo request
/// is pending. The actual wait loop applies the official iDRAC6 refresh policy
/// before accepting a drive table as the capability boundary.
#[cfg(test)]
fn observe_discovery_packet(
    profile: AvmpGenerationProfile,
    packet: &super::protocol::AvmpPacket,
    session_id_response: &mut Option<super::apcp::SessionIdResponse>,
) -> Result<Option<Vec<RemoteDrive>>, String> {
    if packet.packet_type == VDISK_INFO {
        return parse_vdisk_info(packet).map(Some);
    }
    if packet.packet_type == SESSION_ID_RESPONSE {
        if profile != AvmpGenerationProfile::Idrac8Native {
            return Err(
                "iDRAC6 virtual-media received unsupported native SessionIdResponse".to_string(),
            );
        }
        *session_id_response = Some(parse_session_id_response(packet)?);
    }
    Ok(None)
}

struct DriveDiscovery {
    drives: Vec<RemoteDrive>,
    session_id_response: Option<super::apcp::SessionIdResponse>,
}

/// The official iDRAC6 VM output worker updates its heartbeat timestamp only
/// after a packet has been fully written and flushed. Keep the same boundary:
/// a failed write must not postpone the next liveness decision.
async fn write_packet_and_track(
    stream: &mut AvmpIo,
    packet: &super::protocol::AvmpPacket,
    last_packet_sent_at: &mut Instant,
) -> Result<(), String> {
    tokio::time::timeout(Duration::from_secs(5), write_packet(stream, packet))
        .await
        .map_err(|_| "AVMP write timed out; remote result is unknown".to_string())??;
    *last_packet_sent_at = Instant::now();
    Ok(())
}

async fn query_server_storage(
    stream: &mut AvmpIo,
    last_packet_sent_at: &mut Instant,
) -> Result<(Vec<ServerImageInfoEntry>, ServerImageSpace), String> {
    write_packet_and_track(stream, &server_image_info_request(), last_packet_sent_at).await?;
    let info = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let packet = stream.next_wire_packet().await?;
            match packet.packet_type {
                SERVER_IMAGE_INFO_RESPONSE => return parse_server_image_info(&packet),
                HEARTBEAT_RESPONSE => {
                    write_packet(stream, &heartbeat()).await?;
                }
                _ => stream.defer_packet(packet)?,
            }
        }
    })
    .await
    .map_err(|_| "ServerImageInfo response timed out".to_string())??;

    const OFFICIAL_SERVER_IMAGE_STORE: u16 = 0x0010;
    write_packet_and_track(
        stream,
        &server_image_space_request(OFFICIAL_SERVER_IMAGE_STORE),
        last_packet_sent_at,
    )
    .await?;
    let space = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let packet = stream.next_wire_packet().await?;
            match packet.packet_type {
                SERVER_IMAGE_SPACE_RESPONSE => return parse_server_image_space(&packet),
                HEARTBEAT_RESPONSE => {
                    write_packet(stream, &heartbeat()).await?;
                }
                _ => stream.defer_packet(packet)?,
            }
        }
    })
    .await
    .map_err(|_| "ServerImageSpace response timed out".to_string())??;
    Ok((info, space))
}

async fn wait_server_image_status(
    stream: &mut AvmpIo,
    expected_type: u16,
) -> Result<super::protocol::ServerImageStatus, String> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let packet = stream.next_wire_packet().await?;
            match packet.packet_type {
                packet_type if packet_type == expected_type => {
                    return parse_server_image_status(&packet, expected_type);
                }
                HEARTBEAT_RESPONSE => write_packet(stream, &heartbeat()).await?,
                _ => stream.defer_packet(packet)?,
            }
        }
    })
    .await
    .map_err(|_| "ServerImage operation response timed out".to_string())?
}

async fn upload_server_image(
    stream: &mut AvmpIo,
    name: &str,
    path: &std::path::Path,
    size: u64,
    image_type: u8,
    last_packet_sent_at: &mut Instant,
) -> Result<ServerImageInfoEntry, String> {
    use bmc_idrac_model::storage::{Upload, UploadEffect, UploadEvent};
    const IMAGE_INDEX: u16 = 1;
    let mut model = Upload::new(size);
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("could not open staged ServerImage: {error}"))?;
    if file
        .metadata()
        .await
        .map_err(|error| format!("could not inspect staged ServerImage: {error}"))?
        .len()
        != size
    {
        return Err("staged ServerImage size changed before upload".to_string());
    }
    let (before, space) = query_server_storage(stream, last_packet_sent_at).await?;
    let mut event = UploadEvent::Catalog {
        empty: before.is_empty(),
        writable: space.total_blocks > 0 && space.free_blocks > 0,
    };
    let mut verified = None;
    let mut rejection = None;
    let result = async {
        loop {
            let transition = model.step(event);
            model = transition.state;
            event = match transition.effect {
                UploadEffect::Create { blocks } => {
                    let create = server_image_create_request(
                        IMAGE_INDEX,
                        blocks,
                        0,
                        image_type,
                        0,
                        1,
                        name.as_bytes(),
                    )?;
                    write_packet_and_track(stream, &create, last_packet_sent_at).await?;
                    let response =
                        wait_server_image_status(stream, SERVER_IMAGE_CREATE_RESPONSE).await?;
                    rejection = Some(format!(
                        "BMC rejected ServerImage create with status {} (index={})",
                        response.status, response.index
                    ));
                    UploadEvent::Created {
                        accepted: response.status == 0 && response.index == IMAGE_INDEX,
                    }
                }
                UploadEffect::Chunk {
                    offset_blocks,
                    bytes,
                } => {
                    let mut chunk = vec![0u8; bytes];
                    tokio::io::AsyncReadExt::read_exact(&mut file, &mut chunk)
                        .await
                        .map_err(|error| format!("could not read staged ServerImage: {error}"))?;
                    let packet = server_image_data_request(IMAGE_INDEX, offset_blocks, &chunk)?;
                    write_packet_and_track(stream, &packet, last_packet_sent_at).await?;
                    let response =
                        wait_server_image_status(stream, SERVER_IMAGE_DATA_RESPONSE).await?;
                    rejection = Some(format!(
                        "BMC rejected ServerImage data with status {} (index={})",
                        response.status, response.index
                    ));
                    UploadEvent::DataAcknowledged {
                        accepted: response.status == 0 && response.index == IMAGE_INDEX,
                    }
                }
                UploadEffect::Finalize => {
                    if file
                        .metadata()
                        .await
                        .map_err(|error| format!("could not inspect staged ServerImage: {error}"))?
                        .len()
                        != size
                    {
                        return Err("staged ServerImage size changed during upload".to_string());
                    }
                    let finalize = server_image_data_request(IMAGE_INDEX, 0, [])?;
                    write_packet_and_track(stream, &finalize, last_packet_sent_at).await?;
                    let response =
                        wait_server_image_status(stream, SERVER_IMAGE_DATA_RESPONSE).await?;
                    rejection = Some(format!(
                        "BMC rejected ServerImage finalize with status {} (index={})",
                        response.status, response.index
                    ));
                    UploadEvent::Finalized {
                        accepted: response.status == 0 && response.index == IMAGE_INDEX,
                    }
                }
                UploadEffect::QueryCatalog => {
                    let (images, _) = query_server_storage(stream, last_packet_sent_at).await?;
                    verified = images
                        .into_iter()
                        .find(|image| image.index == IMAGE_INDEX && image.name == name.as_bytes());
                    UploadEvent::Verified {
                        matches: verified.is_some(),
                    }
                }
                UploadEffect::Complete => {
                    return verified.take().ok_or_else(|| {
                        "ServerImage inventory confirmation was missing".to_string()
                    })
                }
                UploadEffect::Reject => {
                    return Err(rejection.take().unwrap_or_else(|| {
                        "BMC inventory is not empty or image size/storage is unsupported"
                            .to_string()
                    }))
                }
                UploadEffect::ReportUnknown | UploadEffect::IgnoreTerminal => return Err(
                    "ServerImage operation ended without confirmation; remote result is unknown"
                        .to_string(),
                ),
            };
        }
    }
    .await;
    if result.is_err() {
        let _ = model.step(UploadEvent::Fault);
    }
    result
}

async fn delete_server_image(
    stream: &mut AvmpIo,
    index: u16,
    last_packet_sent_at: &mut Instant,
) -> Result<(), String> {
    let (before, _) = query_server_storage(stream, last_packet_sent_at).await?;
    if !before.iter().any(|image| image.index == index) {
        return Err("ServerImage does not exist".to_string());
    }
    use bmc_idrac_model::operation::{Operation, OperationEffect, OperationEvent};
    let origin = tokio::time::Instant::now();
    let mut model = Operation::new(Duration::ZERO, Duration::from_secs(30));
    write_packet_and_track(
        stream,
        &server_image_delete_request(index),
        last_packet_sent_at,
    )
    .await?;
    model = model
        .step(OperationEvent::WriteFinished {
            now: origin.elapsed(),
            accepted: true,
        })
        .state;
    let result = tokio::time::timeout_at(origin + model.deadline(), async {
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let (images, _) = query_server_storage(stream, last_packet_sent_at).await?;
            let transition = model.step(OperationEvent::Observation {
                now: origin.elapsed(),
                matches: !images.iter().any(|image| image.index == index),
            });
            model = transition.state;
            match transition.effect {
                OperationEffect::Confirm => return Ok(()),
                OperationEffect::Observe | OperationEffect::Wait => {}
                OperationEffect::Reject
                | OperationEffect::ReportUnknown
                | OperationEffect::Cancel
                | OperationEffect::IgnoreTerminal => {
                    return Err(
                        "BMC did not confirm ServerImage deletion; remote result is unknown"
                            .to_string(),
                    )
                }
            }
        }
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            let _ = model.step(OperationEvent::Tick {
                now: origin.elapsed(),
            });
            Err("BMC ServerImage deletion timed out; remote result is unknown".to_string())
        }
    }
}

async fn run_cancellable(
    mut stream: AvmpIo,
    commands: mpsc::Receiver<Command>,
    status: Arc<Mutex<MediaStatus>>,
    config: IdracLegacyMediaConfig,
    mut initial_reconnect: NativeReconnectState,
    extended_device_counts: bool,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<(), String> {
    let result = tokio::select! {
        biased;
        _ = async { if !*shutdown_rx.borrow() { let _ = shutdown_rx.changed().await; } } => {
            initial_reconnect.lifecycle = initial_reconnect.lifecycle.step(bmc_idrac_model::session::SessionEvent::Stop).state;
            Ok(())
        }
        result = run_inner(
        &mut stream,
        commands,
        status.clone(),
        config.clone(),
        &mut initial_reconnect,
        extended_device_counts,
    ) => result,
    };
    if initial_reconnect.lifecycle.state() == bmc_idrac_model::session::SessionState::Closed {
        let mut last_sent = Instant::now();
        let _ = tokio::time::timeout(
            Duration::from_millis(500),
            close_transport(&mut stream, &status, config.profile, &mut last_sent),
        )
        .await;
    } else {
        initial_reconnect.lifecycle = initial_reconnect
            .lifecycle
            .step(bmc_idrac_model::session::SessionEvent::ProtocolFault {
                epoch: initial_reconnect.lifecycle.epoch(),
            })
            .state;
        debug_assert_eq!(
            initial_reconnect.lifecycle.state(),
            bmc_idrac_model::session::SessionState::Failed
        );
    }
    drop(stream);
    {
        let mut current = status.lock().await;
        current.mappings.clear();
        current.reconnecting = false;
        current.connected = false;
    }
    result
}

#[cfg(test)]
async fn run(
    mut stream: AvmpIo,
    commands: mpsc::Receiver<Command>,
    status: Arc<Mutex<MediaStatus>>,
    config: IdracLegacyMediaConfig,
    mut reconnect: NativeReconnectState,
    extended_device_counts: bool,
) -> Result<(), String> {
    run_inner(
        &mut stream,
        commands,
        status,
        config,
        &mut reconnect,
        extended_device_counts,
    )
    .await
}

async fn run_inner(
    stream: &mut AvmpIo,
    mut commands: mpsc::Receiver<Command>,
    status: Arc<Mutex<MediaStatus>>,
    config: IdracLegacyMediaConfig,
    reconnect: &mut NativeReconnectState,
    extended_device_counts: bool,
) -> Result<(), String> {
    let mut images: BTreeMap<u16, MappedImage> = BTreeMap::new();
    let mut last_packet_sent_at = Instant::now();
    let mut drive_discovery_retries = 0;
    let mut pending_drive_refresh: Option<PendingDriveRefresh> = None;
    // iDRAC6 can omit an idle sibling target while another target is
    // attached. Retain only topology advertised by this authenticated AVMP
    // session; reconnect creates a new worker and therefore a new boundary.
    let mut session_drive_topology: BTreeMap<u16, RemoteDrive> = status
        .lock()
        .await
        .remote_drives
        .iter()
        .map(|drive| (drive.id, *drive))
        .collect();
    // `ApplianceSession::mapDrive` consumes the DriveInfo[] produced by its
    // immediately preceding getVDiskInfoAll call. Keep that verified snapshot
    // across the feature layer's media-resolution work so a later unsolicited
    // partial table cannot make the same operation select a different drive.
    let mut verified_map_drives: BTreeMap<MsdMediaKind, Vec<RemoteDrive>> = BTreeMap::new();
    // The iDRAC6 worker polls every 100 ms and only sends a heartbeat after
    // the configured period has elapsed without *any* successful output.
    // Using the same poll interval also makes configured native profiles
    // maximum-idle intervals instead of blindly interleaving heartbeats with
    // active BMC read traffic.
    let mut heartbeat_timer = config
        .heartbeat_interval
        .map(|_| tokio::time::interval(Duration::from_millis(100)));
    loop {
        // Apply observations deferred by the preceding operation before a
        // later command can plan against stale drive or device status.
        let commands_admitted = stream.deferred.is_empty();
        let drive_refresh_deadline = pending_drive_refresh
            .as_ref()
            .map(|pending| pending.origin + pending.model.deadline());
        tokio::select! {
            _ = async {
                match drive_refresh_deadline {
                    Some(deadline) => {
                        tokio::time::sleep(deadline.saturating_duration_since(Instant::now())).await;
                    }
                    None => std::future::pending::<()>().await,
                }
            } => {

                drive_discovery_retries = 0;
                if let Some(mut pending) = pending_drive_refresh.take() {
                    pending.model = pending.model.step_at(DriveRefreshEvent::Timeout, pending.origin.elapsed()).state;
                    let _ = pending.reply.send(Err(
                        "iDRAC virtual-media drive discovery timed out; remote result is unknown".to_string(),
                    ));
                    return Err("AVMP drive discovery deadline ended the transport epoch".to_string());
                }
            }
            _ = async {
                match heartbeat_timer.as_mut() {
                    Some(timer) => { timer.tick().await; }
                    None => std::future::pending::<()>().await,
                }
            } => {
                let Some(interval) = config.heartbeat_interval else {
                    continue;
                };
                if last_packet_sent_at.elapsed() < interval {
                    continue;
                }
                if let Err(error) = write_packet_and_track(
                    stream,
                    &heartbeat(),
                    &mut last_packet_sent_at,
                ).await {

                    drive_discovery_retries = 0;
                    if let Some(mut pending) = pending_drive_refresh.take() {
                        pending.model = pending.model.step_at(DriveRefreshEvent::TransportLost, pending.origin.elapsed()).state;
                        let _ = pending.reply.send(Err(
                            "iDRAC virtual-media drive refresh was interrupted by transport loss"
                                .to_string(),
                        ));
                    }
                    verified_map_drives.clear();
                    recover_after_transport_loss(
                        &config,
                        stream,
                        &status,
                        &mut images,
                        reconnect,
                        &mut last_packet_sent_at,
                        error,
                    )
                    .await?;
                    session_drive_topology = status.lock().await.remote_drives.iter().map(|drive| (drive.id, *drive)).collect();
                }
            }
            command = commands.recv(), if commands_admitted => {
                if let Some(pending_command) = command.as_ref() {
                    if pending_drive_refresh.as_ref().is_some_and(|pending| !pending.model.admits(pending_command.kind())) {
                        command.expect("checked command").reject_busy();
                        continue;
                    }
                }
                match command {
                Some(Command::UploadServerImage { name, path, size, image_type, reply }) => {
                    if !status.lock().await.mappings.is_empty() {
                        let _ = reply.send(Err(
                            "BMC storage cannot be modified while virtual media is mapped".to_string(),
                        ));
                        continue;
                    }
                    let revision = stream.write_revision;
                    let result = upload_server_image(
                        stream,
                        &name,
                        &path,
                        size,
                        image_type,
                        &mut last_packet_sent_at,
                    ).await;
                    if let Ok(image) = &result {
                        status.lock().await.server_images = vec![image.clone()];
                    }
                    let uncertain = result.is_err() && stream.write_revision != revision;
                    let _ = reply.send(result);
                    if uncertain { return Err("AVMP operation failed after output; ending the transport epoch because the remote result is unknown".to_string()); }
                }
                Some(Command::DeleteServerImage { index, reply }) => {
                    if !status.lock().await.mappings.is_empty() {
                        let _ = reply.send(Err(
                            "BMC storage cannot be modified while virtual media is mapped".to_string(),
                        ));
                        continue;
                    }
                    let revision = stream.write_revision;
                    let result = delete_server_image(
                        stream,
                        index,
                        &mut last_packet_sent_at,
                    ).await;
                    if result.is_ok() {
                        status.lock().await.server_images.retain(|image| image.index != index);
                    }
                    let uncertain = result.is_err() && stream.write_revision != revision;
                    let _ = reply.send(result);
                    if uncertain { return Err("AVMP operation failed after output; ending the transport epoch because the remote result is unknown".to_string()); }
                }
                Some(Command::RefreshServerStorage { reply }) => {
                    if !status.lock().await.mappings.is_empty() {
                        let _ = reply.send(Err(
                            "BMC storage cannot be probed while virtual media is mapped".to_string(),
                        ));
                        continue;
                    }
                    let result = query_server_storage(stream, &mut last_packet_sent_at).await;
                    match result {
                        Ok((server_images, server_image_space)) => {
                            let mut current = status.lock().await;
                            current.server_images = server_images;
                            current.server_image_space = Some(server_image_space);
                            current.server_storage_probed = true;
                            let _ = reply.send(Ok(()));
                        }
                        Err(error) => {
                            status.lock().await.server_storage_probed = true;
                            let _ = reply.send(Err(error));
                            return Err("AVMP storage query failed; ending the response stream".to_string());
                        }
                    }
                }
                Some(Command::RefreshDrives { requested_media_type, reply }) => {
                    if pending_drive_refresh.is_some() {
                        let _ = reply.send(Err(
                            "iDRAC virtual-media drive refresh is already pending".to_string(),
                        ));
                        continue;
                    }
                    if let Some(media_type) = requested_media_type {
                        verified_map_drives.remove(&media_type);
                    }
                    let refresh_started = Instant::now();
                    write_packet_and_track(
                        stream,
                        &get_vdisk_info(config.profile),
                        &mut last_packet_sent_at,
                    )
                    .await?;
                    drive_discovery_retries = 0;
                    pending_drive_refresh = Some(PendingDriveRefresh {
                        requested_media_type,
                        model: DriveRefresh::default(),
                        origin: refresh_started,
                        reply,
                    });
                }
                Some(Command::Map { target_id, path, size, media_type, reply }) => {
                    let verified_drives = verified_map_drives.remove(&media_type);
                    let revision = stream.write_revision;
                    let result = map_image(
                        stream,
                        &status,
                        &mut images,
                        config.profile,
                        &target_id,
                        &path,
                        size,
                        media_type,
                        verified_drives.as_deref(),
                        &mut last_packet_sent_at,
                    )
                    .await;
                    let uncertain = result.is_err() && stream.write_revision != revision;
                    let _ = reply.send(result);
                    if uncertain { return Err("AVMP operation failed after output; ending the transport epoch because the remote result is unknown".to_string()); }
                }
                Some(Command::Unmap { target_id, reply }) => {
                    let revision = stream.write_revision;
                    let result = unmap_image(
                        stream,
                        &status,
                        &mut images,
                        &target_id,
                        &mut last_packet_sent_at,
                    ).await;
                    let uncertain = result.is_err() && stream.write_revision != revision;
                    let _ = reply.send(result);
                    if uncertain { return Err("AVMP operation failed after output; ending the transport epoch because the remote result is unknown".to_string()); }
                }
                None => {
                    if let Some(mut pending) = pending_drive_refresh.take() {
                        pending.model = pending.model.step_at(DriveRefreshEvent::Stop, pending.origin.elapsed()).state;
                        let _ = pending.reply.send(Err("iDRAC virtual-media session closed during drive refresh".to_string()));
                    }
                    close_transport(
                        stream,
                        &status,
                        config.profile,
                        &mut last_packet_sent_at,
                    ).await;
                    return Ok(());
                },
                }
            },
            packet = stream.next_packet() => {
                let packet = match packet {
                    Ok(packet) => packet,
                    Err(error) => {

                        drive_discovery_retries = 0;
                        if let Some(mut pending) = pending_drive_refresh.take() {
                            pending.model = pending.model.step_at(DriveRefreshEvent::TransportLost, pending.origin.elapsed()).state;
                            let _ = pending.reply.send(Err(
                                "iDRAC virtual-media drive refresh was interrupted by transport loss"
                                    .to_string(),
                            ));
                        }
                        verified_map_drives.clear();
                        recover_after_transport_loss(
                            &config,
                            stream,
                            &status,
                            &mut images,
                            reconnect,
                            &mut last_packet_sent_at,
                            error,
                        )
                        .await?;
                        session_drive_topology = status.lock().await.remote_drives.iter().map(|drive| (drive.id, *drive)).collect();
                        continue;
                    }
                };
                reconnect.observe_packet();
                if let Some(pending) = pending_drive_refresh.as_ref() {
                    tracing::debug!(
                        packet_type = format_args!("0x{:04x}", packet.packet_type),
                        phase = ?pending.model.state(),
                        "iDRAC virtual-media packet received during drive refresh"
                    );
                }
                if (MediaEvents { status: &status, images: &mut images, last_sent: &mut last_packet_sent_at }).dispatch(stream, &packet).await? { continue; }
                match packet.packet_type {
                    // The appliance can re-announce the remote-drive table
                    // after initial discovery and before/after a map. This is
                    // the same 0x8200 VDiskInfo record consumed during setup,
                    // not a terminal protocol error.
                    VDISK_INFO => {
                        let drives = parse_vdisk_info(&packet)?;
                        tracing::debug!(
                            profile = ?config.profile,
                            drives = ?drives,
                            "iDRAC virtual-media drive table refreshed"
                        );
                        if config.profile == AvmpGenerationProfile::Idrac6Java {
                            for drive in &drives {
                                session_drive_topology.insert(drive.id, *drive);
                            }
                        }
                        let requested_media_type = pending_drive_refresh
                            .as_ref()
                            .and_then(|pending| pending.requested_media_type);
                        let awaiting_device_count = pending_drive_refresh.as_ref().is_some_and(
                            |pending| pending.model.state() == DriveRefreshState::AwaitingDeviceCount,
                        );
                        {
                            let mut current = status.lock().await;
                            current.known_media_types.extend(advertised_media_types(
                                config.profile,
                                &drives,
                            ));
                            current.removable_target_available |= drives
                                .iter()
                                .any(|drive| drive.drive_type == IDRAC8_REMOVABLE_DRIVE_TYPE);
                            current.dedicated_floppy_target_available |= config.profile
                                == AvmpGenerationProfile::Idrac8Native
                                && drives
                                    .iter()
                                    .any(|drive| drive.drive_type == IDRAC8_FLOPPY_DRIVE_TYPE);
                            current.flash_layout_enabled |= supported_media_types(
                                config.profile,
                                &drives,
                            )
                            .contains(&MsdMediaKind::Flash);
                            current.remote_drives = drives.clone();
                        }
                        if awaiting_device_count {
                            let converged = requested_media_type.is_some_and(|requested| {
                                requested_layout_converged(config.profile, requested, &drives)
                            });
                            let convergence = pending_drive_refresh.as_mut().map(|pending| {
                                let transition = pending.model.step_at(DriveRefreshEvent::DriveTable { converged }, pending.origin.elapsed());
                                pending.model = transition.state;
                                transition.effect
                            });
                            if convergence == Some(DriveRefreshEffect::Complete) {
                                if supported_media_types(config.profile, &drives)
                                    .contains(&MsdMediaKind::Flash)
                                {
                                    let mut current = status.lock().await;
                                    current.flash_layout_enabled = true;
                                    current.known_media_types.insert(MsdMediaKind::Flash);
                                }
                                if let Some(requested) = requested_media_type {
                                    verified_map_drives.insert(requested, drives.clone());
                                }
                                drive_discovery_retries = 0;

                                let pending = pending_drive_refresh.take().expect("checked above");
                                let _ = pending.reply.send(Ok(()));
                            }
                            continue;
                        }
                        if should_retry_idrac6_drive_info(
                            config.profile,
                            &drives,
                            requested_media_type,
                            drive_discovery_retries,
                        ) {
                            drive_discovery_retries += 1;
                            tokio::time::sleep(IDRAC6_DRIVE_DISCOVERY_RETRY_DELAY).await;
                            write_packet_and_track(
                                stream,
                                &get_vdisk_info(config.profile),
                                &mut last_packet_sent_at,
                            )
                            .await?;
                            continue;
                        }
                        let device_count_attempted = pending_drive_refresh
                            .as_ref()
                            .is_some_and(|pending| pending.model.layout_attempted());
                        let flash_layout_enabled = status.lock().await.flash_layout_enabled;
                        if !device_count_attempted
                            && should_activate_idrac8_virtual_devices(
                                config.profile,
                                &drives,
                                requested_media_type,
                                flash_layout_enabled,
                            )
                        {
                            if requested_media_type == Some(MsdMediaKind::Flash)
                                && !images.is_empty()
                            {

                                if let Some(pending) = pending_drive_refresh.take() {
                                    let _ = pending.reply.send(Err(
                                        "disconnect existing iDRAC media before enabling the flash device layout"
                                            .to_string(),
                                    ));
                                }
                                continue;
                            }
                            write_packet_and_track(
                                stream,
                                &set_device_count_request(
                                    extended_device_counts,
                                    requested_device_counts(
                                        requested_media_type,
                                        extended_device_counts,
                                    ),
                                )?,
                                &mut last_packet_sent_at,
                            )
                            .await?;
                            if let Some(pending) = pending_drive_refresh.as_mut() {
                                let transition = pending.model.step_at(DriveRefreshEvent::LayoutWritten, pending.origin.elapsed());
                                pending.model = transition.state;
                            }
                            // Keep the original refresh deadline across layout activation.
                            continue;
                        }
                        drive_discovery_retries = 0;

                        if let Some(mut pending) = pending_drive_refresh.take() {
                            let verified_snapshot = pending.requested_media_type.and_then(
                                |requested| {
                                    if select_map_drive(
                                        config.profile,
                                        requested,
                                        &drives,
                                        images.keys().copied(),
                                    )
                                    .is_some()
                                    {
                                        return Some(drives.clone());
                                    }
                                    if config.profile != AvmpGenerationProfile::Idrac6Java {
                                        return None;
                                    }
                                    let topology: Vec<_> =
                                        session_drive_topology.values().copied().collect();
                                    select_map_drive(
                                        config.profile,
                                        requested,
                                        &topology,
                                        images.keys().copied(),
                                    )
                                    .map(|_| topology)
                                },
                            );
                            let result = if pending.requested_media_type.is_some()
                                && verified_snapshot.is_none()
                            {
                                Err("iDRAC accepted the virtual-device layout but did not expose the requested drive".to_string())
                            } else {
                                if let (Some(requested), Some(snapshot)) =
                                    (pending.requested_media_type, verified_snapshot)
                                {
                                    verified_map_drives.insert(requested, snapshot);
                                }
                                Ok(())
                            };
                            let transition = pending.model.step_at(DriveRefreshEvent::VerificationFinished { accepted: result.is_ok() }, pending.origin.elapsed());
                            pending.model = transition.state;
                            if transition.effect == DriveRefreshEffect::Complete {
                                let _ = pending.reply.send(result);
                            } else {
                                let _ = pending.reply.send(Err("iDRAC drive refresh expired or could not be verified".to_string()));
                                return Err("iDRAC drive refresh has no authoritative completion".to_string());
                            }
                        }
                    }
                    SET_DEVICE_COUNT_RESPONSE => {
                        let counts = parse_set_device_count_response(&packet)?;
                        let Some(pending) = pending_drive_refresh.as_mut() else {
                            continue;
                        };
                        let needs_flash = pending.requested_media_type == Some(MsdMediaKind::Flash);
                        let accepted = counts.floppy >= 1 && counts.optical >= 1 && (!needs_flash || counts.flash >= 1);
                        let transition = pending.model.step_at(DriveRefreshEvent::CountResponse { accepted }, pending.origin.elapsed());
                        pending.model = transition.state;
                        match transition.effect {
                            DriveRefreshEffect::IgnoreUnsolicited | DriveRefreshEffect::IgnoreTerminal => continue,
                            DriveRefreshEffect::Reject => {
                                let pending = pending_drive_refresh.take().expect("checked above");
                                let _ = pending.reply.send(Err("iDRAC rejected the requested virtual-device layout or exceeded its deadline".to_string()));
                                return Err("iDRAC virtual-device layout was not confirmed".to_string());
                            }
                            DriveRefreshEffect::QueryDrives => {
                                write_packet_and_track(stream, &get_vdisk_info(config.profile), &mut last_packet_sent_at).await?;
                            }
                            DriveRefreshEffect::Wait | DriveRefreshEffect::Complete => return Err("invalid drive refresh count transition".to_string()),
                        }
                        drive_discovery_retries = 0;
                    }
                    SESSION_ID_RESPONSE => {
                        if config.profile != AvmpGenerationProfile::Idrac8Native {
                            return Err("iDRAC6 virtual-media received unsupported native SessionIdResponse".to_string());
                        }
                        let response = parse_session_id_response(&packet)?;
                        reconnect.session_id = Some(response.session_id);
                        reconnect.reconnect_check_timeout =
                            Duration::from_secs(u64::from(response.reconnect_timeout_seconds / 2));
                        let mut current = status.lock().await;
                        record_reconnect_session(&mut current, response);
                    }
                    // Heartbeat, writable media, preemption and all remaining
                    // factory-recognised records require source-specific
                    // workers/policies that this readonly backend has not
                    // ported. Do not leave the MSD operation pending.
                    other => return Err(format!("unsupported iDRAC virtual-media packet 0x{other:04x}")),
                }
            }
        }
    }
}

/// Effect interpreter shared by normal polling and operation waiters. The
/// single worker still owns the stream, files, and ordering of observations.
struct MediaEvents<'a> {
    status: &'a Arc<Mutex<MediaStatus>>,
    images: &'a mut BTreeMap<u16, MappedImage>,
    last_sent: &'a mut Instant,
}
impl MediaEvents<'_> {
    async fn dispatch(
        &mut self,
        stream: &mut AvmpIo,
        packet: &super::protocol::AvmpPacket,
    ) -> Result<bool, String> {
        use bmc_idrac_model::media::{live_packet_action, LivePacketAction};
        match live_packet_action(packet.packet_type) {
            LivePacketAction::OperationOwned => return Ok(false),
            LivePacketAction::Configuration => {
                let option = DeviceConfigurationOption::try_from(packet)?;
                let mut current = self.status.lock().await;
                current
                    .configuration_options
                    .entry(option.remote_disk_id)
                    .or_default()
                    .insert(option.option_id, option.value);
                if option.option_id == 1 {
                    current
                        .remote_readonly
                        .insert(option.remote_disk_id, option.value != 0);
                }
            }
            LivePacketAction::Storage => {
                record_async_server_storage_response(self.status, packet).await?
            }
            LivePacketAction::Heartbeat => {
                write_packet_and_track(stream, &heartbeat(), self.last_sent).await?
            }
            LivePacketAction::DeviceStatus => {
                let update = DeviceStatus::try_from(packet)?;
                if apply_async_device_status(&mut *self.status.lock().await, update) {
                    self.images.remove(&update.remote_disk_id);
                }
            }
            LivePacketAction::UnsolicitedRelease => {
                // A matching active release waiter consumes its own ACK first.
                // A duplicate cannot invalidate a newer mapping of this drive.
                tracing::debug!(
                    remote_disk_id = packet.header_extension,
                    "ignored unsolicited AVMP release acknowledgement"
                );
            }
            LivePacketAction::Read => {
                let request = ReadRequest::try_from(packet)?;
                let result =
                    tokio::time::timeout(Duration::from_secs(10), self.serve_read(stream, request))
                        .await
                        .unwrap_or_else(|_| {
                            Err("AVMP read servicing exceeded its deadline".to_string())
                        });
                if let Err(error) = result {
                    let _ = write_packet_and_track(
                        stream,
                        &client_status(request.remote_disk_id, 2),
                        self.last_sent,
                    )
                    .await;
                    return Err(error);
                }
            }
        }
        Ok(true)
    }
    async fn serve_read(
        &mut self,
        stream: &mut AvmpIo,
        request: ReadRequest,
    ) -> Result<(), String> {
        let image = self
            .images
            .get_mut(&request.remote_disk_id)
            .ok_or_else(|| "iDRAC requested media blocks before an image was mapped".to_string())?;
        use bmc_idrac_model::media::{ReadEffect, ReadEvent, ReadTransfer};
        let mut model = ReadTransfer::new(
            request.offset_blocks,
            request.block_count,
            request.max_blocks_per_packet,
            image.block_size,
            image.size,
        )
        .map_err(|fault| format!("AVMP read request rejected: {fault:?}"))?;
        image.read_requests = image.read_requests.saturating_add(1);
        image.read_blocks = image
            .read_blocks
            .saturating_add(u64::from(request.block_count));
        {
            let mut current = self.status.lock().await;
            current.read_requests = current.read_requests.saturating_add(1);
            current.read_blocks = current
                .read_blocks
                .saturating_add(u64::from(request.block_count));
            if let Some(mapping) = current
                .mappings
                .values_mut()
                .find(|mapping| mapping.remote_drive_id == request.remote_disk_id)
            {
                mapping.read_requests = image.read_requests;
                mapping.read_blocks = image.read_blocks;
            }
        }
        let mut event = ReadEvent::Next;
        loop {
            let transition = model.step(event);
            model = transition.state;
            event = match transition.effect {
                ReadEffect::Chunk { offset, blocks } => {
                    let chunk = ReadRequest {
                        remote_disk_id: request.remote_disk_id,
                        offset_blocks: offset,
                        block_count: blocks,
                        max_blocks_per_packet: request.max_blocks_per_packet,
                    };
                    let output = async {
                        let response = serve_mapped_read_request(self.images, chunk).await?;
                        write_packet_and_track(stream, &response, self.last_sent).await
                    }
                    .await;
                    if let Err(error) = output {
                        let _ = model.step(ReadEvent::Written { success: false });
                        return Err(error);
                    }
                    ReadEvent::Written { success: true }
                }
                ReadEffect::Continue => ReadEvent::Next,
                ReadEffect::Complete => {
                    return write_packet_and_track(
                        stream,
                        &client_status(request.remote_disk_id, 0),
                        self.last_sent,
                    )
                    .await
                }
                ReadEffect::Reject(fault) => return Err(format!("AVMP read failed: {fault:?}")),
                ReadEffect::IgnoreTerminal => return Err("AVMP read already ended".to_string()),
            };
        }
    }
}

async fn record_async_server_storage_response(
    status: &Arc<Mutex<MediaStatus>>,
    packet: &super::protocol::AvmpPacket,
) -> Result<(), String> {
    let mut current = status.lock().await;
    match packet.packet_type {
        SERVER_IMAGE_INFO_RESPONSE => {
            current.server_images = parse_server_image_info(packet)?;
            current.server_storage_probed = true;
            Ok(())
        }
        SERVER_IMAGE_SPACE_RESPONSE => {
            current.server_image_space = Some(parse_server_image_space(packet)?);
            current.server_storage_probed = true;
            Ok(())
        }
        other => Err(format!(
            "packet 0x{other:04x} is not an asynchronous ServerImage response"
        )),
    }
}

/// Apply the source-confirmed observable effects of native `0x8102` without
/// exposing its opaque session id in a browser-visible status payload.
fn record_reconnect_session(status: &mut MediaStatus, response: super::apcp::SessionIdResponse) {
    // The native processor stores this raw value, including zero. Receipt of
    // a well-formed record is the only fact status needs to reveal.
    status.reconnect_session_response_received = true;
    status.reconnect_check_interval_seconds =
        (response.reconnect_timeout_seconds != 0).then_some(response.reconnect_timeout_seconds / 2);
}

fn apply_async_device_status(current: &mut MediaStatus, update: DeviceStatus) -> bool {
    if update.status == DEVICE_STATUS_OK {
        return false;
    }
    remove_status_mapping(current, update.remote_disk_id)
}

fn apply_remote_release(current: &mut MediaStatus, remote_disk_id: u16) -> bool {
    remove_status_mapping(current, remote_disk_id)
}

fn remove_status_mapping(current: &mut MediaStatus, remote_disk_id: u16) -> bool {
    let Some(target_id) = current.mappings.iter().find_map(|(target_id, mapping)| {
        (mapping.remote_drive_id == remote_disk_id).then_some(target_id.clone())
    }) else {
        return false;
    };
    current.mappings.remove(&target_id);
    true
}

async fn serve_mapped_read_request(
    images: &mut BTreeMap<u16, MappedImage>,
    request: ReadRequest,
) -> Result<super::protocol::AvmpPacket, String> {
    let image = images
        .get_mut(&request.remote_disk_id)
        .ok_or_else(|| "iDRAC requested media blocks before an image was mapped".to_string())?;
    serve_read_request(&mut image.file, request, image.block_size).await
}

#[allow(clippy::too_many_arguments)]
async fn map_image(
    stream: &mut AvmpIo,
    status: &Arc<Mutex<MediaStatus>>,
    images: &mut BTreeMap<u16, MappedImage>,
    profile: AvmpGenerationProfile,
    target_id: &str,
    path: &str,
    size: u64,
    media_type: MsdMediaKind,
    verified_drives: Option<&[RemoteDrive]>,
    last_packet_sent_at: &mut Instant,
) -> Result<(), String> {
    if size == 0 {
        return Err("iDRAC virtual-media cannot map an empty media image".to_string());
    }
    if status.lock().await.mappings.contains_key(target_id) {
        return Err(format!(
            "disconnect target '{target_id}' before replacing it"
        ));
    }
    let remote_id = {
        let current = status.lock().await;
        let drives = verified_drives.unwrap_or(&current.remote_drives);
        select_map_drive(profile, media_type, drives, images.keys().copied())
            .map(|drive| drive.id)
            .ok_or_else(|| {
                format!("iDRAC does not advertise an available {media_type:?} media slot")
            })?
    };
    let block_size = match media_type {
        MsdMediaKind::Optical => OPTICAL_BLOCK_SIZE,
        MsdMediaKind::Floppy | MsdMediaKind::Flash => FLOPPY_BLOCK_SIZE,
        MsdMediaKind::Disk => {
            return Err(
                "the selected iDRAC media type has no source-confirmed readonly map path"
                    .to_string(),
            )
        }
    };
    if !size.is_multiple_of(u64::from(block_size)) {
        return Err(
            "iDRAC media image size must be an exact multiple of its block size".to_string(),
        );
    }
    let capacity = u32::try_from(size / u64::from(block_size))
        .map_err(|_| "media image is too large for legacy virtual media".to_string())?;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("open local media image: {error}"))?;
    let map_request = match media_type {
        // `sendMapRequest` sets drive flags to zero for LOCAL_DRIVE_IS_CD_IMAGE,
        // request-block-limit to 0xff, and uses the fixed image block geometry.
        MsdMediaKind::Optical => vdisk_map_request(remote_id, block_size, capacity, 0, 0xff, &[])?,
        // Both official generations use VDiskRequest2 with a readonly flag
        // and geometry recovered from the image BPB. iDRAC6 exposes this in
        // ApplianceSession::cmdMapFloppyDrive; iDRAC8 does so through the
        // native AVMPSession::sendMapRequest2 path.
        MsdMediaKind::Floppy | MsdMediaKind::Flash => {
            let geometry = removable_geometry(&mut file, capacity).await?;
            vdisk_floppy_map_request(
                remote_id,
                block_size,
                geometry.capacity_blocks,
                true,
                geometry.cylinders,
                geometry.heads,
                geometry.sectors,
            )?
        }
        MsdMediaKind::Disk => unreachable!(),
    };
    images.insert(
        remote_id,
        MappedImage {
            target_id: target_id.to_string(),
            path: path.to_string(),
            size,
            media_type,
            block_size,
            read_requests: 0,
            read_blocks: 0,
            file,
        },
    );
    let origin = tokio::time::Instant::now();
    let operation =
        bmc_idrac_model::operation::Operation::new(Duration::ZERO, Duration::from_secs(10));
    let result = async {
        write_packet_and_track(stream, &map_request, last_packet_sent_at).await?;
        let operation = operation
            .step(bmc_idrac_model::operation::OperationEvent::WriteFinished {
                now: origin.elapsed(),
                accepted: true,
            })
            .state;
        let mut events = MediaEvents {
            status,
            images,
            last_sent: last_packet_sent_at,
        };
        wait_for_device_status(
            stream,
            remote_id,
            DEVICE_STATUS_OK,
            origin,
            operation,
            &mut events,
        )
        .await
    }
    .await;
    if let Err(error) = result {
        images.remove(&remote_id);
        return Err(error);
    }
    let image = images
        .get(&remote_id)
        .ok_or_else(|| "iDRAC map confirmation lost its staged media".to_string())?;
    let mut current = status.lock().await;
    current.mappings.insert(
        target_id.to_string(),
        MediaMappingStatus {
            remote_drive_id: remote_id,
            media_type,
            image_size: size,
            read_requests: image.read_requests,
            read_blocks: image.read_blocks,
        },
    );
    Ok(())
}

/// iDRAC8 advertises the conventional CD slot as type two. The real iDRAC6
/// raw-AVMP device advertises its sole, map-capable Virtual Media slot as type
/// zero with capability bit zero set; map/read/release hardware interop proves
/// that it accepts the exact ISO map request used here.
pub fn supported_media_types(
    profile: AvmpGenerationProfile,
    drives: &[RemoteDrive],
) -> Vec<MsdMediaKind> {
    [
        MsdMediaKind::Optical,
        MsdMediaKind::Floppy,
        MsdMediaKind::Flash,
    ]
    .into_iter()
    .filter(|media_type| {
        select_map_drive(profile, *media_type, drives, std::iter::empty()).is_some()
    })
    .collect()
}

pub fn advertised_media_types(
    profile: AvmpGenerationProfile,
    drives: &[RemoteDrive],
) -> Vec<MsdMediaKind> {
    let supported = supported_media_types(profile, drives);
    if supported.is_empty() && is_idrac8_disabled_dual_device_layout(profile, drives) {
        vec![MsdMediaKind::Optical, MsdMediaKind::Floppy]
    } else {
        supported
    }
}

fn is_idrac8_disabled_dual_device_layout(
    profile: AvmpGenerationProfile,
    drives: &[RemoteDrive],
) -> bool {
    profile == AvmpGenerationProfile::Idrac8Native
        && drives.len() == 2
        && drives
            .iter()
            .all(|drive| drive.drive_type == 0 && drive.status == 2 && drive.capabilities & 1 != 0)
}

/// The tested iDRAC8 reports two disabled type-zero placeholders while its
/// web UI and USB gadget support one optical and one floppy device. The
/// official native API materialises that layout with SetDeviceCount; only
/// this exact placeholder state and an explicit optical/floppy request may
/// trigger the mutation.
fn should_activate_idrac8_virtual_devices(
    profile: AvmpGenerationProfile,
    drives: &[RemoteDrive],
    requested_media_type: Option<MsdMediaKind>,
    flash_layout_enabled: bool,
) -> bool {
    if profile != AvmpGenerationProfile::Idrac8Native {
        return false;
    }
    match requested_media_type {
        Some(MsdMediaKind::Flash) => !flash_layout_enabled,
        Some(MsdMediaKind::Optical | MsdMediaKind::Floppy) => {
            is_idrac8_disabled_dual_device_layout(profile, drives)
        }
        _ => false,
    }
}

fn requested_device_counts(
    requested_media_type: Option<MsdMediaKind>,
    extended_device_counts: bool,
) -> VirtualDeviceCounts {
    VirtualDeviceCounts {
        floppy: 1,
        optical: 1,
        // AVMP 1.2 exposes independent semantic counts. Materialize all
        // source-confirmed roles on the first explicit layout activation so
        // selection order cannot require mutating an already-mounted layout.
        // The returned drive table still decides real target concurrency.
        flash: u8::from(extended_device_counts && requested_media_type.is_some()),
    }
}

fn requested_layout_converged(
    profile: AvmpGenerationProfile,
    requested: MsdMediaKind,
    drives: &[RemoteDrive],
) -> bool {
    if requested != MsdMediaKind::Flash {
        return supported_media_types(profile, drives).contains(&requested);
    }
    profile == AvmpGenerationProfile::Idrac8Native
        && select_map_drive(profile, MsdMediaKind::Flash, drives, std::iter::empty()).is_some()
}

fn should_retry_idrac6_drive_info(
    profile: AvmpGenerationProfile,
    drives: &[RemoteDrive],
    requested_media_type: Option<MsdMediaKind>,
    retries: u8,
) -> bool {
    if profile != AvmpGenerationProfile::Idrac6Java || retries >= IDRAC6_DRIVE_DISCOVERY_RETRIES {
        return false;
    }
    let media_types = supported_media_types(profile, drives);
    requested_media_type
        .map(|requested| !media_types.contains(&requested))
        .unwrap_or_else(|| media_types.is_empty())
}

fn select_map_drive(
    profile: AvmpGenerationProfile,
    media_type: MsdMediaKind,
    drives: &[RemoteDrive],
    occupied: impl IntoIterator<Item = u16>,
) -> Option<RemoteDrive> {
    let occupied: BTreeSet<_> = occupied.into_iter().collect();
    let available = |drive: &&RemoteDrive| drive.status == 0 && !occupied.contains(&drive.id);
    match media_type {
        MsdMediaKind::Optical => drives
            .iter()
            .filter(available)
            .copied()
            .find(|drive| drive.drive_type == CD_DRIVE_TYPE)
            .or_else(|| {
                (profile == AvmpGenerationProfile::Idrac6Java).then(|| {
                    drives
                        .iter()
                        .filter(available)
                        .copied()
                        .find(|drive| drive.drive_type == 0 && drive.capabilities & 1 != 0)
                })?
            }),
        MsdMediaKind::Floppy => {
            let preferred = match profile {
                AvmpGenerationProfile::Idrac6Java => IDRAC6_FLOPPY_DRIVE_TYPE,
                AvmpGenerationProfile::Idrac8Native => IDRAC8_FLOPPY_DRIVE_TYPE,
            };
            drives
                .iter()
                .filter(available)
                .copied()
                .find(|drive| drive.drive_type == preferred)
                .or_else(|| {
                    (profile == AvmpGenerationProfile::Idrac8Native).then(|| {
                        drives
                            .iter()
                            .filter(available)
                            .copied()
                            .find(|drive| drive.drive_type == IDRAC8_REMOVABLE_DRIVE_TYPE)
                    })?
                })
        }
        MsdMediaKind::Flash => drives
            .iter()
            .filter(available)
            .copied()
            .find(|drive| drive.drive_type == IDRAC8_REMOVABLE_DRIVE_TYPE),
        MsdMediaKind::Disk => None,
    }
}

pub fn dynamic_target_capacity(
    profile: AvmpGenerationProfile,
    media_type: MsdMediaKind,
    drives: &[RemoteDrive],
) -> usize {
    let mut occupied = Vec::new();
    while let Some(drive) = select_map_drive(profile, media_type, drives, occupied.iter().copied())
    {
        occupied.push(drive.id);
    }
    occupied.len()
}

/// The native receiver and sender share one reconnect state machine.  A
/// failed heartbeat write is therefore handled exactly like a failed inbound
/// read; only successful discovery and remapping make the attachment live.
async fn recover_after_transport_loss(
    config: &IdracLegacyMediaConfig,
    stream: &mut AvmpIo,
    status: &Arc<Mutex<MediaStatus>>,
    images: &mut BTreeMap<u16, MappedImage>,
    reconnect: &mut NativeReconnectState,
    last_packet_sent_at: &mut Instant,
    transport_error: String,
) -> Result<(), String> {
    use bmc_idrac_model::framing::{FramingFault, FramingState};
    use bmc_idrac_model::session::{SessionEffect, SessionEvent};
    if matches!(
        stream.framing.state(),
        FramingState::Failed(
            FramingFault::InvalidMagic
                | FramingFault::InvalidLength
                | FramingFault::ExcessData
                | FramingFault::Cancelled
        )
    ) {
        reconnect.lifecycle = reconnect
            .lifecycle
            .step(SessionEvent::ProtocolFault {
                epoch: reconnect.lifecycle.epoch(),
            })
            .state;
        return Err(transport_error);
    }
    let recoverable = reconnect.session_id.is_some()
        && config.profile == AvmpGenerationProfile::Idrac8Native
        && status.lock().await.reconnect_available;
    let transition = reconnect.lifecycle.step(SessionEvent::TransportLost {
        epoch: reconnect.lifecycle.epoch(),
        recoverable,
    });
    reconnect.lifecycle = transition.state;
    let SessionEffect::Recover { epoch } = transition.effect else {
        return Err(transport_error);
    };
    let session_id = reconnect
        .session_id
        .expect("recoverability requires a retained session");
    {
        let mut current = status.lock().await;
        current.reconnecting = true;
        current.connected = false;
    }
    let mapped_intents: Vec<_> = images
        .values()
        .map(|image| {
            (
                image.target_id.clone(),
                image.path.clone(),
                image.size,
                image.media_type,
            )
        })
        .collect();
    if !mapped_intents.is_empty() {
        // The old mapping belongs to the dead socket. Retain only the local
        // ISO intent; a fresh VDiskInfo plus VDiskMap acknowledgement is the
        // completion boundary.
        let mut current = status.lock().await;
        current.mappings.clear();
    }
    let established = match reconnect_authenticated_transport(config, reconnect, session_id).await {
        Ok(established) => established,
        Err(reconnect_error) => {
            reconnect.lifecycle = reconnect
                .lifecycle
                .step(SessionEvent::RecoveryFailed { epoch })
                .state;
            status.lock().await.reconnecting = false;
            return Err(format!(
                "{transport_error}; native reconnect failed: {reconnect_error}"
            ));
        }
    };
    *stream = established.stream;
    *last_packet_sent_at = Instant::now();
    reconnect.last_packet_at = Instant::now();
    {
        let mut current = status.lock().await;
        current.known_media_types = advertised_media_types(config.profile, &established.drives)
            .into_iter()
            .collect();
        current.flash_layout_enabled = supported_media_types(config.profile, &established.drives)
            .contains(&MsdMediaKind::Flash);
        current.removable_target_available = established
            .drives
            .iter()
            .any(|drive| drive.drive_type == IDRAC8_REMOVABLE_DRIVE_TYPE);
        current.dedicated_floppy_target_available = established
            .drives
            .iter()
            .any(|drive| drive.drive_type == IDRAC8_FLOPPY_DRIVE_TYPE);
        current.configuration_options.clear();
        current.remote_readonly.clear();
        current.server_images.clear();
        current.server_image_space = None;
        current.server_storage_probed = false;
        current.remote_drives = established.drives;
        current.reconnect_available = established.reconnect_available;
        if let Some(response) = established.session_id_response {
            reconnect.session_id = Some(response.session_id);
            reconnect.reconnect_check_timeout =
                Duration::from_secs(u64::from(response.reconnect_timeout_seconds / 2));
            record_reconnect_session(&mut current, response);
        }
    }
    images.clear();
    for (target_id, path, size, media_type) in mapped_intents {
        if let Err(remap_error) = map_image(
            stream,
            status,
            images,
            config.profile,
            &target_id,
            &path,
            size,
            media_type,
            None,
            last_packet_sent_at,
        )
        .await
        {
            reconnect.lifecycle = reconnect
                .lifecycle
                .step(SessionEvent::RecoveryFailed { epoch })
                .state;
            status.lock().await.reconnecting = false;
            return Err(format!(
                "{transport_error}; native reconnect remap failed: {remap_error}"
            ));
        }
    }
    let transition = reconnect
        .lifecycle
        .step(SessionEvent::Established { epoch });
    reconnect.lifecycle = transition.state;
    if transition.effect != SessionEffect::Online {
        return Err("iDRAC recovered transport belongs to an invalid session epoch".to_string());
    }
    {
        let mut current = status.lock().await;
        current.reconnecting = false;
        current.connected = true;
    }
    Ok(())
}

/// Result of the complete source-confirmed setup sequence.  Reconnect must
/// call this same routine and may not announce recovery until it has also
/// restored the mapped drive after the returned VDiskInfo snapshot.
struct EstablishedTransport {
    stream: AvmpIo,
    drives: Vec<RemoteDrive>,
    reconnect_available: bool,
    session_id_response: Option<super::apcp::SessionIdResponse>,
    extended_device_counts: bool,
}

async fn establish_authenticated_transport(
    config: &IdracLegacyMediaConfig,
) -> Result<EstablishedTransport, String> {
    establish_authenticated_transport_with_request(config, TransportRequest::Initial).await
}

async fn establish_authenticated_transport_with_request(
    config: &IdracLegacyMediaConfig,
    request: TransportRequest,
) -> Result<EstablishedTransport, String> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let (mut stream, setup) = connect_transport(config, request).await?;
        write_packet(
            &mut stream,
            &login_for_version(
                &config.username,
                &config.password,
                setup.avmp_major,
                setup.avmp_minor,
            )?,
        )
        .await?;
        let login = parse_login_response(&stream.next_wire_packet().await?, config.profile)?;
        if login.status == 8 {
            return Err("iDRAC virtual-media session is already in use".to_string());
        }
        if login.status != 0 {
            return Err(format!(
                "iDRAC virtual-media login was denied (status={})",
                login.status
            ));
        }
        write_packet(&mut stream, &get_vdisk_info(config.profile)).await?;
        let discovery = wait_for_drive_info(&mut stream, config.profile).await?;
        Ok(EstablishedTransport {
            stream,
            drives: discovery.drives,
            reconnect_available: setup.reconnect_available,
            session_id_response: discovery.session_id_response,
            // Exact native `isAVMPVersion12()` predicate.
            extended_device_counts: (setup.avmp_major == 1 && setup.avmp_minor > 1)
                || setup.avmp_major > 2,
        })
    })
    .await
    .map_err(|_| "iDRAC virtual-media setup exceeded its absolute deadline".to_string())?
}

/// Native `ReconnectThread` does not reconnect on the first deadline edge:
/// it waits half the check interval and requires elapsed time to be strictly
/// greater than the interval. Once due, failed socket attempts are separated
/// by five seconds until the launcher's 180-second reconnect-attempt timeout.
async fn reconnect_authenticated_transport(
    config: &IdracLegacyMediaConfig,
    state: &NativeReconnectState,
    session_id: u32,
) -> Result<EstablishedTransport, String> {
    use bmc_idrac_model::media::{Reconnect, ReconnectEffect, ReconnectEvent};
    let origin = state.last_packet_at;
    let mut model = Reconnect::new(Duration::ZERO, state.reconnect_check_timeout);
    let mut event = ReconnectEvent::Tick {
        now: origin.elapsed(),
    };
    let mut result = None;
    loop {
        let transition = model.step(event);
        model = transition.state;
        event = match transition.effect {
            ReconnectEffect::Sleep(delay) => {
                tokio::time::sleep(delay).await;
                ReconnectEvent::Tick {
                    now: origin.elapsed(),
                }
            }
            ReconnectEffect::Connect { remaining } => {
                result = Some(
                    tokio::time::timeout(
                        remaining,
                        establish_authenticated_transport_with_request(
                            config,
                            TransportRequest::Reconnect { session_id },
                        ),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        Err("iDRAC native reconnect attempt timed out".to_string())
                    }),
                );
                ReconnectEvent::AttemptFinished {
                    now: origin.elapsed(),
                    success: result.as_ref().is_some_and(Result::is_ok),
                }
            }
            ReconnectEffect::Complete => return result.take().expect("attempt produced a result"),
            ReconnectEffect::Fail | ReconnectEffect::IgnoreTerminal => {
                return Err(result
                    .take()
                    .and_then(Result::err)
                    .unwrap_or_else(|| "iDRAC native reconnect deadline exceeded".to_string()))
            }
        };
    }
}

async fn close_transport(
    stream: &mut AvmpIo,
    status: &Arc<Mutex<MediaStatus>>,
    profile: AvmpGenerationProfile,
    last_packet_sent_at: &mut Instant,
) {
    let remote_drive_ids: Vec<_> = status
        .lock()
        .await
        .mappings
        .values()
        .map(|mapping| mapping.remote_drive_id)
        .collect();
    for remote_drive_id in remote_drive_ids {
        let _ =
            write_packet_and_track(stream, &vdisk_release(remote_drive_id), last_packet_sent_at)
                .await;
    }
    if requires_native_session_close(profile) {
        let _ = write_packet_and_track(stream, &native_session_close(), last_packet_sent_at).await;
    }
}

fn requires_native_session_close(profile: AvmpGenerationProfile) -> bool {
    profile == AvmpGenerationProfile::Idrac8Native
}

async fn unmap_image(
    stream: &mut AvmpIo,
    status: &Arc<Mutex<MediaStatus>>,
    images: &mut BTreeMap<u16, MappedImage>,
    target_id: &str,
    last_packet_sent_at: &mut Instant,
) -> Result<(), String> {
    let remote_id = status
        .lock()
        .await
        .mappings
        .get(target_id)
        .map(|mapping| mapping.remote_drive_id)
        .ok_or_else(|| format!("iDRAC target '{target_id}' is not mapped"))?;
    use bmc_idrac_model::operation::{Operation, OperationEffect, OperationEvent};
    let origin = tokio::time::Instant::now();
    let mut operation = Operation::new(Duration::ZERO, Duration::from_secs(10));
    write_packet_and_track(stream, &vdisk_release(remote_id), last_packet_sent_at).await?;
    operation = operation
        .step(OperationEvent::WriteFinished {
            now: origin.elapsed(),
            accepted: true,
        })
        .state;
    let mut events = MediaEvents {
        status,
        images,
        last_sent: last_packet_sent_at,
    };
    let result = tokio::time::timeout_at(origin + operation.deadline(), async {
        loop {
            let packet = stream.next_wire_packet().await?;
            if packet.packet_type == VDISK_RELEASE_RESPONSE && packet.header_extension == remote_id {
                let transition = operation.step(OperationEvent::Observation { now: origin.elapsed(), matches: true });
                operation = transition.state;
                return if transition.effect == OperationEffect::Confirm { Ok(()) } else { Err("iDRAC release acknowledgement arrived after its deadline; remote result is unknown".to_string()) };
            }
            if !events.dispatch(stream, &packet).await? { stream.defer_packet(packet)?; }
        }
    }).await;
    match result {
        Ok(result) => result?,
        Err(_) => {
            let _ = operation.step(OperationEvent::Tick {
                now: origin.elapsed(),
            });
            return Err(
                "iDRAC release confirmation timed out; remote result is unknown".to_string(),
            );
        }
    }
    apply_remote_release(&mut *status.lock().await, remote_id);
    images.remove(&remote_id);
    Ok(())
}

async fn wait_for_device_status(
    stream: &mut AvmpIo,
    remote_id: u16,
    expected: u32,
    origin: tokio::time::Instant,
    mut operation: bmc_idrac_model::operation::Operation,
    events: &mut MediaEvents<'_>,
) -> Result<(), String> {
    use bmc_idrac_model::operation::{OperationEffect, OperationEvent};
    let result = tokio::time::timeout_at(origin + operation.deadline(), async {
        loop {
            let packet = stream.next_wire_packet().await?;
            if packet.packet_type == DEVICE_STATUS {
                let update = DeviceStatus::try_from(&packet)?;
                if update.remote_disk_id != remote_id { events.dispatch(stream, &packet).await?; continue; }
                let transition = operation.step(if update.status == expected {
                    OperationEvent::Observation { now: origin.elapsed(), matches: true }
                } else { OperationEvent::RemoteRejected });
                operation = transition.state;
                return if transition.effect == OperationEffect::Confirm { Ok(()) }
                else { Err(format!("iDRAC virtual drive did not confirm the request (status={}); remote result may be unknown", update.status)) };
            }
            if !events.dispatch(stream, &packet).await? { stream.defer_packet(packet)?; }
        }
    }).await;
    match result {
        Ok(result) => result,
        Err(_) => {
            let _ = operation.step(OperationEvent::Tick {
                now: origin.elapsed(),
            });
            Err(
                "iDRAC virtual-media map confirmation timed out; remote result is unknown"
                    .to_string(),
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::protocol::{read_packet, AvmpPacket};
    use super::*;

    #[tokio::test]
    async fn read_dispatch_chunks_to_wire_capacity_and_rejects_empty_or_out_of_image_requests() {
        use super::super::protocol::{CLIENT_STATUS, READ_REQUEST, READ_RESPONSE};
        use std::io::Write;
        let data = vec![0x5a; 513 * 2048];
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&data).unwrap();
        for blocks in [513u32, 0, 514] {
            let (client, mut peer) = tokio::io::duplex(8192);
            let mut stream = AvmpIo::new(client);
            let status = Arc::new(Mutex::new(MediaStatus::default()));
            let mut images = BTreeMap::from([(
                2,
                MappedImage {
                    target_id: "optical".into(),
                    path: file.path().to_string_lossy().into_owned(),
                    size: data.len() as u64,
                    media_type: MsdMediaKind::Optical,
                    block_size: 2048,
                    read_requests: 0,
                    read_blocks: 0,
                    file: tokio::fs::File::open(file.path()).await.unwrap(),
                },
            )]);
            let mut last_sent = Instant::now();
            let mut events = MediaEvents {
                status: &status,
                images: &mut images,
                last_sent: &mut last_sent,
            };
            let mut request = AvmpPacket::new(
                READ_REQUEST,
                [
                    0u32.to_be_bytes(),
                    blocks.to_be_bytes(),
                    u32::MAX.to_be_bytes(),
                ]
                .concat(),
            )
            .unwrap();
            request.header_extension = 2;
            let device = async {
                if blocks == 513 {
                    for expected in [511, 2] {
                        let packet = read_packet(&mut peer).await.unwrap();
                        assert_eq!(packet.packet_type, READ_RESPONSE);
                        assert_eq!(
                            u32::from_be_bytes(packet.payload[4..8].try_into().unwrap()),
                            expected
                        );
                        assert_eq!(packet.payload.len(), 8 + expected as usize * 2048);
                        assert!(packet.payload[8..].iter().all(|byte| *byte == 0x5a));
                    }
                }
                let status = read_packet(&mut peer).await.unwrap();
                assert_eq!(
                    status.packet_type, CLIENT_STATUS,
                    "invalid reads must not emit image data"
                );
                assert_eq!(
                    status.payload,
                    if blocks == 513 { 0u32 } else { 2u32 }.to_be_bytes()
                );
            };
            let scenario = async { tokio::join!(events.dispatch(&mut stream, &request), device) };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(5), scenario)
                .await
                .unwrap();
            assert_eq!(result.is_ok(), blocks == 513);
            assert_eq!(status.lock().await.read_requests, u64::from(blocks == 513));
        }
    }

    #[tokio::test]
    async fn media_reads_progress_before_map_and_release_acknowledgements() {
        use super::super::protocol::{CLIENT_STATUS, READ_REQUEST, READ_RESPONSE};
        use std::io::Write;
        let bytes = vec![0x5a; 2048];
        let mut file = tempfile::NamedTempFile::new().unwrap();
        file.write_all(&bytes).unwrap();
        let (client, mut peer) = tokio::io::duplex(8192);
        let mut stream = AvmpIo::new(client);
        let status = Arc::new(Mutex::new(MediaStatus {
            remote_drives: vec![RemoteDrive {
                id: 2,
                drive_type: 2,
                status: 0,
                capabilities: 1,
            }],
            ..MediaStatus::default()
        }));
        let mut images = BTreeMap::new();
        let mut last_sent = Instant::now();
        let observed = status.clone();
        let device = async {
            let map = read_packet(&mut peer).await.unwrap();
            assert_eq!(map.packet_type, super::super::protocol::VDISK_MAP_REQUEST);
            let mut read = AvmpPacket::new(
                READ_REQUEST,
                [0u32.to_be_bytes(), 1u32.to_be_bytes(), 1u32.to_be_bytes()].concat(),
            )
            .unwrap();
            read.header_extension = 2;
            write_packet(&mut peer, &read).await.unwrap();
            // The BMC deliberately waits for the media response before its ACK.
            let response = read_packet(&mut peer).await.unwrap();
            assert_eq!(response.packet_type, READ_RESPONSE);
            assert_eq!(&response.payload[8..], bytes);
            assert_eq!(
                read_packet(&mut peer).await.unwrap().packet_type,
                CLIENT_STATUS
            );
            assert!(observed.lock().await.mappings.is_empty());
            let mut ack = AvmpPacket::new(DEVICE_STATUS, 0u32.to_be_bytes()).unwrap();
            ack.header_extension = 2;
            write_packet(&mut peer, &ack).await.unwrap();
        };
        let path = file.path().to_string_lossy().into_owned();
        let scenario = async {
            tokio::join!(
                map_image(
                    &mut stream,
                    &status,
                    &mut images,
                    AvmpGenerationProfile::Idrac8Native,
                    "optical",
                    &path,
                    2048,
                    MsdMediaKind::Optical,
                    None,
                    &mut last_sent
                ),
                device
            )
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), scenario)
            .await
            .unwrap();
        result.unwrap();
        assert_eq!(status.lock().await.mappings["optical"].read_requests, 1);
        let observed = status.clone();
        let device = async {
            assert_eq!(
                read_packet(&mut peer).await.unwrap().packet_type,
                super::super::protocol::VDISK_RELEASE
            );
            let mut read = AvmpPacket::new(
                READ_REQUEST,
                [0u32.to_be_bytes(), 1u32.to_be_bytes(), 1u32.to_be_bytes()].concat(),
            )
            .unwrap();
            read.header_extension = 2;
            write_packet(&mut peer, &read).await.unwrap();
            assert_eq!(&read_packet(&mut peer).await.unwrap().payload[8..], bytes);
            assert_eq!(
                read_packet(&mut peer).await.unwrap().packet_type,
                CLIENT_STATUS
            );
            assert!(observed.lock().await.mappings.contains_key("optical"));
            let mut ack = AvmpPacket::new(VDISK_RELEASE_RESPONSE, []).unwrap();
            ack.header_extension = 2;
            write_packet(&mut peer, &ack).await.unwrap();
        };
        let scenario = async {
            tokio::join!(
                unmap_image(&mut stream, &status, &mut images, "optical", &mut last_sent),
                device
            )
        };
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), scenario)
            .await
            .unwrap();
        result.unwrap();
        assert!(status.lock().await.mappings.is_empty());
        assert!(images.is_empty());
    }

    #[tokio::test]
    async fn map_waiter_preserves_another_drives_status_and_unsolicited_topology() {
        use bmc_idrac_model::operation::{Operation, OperationEvent};
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut stream = AvmpIo::new(client);
        let status = Arc::new(Mutex::new(MediaStatus::default()));
        status.lock().await.mappings.insert(
            "other".to_string(),
            MediaMappingStatus {
                remote_drive_id: 8,
                media_type: MsdMediaKind::Optical,
                image_size: 2048,
                read_requests: 0,
                read_blocks: 0,
            },
        );
        let mut images = BTreeMap::new();
        let mut last_sent = Instant::now();
        let mut events = MediaEvents {
            status: &status,
            images: &mut images,
            last_sent: &mut last_sent,
        };
        let device = async {
            let mut other = AvmpPacket::new(DEVICE_STATUS, 3u32.to_be_bytes()).unwrap();
            other.header_extension = 8;
            write_packet(&mut peer, &other).await.unwrap();
            write_packet(&mut peer, &AvmpPacket::new(VDISK_INFO, []).unwrap())
                .await
                .unwrap();
            other.header_extension = 7;
            other.payload = 0u32.to_be_bytes().to_vec();
            write_packet(&mut peer, &other).await.unwrap();
        };
        let origin = tokio::time::Instant::now();
        let model = Operation::new(Duration::ZERO, Duration::from_secs(10))
            .step(OperationEvent::WriteFinished {
                now: Duration::ZERO,
                accepted: true,
            })
            .state;
        let (result, ()) = tokio::join!(
            wait_for_device_status(&mut stream, 7, 0, origin, model, &mut events),
            device
        );
        result.unwrap();
        assert!(status.lock().await.mappings.is_empty());
        assert_eq!(stream.next_packet().await.unwrap().packet_type, VDISK_INFO);
    }

    #[tokio::test]
    async fn cancelled_partial_header_and_payload_reads_preserve_the_next_record() {
        use tokio::io::AsyncWriteExt;
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut stream = AvmpIo::new(client);
        let packet = AvmpPacket::new(VDISK_INFO, [0, 1, 2, 3, 4, 5]).unwrap();
        let bytes = packet.encode();
        peer.write_all(&bytes[..5]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(5), stream.next_packet())
                .await
                .is_err()
        );
        peer.write_all(&bytes[5..14]).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(5), stream.next_packet())
                .await
                .is_err()
        );
        peer.write_all(&bytes[14..]).await.unwrap();
        peer.write_all(&heartbeat().encode()).await.unwrap();
        let decoded = stream.next_packet().await.unwrap();
        assert_eq!(decoded.packet_type, packet.packet_type);
        assert_eq!(decoded.payload, packet.payload);
        assert_eq!(
            stream.next_packet().await.unwrap().packet_type,
            heartbeat().packet_type
        );
    }

    #[tokio::test]
    async fn partial_record_deadline_survives_read_cancellation() {
        use tokio::io::AsyncWriteExt;
        let (client, mut peer) = tokio::io::duplex(128);
        let mut stream = AvmpIo::new(client);
        peer.write_all(b"AV").await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(5), stream.next_packet())
                .await
                .is_err()
        );
        stream.origin -= Duration::from_secs(11);
        let error = tokio::time::timeout(Duration::from_millis(100), stream.next_packet())
            .await
            .unwrap()
            .unwrap_err();
        assert!(error.contains("TimedOut"));
    }

    #[tokio::test]
    async fn release_keeps_mapping_until_the_matching_remote_acknowledgement() {
        let (client, mut peer) = tokio::io::duplex(1024);
        let mut stream = AvmpIo::new(client);
        let status = Arc::new(Mutex::new(MediaStatus::default()));
        status.lock().await.mappings.insert(
            "optical".to_string(),
            MediaMappingStatus {
                remote_drive_id: 7,
                media_type: MsdMediaKind::Optical,
                image_size: 2048,
                read_requests: 0,
                read_blocks: 0,
            },
        );
        let mut images = BTreeMap::new();
        let mut last_sent = Instant::now();
        let observed = status.clone();
        let device = async {
            assert_eq!(
                read_packet(&mut peer).await.unwrap().packet_type,
                super::super::protocol::VDISK_RELEASE
            );
            assert!(observed.lock().await.mappings.contains_key("optical"));
            let mut unrelated = AvmpPacket::new(VDISK_RELEASE_RESPONSE, []).unwrap();
            unrelated.header_extension = 8;
            write_packet(&mut peer, &unrelated).await.unwrap();
            assert!(observed.lock().await.mappings.contains_key("optical"));
            unrelated.header_extension = 7;
            write_packet(&mut peer, &unrelated).await.unwrap();
        };
        let (result, ()) = tokio::join!(
            unmap_image(&mut stream, &status, &mut images, "optical", &mut last_sent),
            device
        );
        result.unwrap();
        assert!(status.lock().await.mappings.is_empty());
    }

    #[tokio::test]
    async fn shutdown_preempts_a_stalled_command_and_clears_observed_mappings() {
        let (client, mut peer) = tokio::io::duplex(1024);
        let status = Arc::new(Mutex::new(MediaStatus::default()));
        let (commands, receiver) = mpsc::channel(8);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let worker = tokio::spawn(run_cancellable(
            AvmpIo::new(client),
            receiver,
            status.clone(),
            media_config(8, None),
            NativeReconnectState::initial(),
            true,
            shutdown_rx,
        ));
        let (reply, result) = oneshot::channel();
        commands
            .send(Command::RefreshServerStorage { reply })
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut peer).await.unwrap().packet_type,
            super::super::protocol::SERVER_IMAGE_INFO_REQUEST
        );
        // The command is now blocked waiting for a response. Close uses a
        // separate channel and must not queue behind this request.
        shutdown.send_replace(true);
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(result.await.is_err());
        assert!(status.lock().await.mappings.is_empty());
        assert!(!status.lock().await.reconnecting);
    }

    fn media_config(generation: u8, heartbeat_interval_s: Option<u32>) -> IdracLegacyMediaConfig {
        IdracLegacyMediaConfig::new(
            "idrac.example.test".to_string(),
            5900,
            "user".to_string(),
            "password".to_string(),
            generation,
            heartbeat_interval_s,
            None,
        )
        .unwrap()
    }

    fn server_image_info_response(entries: &[(&str, u32)]) -> AvmpPacket {
        let mut payload = vec![0u8; entries.len() * 268];
        for (position, (name, size_blocks)) in entries.iter().enumerate() {
            let entry = &mut payload[position * 268..(position + 1) * 268];
            entry[0..2].copy_from_slice(&((position + 1) as u16).to_be_bytes());
            entry[4..8].copy_from_slice(&size_blocks.to_be_bytes());
            entry[8..12].copy_from_slice(&0u32.to_be_bytes());
            entry[12] = 1;
            entry[14] = 1;
            entry[15..15 + name.len()].copy_from_slice(name.as_bytes());
        }
        AvmpPacket {
            magic: super::super::protocol::AVMP_MAGIC,
            packet_type: SERVER_IMAGE_INFO_RESPONSE,
            header_extension: entries.len() as u16,
            payload,
        }
    }

    fn server_image_space_response(total: u32, free: u32) -> AvmpPacket {
        AvmpPacket {
            magic: super::super::protocol::AVMP_MAGIC,
            packet_type: SERVER_IMAGE_SPACE_RESPONSE,
            header_extension: 0x10,
            payload: [total.to_be_bytes(), free.to_be_bytes()].concat(),
        }
    }

    #[tokio::test]
    async fn delayed_server_image_responses_update_state_without_ending_media_session() {
        let status = Arc::new(Mutex::new(MediaStatus::default()));

        record_async_server_storage_response(
            &status,
            &server_image_info_response(&[("late.iso", 42)]),
        )
        .await
        .unwrap();
        record_async_server_storage_response(&status, &server_image_space_response(4096, 2048))
            .await
            .unwrap();

        let current = status.lock().await;
        assert!(current.server_storage_probed);
        assert_eq!(current.server_images.len(), 1);
        assert_eq!(current.server_images[0].name, b"late.iso");
        assert_eq!(current.server_image_space.unwrap().total_blocks, 4096);
        assert_eq!(current.server_image_space.unwrap().free_blocks, 2048);
    }

    #[tokio::test]
    async fn server_image_upload_follows_create_chunks_finalize_and_inventory_refresh() {
        let (client, mut server) = tokio::io::duplex(128 * 1024);
        let server_task = tokio::spawn(async move {
            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0500);
            write_packet(&mut server, &server_image_info_response(&[]))
                .await
                .unwrap();
            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0530);
            write_packet(&mut server, &server_image_space_response(4096, 4096))
                .await
                .unwrap();

            let create = read_packet(&mut server).await.unwrap();
            assert_eq!(create.packet_type, 0x0540);
            assert_eq!(create.header_extension, 1);
            write_packet(
                &mut server,
                &AvmpPacket {
                    magic: super::super::protocol::AVMP_MAGIC,
                    packet_type: SERVER_IMAGE_CREATE_RESPONSE,
                    header_extension: 1,
                    payload: vec![0],
                },
            )
            .await
            .unwrap();

            for expected_offset in [0u32, 64, 0] {
                let data = read_packet(&mut server).await.unwrap();
                assert_eq!(data.packet_type, 0x0570);
                assert_eq!(
                    u32::from_be_bytes(data.payload[..4].try_into().unwrap()),
                    expected_offset
                );
                write_packet(
                    &mut server,
                    &AvmpPacket {
                        magic: super::super::protocol::AVMP_MAGIC,
                        packet_type: SERVER_IMAGE_DATA_RESPONSE,
                        header_extension: 1,
                        payload: vec![0],
                    },
                )
                .await
                .unwrap();
            }

            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0500);
            write_packet(
                &mut server,
                &server_image_info_response(&[("uploaded.iso", 65)]),
            )
            .await
            .unwrap();
            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0530);
            write_packet(&mut server, &server_image_space_response(4096, 4031))
                .await
                .unwrap();
        });

        let mut stream: AvmpIo = AvmpIo::new(client);
        let upload = tempfile::NamedTempFile::new().expect("staged upload");
        std::fs::write(upload.path(), vec![0x5a; 32 * 1024 + 1]).expect("write staged upload");
        let image = upload_server_image(
            &mut stream,
            "uploaded.iso",
            upload.path(),
            32 * 1024 + 1,
            1,
            &mut Instant::now(),
        )
        .await
        .unwrap();
        assert_eq!(image.index, 1);
        assert_eq!(image.name, b"uploaded.iso");
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn server_image_upload_stops_on_nonzero_create_status() {
        let (client, mut server) = tokio::io::duplex(32 * 1024);
        let server_task = tokio::spawn(async move {
            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0500);
            write_packet(&mut server, &server_image_info_response(&[]))
                .await
                .unwrap();
            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0530);
            write_packet(&mut server, &server_image_space_response(4096, 4096))
                .await
                .unwrap();
            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0540);
            write_packet(
                &mut server,
                &AvmpPacket {
                    magic: super::super::protocol::AVMP_MAGIC,
                    packet_type: SERVER_IMAGE_CREATE_RESPONSE,
                    header_extension: 0,
                    payload: vec![1],
                },
            )
            .await
            .unwrap();
        });

        let mut stream: AvmpIo = AvmpIo::new(client);
        let upload = tempfile::NamedTempFile::new().expect("staged upload");
        std::fs::write(upload.path(), [0x5a; 512]).expect("write staged upload");
        let error = upload_server_image(
            &mut stream,
            "rejected.iso",
            upload.path(),
            512,
            1,
            &mut Instant::now(),
        )
        .await
        .unwrap_err();
        assert!(error.contains("status 1"));
        server_task.await.unwrap();
    }

    #[tokio::test]
    async fn server_image_delete_requires_inventory_to_remove_the_index() {
        let (client, mut server) = tokio::io::duplex(64 * 1024);
        let server_task = tokio::spawn(async move {
            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0500);
            write_packet(&mut server, &server_image_info_response(&[("old.iso", 4)]))
                .await
                .unwrap();
            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0530);
            write_packet(&mut server, &server_image_space_response(4096, 4092))
                .await
                .unwrap();

            let delete = read_packet(&mut server).await.unwrap();
            assert_eq!(delete.packet_type, 0x0550);
            assert_eq!(delete.header_extension, 1);

            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0500);
            write_packet(&mut server, &server_image_info_response(&[]))
                .await
                .unwrap();
            assert_eq!(read_packet(&mut server).await.unwrap().packet_type, 0x0530);
            write_packet(&mut server, &server_image_space_response(4096, 4096))
                .await
                .unwrap();
        });

        let mut stream: AvmpIo = AvmpIo::new(client);
        delete_server_image(&mut stream, 1, &mut Instant::now())
            .await
            .unwrap();
        server_task.await.unwrap();
    }

    #[test]
    fn idrac6_uses_the_official_fixed_idle_heartbeat_without_config() {
        let config = media_config(6, None);
        assert_eq!(config.heartbeat_interval, Some(Duration::from_secs(10)));
    }

    #[test]
    fn idrac6_rejects_non_official_heartbeat_intervals() {
        assert!(IdracLegacyMediaConfig::new(
            "idrac.example.test".into(),
            5900,
            "user".into(),
            "password".into(),
            6,
            Some(9),
            None,
        )
        .unwrap_err()
        .contains("fixed 10-second"));
    }

    #[test]
    fn idrac6_drive_discovery_retry_budget_is_generation_scoped() {
        assert_eq!(
            IDRAC6_DRIVE_DISCOVERY_RETRY_DELAY,
            Duration::from_millis(40)
        );
        assert_eq!(IDRAC6_DRIVE_DISCOVERY_RETRIES, 20);
        assert!(should_retry_idrac6_drive_info(
            AvmpGenerationProfile::Idrac6Java,
            &[],
            None,
            0,
        ));
        assert!(!should_retry_idrac6_drive_info(
            AvmpGenerationProfile::Idrac6Java,
            &[],
            None,
            IDRAC6_DRIVE_DISCOVERY_RETRIES,
        ));
        assert!(!should_retry_idrac6_drive_info(
            AvmpGenerationProfile::Idrac8Native,
            &[],
            None,
            0,
        ));
        let optical_only = [RemoteDrive {
            id: 0,
            drive_type: CD_DRIVE_TYPE,
            status: 0,
            capabilities: 1,
        }];
        assert!(should_retry_idrac6_drive_info(
            AvmpGenerationProfile::Idrac6Java,
            &optical_only,
            Some(MsdMediaKind::Floppy),
            0,
        ));
        assert!(!should_retry_idrac6_drive_info(
            AvmpGenerationProfile::Idrac6Java,
            &optical_only,
            Some(MsdMediaKind::Optical),
            0,
        ));
    }

    #[test]
    fn remote_release_clears_only_its_matching_mapping() {
        let mut status = MediaStatus::default();
        for (target_id, remote_drive_id) in [("optical-0", 7), ("optical-1", 8)] {
            status.mappings.insert(
                target_id.to_string(),
                MediaMappingStatus {
                    remote_drive_id,
                    media_type: MsdMediaKind::Optical,
                    image_size: 1024,
                    read_requests: 0,
                    read_blocks: 0,
                },
            );
        }
        assert!(!apply_remote_release(&mut status, 9));
        assert_eq!(status.mappings.len(), 2);
        assert!(apply_remote_release(&mut status, 7));
        assert_eq!(status.mappings.len(), 1);
        assert!(status.mappings.contains_key("optical-1"));
    }

    #[test]
    fn close_record_is_native_idrac8_only() {
        assert!(requires_native_session_close(
            AvmpGenerationProfile::Idrac8Native
        ));
        assert!(!requires_native_session_close(
            AvmpGenerationProfile::Idrac6Java
        ));
    }

    #[test]
    fn session_id_response_updates_only_safe_reconnect_status() {
        let mut status = MediaStatus::default();
        record_reconnect_session(
            &mut status,
            super::super::apcp::SessionIdResponse {
                session_id: 0x0102_0304,
                reconnect_timeout_seconds: 30,
            },
        );
        assert!(status.reconnect_session_response_received);
        assert_eq!(status.reconnect_check_interval_seconds, Some(15));
    }

    #[test]
    fn asynchronous_device_status_ignores_late_release_and_clears_active_failure() {
        let mut current = MediaStatus::default();
        let late = DeviceStatus {
            remote_disk_id: 4,
            status: 1,
        };
        assert!(!apply_async_device_status(&mut current, late));

        current.mappings.insert(
            "optical-0".to_string(),
            MediaMappingStatus {
                remote_drive_id: 4,
                media_type: MsdMediaKind::Optical,
                image_size: 4096,
                read_requests: 0,
                read_blocks: 0,
            },
        );
        assert!(apply_async_device_status(&mut current, late));
        assert!(current.mappings.is_empty());
    }

    #[test]
    fn native_reconnect_due_check_is_strictly_after_the_timeout() {
        let start = Instant::now();
        let state = NativeReconnectState {
            session_id: Some(1),
            reconnect_check_timeout: Duration::from_secs(30),
            last_packet_at: start,
            ..NativeReconnectState::initial()
        };
        assert!(!state.due_at(start + Duration::from_secs(30)));
        assert!(state.due_at(start + Duration::from_secs(31)));
    }

    #[test]
    fn native_reconnect_allows_the_official_zero_after_timeout_halving() {
        let start = Instant::now();
        let state = NativeReconnectState {
            session_id: Some(1),
            reconnect_check_timeout: Duration::ZERO,
            last_packet_at: start,
            ..NativeReconnectState::initial()
        };
        assert!(!state.due_at(start));
        assert!(state.due_at(start + Duration::from_nanos(1)));
    }

    #[test]
    fn discovery_retains_session_id_before_the_drive_table() {
        let mut retained = None;
        let session_id = AvmpPacket {
            magic: super::super::protocol::APCP_MAGIC,
            packet_type: SESSION_ID_RESPONSE,
            header_extension: 0,
            payload: [1, 2, 3, 4, 0, 30].to_vec(),
        };
        assert!(observe_discovery_packet(
            AvmpGenerationProfile::Idrac8Native,
            &session_id,
            &mut retained,
        )
        .unwrap()
        .is_none());
        assert_eq!(retained.unwrap().session_id, 0x0102_0304);

        let drives = AvmpPacket::new(VDISK_INFO, [0, 1, 2, 0, 0, 4]).unwrap();
        assert_eq!(
            observe_discovery_packet(AvmpGenerationProfile::Idrac8Native, &drives, &mut retained,)
                .unwrap()
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn discovery_rejects_native_session_id_on_idrac6() {
        let mut retained = None;
        let packet = AvmpPacket {
            magic: super::super::protocol::APCP_MAGIC,
            packet_type: SESSION_ID_RESPONSE,
            header_extension: 0,
            payload: [0, 0, 0, 1].to_vec(),
        };
        assert!(observe_discovery_packet(
            AvmpGenerationProfile::Idrac6Java,
            &packet,
            &mut retained,
        )
        .is_err());
    }

    #[test]
    fn i_drac6_type_zero_capability_slot_is_a_verified_iso_target() {
        let slots = [RemoteDrive {
            id: 0,
            drive_type: 0,
            status: 0,
            capabilities: 1,
        }];
        assert_eq!(
            select_map_drive(
                AvmpGenerationProfile::Idrac6Java,
                MsdMediaKind::Optical,
                &slots,
                std::iter::empty(),
            ),
            Some(slots[0])
        );
        assert_eq!(
            select_map_drive(
                AvmpGenerationProfile::Idrac8Native,
                MsdMediaKind::Optical,
                &slots,
                std::iter::empty(),
            ),
            None
        );
    }

    #[test]
    fn advertised_drive_types_map_to_generic_media_enums_inside_the_backend() {
        let idrac6_slots = [
            RemoteDrive {
                id: 0,
                drive_type: CD_DRIVE_TYPE,
                status: 0,
                capabilities: 1,
            },
            RemoteDrive {
                id: 1,
                drive_type: IDRAC6_FLOPPY_DRIVE_TYPE,
                status: 0,
                capabilities: 1,
            },
        ];
        assert_eq!(
            supported_media_types(AvmpGenerationProfile::Idrac6Java, &idrac6_slots),
            vec![
                MsdMediaKind::Optical,
                MsdMediaKind::Floppy,
                MsdMediaKind::Flash,
            ]
        );

        let idrac8_slots = [
            RemoteDrive {
                id: 0,
                drive_type: CD_DRIVE_TYPE,
                status: 0,
                capabilities: 1,
            },
            RemoteDrive {
                id: 1,
                drive_type: IDRAC8_FLOPPY_DRIVE_TYPE,
                status: 0,
                capabilities: 1,
            },
        ];
        assert_eq!(
            supported_media_types(AvmpGenerationProfile::Idrac8Native, &idrac8_slots),
            vec![MsdMediaKind::Optical, MsdMediaKind::Floppy]
        );
        assert_eq!(
            select_map_drive(
                AvmpGenerationProfile::Idrac8Native,
                MsdMediaKind::Floppy,
                &idrac6_slots,
                std::iter::empty(),
            ),
            Some(idrac6_slots[1])
        );
    }

    #[test]
    fn flash_uses_an_unoccupied_removable_target() {
        let slots = [
            RemoteDrive {
                id: 10,
                drive_type: IDRAC8_REMOVABLE_DRIVE_TYPE,
                status: 0,
                capabilities: 1,
            },
            RemoteDrive {
                id: 11,
                drive_type: IDRAC8_REMOVABLE_DRIVE_TYPE,
                status: 0,
                capabilities: 1,
            },
        ];
        assert_eq!(
            select_map_drive(
                AvmpGenerationProfile::Idrac8Native,
                MsdMediaKind::Flash,
                &slots,
                [10],
            ),
            Some(slots[1])
        );
    }

    #[test]
    fn flash_layout_requires_a_removable_target_not_a_floppy_target() {
        let removable = [RemoteDrive {
            id: 1,
            drive_type: IDRAC8_REMOVABLE_DRIVE_TYPE,
            status: 0,
            capabilities: 1,
        }];
        assert!(requested_layout_converged(
            AvmpGenerationProfile::Idrac8Native,
            MsdMediaKind::Flash,
            &removable,
        ));

        let floppy = [RemoteDrive {
            id: 2,
            drive_type: IDRAC8_FLOPPY_DRIVE_TYPE,
            status: 0,
            capabilities: 1,
        }];
        assert!(!requested_layout_converged(
            AvmpGenerationProfile::Idrac8Native,
            MsdMediaKind::Flash,
            &floppy,
        ));
        assert!(!requested_layout_converged(
            AvmpGenerationProfile::Idrac6Java,
            MsdMediaKind::Flash,
            &removable,
        ));
    }

    #[test]
    fn flash_activation_requests_the_official_three_device_layout() {
        assert_eq!(
            requested_device_counts(Some(MsdMediaKind::Flash), true),
            VirtualDeviceCounts {
                floppy: 1,
                optical: 1,
                flash: 1,
            }
        );
        assert_eq!(
            requested_device_counts(Some(MsdMediaKind::Floppy), true),
            VirtualDeviceCounts {
                floppy: 1,
                optical: 1,
                flash: 1,
            }
        );
    }

    #[test]
    fn idrac8_disabled_placeholders_are_not_advertised_as_mappable_media() {
        let slots = [
            RemoteDrive {
                id: 0,
                drive_type: 0,
                status: 2,
                capabilities: 1,
            },
            RemoteDrive {
                id: 1,
                drive_type: 0,
                status: 2,
                capabilities: 1,
            },
        ];
        assert!(supported_media_types(AvmpGenerationProfile::Idrac8Native, &slots).is_empty());
        assert!(supported_media_types(AvmpGenerationProfile::Idrac6Java, &slots).is_empty());
        assert_eq!(
            advertised_media_types(AvmpGenerationProfile::Idrac8Native, &slots),
            vec![MsdMediaKind::Optical, MsdMediaKind::Floppy]
        );
        assert!(advertised_media_types(AvmpGenerationProfile::Idrac6Java, &slots).is_empty());
        assert!(should_activate_idrac8_virtual_devices(
            AvmpGenerationProfile::Idrac8Native,
            &slots,
            Some(MsdMediaKind::Floppy),
            false,
        ));
        assert!(should_activate_idrac8_virtual_devices(
            AvmpGenerationProfile::Idrac8Native,
            &slots,
            Some(MsdMediaKind::Optical),
            false,
        ));
        assert!(!should_activate_idrac8_virtual_devices(
            AvmpGenerationProfile::Idrac6Java,
            &slots,
            Some(MsdMediaKind::Floppy),
            false,
        ));
        assert!(!should_activate_idrac8_virtual_devices(
            AvmpGenerationProfile::Idrac8Native,
            &slots,
            Some(MsdMediaKind::Disk),
            false,
        ));

        let mut non_placeholder = slots;
        non_placeholder[1].drive_type = IDRAC8_FLOPPY_DRIVE_TYPE;
        non_placeholder[1].status = 0;
        assert!(!should_activate_idrac8_virtual_devices(
            AvmpGenerationProfile::Idrac8Native,
            &non_placeholder,
            Some(MsdMediaKind::Optical),
            false,
        ));
    }

    #[tokio::test]
    async fn idrac8_floppy_refresh_materializes_the_official_dual_device_layout() {
        let (client, mut appliance) = tokio::io::duplex(4096);
        let status = Arc::new(Mutex::new(MediaStatus::default()));
        let config = media_config(8, None);
        let (commands, receiver) = mpsc::channel(1);
        let worker = tokio::spawn(run(
            AvmpIo::new(client),
            receiver,
            status.clone(),
            config,
            NativeReconnectState::initial(),
            true,
        ));
        let (reply, result) = oneshot::channel();
        commands
            .send(Command::RefreshDrives {
                requested_media_type: Some(MsdMediaKind::Floppy),
                reply,
            })
            .await
            .unwrap();

        assert_eq!(
            read_packet(&mut appliance).await.unwrap().packet_type,
            0x0200
        );
        write_packet(
            &mut appliance,
            &AvmpPacket::new(VDISK_INFO, [0, 2, 0, 2, 0, 1, 0, 2, 0, 1]).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            read_packet(&mut appliance).await.unwrap(),
            set_device_count_request(
                true,
                VirtualDeviceCounts {
                    floppy: 1,
                    optical: 1,
                    flash: 1,
                },
            )
            .unwrap()
        );
        write_packet(
            &mut appliance,
            &AvmpPacket::new(SET_DEVICE_COUNT_RESPONSE, [0, 1, 1, 1]).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            read_packet(&mut appliance).await.unwrap().packet_type,
            0x0200
        );
        write_packet(
            &mut appliance,
            &AvmpPacket::new(VDISK_INFO, [0, 2, 2, 0, 0, 1, 4, 0, 0, 1]).unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(result.await.unwrap(), Ok(()));
        assert_eq!(
            status.lock().await.known_media_types,
            BTreeSet::from([MsdMediaKind::Optical, MsdMediaKind::Floppy])
        );
        assert_eq!(
            supported_media_types(
                AvmpGenerationProfile::Idrac8Native,
                &status.lock().await.remote_drives,
            ),
            vec![MsdMediaKind::Optical, MsdMediaKind::Floppy]
        );
        worker.abort();
    }

    #[tokio::test]
    async fn idrac8_floppy_refresh_accepts_verified_drive_table_convergence() {
        let (client, mut appliance) = tokio::io::duplex(4096);
        let status = Arc::new(Mutex::new(MediaStatus::default()));
        let config = media_config(8, None);
        let (commands, receiver) = mpsc::channel(1);
        let worker = tokio::spawn(run(
            AvmpIo::new(client),
            receiver,
            status.clone(),
            config,
            NativeReconnectState::initial(),
            true,
        ));
        let (reply, result) = oneshot::channel();
        commands
            .send(Command::RefreshDrives {
                requested_media_type: Some(MsdMediaKind::Floppy),
                reply,
            })
            .await
            .unwrap();

        assert_eq!(
            read_packet(&mut appliance).await.unwrap().packet_type,
            0x0200
        );
        write_packet(
            &mut appliance,
            &AvmpPacket::new(VDISK_INFO, [0, 2, 0, 2, 0, 1, 0, 2, 0, 1]).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(
            read_packet(&mut appliance).await.unwrap(),
            set_device_count_request(
                true,
                VirtualDeviceCounts {
                    floppy: 1,
                    optical: 1,
                    flash: 1,
                },
            )
            .unwrap()
        );
        // This is the exact lab-firmware sequence: DeviceStatus is not a
        // count acknowledgement, while the following table proves that the
        // requested map target now exists.
        write_packet(
            &mut appliance,
            &AvmpPacket::new(DEVICE_STATUS, 0u32.to_be_bytes()).unwrap(),
        )
        .await
        .unwrap();
        write_packet(
            &mut appliance,
            &AvmpPacket::new(VDISK_INFO, [0, 2, 2, 0, 0, 1, 1, 0, 0, 1]).unwrap(),
        )
        .await
        .unwrap();

        assert_eq!(result.await.unwrap(), Ok(()));
        assert_eq!(
            supported_media_types(
                AvmpGenerationProfile::Idrac8Native,
                &status.lock().await.remote_drives,
            ),
            vec![
                MsdMediaKind::Optical,
                MsdMediaKind::Floppy,
                MsdMediaKind::Flash,
            ]
        );
        worker.abort();
    }

    #[tokio::test]
    async fn requested_idrac6_drive_snapshot_is_used_by_the_following_map() {
        use std::io::Write;

        let (client, mut appliance) = tokio::io::duplex(4096);
        let status = Arc::new(Mutex::new(MediaStatus::default()));
        let config = media_config(6, None);
        let (commands, receiver) = mpsc::channel(2);
        let worker = tokio::spawn(run(
            AvmpIo::new(client),
            receiver,
            status.clone(),
            config,
            NativeReconnectState::initial(),
            false,
        ));

        let (refresh_reply, refresh_result) = oneshot::channel();
        commands
            .send(Command::RefreshDrives {
                requested_media_type: Some(MsdMediaKind::Flash),
                reply: refresh_reply,
            })
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut appliance).await.unwrap().packet_type,
            0x0200
        );
        write_packet(
            &mut appliance,
            &AvmpPacket::new(VDISK_INFO, [0, 2, 2, 0, 0, 1, 1, 0, 0, 1]).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(refresh_result.await.unwrap(), Ok(()));

        // The lab iDRAC6 can immediately emit a shorter, unsolicited table.
        // The official mapDrive path has already selected the drive from the
        // requested refresh and does not revalidate against this later table.
        write_packet(
            &mut appliance,
            &AvmpPacket::new(VDISK_INFO, [0, 1, 2, 0, 0, 1]).unwrap(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if status.lock().await.remote_drives.len() == 1 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("late drive table was processed");

        let mut image = vec![0u8; 1_474_560];
        image[0] = 0xeb;
        image[24..26].copy_from_slice(&18u16.to_le_bytes());
        image[26..28].copy_from_slice(&2u16.to_le_bytes());
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        temp.write_all(&image).unwrap();
        let (map_reply, map_result) = oneshot::channel();
        commands
            .send(Command::Map {
                target_id: "removable".to_string(),
                path: temp.path().to_string_lossy().into_owned(),
                size: image.len() as u64,
                media_type: MsdMediaKind::Flash,
                reply: map_reply,
            })
            .await
            .unwrap();

        let map = tokio::time::timeout(Duration::from_secs(1), read_packet(&mut appliance))
            .await
            .expect("map request for the verified removable drive")
            .unwrap();
        assert_eq!(map.packet_type, 0x0211);
        assert_eq!(map.header_extension, 1);
        write_packet(
            &mut appliance,
            &AvmpPacket {
                magic: super::super::protocol::AVMP_MAGIC,
                packet_type: DEVICE_STATUS,
                header_extension: 1,
                payload: DEVICE_STATUS_OK.to_be_bytes().to_vec(),
            },
        )
        .await
        .unwrap();
        assert_eq!(map_result.await.unwrap(), Ok(()));
        worker.abort();
    }

    #[tokio::test]
    async fn idrac6_session_topology_retains_a_hidden_second_target_for_map() {
        use std::io::Write;

        let (client, mut appliance) = tokio::io::duplex(8192);
        let status = Arc::new(Mutex::new(MediaStatus::default()));
        let config = media_config(6, None);
        let (commands, receiver) = mpsc::channel(2);
        let worker = tokio::spawn(run(
            AvmpIo::new(client),
            receiver,
            status.clone(),
            config,
            NativeReconnectState::initial(),
            false,
        ));

        // The authenticated session first proves both physical targets.
        write_packet(
            &mut appliance,
            &AvmpPacket::new(VDISK_INFO, [0, 2, 2, 0, 0, 1, 1, 0, 0, 1]).unwrap(),
        )
        .await
        .unwrap();
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if status.lock().await.remote_drives.len() == 2 {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("complete drive topology was processed");

        let (refresh_reply, refresh_result) = oneshot::channel();
        commands
            .send(Command::RefreshDrives {
                requested_media_type: Some(MsdMediaKind::Flash),
                reply: refresh_reply,
            })
            .await
            .unwrap();
        // Once optical is attached, the lab firmware repeatedly publishes
        // only that attached target. The already proven idle type-1 target is
        // absent rather than renumbered or explicitly disabled.
        for _ in 0..=IDRAC6_DRIVE_DISCOVERY_RETRIES {
            assert_eq!(
                read_packet(&mut appliance).await.unwrap().packet_type,
                0x0200
            );
            write_packet(
                &mut appliance,
                &AvmpPacket::new(VDISK_INFO, [0, 1, 2, 1, 0, 1]).unwrap(),
            )
            .await
            .unwrap();
        }
        assert_eq!(refresh_result.await.unwrap(), Ok(()));

        let mut image = vec![0u8; 1_474_560];
        image[0] = 0xeb;
        image[24..26].copy_from_slice(&18u16.to_le_bytes());
        image[26..28].copy_from_slice(&2u16.to_le_bytes());
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        temp.write_all(&image).unwrap();
        let (map_reply, map_result) = oneshot::channel();
        commands
            .send(Command::Map {
                target_id: "removable".to_string(),
                path: temp.path().to_string_lossy().into_owned(),
                size: image.len() as u64,
                media_type: MsdMediaKind::Flash,
                reply: map_reply,
            })
            .await
            .unwrap();
        let map = read_packet(&mut appliance).await.unwrap();
        assert_eq!(map.packet_type, 0x0211);
        assert_eq!(map.header_extension, 1);
        write_packet(
            &mut appliance,
            &AvmpPacket {
                magic: super::super::protocol::AVMP_MAGIC,
                packet_type: DEVICE_STATUS,
                header_extension: 1,
                payload: DEVICE_STATUS_OK.to_be_bytes().to_vec(),
            },
        )
        .await
        .unwrap();
        assert_eq!(map_result.await.unwrap(), Ok(()));
        worker.abort();
    }

    #[tokio::test]
    async fn floppy_bpb_geometry_matches_the_official_java_extraction() {
        use std::io::Write;

        let mut image = vec![0u8; 1_474_560];
        image[0] = 0xeb;
        image[23] = 0;
        image[24] = 18;
        image[25] = 0;
        image[26] = 2;
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        temp.write_all(&image).unwrap();
        let mut file = tokio::fs::File::open(temp.path()).await.unwrap();
        let geometry = removable_geometry(&mut file, 2880).await.unwrap();
        assert_eq!(geometry.capacity_blocks, 2880);
        assert_eq!(geometry.cylinders, 80);
        assert_eq!(geometry.heads, 2);
        assert_eq!(geometry.sectors, 18);
    }

    #[tokio::test]
    async fn removable_mbr_geometry_matches_native_translated_chs_fallback() {
        use std::io::Write;

        let mut image = vec![0u8; 8 * 1024 * 1024];
        image[0] = 0xfa;
        let partition = &mut image[446..462];
        partition[1] = 0xfe;
        partition[2] = 0xff;
        partition[3] = 0xff;
        partition[4] = 0x0c;
        partition[5] = 0xfe;
        partition[6] = 0xff;
        partition[7] = 0xff;
        partition[8..12].copy_from_slice(&63u32.to_le_bytes());
        partition[12..16].copy_from_slice(&8192u32.to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xaa;
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        temp.write_all(&image).unwrap();
        let mut file = tokio::fs::File::open(temp.path()).await.unwrap();

        let geometry = removable_geometry(&mut file, 16_384).await.unwrap();
        assert_eq!(geometry.capacity_blocks, 16_384);
        assert_eq!(geometry.cylinders, 17);
        assert_eq!(geometry.heads, 16);
        assert_eq!(geometry.sectors, 63);
    }

    #[tokio::test]
    async fn removable_mbr_rejects_partition_past_end_of_image() {
        use std::io::Write;

        let mut image = vec![0u8; 4096];
        image[0] = 0xfa;
        image[454..458].copy_from_slice(&1u32.to_le_bytes());
        image[458..462].copy_from_slice(&8u32.to_le_bytes());
        image[510] = 0x55;
        image[511] = 0xaa;
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        temp.write_all(&image).unwrap();
        let mut file = tokio::fs::File::open(temp.path()).await.unwrap();

        let error = removable_geometry(&mut file, 8).await.unwrap_err();
        assert!(error.contains("extends beyond"));
    }

    #[tokio::test]
    async fn removable_gpt_geometry_validates_header_and_keeps_full_capacity() {
        use std::io::Write;

        let blocks = 16_384u32;
        let mut image = vec![0u8; blocks as usize * 512];
        let protective = &mut image[446..462];
        protective[4] = 0xee;
        protective[8..12].copy_from_slice(&1u32.to_le_bytes());
        protective[12..16].copy_from_slice(&(blocks - 1).to_le_bytes());
        image[510..512].copy_from_slice(&[0x55, 0xaa]);
        let header = &mut image[512..1024];
        header[..8].copy_from_slice(b"EFI PART");
        header[12..16].copy_from_slice(&92u32.to_le_bytes());
        header[40..48].copy_from_slice(&34u64.to_le_bytes());
        header[48..56].copy_from_slice(&((blocks - 34) as u64).to_le_bytes());
        let mut temp = tempfile::NamedTempFile::new().unwrap();
        temp.write_all(&image).unwrap();
        let mut file = tokio::fs::File::open(temp.path()).await.unwrap();

        let geometry = removable_geometry(&mut file, blocks).await.unwrap();
        assert_eq!(geometry.capacity_blocks, blocks);
        assert_eq!(geometry.heads, 16);
        assert_eq!(geometry.sectors, 63);
    }

    #[test]
    fn dynamic_targets_require_multiple_compatible_physical_drives() {
        let one = [RemoteDrive {
            id: 1,
            drive_type: CD_DRIVE_TYPE,
            status: 0,
            capabilities: 1,
        }];
        assert_eq!(
            dynamic_target_capacity(
                AvmpGenerationProfile::Idrac8Native,
                MsdMediaKind::Optical,
                &one
            ),
            1
        );

        let two = [
            one[0],
            RemoteDrive {
                id: 2,
                drive_type: CD_DRIVE_TYPE,
                status: 0,
                capabilities: 1,
            },
        ];
        assert_eq!(
            dynamic_target_capacity(
                AvmpGenerationProfile::Idrac8Native,
                MsdMediaKind::Optical,
                &two
            ),
            2
        );
    }

    #[tokio::test]
    async fn removable_standard_e9_bpb_and_raw_images_are_supported() {
        use std::io::Write;

        let mut bpb = vec![0u8; 1_474_560];
        bpb[0] = 0xe9;
        bpb[24..26].copy_from_slice(&18u16.to_le_bytes());
        bpb[26..28].copy_from_slice(&2u16.to_le_bytes());
        let mut bpb_temp = tempfile::NamedTempFile::new().unwrap();
        bpb_temp.write_all(&bpb).unwrap();
        let mut bpb_file = tokio::fs::File::open(bpb_temp.path()).await.unwrap();
        let bpb_geometry = removable_geometry(&mut bpb_file, 2_880).await.unwrap();
        assert_eq!(bpb_geometry.cylinders, 80);
        assert_eq!(bpb_geometry.heads, 2);
        assert_eq!(bpb_geometry.sectors, 18);

        let raw = vec![0u8; 4 * 1024 * 1024];
        let mut raw_temp = tempfile::NamedTempFile::new().unwrap();
        raw_temp.write_all(&raw).unwrap();
        let mut raw_file = tokio::fs::File::open(raw_temp.path()).await.unwrap();
        let raw_geometry = removable_geometry(&mut raw_file, 8_192).await.unwrap();
        assert_eq!(raw_geometry.capacity_blocks, 8_192);
        assert_eq!(raw_geometry.cylinders, 9);
        assert_eq!(raw_geometry.heads, 16);
        assert_eq!(raw_geometry.sectors, 63);
    }

    #[tokio::test]
    async fn mapped_reads_are_routed_by_remote_drive_id() {
        use std::io::Write;

        let mut optical = tempfile::NamedTempFile::new().unwrap();
        optical.write_all(b"OPTICAL!").unwrap();
        let mut floppy = tempfile::NamedTempFile::new().unwrap();
        floppy.write_all(b"FLOPPY!!").unwrap();
        let mut images = BTreeMap::new();
        for (remote_id, target_id, path, media_type) in [
            (2, "optical-0", optical.path(), MsdMediaKind::Optical),
            (4, "removable-0", floppy.path(), MsdMediaKind::Floppy),
        ] {
            images.insert(
                remote_id,
                MappedImage {
                    target_id: target_id.to_string(),
                    path: path.to_string_lossy().to_string(),
                    size: 8,
                    media_type,
                    block_size: 4,
                    read_requests: 0,
                    read_blocks: 0,
                    file: tokio::fs::File::open(path).await.unwrap(),
                },
            );
        }

        let optical_response = serve_mapped_read_request(
            &mut images,
            ReadRequest {
                remote_disk_id: 2,
                offset_blocks: 0,
                block_count: 1,
                max_blocks_per_packet: 1,
            },
        )
        .await
        .unwrap();
        let floppy_response = serve_mapped_read_request(
            &mut images,
            ReadRequest {
                remote_disk_id: 4,
                offset_blocks: 1,
                block_count: 1,
                max_blocks_per_packet: 1,
            },
        )
        .await
        .unwrap();

        assert_eq!(&optical_response.payload[8..], b"OPTI");
        assert_eq!(&floppy_response.payload[8..], b"PY!!");
        assert_eq!(optical_response.header_extension, 2);
        assert_eq!(floppy_response.header_extension, 4);
    }

    /// Explicit opt-in hardware probe. It is ignored by default and performs
    /// only ServerImageInfo/ServerImageSpace reads after AVMP authentication.
    #[tokio::test]
    #[ignore = "requires IDRAC_HOST, IDRAC_USERNAME, IDRAC_PASSWORD, and IDRAC_GENERATION (6 or 8)"]
    async fn hardware_server_image_read_only_interop() {
        let profile = match std::env::var("IDRAC_GENERATION").as_deref() {
            Ok("6") => AvmpGenerationProfile::Idrac6Java,
            Ok("8") => AvmpGenerationProfile::Idrac8Native,
            Ok(other) => panic!("unsupported IDRAC_GENERATION {other}"),
            Err(_) => panic!("IDRAC_GENERATION"),
        };
        let config = IdracLegacyMediaConfig {
            host: std::env::var("IDRAC_HOST").expect("IDRAC_HOST"),
            port: 5900,
            username: std::env::var("IDRAC_USERNAME").expect("IDRAC_USERNAME"),
            password: std::env::var("IDRAC_PASSWORD").expect("IDRAC_PASSWORD"),
            profile,
            heartbeat_interval: None,
            reconnect_method: NativeReconnectMethod::Firmware,
        };
        let mut transport = establish_authenticated_transport(&config)
            .await
            .expect("authenticated AVMP transport");
        write_packet(
            &mut transport.stream,
            &super::super::protocol::server_image_info_request(),
        )
        .await
        .expect("ServerImageInfo request");
        let packet = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let packet = transport.stream.next_packet().await?;
                if packet.packet_type == super::super::protocol::SERVER_IMAGE_INFO_RESPONSE {
                    return Ok::<_, String>(packet);
                }
            }
        })
        .await
        .expect("ServerImageInfo response timeout")
        .expect("ServerImageInfo response read");
        let images = super::super::protocol::parse_server_image_info(&packet)
            .expect("bounded ServerImageInfo response");
        eprintln!(
            "server-image read-only probe: entries={}, images={:?}",
            images.len(),
            images
                .iter()
                .map(|image| (
                    image.index,
                    image.remote_disk_id,
                    image.size_blocks,
                    image.flags,
                    image.image_type,
                    image.image_format,
                    image.store,
                    String::from_utf8_lossy(&image.name).into_owned(),
                ))
                .collect::<Vec<_>>()
        );
        const OFFICIAL_SERVER_IMAGE_STORE: u16 = 0x0010;
        let mut stores = vec![OFFICIAL_SERVER_IMAGE_STORE];
        for store in images.iter().map(|image| u16::from(image.store)) {
            if !stores.contains(&store) {
                stores.push(store);
            }
        }
        for store in stores {
            write_packet(
                &mut transport.stream,
                &super::super::protocol::server_image_space_request(store),
            )
            .await
            .expect("ServerImageSpace request");
            let packet = tokio::time::timeout(Duration::from_secs(10), async {
                loop {
                    let packet = transport.stream.next_packet().await?;
                    if packet.packet_type == super::super::protocol::SERVER_IMAGE_SPACE_RESPONSE {
                        return Ok::<_, String>(packet);
                    }
                }
            })
            .await
            .expect("ServerImageSpace response timeout")
            .expect("ServerImageSpace response read");
            let space = super::super::protocol::parse_server_image_space(&packet)
                .expect("bounded ServerImageSpace response");
            assert_eq!(space.store, store);
            eprintln!(
                "server-image space: store={}, total_blocks={}, free_blocks={}",
                space.store, space.total_blocks, space.free_blocks
            );
        }
    }

    /// Deliberately isolated probe for a real iDRAC6 that selected raw APCP
    /// capability one. It must authenticate and return VDiskInfo before raw
    /// transport becomes a supported product path. This sends no map, write,
    /// or release operation.
    #[tokio::test]
    #[ignore = "requires IDRAC_HOST, IDRAC_USERNAME, and IDRAC_PASSWORD for a capability-1 iDRAC6"]
    async fn hardware_idrac6_raw_capability_one_probe() {
        let host = std::env::var("IDRAC_HOST").expect("IDRAC_HOST");
        let username = std::env::var("IDRAC_USERNAME").expect("IDRAC_USERNAME");
        let password = std::env::var("IDRAC_PASSWORD").expect("IDRAC_PASSWORD");
        let address = format!("{host}:5900");
        let mut stream =
            tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&address))
                .await
                .expect("raw iDRAC6 TCP connect timeout")
                .expect("raw iDRAC6 TCP connect");
        stream.set_nodelay(true).expect("raw iDRAC6 TCP nodelay");
        write_session_request(&mut stream, AvmpGenerationProfile::Idrac6Java)
            .await
            .expect("iDRAC6 APCP request");
        let setup = read_session_setup(
            &mut stream,
            AvmpGenerationProfile::Idrac6Java,
            NativeReconnectMethod::Firmware,
        )
        .await
        .expect("iDRAC6 APCP setup");
        assert_eq!(setup.capabilities, 1, "raw probe requires capability one");

        write_packet(
            &mut stream,
            &login_for_version(&username, &password, setup.avmp_major, setup.avmp_minor)
                .expect("iDRAC6 raw login encoder"),
        )
        .await
        .expect("iDRAC6 raw login write");
        let login = tokio::time::timeout(Duration::from_secs(10), read_packet(&mut stream))
            .await
            .expect("iDRAC6 raw login response timeout")
            .expect("iDRAC6 raw login response read");
        let login = parse_login_response(&login, AvmpGenerationProfile::Idrac6Java)
            .expect("iDRAC6 raw login response shape");
        assert_eq!(login.status, 0, "iDRAC6 raw login status");

        write_packet(
            &mut stream,
            &get_vdisk_info(AvmpGenerationProfile::Idrac6Java),
        )
        .await
        .expect("iDRAC6 raw VDiskInfo request");
        let drives = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let packet = read_packet(&mut stream).await?;
                if packet.packet_type == VDISK_INFO {
                    return parse_vdisk_info(&packet);
                }
            }
        })
        .await
        .expect("iDRAC6 raw VDiskInfo response timeout")
        .expect("iDRAC6 raw VDiskInfo response");
        eprintln!("iDRAC6 raw AVMP probe discovered {} drive(s)", drives.len());
    }

    /// Full, explicitly opt-in iDRAC6 raw-AVMP interoperability probe. The
    /// image is temporary operator-provided test media: map it to the exact
    /// advertised slot, serve at least one BMC read, then wait for release.
    #[tokio::test]
    #[ignore = "requires IDRAC_* plus IDRAC_MEDIA_PROBE_ISO for the real iDRAC6"]
    async fn hardware_idrac6_raw_map_read_release_interop() {
        let host = std::env::var("IDRAC_HOST").expect("IDRAC_HOST");
        let username = std::env::var("IDRAC_USERNAME").expect("IDRAC_USERNAME");
        let password = std::env::var("IDRAC_PASSWORD").expect("IDRAC_PASSWORD");
        let image_path = std::env::var("IDRAC_MEDIA_PROBE_ISO").expect("IDRAC_MEDIA_PROBE_ISO");
        let metadata = tokio::fs::metadata(&image_path)
            .await
            .expect("probe ISO metadata");
        assert!(metadata.len() > 0, "probe ISO must not be empty");

        let mut stream = TcpStream::connect(format!("{host}:5900"))
            .await
            .expect("raw iDRAC6 TCP connect");
        stream.set_nodelay(true).expect("raw iDRAC6 TCP nodelay");
        write_session_request(&mut stream, AvmpGenerationProfile::Idrac6Java)
            .await
            .expect("iDRAC6 APCP request");
        let setup = read_session_setup(
            &mut stream,
            AvmpGenerationProfile::Idrac6Java,
            NativeReconnectMethod::Firmware,
        )
        .await
        .expect("iDRAC6 APCP setup");
        assert_eq!(setup.capabilities, 1, "raw probe requires capability one");
        write_packet(
            &mut stream,
            &login_for_version(&username, &password, setup.avmp_major, setup.avmp_minor)
                .expect("iDRAC6 raw login encoder"),
        )
        .await
        .expect("iDRAC6 raw login write");
        let login = parse_login_response(
            &read_packet(&mut stream)
                .await
                .expect("iDRAC6 raw login response"),
            AvmpGenerationProfile::Idrac6Java,
        )
        .expect("iDRAC6 raw login response shape");
        assert_eq!(login.status, 0, "iDRAC6 raw login status");

        write_packet(
            &mut stream,
            &get_vdisk_info(AvmpGenerationProfile::Idrac6Java),
        )
        .await
        .expect("iDRAC6 VDiskInfo request");
        let drives = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let packet = read_packet(&mut stream).await?;
                if packet.packet_type == VDISK_INFO {
                    return parse_vdisk_info(&packet);
                }
            }
        })
        .await
        .expect("iDRAC6 VDiskInfo timeout")
        .expect("iDRAC6 VDiskInfo response");
        let drive = drives
            .iter()
            .find(|drive| drive.id == 0 && drive.status == 0 && drive.capabilities & 1 != 0)
            .unwrap_or_else(|| {
                panic!(
                    "advertised iDRAC6 Virtual Media slot; discovered drives: {:?}",
                    drives
                )
            });
        let capacity = u32::try_from(metadata.len().div_ceil(u64::from(OPTICAL_BLOCK_SIZE)))
            .expect("probe ISO capacity");
        write_packet(
            &mut stream,
            &vdisk_map_request(drive.id, OPTICAL_BLOCK_SIZE, capacity, 0, 0xff, &[])
                .expect("iDRAC6 VDiskMap encoder"),
        )
        .await
        .expect("iDRAC6 VDiskMap write");

        let mut image = tokio::fs::File::open(&image_path)
            .await
            .expect("probe ISO open");
        let mut mapped = false;
        let mut reads = 0u64;
        tokio::time::timeout(Duration::from_secs(25), async {
            while !mapped || reads == 0 {
                let packet = read_packet(&mut stream).await?;
                if packet.packet_type == DEVICE_STATUS && packet.header_extension == drive.id {
                    let status = u32::from_be_bytes(
                        packet.payload[..4]
                            .try_into()
                            .map_err(|_| "bad DeviceStatus")?,
                    );
                    if status != DEVICE_STATUS_OK {
                        return Err(format!("iDRAC6 VDiskMap rejected with status {status}"));
                    }
                    mapped = true;
                } else if packet.packet_type == super::super::protocol::READ_REQUEST {
                    let request = ReadRequest::try_from(&packet)?;
                    let response =
                        serve_read_request(&mut image, request, OPTICAL_BLOCK_SIZE).await?;
                    reads += u64::from(request.block_count);
                    write_packet(&mut stream, &response).await?;
                    write_packet(&mut stream, &client_status(request.remote_disk_id, 0)).await?;
                }
            }
            Ok::<_, String>(())
        })
        .await
        .expect("iDRAC6 map/read timeout")
        .expect("iDRAC6 map/read response");
        write_packet(&mut stream, &vdisk_release(drive.id))
            .await
            .expect("iDRAC6 release write");
        eprintln!(
            "iDRAC6 raw map/read completed and release sent for drive {} ({reads} block(s))",
            drive.id
        );
    }

    /// Persistent, explicitly opt-in hardware validation for the hardened
    /// create serializer. It deliberately uses the first unadvertised index
    /// and never deletes or overwrites an existing server image.
    #[tokio::test]
    #[ignore = "requires IDRAC_* plus IDRAC_SERVER_IMAGE_WRITE=1"]
    async fn hardware_hardened_server_image_create_interop() {
        assert_eq!(
            std::env::var("IDRAC_SERVER_IMAGE_WRITE").as_deref(),
            Ok("1"),
            "set IDRAC_SERVER_IMAGE_WRITE=1 to permit persistent BMC mutation"
        );
        let config = IdracLegacyMediaConfig {
            host: std::env::var("IDRAC_HOST").expect("IDRAC_HOST"),
            port: 5900,
            username: std::env::var("IDRAC_USERNAME").expect("IDRAC_USERNAME"),
            password: std::env::var("IDRAC_PASSWORD").expect("IDRAC_PASSWORD"),
            profile: AvmpGenerationProfile::Idrac8Native,
            heartbeat_interval: None,
            reconnect_method: NativeReconnectMethod::Firmware,
        };
        let mut transport = establish_authenticated_transport(&config)
            .await
            .expect("authenticated AVMP transport");
        write_packet(
            &mut transport.stream,
            &super::super::protocol::server_image_info_request(),
        )
        .await
        .expect("pre-create ServerImageInfo request");
        let inventory = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let packet = transport.stream.next_packet().await?;
                if packet.packet_type == super::super::protocol::SERVER_IMAGE_INFO_RESPONSE {
                    return Ok::<_, String>(packet);
                }
            }
        })
        .await
        .expect("pre-create ServerImageInfo response timeout")
        .expect("pre-create ServerImageInfo response read");
        let existing = super::super::protocol::parse_server_image_info(&inventory)
            .expect("bounded pre-create ServerImageInfo response");

        // The official RP_VMAPI_LIB::uploadServerImage implementation returns
        // before it creates anything when the server-image list is nonempty.
        // Preserve that condition: a raw create against such an appliance has
        // already been shown to be rejected and must not be retried.
        let allow_nonempty =
            std::env::var("IDRAC_SERVER_IMAGE_ALLOW_NONEMPTY").as_deref() == Ok("1");
        if !existing.is_empty() && !allow_nonempty {
            eprintln!(
                "skipping persistent create: official uploader requires an empty server-image list (found {})",
                existing.len()
            );
            return;
        }
        assert!(
            !existing.iter().any(|image| image.index == 7),
            "persistent create probe index 7 is already occupied"
        );
        let request = super::super::protocol::server_image_create_request(
            7,
            1,
            0,
            // Official ServerImageUploadThread uses type 1 for a file-backed
            // image, with flags and format both set to zero.
            1,
            0,
            1,
            b"bmc-aggregator-interop",
        )
        .expect("hardened create request");
        write_packet(&mut transport.stream, &request)
            .await
            .expect("ServerImageCreate request");
        let response = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let packet = transport.stream.next_packet().await?;
                if packet.packet_type == super::super::protocol::SERVER_IMAGE_CREATE_RESPONSE {
                    return Ok::<_, String>(packet);
                }
            }
        })
        .await
        .expect("ServerImageCreate response timeout")
        .expect("ServerImageCreate response read");
        eprintln!(
            "server-image create response: extension={}, payload={:?}",
            response.header_extension, response.payload
        );
        let status = super::super::protocol::parse_server_image_status(
            &response,
            super::super::protocol::SERVER_IMAGE_CREATE_RESPONSE,
        )
        .expect("bounded ServerImageCreate response");
        // This firmware uses extension 0 for a rejected create.  The native
        // client consumes that extension too, so require index 7 only after
        // the create succeeded.
        if status.status == 0 {
            assert_eq!(status.index, 7);
        } else if allow_nonempty && !existing.is_empty() {
            eprintln!(
                "non-empty ServerImage inventory rejected create with status {} as expected",
                status.status
            );
            return;
        }
        assert_eq!(status.status, 0, "BMC rejected hardened ServerImageCreate");
    }
}
