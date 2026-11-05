use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bytes::{Buf, BytesMut};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::broadcast;
use tokio::sync::{mpsc, watch, Mutex, Notify, RwLock};
use tracing::{debug, error, info, warn};

use crate::framebuffer::Framebuffer;
use crate::hid;
use crate::msd as msd_proto;
use crate::perf::PerfCounters;
use crate::protocol::*;
use bmc_backend_model::connection::{acquire, Acquire, Event as SessionEvent, Session, Slot};
use bmc_backend_model::control::{Control, Permission};
use bmc_feature_api::video::RawPixelFormat;
use gstreamer as gst;

fn recover_mutex<'a, T>(
    mutex: &'a std::sync::Mutex<T>,
    name: &str,
) -> std::sync::MutexGuard<'a, T> {
    mutex.lock().unwrap_or_else(|poisoned| {
        warn!("Mahogany pool mutex '{name}' was poisoned; recovering state");
        poisoned.into_inner()
    })
}

/// Pool key — uniquely identifies a BMC target connection.
#[derive(Clone, Hash, Eq, PartialEq, Debug)]
pub struct PoolKey {
    pub host: String,
    pub port: u16,
    pub kvm_port: u16,
    pub username: String,
    /// Non-reversible in-process discriminator.  It ensures a rotated
    /// password cannot reuse a TCP session authenticated with the old one.
    password_fingerprint: u64,
}

impl PoolKey {
    pub fn from_params(params: &ConnParams) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        params.password.hash(&mut hasher);
        Self {
            host: params.host.clone(),
            port: params.port,
            kvm_port: params.kvm_port.unwrap_or(params.port),
            username: params.username.clone(),
            password_fingerprint: hasher.finish(),
        }
    }
}

/// Connection parameters (extracted from DeviceConfig or FeatureConfig).
#[derive(Clone)]
pub struct ConnParams {
    pub id: String,
    pub host: String,
    pub port: u16,
    pub kvm_port: Option<u16>,
    pub username: String,
    pub password: String,
    pub max_fps: u32,
}

#[derive(Clone, Debug)]
pub struct MahoganyFrame {
    pub data: gst::Buffer,
    pub width: u32,
    pub height: u32,
    pub pixel_format: RawPixelFormat,
    pub trace_id: u64,
    pub created_at: std::time::Instant,
}

/// Information about the remote device (resolution + online status).
pub struct DeviceInfo {
    pub online: bool,
    pub width: u16,
    pub height: u16,
}

/// A single BMC TCP connection, shared by all features targeting
/// the same (host, port, username).
pub struct DeviceEntry {
    lifecycle: Arc<std::sync::Mutex<Session>>,
    pub(crate) keyboard_leds: Arc<AtomicU8>,
    pub(crate) tcp_tx: mpsc::Sender<Vec<u8>>,
    pub(crate) shutdown_tx: watch::Sender<bool>,
    pub(crate) disconnect_tx: watch::Sender<bool>,
    pub(crate) fb: Arc<Mutex<Framebuffer>>,
    pub raw_frame_tx: broadcast::Sender<MahoganyFrame>,
    pub(crate) msd: Arc<msd_proto::MsdBackend>,
    pub(crate) perf: Arc<PerfCounters>,
    control: Arc<std::sync::Mutex<Control>>,
    pub(crate) control_notify: Arc<Notify>,
    pub(crate) firmware_version: Arc<RwLock<Option<String>>>,
    pub(crate) firmware_tx: watch::Sender<u64>,
    _shutdown_rx: watch::Receiver<bool>,
}

impl DeviceEntry {
    pub(crate) fn is_online(&self) -> bool {
        recover_mutex(&self.lifecycle, "session").accepts_commands()
    }

    pub fn keyboard_led_mask(&self) -> u8 {
        self.keyboard_leds.load(Ordering::SeqCst)
    }
    pub fn subscribe_disconnect(&self) -> watch::Receiver<bool> {
        let mut receiver = self.disconnect_tx.subscribe();
        let terminal = *receiver.borrow();
        if terminal {
            receiver.mark_changed();
        }
        receiver
    }
    pub fn subscribe_msd_status(&self) -> watch::Receiver<u64> {
        self.msd.subscribe_status()
    }
    pub fn subscribe_firmware(&self) -> watch::Receiver<u64> {
        self.firmware_tx.subscribe()
    }
    pub async fn firmware_version(&self) -> Option<String> {
        self.firmware_version.read().await.clone()
    }
    pub fn perf_snapshot(&self) -> serde_json::Value {
        self.perf.snapshot()
    }

