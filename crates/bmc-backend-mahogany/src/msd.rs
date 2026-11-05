use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, watch, Mutex};

use bmc_feature_api::msd::{
    MsdAttachment, MsdBackend as MsdBackendTrait, MsdCapabilities, MsdMedia, MsdMediaKind,
    MsdMediaSource, MsdMode, MsdSlotCapabilities, MsdStorageCapabilities, MsdStorageLocation,
    MsdStorageUnavailableReason, MsdUnavailableStorage,
};

use crate::storage_server::MahoganyStorageServer;
pub use bmc_backend_model::media::StorageShareStatus;
use bmc_backend_model::media::{Attachment, AttachmentEvent};

const OPTICAL_SLOT_ID: &str = "optical";
const STORAGE_OPERATION_TIMEOUT: Duration = Duration::from_secs(10);
const OPTICAL_BLOCK_SIZE: u64 = 2048;

fn capabilities(storage_available: bool) -> MsdCapabilities {
    if storage_available {
        return MsdCapabilities {
            virtual_drive: false,
            slots: vec![MsdSlotCapabilities::new(
                OPTICAL_SLOT_ID,
                vec![MsdMediaKind::Optical],
            )],
            storage_locations: vec![MsdStorageCapabilities::aggregator(vec![
                MsdMediaKind::Optical,
            ])],
            unavailable_storage_locations: Vec::new(),
            target_management: Default::default(),
        };
    }

    MsdCapabilities {
        virtual_drive: false,
        slots: Vec::new(),
        storage_locations: Vec::new(),
        unavailable_storage_locations: vec![MsdUnavailableStorage {
            location: MsdStorageLocation::Aggregator,
            reason: MsdStorageUnavailableReason::ProtocolUnavailable,
            hint_key: "msd.mahoganyStoragePrivilegeUnavailableHint".to_string(),
        }],
        target_management: Default::default(),
    }
}

#[derive(Debug, Clone)]
struct MsdControl {
    tcp_tx: mpsc::Sender<Vec<u8>>,
    local_addr: SocketAddr,
    peer_ip: IpAddr,
    storage_allowed: bool,
}

#[derive(Debug)]
struct ActiveMount {
    server: Arc<MahoganyStorageServer>,
    accept_task: tokio::task::AbortHandle,
    port: u16,
    size: u64,
}

/// Cancelling a mount still asks the BMC to release the peer. No async work in Drop.
struct MountAttempt {
    tcp_tx: mpsc::Sender<Vec<u8>>,
    armed: bool,
}
impl Drop for MountAttempt {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.tcp_tx.try_send(build_storage_client_disconnect());
        }
    }
}

impl Drop for ActiveMount {
    fn drop(&mut self) {
        self.accept_task.abort();
    }
}

/// MSD (Mass Storage Device) backend state.
#[derive(Debug, Clone)]
pub struct MsdBackend {
    inner: Arc<Mutex<MsdInner>>,
    status_tx: watch::Sender<u64>,
    operation: Arc<Mutex<()>>,
    control: Option<MsdControl>,
}

#[derive(Debug, Clone)]
pub struct MsdInner {
    pub enabled: bool,
    pub connected: bool,
    attachment: Attachment,
    pub image_size: u64,
    pub storage_status: Option<StorageStatus>,
    active_mount: Option<Arc<ActiveMount>>,
}

impl MsdBackend {
    pub fn new() -> Self {
        let (status_tx, _) = watch::channel(0);
        Self {
            inner: Arc::new(Mutex::new(MsdInner {
                enabled: false,
                connected: false,
                attachment: Attachment::Unknown,
                image_size: 0,
                storage_status: None,
                active_mount: None,
            })),
            status_tx,
            operation: Arc::new(Mutex::new(())),
            control: None,
        }
    }