    pub async fn request_full_frame(&self) -> bool {
        let f = self.fb.lock().await;
        let width = f.width;
        let height = f.height;
        drop(f);
        request_full_frame_on(&self.tcp_tx, width, height)
    }

    pub async fn publish_snapshot(&self) -> Result<bool, String> {
        let mut f = self.fb.lock().await;
        let buf = f.snapshot_or_blank()?;
        let width = f.width.into();
        let height = f.height.into();
        let pixel_format = Framebuffer::PIXEL_FORMAT;
        drop(f);

        let _ = self.raw_frame_tx.send(MahoganyFrame {
            data: buf,
            width,
            height,
            pixel_format,
            trace_id: self.perf.next_trace_id(),
            created_at: std::time::Instant::now(),
        });
        Ok(true)
    }

    pub async fn wait_for_keyboard_control(&self, timeout: Duration) -> Result<(), String> {
        let origin = tokio::time::Instant::now();
        let _ = self.request_primary_control();
        loop {
            let state = *recover_mutex(&self.control, "control");
            match state.permission(origin.elapsed(), timeout) {
                Permission::Allow => return Ok(()),
                Permission::Deny => return Err("BMC keyboard privilege is disabled".into()),
                Permission::TimedOut => {
                    return Err("BMC full control was not granted before deadline".into())
                }
                Permission::Wait => {}
            }
            tokio::select! {
                _ = self.control_notify.notified() => {}
                _ = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
    }

    pub fn request_primary_control(&self) -> bool {
        match self.tcp_tx.try_send(build_request_primary_control()) {
            Ok(()) => true,
            Err(e) => {
                warn!("primary-control request failed: {}", e);
                false
            }
        }
    }
}

/// Thread-safe pool keyed by the complete transport/authentication identity.
pub struct DevicePool {
    inner: std::sync::Mutex<HashMap<PoolKey, PoolSlot>>,
    connect_locks: std::sync::Mutex<HashMap<PoolKey, Arc<ConnectLockEntry>>>,
}

/// Global mahogany device pool shared by all feature modules.
pub static POOL: std::sync::LazyLock<DevicePool> = std::sync::LazyLock::new(DevicePool::new);

struct PoolSlot {
    entry: Arc<DeviceEntry>,
    leases: usize,
}

struct ConnectLockEntry {
    mutex: tokio::sync::Mutex<()>,
    waiters: AtomicU64,
}

impl ConnectLockEntry {
    fn new() -> Self {
        Self {
            mutex: tokio::sync::Mutex::new(()),
            waiters: AtomicU64::new(0),
        }
    }
}

/// Lease for a pooled Mahogany TCP connection.
///
/// Feature adapters hold this in backend runtime. Dropping the last lease
/// removes the pool slot and shuts down the TCP tasks.
pub struct PoolLease {
    key: PoolKey,
    entry: Arc<DeviceEntry>,
    pool: &'static DevicePool,
}

impl PoolLease {
    pub fn entry(&self) -> &Arc<DeviceEntry> {
        &self.entry
    }
}

impl Drop for PoolLease {
    fn drop(&mut self) {
        self.pool.release(&self.key, &self.entry);
    }
}

struct ConnectWaiter {
    pool: &'static DevicePool,
    key: PoolKey,
    entry: Arc<ConnectLockEntry>,
}
impl Drop for ConnectWaiter {
    fn drop(&mut self) {
        self.pool.release_connect_lock(&self.key, &self.entry);
    }
}

impl DevicePool {
    pub fn new() -> Self {
        Self {
            inner: std::sync::Mutex::new(HashMap::new()),
            connect_locks: std::sync::Mutex::new(HashMap::new()),
        }
    }

    /// Get or create a connection.
    pub async fn get_or_connect(
        &'static self,
        key: &PoolKey,
        params: &ConnParams,
    ) -> Result<PoolLease, anyhow::Error> {
        {
            let mut map = recover_mutex(&self.inner, "entries");
            let snapshot = match map.get(key) {
                Some(slot) => {
                    if slot.entry.is_online() {
                        Slot::Online
                    } else {
                        Slot::Disconnected
                    }
                }
                None => Slot::Vacant,
            };
            if acquire(snapshot) == Acquire::Lease {
                let slot = map.get_mut(key).expect("model admitted existing slot");
                slot.leases += 1;
                return Ok(PoolLease {
                    key: key.clone(),
                    entry: slot.entry.clone(),
                    pool: self,
                });
            }
        }

        let connect_lock = {
            let mut locks = recover_mutex(&self.connect_locks, "connect_locks");
            let entry = locks
                .entry(key.clone())
                .or_insert_with(|| Arc::new(ConnectLockEntry::new()))
                .clone();
            entry.waiters.fetch_add(1, Ordering::SeqCst);
            entry
        };
        let _waiter = ConnectWaiter {
            pool: self,
            key: key.clone(),
            entry: connect_lock.clone(),
        };
        self.get_or_connect_with_lock(key, params, &connect_lock)
            .await
    }

    async fn get_or_connect_with_lock(
        &'static self,
        key: &PoolKey,
        params: &ConnParams,
        connect_lock: &ConnectLockEntry,
    ) -> Result<PoolLease, anyhow::Error> {
        let _connect_guard = connect_lock.mutex.lock().await;

        {
            let mut map = recover_mutex(&self.inner, "entries");
            let snapshot = match map.get(key) {
                Some(slot) => {
                    if slot.entry.is_online() {
                        Slot::Online
                    } else {
                        Slot::Disconnected
                    }
                }
                None => Slot::Vacant,
            };
            if acquire(snapshot) == Acquire::Lease {
                let slot = map.get_mut(key).expect("model admitted existing slot");
                slot.leases += 1;
                return Ok(PoolLease {
                    key: key.clone(),
                    entry: slot.entry.clone(),
                    pool: self,
                });
            }
        }

        let entry = connect_new(params).await?;
        let mut map = recover_mutex(&self.inner, "entries");
        map.insert(
            key.clone(),
            PoolSlot {
                entry: entry.clone(),
                leases: 1,
            },
        );
        Ok(PoolLease {
            key: key.clone(),
            entry,
            pool: self,
        })
    }

    fn release_connect_lock(&self, key: &PoolKey, entry: &Arc<ConnectLockEntry>) {
        if entry.waiters.fetch_sub(1, Ordering::SeqCst) != 1 {
            return;
        }
        let mut locks = recover_mutex(&self.connect_locks, "connect_locks");
        if locks.get(key).is_some_and(|current| {
            Arc::ptr_eq(current, entry) && entry.waiters.load(Ordering::SeqCst) == 0
        }) {
            locks.remove(key);
        }
    }

    /// Remove a pool entry because the underlying TCP connection died.
    pub fn disconnect(&self, key: &PoolKey, entry: &Arc<DeviceEntry>) {
        let mut map = recover_mutex(&self.inner, "entries");
        if map
            .get(key)
            .is_some_and(|slot| Arc::ptr_eq(&slot.entry, entry))
        {
            if let Some(slot) = map.remove(key) {
                let _ = slot.entry.shutdown_tx.send(true);
            }
        }
    }

    fn release(&self, key: &PoolKey, entry: &Arc<DeviceEntry>) {
        let mut map = recover_mutex(&self.inner, "entries");
        let Some(slot) = map.get_mut(key) else {
            return;
        };
        if !Arc::ptr_eq(&slot.entry, entry) {
            return;
        }
        slot.leases = slot.leases.saturating_sub(1);
        if slot.leases == 0 {
            let Some(slot) = map.remove(key) else {
                return;
            };
            let _ = slot.entry.shutdown_tx.send(true);
        }
    }
}

impl Default for DevicePool {
    fn default() -> Self {
        Self::new()
    }
}

// ── Connection lifecycle ────────────────────────────────────────

struct PendingConnection(Option<watch::Sender<bool>>);
impl Drop for PendingConnection {
    fn drop(&mut self) {
        if let Some(shutdown) = self.0.take() {
            let _ = shutdown.send(true);
        }
    }
}

async fn connect_new(params: &ConnParams) -> Result<Arc<DeviceEntry>, anyhow::Error> {
    tokio::time::timeout(Duration::from_secs(30), connect_new_inner(params))
        .await
        .map_err(|_| anyhow::anyhow!("Mahogany negotiation timed out"))?
}

async fn connect_new_inner(params: &ConnParams) -> Result<Arc<DeviceEntry>, anyhow::Error> {
    use bmc_backend_model::negotiation::{Event, State};
    let mut negotiation = State::Greeting;
    let body = super::auth::fetch_bmc_page(
        &params.host,
        params.port,
        &params.username,
        &params.password,
    )
    .await?;
    let httpdata = extract_httpdata(&body)
        .ok_or_else(|| anyhow::anyhow!("httpdata not found in BMC response"))?;
    info!(
        "[{}] httpdata obtained ({} chars)",
        params.id,
        httpdata.len()
    );

    let kvm_port = params
        .kvm_port
        .or_else(|| extract_kvm_port(&body))
        .unwrap_or(params.port);
    info!("[{}] using KVM port {}", params.id, kvm_port);

    let kvm_addr = bmc_backend_support::endpoint::authority(&params.host, kvm_port)
        .map_err(anyhow::Error::msg)?;
    let stream = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::net::TcpStream::connect(&kvm_addr),
    )
    .await
    .map_err(|_| anyhow::anyhow!("TCP connect timeout to {} (10s)", kvm_addr))?
    .map_err(|e| anyhow::anyhow!("TCP connect error to {}: {}", kvm_addr, e))?;
    info!("[{}] TCP connected to {}", params.id, kvm_addr);
    set_tcp_keepalive(&stream)?;
    let local_addr = stream.local_addr()?;
    let peer_ip = stream.peer_addr()?.ip();
    info!("[{}] connected from {}", params.id, local_addr);
    let (mut read_half, mut write_half) = stream.into_split();

    // NOP
    write_half.write_all(&hid::encode_nop()).await?;
    info!("[{}] NOP (0xD2) sent...", params.id);

    // Server handshake
    let mut type_buf = [0u8; 1];
    tokio::time::timeout(Duration::from_secs(10), read_half.read_exact(&mut type_buf))
        .await
        .map_err(|_| anyhow::anyhow!("timeout waiting for server response"))?
        .map_err(|e| anyhow::anyhow!("read type byte error: {}", e))?;

    let server_privileges = match type_buf[0] {
        msg::SERVER_HANDSHAKE => {
            let mut rest = [0u8; 32];
            read_half
                .read_exact(&mut rest)
                .await
                .map_err(|e| anyhow::anyhow!("read ServerHandshake payload error: {}", e))?;
            let privileges = parse_server_privileges(&rest);
            info!(
                "[{}] ServerHandshake: privilege='{}' keyboard={} video={} mouse={} storage={}",
                params.id,
                privileges.raw,
                privileges.keyboard,
                privileges.video,
                privileges.mouse,
                privileges.storage
            );
            privileges
        }
        msg::SERVER_DISCONNECT => {
            let mut reason_buf = [0u8; 4];
            read_half.read_exact(&mut reason_buf).await?;
            let reason = u32::from_le_bytes(reason_buf);
            let mut len_buf = [0u8; 2];
            read_half.read_exact(&mut len_buf).await?;
            let msg_len = u16::from_le_bytes(len_buf) as usize;
            let mut msg_body = vec![0u8; msg_len];
            if msg_len > 0 {
                read_half.read_exact(&mut msg_body).await?;
            }
            anyhow::bail!(
                "server disconnected: reason={}, message='{}'",
                reason,
                String::from_utf8_lossy(&msg_body).trim_matches(char::from(0))
            );
        }
        other => {
            let mut buf = vec![other];
            let mut more = [0u8; 255];
            let n = read_half.read(&mut more).await?;
            buf.extend_from_slice(&more[..n]);
            anyhow::bail!(
                "unexpected first message type 0x{:02x} ({} bytes): {:02x?}",
                other,
                buf.len(),
                buf
            );
        }
    };

    negotiation = negotiation.step(Event::GreetingAccepted);

    // Client handshake
    let handshake = build_client_handshake(&params.username, &httpdata);
    write_half.write_all(&handshake).await?;
    info!(
        "[{}] ClientHandshake sent ({} bytes)",
        params.id,
        handshake.len()
    );

    negotiation = negotiation.step(Event::Authenticated);

    // Control messages
    write_half
        .write_all(&build_request_primary_control())
        .await?;
    write_half.write_all(&build_absolute_mode(true)).await?;
    write_half
        .write_all(&build_inform_hlevel_compression(false))
        .await?;
    info!("[{}] control messages sent", params.id);

    negotiation = negotiation.step(Event::Initialized);
    anyhow::ensure!(
        negotiation == State::Online,
        "invalid Mahogany negotiation state"
    );

    // Init shared state
    let pool_size = (params.max_fps / 10).clamp(3, 8) as usize;
    let fb = Arc::new(Mutex::new(Framebuffer::new(1024, 768, pool_size)));
    let info = Arc::new(RwLock::new(DeviceInfo {
        online: true,
        width: 1024,
        height: 768,
    }));
    let (tcp_tx, tcp_rx) = mpsc::channel(32);
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let (disconnect_tx, _) = watch::channel(false);
    let (raw_frame_tx, _) = broadcast::channel(16);
    let perf = Arc::new(PerfCounters::new());
    let frame_notify = Arc::new(Notify::new());
    let control = Arc::new(std::sync::Mutex::new(Control::new(
        server_privileges.keyboard,
    )));
    let keyboard_leds = Arc::new(AtomicU8::new(0));
    let control_notify = Arc::new(Notify::new());
    let msd = Arc::new(msd_proto::MsdBackend::with_control(
        tcp_tx.clone(),
        local_addr,
        peer_ip,
        server_privileges.storage,
    ));
    let firmware_version = Arc::new(RwLock::new(None));
    let (firmware_tx, _) = watch::channel(0);

    let lifecycle = Arc::new(std::sync::Mutex::new(
        Session::Connecting.step(SessionEvent::Established),
    ));
    let entry = Arc::new(DeviceEntry {
        lifecycle: lifecycle.clone(),
        tcp_tx: tcp_tx.clone(),
        shutdown_tx: shutdown_tx.clone(),
        disconnect_tx: disconnect_tx.clone(),
        fb: fb.clone(),
        raw_frame_tx: raw_frame_tx.clone(),
        msd: msd.clone(),
        perf: perf.clone(),
        control: control.clone(),
        control_notify: control_notify.clone(),
        keyboard_leds: keyboard_leds.clone(),
        firmware_version: firmware_version.clone(),
        firmware_tx: firmware_tx.clone(),
        _shutdown_rx: shutdown_rx.clone(),
    });

    let ctx = PoolTaskContext {
        lifecycle,
        fb,
        info,
        tcp_tx: tcp_tx.clone(),
        perf,
        frame_notify,
        raw_frame_tx,
        control,
        control_notify,
        keyboard_leds,
        msd,
        firmware_version,
        firmware_tx,
        shutdown_rx: shutdown_rx.clone(),
        shutdown_tx: shutdown_tx.clone(),
        disconnect_tx: disconnect_tx.clone(),
        max_fps: params.max_fps,
        device_id: params.id.clone(),
    };

    let mut pending_connection = PendingConnection(Some(shutdown_tx.clone()));
    spawn_writer(tcp_rx, write_half, &ctx);
    spawn_framebuffer_task(&ctx);
    spawn_keepalive_task(tcp_tx, ctx.shutdown_rx.clone());
    spawn_reader(read_half, &ctx);
    let invalidate_requested = entry.request_full_frame().await;
    info!(
        "[{}] initial framebuffer invalidate requested={}",
        params.id, invalidate_requested
    );

    pending_connection.0 = None;
    Ok(entry)
}

fn set_tcp_keepalive(stream: &tokio::net::TcpStream) -> std::io::Result<()> {
    let sock_ref = socket2::SockRef::from(stream);
    sock_ref.set_keepalive(true)?;
    sock_ref.set_tcp_keepalive(
        &socket2::TcpKeepalive::new()
            .with_time(Duration::from_secs(5))
            .with_interval(Duration::from_secs(3)),
    )?;
    Ok(())
}

// ── Background tasks ────────────────────────────────────────────

struct PoolTaskContext {
    lifecycle: Arc<std::sync::Mutex<Session>>,
    shutdown_tx: watch::Sender<bool>,
    fb: Arc<Mutex<Framebuffer>>,
    info: Arc<RwLock<DeviceInfo>>,
    tcp_tx: mpsc::Sender<Vec<u8>>,
    perf: Arc<PerfCounters>,
    frame_notify: Arc<Notify>,
    raw_frame_tx: broadcast::Sender<MahoganyFrame>,
    control: Arc<std::sync::Mutex<Control>>,
    control_notify: Arc<Notify>,
    keyboard_leds: Arc<AtomicU8>,
    msd: Arc<msd_proto::MsdBackend>,
    firmware_version: Arc<RwLock<Option<String>>>,
    firmware_tx: watch::Sender<u64>,
    shutdown_rx: watch::Receiver<bool>,
    disconnect_tx: watch::Sender<bool>,
    max_fps: u32,
    device_id: String,
}

fn spawn_writer(
    mut tcp_rx: mpsc::Receiver<Vec<u8>>,
    mut write_half: tokio::net::tcp::OwnedWriteHalf,
    ctx: &PoolTaskContext,
) {
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    let disconnect_tx = ctx.disconnect_tx.clone();
    let lifecycle = ctx.lifecycle.clone();
    let shutdown_tx = ctx.shutdown_tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = tcp_rx.recv() => {
                    match msg {
                        Some(data) => {
                            if !matches!(tokio::time::timeout(Duration::from_secs(5), write_half.write_all(&data)).await, Ok(Ok(()))) {
                                warn!("Mahogany TCP write failed or timed out; result unknown");
                                let mut state = recover_mutex(&lifecycle, "session");
                                *state = state.step(SessionEvent::Fault);
                                drop(state);
                                let _ = disconnect_tx.send(true);
                                let _ = shutdown_tx.send(true);
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = shutdown_rx.changed() => break,
            }
        }
        let mut state = recover_mutex(&lifecycle, "session");
        *state = state.step(SessionEvent::Stop);
        drop(state);
        let _ = shutdown_tx.send(true);
        debug!("writer task exited");
    });
}

fn spawn_framebuffer_task(ctx: &PoolTaskContext) {
    let fb = ctx.fb.clone();
    let perf = ctx.perf.clone();
    let raw_frame_tx = ctx.raw_frame_tx.clone();
    let notify = ctx.frame_notify.clone();
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    let disconnect_tx = ctx.disconnect_tx.clone();
    let min_interval = Duration::from_micros((1_000_000 / ctx.max_fps.max(1)) as u64);
    tokio::spawn(async move {
        let mut last_push = std::time::Instant::now();
        let mut first_frame = true;
        loop {
            tokio::select! {
                _ = notify.notified() => {
                    if !first_frame {
                        let elapsed = last_push.elapsed();
                        if elapsed < min_interval {
                            tokio::time::sleep(min_interval - elapsed).await;
                        }
                    }
                    first_frame = false;
                    let mut f = fb.lock().await;
                    if f.dirty {
                        f.dirty = false;
                        f.frame_count += 1;
                        let pipeline_us = f.frame_start.take()
                            .map(|t| t.elapsed().as_micros() as u64)
                            .unwrap_or(0);
                        if pipeline_us > 0 {
                            perf.pipeline_total_us.fetch_add(pipeline_us, Ordering::Relaxed);
                            perf.pipeline_count.fetch_add(1, Ordering::Relaxed);
                            if pipeline_us > perf.pipeline_max_us.load(Ordering::Relaxed) {
                                perf.pipeline_max_us.store(pipeline_us, Ordering::Relaxed);
                            }
                        }
                        let snap = std::time::Instant::now();
                        let buf = match f.try_finish_frame() {
                            Ok(buffer) => buffer,
                            Err(error) => {
                                warn!("framebuffer snapshot failed: {error}");
                                let _ = disconnect_tx.send(true);
                                break;
                            }
                        };
                        let w = f.width;
                        let h = f.height;
                        let pixel_format = Framebuffer::PIXEL_FORMAT;
                        let _ = raw_frame_tx.send(MahoganyFrame {
                            data: buf,
                            width: w.into(),
                            height: h.into(),
                            pixel_format,
                            trace_id: perf.next_trace_id(),
                            created_at: std::time::Instant::now(),
                        });
                        perf.framebuffer_snapshot_us.store(
                            snap.elapsed().as_micros() as u64, Ordering::Relaxed,
                        );
                        last_push = std::time::Instant::now();
                    }
                }
                _ = shutdown_rx.changed() => break,
            }
        }
        debug!("framebuffer task exited");
    });
}

fn spawn_keepalive_task(tcp_tx: mpsc::Sender<Vec<u8>>, mut shutdown_rx: watch::Receiver<bool>) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let _ = tcp_tx.try_send(hid::encode_nop());
                }
                _ = shutdown_rx.changed() => break,
            }
        }
    });
}