    pub(crate) fn with_control(
        tcp_tx: mpsc::Sender<Vec<u8>>,
        local_addr: SocketAddr,
        peer_ip: IpAddr,
        storage_allowed: bool,
    ) -> Self {
        let mut backend = Self::new();
        backend.control = Some(MsdControl {
            tcp_tx,
            local_addr,
            peer_ip,
            storage_allowed,
        });
        backend
    }

    pub async fn status(&self) -> MsdInner {
        self.inner.lock().await.clone()
    }

    pub fn subscribe_status(&self) -> watch::Receiver<u64> {
        self.status_tx.subscribe()
    }

    pub async fn apply_storage_status(&self, status: StorageStatus) {
        let reported_connected = status.any_connected();
        let disconnect_error = status.any_disconnect_error();
        let mut inner = self.inner.lock().await;
        // A disconnect error does not prove that the BMC stopped using the
        // storage peer. Keep serving the retained image so the operation can
        // be retried without turning an uncertain remote state into data loss.
        let event = if reported_connected {
            AttachmentEvent::Connected
        } else if disconnect_error {
            AttachmentEvent::Uncertain
        } else {
            AttachmentEvent::Disconnected
        };
        let active_port = inner.active_mount.as_ref().map(|mount| mount.port);
        if active_port.is_some_and(|port| port != status.port) {
            return;
        }
        inner.attachment = inner.attachment.observe(active_port, status.port, event);
        let connected = inner.attachment.connected();
        if inner.storage_status.as_ref() == Some(&status) && inner.connected == connected {
            return;
        }
        inner.connected = connected;
        inner.storage_status = Some(status);
        if !connected {
            inner.active_mount = None;
            inner.enabled = false;
            inner.image_size = 0;
        }
        drop(inner);
        self.notify_status_changed();
    }

    fn forward_peer_state(&self, active: &Arc<ActiveMount>) {
        let active = Arc::downgrade(active);
        let inner = self.inner.clone();
        let status_tx = self.status_tx.clone();
        let Some(active_mount) = active.upgrade() else {
            return;
        };
        let mut peer_ready = active_mount.server.subscribe_peer_ready();
        drop(active_mount);
        peer_ready.borrow_and_update();

        tokio::spawn(async move {
            while peer_ready.changed().await.is_ok() {
                let Some(active) = active.upgrade() else {
                    break;
                };
                let is_current = inner
                    .lock()
                    .await
                    .active_mount
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(current, &active));
                if !is_current {
                    break;
                }
                status_tx.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
        });
    }

    fn disconnect_managed_best_effort(&self) {
        let managed = self
            .inner
            .try_lock()
            .is_ok_and(|inner| inner.active_mount.is_some());
        if managed {
            if let Some(control) = self.control.as_ref() {
                let _ = control.tcp_tx.try_send(build_storage_client_disconnect());
            }
        }
    }

    fn storage_available(&self) -> bool {
        self.control
            .as_ref()
            .is_some_and(|control| control.storage_allowed)
    }

    async fn validate_optical_media(media: &MsdMedia) -> Result<(), String> {
        if media.media_type != Some(MsdMediaKind::Optical) {
            return Err("Mahogany virtual media exposes one optical target".to_string());
        }
        let MsdMediaSource::AggregatorImage { size, file, .. } = &media.source else {
            return Err("Mahogany virtual media requires aggregator-managed media".to_string());
        };
        if *size == 0 || size % OPTICAL_BLOCK_SIZE != 0 {
            return Err(
                "Mahogany optical media must be a non-empty multiple of 2048 bytes".to_string(),
            );
        }
        if *size / OPTICAL_BLOCK_SIZE > u64::from(u32::MAX) {
            return Err("Mahogany optical media exceeds READ CAPACITY(10) limits".to_string());
        }
        let actual = file
            .metadata()
            .map_err(|error| format!("could not inspect Mahogany media: {error}"))?
            .len();
        if actual < *size {
            return Err("Mahogany optical media became shorter after selection".to_string());
        }
        Ok(())
    }

    async fn mount_at(&self, slot_id: &str, media: MsdMedia, port: u16) -> Result<(), String> {
        if slot_id != OPTICAL_SLOT_ID {
            return Err("Mahogany virtual media exposes only the optical target".to_string());
        }
        Self::validate_optical_media(&media).await?;
        let control = self
            .control
            .as_ref()
            .filter(|control| control.storage_allowed)
            .cloned()
            .ok_or_else(|| {
                "the BMC account does not grant Mahogany storage privilege".to_string()
            })?;
        let _operation = self.operation.lock().await;
        if self.inner.lock().await.connected {
            return Err("Mahogany virtual media is already connected".to_string());
        }

        let MsdMediaSource::AggregatorImage { size, file, .. } = media.source else {
            unreachable!("validated aggregator image")
        };
        let server = Arc::new(MahoganyStorageServer::new());
        server
            .set_image_file(file, size)
            .await
            .map_err(|error| format!("could not retain Mahogany media: {error}"))?;
        server.set_expected_peer(control.peer_ip).await;
        server
            .start_on(SocketAddr::new(control.local_addr.ip(), port))
            .await
            .map_err(|error| format!("could not start Mahogany storage peer: {error}"))?;
        let bound_port = server
            .local_port()
            .await
            .ok_or_else(|| "Mahogany storage peer did not expose a listening port".to_string())?;
        let peer_ready = server.subscribe_peer_ready();
        let server_task = tokio::spawn(server.clone().accept_loop());
        let active = Arc::new(ActiveMount {
            server,
            accept_task: server_task.abort_handle(),
            port: bound_port,
            size,
        });
        let status_changed = self.subscribe_status();
        let message = build_storage_client_connect(
            &encode_storage_address(control.local_addr.ip()),
            bound_port,
            "",
            "",
            0xFF,
            0xFF,
            storage_ip_type(control.local_addr.ip()),
        );
        let mut attempt = MountAttempt {
            tcp_tx: control.tcp_tx.clone(),
            armed: true,
        };
        tokio::time::timeout(STORAGE_OPERATION_TIMEOUT, control.tcp_tx.send(message))
            .await
            .map_err(|_| "Mahogany storage connect queue timed out; result unknown".to_string())?
            .map_err(|error| format!("could not send Mahogany storage connect: {error}"))?;

        let confirmation = match tokio::time::timeout(
            STORAGE_OPERATION_TIMEOUT,
            self.wait_for_mount_confirmation(bound_port, status_changed, peer_ready),
        )
        .await
        {
            Ok(confirmation) => confirmation,
            Err(_) => Err("timed out waiting for Mahogany storage connection".to_string()),
        };
        if let Err(error) = confirmation {
            drop(active);
            return Err(error);
        }

        let mut inner = self.inner.lock().await;
        if !inner.connected
            || inner
                .storage_status
                .as_ref()
                .is_none_or(|status| status.port != bound_port)
            || !active.server.peer_ready()
        {
            drop(inner);
            drop(active);
            return Err("Mahogany storage connection changed before commit".to_string());
        }
        inner.enabled = true;
        inner.image_size = size;
        inner.active_mount = Some(active.clone());
        attempt.armed = false;
        drop(inner);
        self.forward_peer_state(&active);
        self.notify_status_changed();
        Ok(())
    }