fn spawn_reader(mut read_half: tokio::net::tcp::OwnedReadHalf, ctx: &PoolTaskContext) {
    const MAX_READ_BUFFER_BYTES: usize = crate::protocol::MAX_SERVER_MESSAGE_BYTES;
    let fb = ctx.fb.clone();
    let info = ctx.info.clone();
    let tcp_tx = ctx.tcp_tx.clone();
    let perf = ctx.perf.clone();
    let control = ctx.control.clone();
    let control_notify = ctx.control_notify.clone();
    let keyboard_leds = ctx.keyboard_leds.clone();
    let msd = ctx.msd.clone();
    let firmware_version = ctx.firmware_version.clone();
    let firmware_tx = ctx.firmware_tx.clone();
    let device_id = ctx.device_id.clone();
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    let disconnect_tx = ctx.disconnect_tx.clone();
    let lifecycle = ctx.lifecycle.clone();
    let shutdown_tx = ctx.shutdown_tx.clone();
    let notify = ctx.frame_notify.clone();
    tokio::spawn(async move {
        let mut read_buf = BytesMut::with_capacity(65536);
        let origin = tokio::time::Instant::now();
        let mut record =
            bmc_backend_model::stream::Record::new(Duration::from_secs(10), MAX_READ_BUFFER_BYTES);
        'reader: loop {
            record = record.step(read_buf.len(), origin.elapsed());
            if record.state() == bmc_backend_model::stream::State::Failed {
                break;
            }
            let deadline = record.deadline();
            tokio::select! {
                _ = async { match deadline {
                    Some(deadline) => tokio::time::sleep_until(origin + deadline).await,
                    None => core::future::pending().await,
                } } => { warn!("Mahogany partial record timed out"); break; }
                result = read_half.read_buf(&mut read_buf) => {
                    match result {
                        Ok(0) => {
                            warn!("[{}] BMC connection closed", device_id);
                            break;
                        }
                        Ok(_) => {
                            record = record.step(read_buf.len(), origin.elapsed());
                            if record.state() == bmc_backend_model::stream::State::Failed { break; }
                            if read_buf.len() > MAX_READ_BUFFER_BYTES {
                                warn!(
                                    "[{}] protocol read buffer exceeded {} bytes; disconnecting",
                                    device_id, MAX_READ_BUFFER_BYTES
                                );
                                break;
                            }
                            loop {
                                let buf_len = read_buf.len();
                                if buf_len == 0 { break; }
                                match try_parse_message(&read_buf[..buf_len]) {
                                    Ok(Some((msg, consumed))) => {
                                        read_buf.advance(consumed);
                                        record = record.step(0, origin.elapsed()).step(read_buf.len(), origin.elapsed());
                                        if let Err(e) = handle_protocol_message(
                                            msg,
                                            &fb,
                                            &info,
                                            &tcp_tx,
                                            &perf,
                                            &notify,
                                            &control,
                                            &control_notify,
                                            &keyboard_leds,
                                            &msd,
                                            &firmware_version,
                                            &firmware_tx,
                                        ).await {
                                            warn!("handle_message error: {}", e);
                                            break 'reader;
                                        }
                                    }
                                    Ok(None) => break,
                                    Err(e) => {
                                        warn!("protocol parse error: {}", e);
                                        break 'reader;
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            error!("read error: {}", e);
                            break;
                        }
                    }
                }
                _ = shutdown_rx.changed() => break,
            }
        }
        {
            let mut state = recover_mutex(&lifecycle, "session");
            *state = state.step(if *shutdown_rx.borrow() {
                SessionEvent::Stop
            } else {
                SessionEvent::Fault
            });
        }
        let _ = shutdown_tx.send(true);
        let mut i = info.write().await;
        i.online = false;
        let _ = disconnect_tx.send(true);
        debug!("[{}] read loop exited", device_id);
    });
}

// ── Protocol message dispatch ───────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn handle_protocol_message(
    msg: ServerMessage,
    fb: &Arc<Mutex<Framebuffer>>,
    info: &Arc<RwLock<DeviceInfo>>,
    tcp_tx: &mpsc::Sender<Vec<u8>>,
    perf: &PerfCounters,
    notify: &Arc<Notify>,
    control: &std::sync::Mutex<Control>,
    control_notify: &Notify,
    keyboard_leds: &AtomicU8,
    msd: &msd_proto::MsdBackend,
    firmware_version: &RwLock<Option<String>>,
    firmware_tx: &watch::Sender<u64>,
) -> Result<(), anyhow::Error> {
    match msg {
        ServerMessage::KeyIndicators {
            num_lock,
            caps_lock,
            scroll_lock,
        } => {
            keyboard_leds.store(
                u8::from(num_lock) | (u8::from(caps_lock) << 1) | (u8::from(scroll_lock) << 2),
                Ordering::SeqCst,
            );
        }
        ServerMessage::Nop => {}
        ServerMessage::MultiUserState(state) => {
            let request = {
                let mut current = recover_mutex(control, "control");
                let (next, request) =
                    current.observe(state.has_full_control, state.can_take_full_control);
                *current = next;
                request
            };
            if request {
                let _ = request_primary_control_on(tcp_tx);
            }
            control_notify.notify_waiters();
        }
        ServerMessage::DisplayParameters { width, height } => {
            info!("display parameters: {}x{}", width, height);
            let mut f = fb.lock().await;
            let changed = f.try_resize(width, height).map_err(anyhow::Error::msg)?;
            drop(f);
            let mut i = info.write().await;
            i.width = width;
            i.height = height;
            drop(i);
            if changed {
                let invalidate_requested = request_full_frame_on(tcp_tx, width, height);
                info!(
                    "display changed to {}x{}, framebuffer invalidate requested={}",
                    width, height, invalidate_requested
                );
            }
        }
        ServerMessage::StorageStatus(status) => {
            debug!("StorageStatus: {:?}", status);
            msd.apply_storage_status(status).await;
        }
        ServerMessage::FirmwareVersion(version) => {
            let mut current = firmware_version.write().await;
            if current.as_ref() != Some(&version) {
                *current = Some(version);
                drop(current);
                firmware_tx.send_modify(|revision| *revision = revision.wrapping_add(1));
            }
        }
        ServerMessage::FramebufferUpdate { sub_type, rects } => {
            let mut f = fb.lock().await;
            debug!(
                "FramebufferUpdate: sub_type=0x{:04x}, {} rects",
                sub_type,
                rects.len()
            );
            let was_clean = !f.dirty;
            perf.tile_count
                .fetch_add(rects.len() as u64, Ordering::Relaxed);
            for rect in &rects {
                f.try_blit_tile(rect.x, rect.y, &rect.raw_data, rect.w, rect.h)
                    .map_err(anyhow::Error::msg)?;
            }
            drop(f);
            if was_clean {
                notify.notify_one();
            }
        }
        ServerMessage::EnhanceBltRaw {
            bilt_type,
            tile_w,
            tile_h,
            snoop_data,
            scrunch_data,
            raw_size,
            scrunch_size: _,
        } => {
            let f = fb.lock().await;
            let cw = f.width;
            let ch = f.height;
            drop(f);
            let rects = decode_enhance_bitblt_tiles(
                bilt_type,
                tile_w,
                tile_h,
                &snoop_data,
                &scrunch_data,
                raw_size,
                cw,
                ch,
            )?;
            let mut f = fb.lock().await;
            debug!("EnhanceBitBlt: sub_type=0x{:04x}, {} rects (tiles {}x{}, canvas {}x{}, scrunch {}B)",
                bilt_type, rects.len(), tile_w, tile_h, cw, ch, scrunch_data.len());
            let was_clean = !f.dirty;
            perf.tile_count
                .fetch_add(rects.len() as u64, Ordering::Relaxed);
            for rect in &rects {
                f.try_blit_tile(rect.x, rect.y, &rect.raw_data, rect.w, rect.h)
                    .map_err(anyhow::Error::msg)?;
            }
            drop(f);
            if was_clean {
                notify.notify_one();
            }
        }
        ServerMessage::Other { type_id, .. } => {
            if bmc_backend_model::control::unimplemented_message(type_id)
                == bmc_backend_model::control::MessagePolicy::Reject
            {
                anyhow::bail!("unsupported Mahogany server message 0x{type_id:02x}");
            }
        }
    }
    Ok(())
}

fn request_primary_control_on(tcp_tx: &mpsc::Sender<Vec<u8>>) -> bool {
    match tcp_tx.try_send(build_request_primary_control()) {
        Ok(()) => true,
        Err(e) => {
            warn!("primary-control request failed: {}", e);
            false
        }
    }
}

fn request_full_frame_on(tcp_tx: &mpsc::Sender<Vec<u8>>, width: u16, height: u16) -> bool {
    match tcp_tx.try_send(build_invalidate_full_frame(width, height)) {
        Ok(()) => true,
        Err(e) => {
            warn!("framebuffer invalidate request failed: {}", e);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ConnParams, PoolKey};

    fn params(kvm_port: Option<u16>, password: &str) -> ConnParams {
        ConnParams {
            id: "device-a".to_string(),
            host: "bmc.example.test".to_string(),
            port: 443,
            kvm_port,
            username: "admin".to_string(),
            password: password.to_string(),
            max_fps: 30,
        }
    }

    #[test]
    fn pool_key_keeps_transport_and_credential_identity_separate() {
        let default_kvm = PoolKey::from_params(&params(None, "first-secret"));
        let explicit_default_kvm = PoolKey::from_params(&params(Some(443), "first-secret"));
        let alternate_kvm = PoolKey::from_params(&params(Some(17990), "first-secret"));
        let rotated_password = PoolKey::from_params(&params(None, "second-secret"));

        assert_eq!(default_kvm, explicit_default_kvm);
        assert_ne!(default_kvm, alternate_kvm);
        assert_ne!(default_kvm, rotated_password);
    }
}