    async fn wait_for_mount_confirmation(
        &self,
        port: u16,
        mut status_changed: watch::Receiver<u64>,
        mut peer_ready: watch::Receiver<bool>,
    ) -> Result<(), String> {
        use bmc_backend_model::media::{Mount, Observation, State};
        let origin = tokio::time::Instant::now();
        let mut model = Mount::new(port, STORAGE_OPERATION_TIMEOUT);
        loop {
            let status = self.inner.lock().await.storage_status.clone();
            model = model.step(
                Observation {
                    port: status.as_ref().map(|status| status.port),
                    connected: status.as_ref().is_some_and(StorageStatus::any_connected),
                    failed: status.as_ref().is_some_and(StorageStatus::any_failed),
                    peer_ready: *peer_ready.borrow(),
                },
                origin.elapsed(),
            );
            match model.state() {
                State::Confirmed => return Ok(()),
                State::Rejected => {
                    return Err("the BMC rejected the Mahogany storage connection".into())
                }
                State::Unknown => return Err("Mahogany storage result unknown".into()),
                State::Waiting => {}
            }
            tokio::select! {
                changed = status_changed.changed() => {
                    changed.map_err(|_| "Mahogany storage status channel closed".to_string())?;
                }
                changed = peer_ready.changed() => {
                    changed.map_err(|_| "Mahogany storage peer stopped before setup".to_string())?;
                }
            }
        }
    }

    async fn unmount_target(&self, slot_id: &str) -> Result<(), String> {
        if slot_id != OPTICAL_SLOT_ID {
            return Err("Mahogany virtual media exposes only the optical target".to_string());
        }
        let control = self
            .control
            .as_ref()
            .filter(|control| control.storage_allowed)
            .cloned()
            .ok_or_else(|| {
                "the BMC account does not grant Mahogany storage privilege".to_string()
            })?;
        let _operation = self.operation.lock().await;
        if !self.inner.lock().await.connected {
            return Ok(());
        }
        let mut status_changed = self.subscribe_status();
        tokio::time::timeout(
            STORAGE_OPERATION_TIMEOUT,
            control.tcp_tx.send(build_storage_client_disconnect()),
        )
        .await
        .map_err(|_| "Mahogany storage disconnect queue timed out; result unknown".to_string())?
        .map_err(|error| format!("could not send Mahogany storage disconnect: {error}"))?;
        tokio::time::timeout(STORAGE_OPERATION_TIMEOUT, async {
            loop {
                status_changed
                    .changed()
                    .await
                    .map_err(|_| "Mahogany storage status channel closed".to_string())?;
                let status = self.inner.lock().await.storage_status.clone();
                if status
                    .as_ref()
                    .is_some_and(StorageStatus::any_disconnect_error)
                {
                    return Err("the BMC reported a Mahogany storage disconnect error".to_string());
                }
                if status
                    .as_ref()
                    .is_some_and(|status| !status.any_connected() && !status.any_disconnect_error())
                {
                    return Ok(());
                }
            }
        })
        .await
        .map_err(|_| "timed out waiting for Mahogany storage disconnect".to_string())??;
        let mut inner = self.inner.lock().await;
        inner.active_mount = None;
        inner.enabled = false;
        inner.image_size = 0;
        inner.attachment = inner.attachment.step(AttachmentEvent::Disconnected);
        inner.connected = inner.attachment.connected();
        drop(inner);
        self.notify_status_changed();
        Ok(())
    }

    fn notify_status_changed(&self) {
        self.status_tx
            .send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}

impl Default for MsdBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl MsdBackendTrait for MsdBackend {
    async fn capabilities(&self) -> MsdCapabilities {
        capabilities(self.storage_available())
    }

    async fn status(&self) -> Result<serde_json::Value, String> {
        let st = self.inner.lock().await.clone();
        Ok(msd_status_json(&st))
    }

    async fn attachment_snapshot(&self) -> Result<Option<Vec<MsdAttachment>>, String> {
        let inner = self.inner.lock().await;
        Ok(Some(msd_attachment_snapshot(&inner)))
    }

    async fn validate_media(&self, media: &MsdMedia) -> Result<(), String> {
        Self::validate_optical_media(media).await
    }

    async fn mount(&self, slot_id: &str, media: MsdMedia) -> Result<(), String> {
        self.mount_at(slot_id, media, 0).await
    }

    async fn unmount(&self, slot_id: &str) -> Result<(), String> {
        self.unmount_target(slot_id).await
    }
}

// ── Feature adapter ──────────────────────────────────────────────

use crate::pool::PoolLease;

pub struct MahoganyMsdFeature {
    lease: PoolLease,
}

impl Drop for MahoganyMsdFeature {
    fn drop(&mut self) {
        self.lease.entry().msd.disconnect_managed_best_effort();
    }
}

#[async_trait]
impl MsdBackendTrait for MahoganyMsdFeature {
    async fn capabilities(&self) -> MsdCapabilities {
        MsdBackendTrait::capabilities(self.lease.entry().msd.as_ref()).await
    }

    async fn status(&self) -> Result<serde_json::Value, String> {
        let st = self.lease.entry().msd.status().await;
        Ok(msd_status_json(&st))
    }

    async fn attachment_snapshot(&self) -> Result<Option<Vec<MsdAttachment>>, String> {
        let st = self.lease.entry().msd.status().await;
        Ok(Some(msd_attachment_snapshot(&st)))
    }

    async fn validate_media(&self, media: &MsdMedia) -> Result<(), String> {
        MsdBackendTrait::validate_media(self.lease.entry().msd.as_ref(), media).await
    }

    async fn mount(&self, slot_id: &str, media: MsdMedia) -> Result<(), String> {
        self.lease.entry().msd.mount_at(slot_id, media, 0).await
    }

    async fn unmount(&self, slot_id: &str) -> Result<(), String> {
        self.lease.entry().msd.unmount_target(slot_id).await
    }
}

impl MahoganyMsdFeature {
    pub fn new(lease: PoolLease) -> Self {
        Self { lease }
    }
}

fn msd_attachment_snapshot(st: &MsdInner) -> Vec<MsdAttachment> {
    st.connected
        .then(|| MsdAttachment {
            slot_id: OPTICAL_SLOT_ID.to_string(),
            mode: MsdMode::Image,
            media: None,
            media_type: Some(MsdMediaKind::Optical),
        })
        .into_iter()
        .collect()
}

fn msd_status_json(st: &MsdInner) -> serde_json::Value {
    let shares = st.storage_status.as_ref().map(|status| {
        serde_json::json!([
            storage_share_json(
                status.share_index0,
                status.share_type0,
                status.share_status0,
                &status.share_path0
            ),
            storage_share_json(
                status.share_index1,
                status.share_type1,
                status.share_status1,
                &status.share_path1
            ),
        ])
    });
    serde_json::json!({
        "connected": st.connected,
        "state": format!("{:?}", st.attachment),
        "enabled": st.enabled,
        "managed": st.active_mount.is_some(),
        "image_size": st.active_mount.as_ref().map_or(st.image_size, |mount| mount.size),
        "listener_port": st.active_mount.as_ref().map(|mount| mount.port),
        "peer_ready": st.active_mount.as_ref().is_some_and(|mount| mount.server.peer_ready()),
        "shares": shares,
    })
}

fn storage_share_json(
    index: u8,
    share_type: u8,
    status: StorageShareStatus,
    path: &str,
) -> serde_json::Value {
    serde_json::json!({
        "index": index,
        "type": share_type,
        "status": status.as_str(),
        "path": path,
    })
}

// ── MSD (Mass Storage Device) Protocol ─────────────────────────────
// Wire formats reverse-engineered from M2.JAR (ServerEngines iLO4 KVM).
//
// Messages flow on the MAIN KVM TCP connection:
//   Client→Server: StorageClientConnect (0x99)
//   Client→Server: StorageClientDisconnect (0x9A)
//   Server→Client: StorageStatus (0x89)

pub(crate) const MSG_STORAGE_CLIENT_CONNECT: u8 = 0x99; // Client → Server
pub(crate) const MSG_STORAGE_CLIENT_DISCONNECT: u8 = 0x9A; // Client → Server

/// Payload size for both StorageClientConnect and StorageStatus.
pub const STORAGE_MSG_PAYLOAD_LEN: usize = 1056;

pub const STORAGE_MSG_WIRE_LEN: usize = 1 + STORAGE_MSG_PAYLOAD_LEN; // 1057

// ── Status constants (from StorageStatus bytecode) ────────────────
impl StorageStatus {
    fn any_connected(&self) -> bool {
        self.share_status0 == StorageShareStatus::Connected
            || self.share_status1 == StorageShareStatus::Connected
    }

    fn any_failed(&self) -> bool {
        matches!(
            self.share_status0,
            StorageShareStatus::ConnectFailed
                | StorageShareStatus::DisconnectError
                | StorageShareStatus::Unknown
        ) || matches!(
            self.share_status1,
            StorageShareStatus::ConnectFailed
                | StorageShareStatus::DisconnectError
                | StorageShareStatus::Unknown
        )
    }

    fn any_disconnect_error(&self) -> bool {
        matches!(
            self.share_status0,
            StorageShareStatus::DisconnectError | StorageShareStatus::Unknown
        ) || matches!(
            self.share_status1,
            StorageShareStatus::DisconnectError | StorageShareStatus::Unknown
        )
    }
}

fn storage_ip_type(address: IpAddr) -> u8 {
    match address {
        IpAddr::V4(_) => 4,
        IpAddr::V6(_) => 6,
    }
}

fn encode_storage_address(address: IpAddr) -> [u8; 16] {
    match address {
        IpAddr::V4(address) => {
            let mut encoded = [0u8; 16];
            encoded[10..12].copy_from_slice(&[0xFF, 0xFF]);
            encoded[12..].copy_from_slice(&address.octets());
            encoded
        }
        IpAddr::V6(address) => address.octets(),
    }
}

#[cfg(test)]
const DEFAULT_STORAGE_PORT: u16 = 5901;

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

pub(crate) fn build_storage_client_connect(
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
#[cfg(test)]
fn build_storage_client_connect_default(ip_type: u8) -> Vec<u8> {
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

pub(crate) fn build_storage_client_disconnect() -> Vec<u8> {
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

    #[tokio::test]
    async fn cancelling_mount_requests_peer_release() {
        let (tcp_tx, mut tcp_rx) = mpsc::channel(8);
        let backend = Arc::new(MsdBackend::with_control(
            tcp_tx,
            SocketAddr::from(([127, 0, 0, 1], 40_000)),
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            true,
        ));
        let (_dir, media) = optical_media(&vec![0x5a; OPTICAL_BLOCK_SIZE as usize]);
        let task = tokio::spawn({
            let backend = backend.clone();
            async move { backend.mount_at(OPTICAL_SLOT_ID, media, 0).await }
        });
        assert_eq!(tcp_rx.recv().await.unwrap()[0], MSG_STORAGE_CLIENT_CONNECT);
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert_eq!(
            tcp_rx.recv().await.unwrap(),
            build_storage_client_disconnect()
        );
        assert!(!backend.status().await.enabled);
    }

    #[tokio::test]
    async fn unknown_storage_status_does_not_clear_a_possibly_active_attachment() {
        let backend = MsdBackend::new();
        backend
            .apply_storage_status(status_for(1234, StorageShareStatus::Connected))
            .await;
        backend
            .apply_storage_status(status_for(1234, StorageShareStatus::Unknown))
            .await;
        let status = backend.status().await;
        assert!(status.connected);
        assert_eq!(status.attachment, Attachment::UncertainAttached);
        assert_eq!(
            StorageShareStatus::from_byte(255),
            StorageShareStatus::Unknown
        );
    }

    #[tokio::test]
    async fn storage_status_updates_authoritative_attachment_and_notifies() {
        let backend = MsdBackend::new();
        let mut changed = backend.subscribe_status();
        let mut raw = vec![0u8; STORAGE_MSG_PAYLOAD_LEN];
        raw[16] = 3;
        raw[22] = 7;
        raw[24] = StorageShareStatus::Connected as u8;
        raw[25] = StorageShareStatus::NotConnected as u8;
        raw[20] = 3;
        raw[32..38].copy_from_slice(&encode_utf16le("iso"));

        backend
            .apply_storage_status(parse_storage_status(&raw).unwrap())
            .await;

        changed.changed().await.unwrap();
        let state = backend.status().await;
        assert!(state.connected);
        assert_eq!(state.storage_status.unwrap().share_path0, "iso");
        let attachments = MsdBackendTrait::attachment_snapshot(&backend)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(attachments.len(), 1);
        assert_eq!(attachments[0].slot_id, OPTICAL_SLOT_ID);
        assert_eq!(attachments[0].media_type, Some(MsdMediaKind::Optical));
    }

    #[tokio::test]
    async fn identical_storage_status_does_not_publish_a_second_revision() {
        let backend = MsdBackend::new();
        let raw = vec![4u8; STORAGE_MSG_PAYLOAD_LEN];
        let status = parse_storage_status(&raw).unwrap();
        let mut changed = backend.subscribe_status();
        backend.apply_storage_status(status.clone()).await;
        changed.changed().await.unwrap();
        let revision = *changed.borrow_and_update();
        backend.apply_storage_status(status).await;
        assert_eq!(*changed.borrow(), revision);
    }

    fn status_for(port: u16, status: StorageShareStatus) -> StorageStatus {
        StorageStatus {
            ip_address: encode_storage_address(IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)),
            share_index0: 0,
            share_index1: 0xFF,
            port,
            share_status0: status,
            share_status1: StorageShareStatus::NotConnected,
            share_type0: 0xFF,
            share_type1: 0xFF,
            ip_type: 4,
            share_path0: String::new(),
            share_path1: String::new(),
        }
    }

    fn optical_media(bytes: &[u8]) -> (tempfile::TempDir, MsdMedia) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("media.iso");
        std::fs::write(&path, bytes).unwrap();
        let file = Arc::new(std::fs::File::open(&path).unwrap());
        (
            dir,
            MsdMedia {
                source: MsdMediaSource::AggregatorImage {
                    path: path.to_string_lossy().into_owned(),
                    size: bytes.len() as u64,
                    file,
                },
                media_type: Some(MsdMediaKind::Optical),
            },
        )
    }

    #[tokio::test]
    async fn mount_waits_for_control_status_and_storage_peer_setup() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let (tcp_tx, mut tcp_rx) = mpsc::channel(8);
        let backend = Arc::new(MsdBackend::with_control(
            tcp_tx,
            SocketAddr::from(([127, 0, 0, 1], 40_000)),
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            true,
        ));
        let (_dir, media) = optical_media(&vec![0x5A; OPTICAL_BLOCK_SIZE as usize]);
        let mounted = {
            let backend = backend.clone();
            tokio::spawn(async move { backend.mount_at(OPTICAL_SLOT_ID, media, 0).await })
        };

        let connect = tcp_rx.recv().await.unwrap();
        assert_eq!(connect[0], MSG_STORAGE_CLIENT_CONNECT);
        let port = u16::from_le_bytes([connect[19], connect[20]]);
        assert_ne!(port, 0);
        assert_eq!(connect[25], 4);

        let mut peer = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let mut setup = [0u8; 544];
        setup[5] = 0xFE;
        peer.write_all(&setup).await.unwrap();
        let mut response = [0u8; 16];
        peer.read_exact(&mut response).await.unwrap();
        backend
            .apply_storage_status(status_for(port, StorageShareStatus::Connected))
            .await;

        mounted.await.unwrap().unwrap();
        backend
            .apply_storage_status(status_for(
                port.wrapping_add(1),
                StorageShareStatus::DisconnectedByRequest,
            ))
            .await;
        assert!(backend.status().await.active_mount.is_some());
        assert!(backend.status().await.connected);
        let capabilities = MsdBackendTrait::capabilities(backend.as_ref()).await;
        assert_eq!(capabilities.slots[0].id, OPTICAL_SLOT_ID);
        assert_eq!(capabilities.storage_locations.len(), 1);
        assert!(backend.status().await.enabled);

        let mut peer_state_changed = backend.subscribe_status();
        peer_state_changed.borrow_and_update();
        drop(peer);
        tokio::time::timeout(Duration::from_secs(1), peer_state_changed.changed())
            .await
            .expect("peer disconnect status event")
            .unwrap();
        let state = backend.status().await;
        assert!(state.connected);
        assert!(state.active_mount.is_some());

        let mut peer = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .unwrap();
        let mut setup = [0u8; 544];
        setup[5] = 0xFE;
        peer.write_all(&setup).await.unwrap();
        peer.read_exact(&mut response).await.unwrap();
        tokio::time::timeout(Duration::from_secs(1), peer_state_changed.changed())
            .await
            .expect("peer reconnect status event")
            .unwrap();

        backend.disconnect_managed_best_effort();
        assert_eq!(
            tcp_rx.recv().await.unwrap(),
            build_storage_client_disconnect()
        );

        let unmounted = {
            let backend = backend.clone();
            tokio::spawn(async move { backend.unmount_target(OPTICAL_SLOT_ID).await })
        };
        assert_eq!(
            tcp_rx.recv().await.unwrap(),
            build_storage_client_disconnect()
        );
        backend
            .apply_storage_status(status_for(port, StorageShareStatus::DisconnectError))
            .await;
        assert!(unmounted
            .await
            .unwrap()
            .unwrap_err()
            .contains("disconnect error"));
        let state = backend.status().await;
        assert!(state.connected);
        assert!(state.active_mount.is_some());

        let unmounted = {
            let backend = backend.clone();
            tokio::spawn(async move { backend.unmount_target(OPTICAL_SLOT_ID).await })
        };
        assert_eq!(
            tcp_rx.recv().await.unwrap(),
            build_storage_client_disconnect()
        );
        backend
            .apply_storage_status(status_for(port, StorageShareStatus::DisconnectedByRequest))
            .await;
        unmounted.await.unwrap().unwrap();
        drop(peer);
        assert!(!backend.status().await.connected);
        assert!(MsdBackendTrait::attachment_snapshot(backend.as_ref())
            .await
            .unwrap()
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn rejected_mount_rolls_back_listener_and_sends_disconnect() {
        let (tcp_tx, mut tcp_rx) = mpsc::channel(8);
        let backend = Arc::new(MsdBackend::with_control(
            tcp_tx,
            SocketAddr::from(([127, 0, 0, 1], 40_000)),
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            true,
        ));
        let (_dir, media) = optical_media(&vec![0; OPTICAL_BLOCK_SIZE as usize]);
        let mounted = {
            let backend = backend.clone();
            tokio::spawn(async move { backend.mount_at(OPTICAL_SLOT_ID, media, 0).await })
        };
        let connect = tcp_rx.recv().await.unwrap();
        let port = u16::from_le_bytes([connect[19], connect[20]]);
        backend
            .apply_storage_status(status_for(port, StorageShareStatus::ConnectFailed))
            .await;
        assert!(mounted.await.unwrap().unwrap_err().contains("rejected"));
        assert_eq!(
            tcp_rx.recv().await.unwrap(),
            build_storage_client_disconnect()
        );
        assert!(!backend.status().await.enabled);
    }

    #[tokio::test]
    async fn missing_storage_privilege_is_not_advertised() {
        let (tcp_tx, _tcp_rx) = mpsc::channel(1);
        let backend = MsdBackend::with_control(
            tcp_tx,
            SocketAddr::from(([127, 0, 0, 1], 40_000)),
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            false,
        );
        let capabilities = MsdBackendTrait::capabilities(&backend).await;
        assert!(capabilities.slots.is_empty());
        assert_eq!(capabilities.unavailable_storage_locations.len(), 1);
        assert_eq!(
            capabilities.unavailable_storage_locations[0].hint_key,
            "msd.mahoganyStoragePrivilegeUnavailableHint"
        );
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
