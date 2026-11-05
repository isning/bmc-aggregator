use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};

use bmc_backend_model::connection::{acquire, Acquire, Event as SessionEvent, Session, Slot};
use bytes::BytesMut;
use gstreamer as gst;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{broadcast, mpsc, watch};

use bmc_feature_api::video::RawPixelFormat;

use super::protocol::{
    connect_and_handshake, framebuffer_update_request, key_event, pointer_event,
    try_parse_server_message, validate_dimensions, HandshakeInfo, Rectangle, ServerMessage,
    VncSecurity,
};

const COMMAND_QUEUE_CAPACITY: usize = 128;
const FRAME_BROADCAST_CAPACITY: usize = 8;
const MAX_READ_BUFFER: usize = 2 * 64 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct VncConfig {
    pub host: String,
    pub port: u16,
    pub password: Option<String>,
    pub security: VncSecurity,
}

impl VncConfig {
    pub fn new(
        host: String,
        port: u16,
        password: Option<String>,
        security: Option<&str>,
    ) -> Result<Self, String> {
        if host.trim().is_empty() {
            return Err("VNC host is empty".to_string());
        }
        let security = VncSecurity::parse(security)?;
        // Most BMCs use their normal account password for direct VNC.  Keep a
        // VNC-specific override for systems that use a separate console
        // credential, while allowing video and HID to share one session from
        // the device-level credential by default.
        if security == VncSecurity::VncAuth
            && password.as_deref().is_none_or(|value| value.is_empty())
        {
            return Err("a VNC password is required for vnc-auth".to_string());
        }
        Ok(Self {
            host,
            port,
            password,
            security,
        })
    }
}

#[derive(Clone, Hash, Eq, PartialEq, Debug)]
struct VncPoolKey {
    host: String,
    port: u16,
    security: VncSecurity,
    password_fingerprint: u64,
}

impl VncPoolKey {
    fn from_config(config: &VncConfig) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        config.password.hash(&mut hasher);
        Self {
            host: config.host.clone(),
            port: config.port,
            security: config.security,
            password_fingerprint: hasher.finish(),
        }
    }
}

enum PoolSlot {
    Connecting(Arc<watch::Sender<bool>>),
    Ready(Arc<VncSession>),
}

static VNC_POOL: LazyLock<Mutex<HashMap<VncPoolKey, PoolSlot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_pool() -> MutexGuard<'static, HashMap<VncPoolKey, PoolSlot>> {
    VNC_POOL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[derive(Clone, Debug)]
pub struct VncFrame {
    /// A ref-counted GstBuffer. Broadcasting clones this handle rather than
    /// allocating another frame-sized Vec for every consumer.
    pub buffer: gst::Buffer,
    pub width: u32,
    pub height: u32,
    pub pixel_format: RawPixelFormat,
}

#[derive(Clone, Debug)]
enum ClientCommand {
    FullFrame,
    Input(Vec<u8>),
}

#[derive(Clone, Debug)]
struct SessionInfo {
    version: String,
    security: &'static str,
    desktop_name: String,
    last_error: Option<String>,
}

pub struct VncSession {
    command_tx: mpsc::Sender<ClientCommand>,
    frame_tx: broadcast::Sender<VncFrame>,
    shutdown_tx: watch::Sender<bool>,
    disconnect_tx: watch::Sender<bool>,
    lifecycle: Mutex<Session>,
    lease_count: AtomicUsize,
    width: AtomicU32,
    height: AtomicU32,
    frames: AtomicU64,
    bytes: AtomicU64,
    errors: AtomicU64,
    info: Mutex<SessionInfo>,
}

impl VncSession {
    async fn connect(config: &VncConfig) -> Result<Arc<Self>, String> {
        let (stream, handshake) = connect_and_handshake(
            &config.host,
            config.port,
            config.security,
            config.password.as_deref(),
        )
        .await?;
        let (command_tx, command_rx) = mpsc::channel(COMMAND_QUEUE_CAPACITY);
        let (frame_tx, _) = broadcast::channel(FRAME_BROADCAST_CAPACITY);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (disconnect_tx, _) = watch::channel(false);
        let session = Arc::new(Self {
            command_tx,
            frame_tx,
            shutdown_tx,
            disconnect_tx,
            lifecycle: Mutex::new(Session::Connecting.step(SessionEvent::Established)),
            lease_count: AtomicUsize::new(0),
            width: AtomicU32::new(u32::from(handshake.width)),
            height: AtomicU32::new(u32::from(handshake.height)),
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            info: Mutex::new(SessionInfo {
                version: handshake.version.clone(),
                security: handshake.security.as_str(),
                desktop_name: handshake.desktop_name.clone(),
                last_error: None,
            }),
        });
        tokio::spawn(run_session(
            stream,
            session.clone(),
            command_rx,
            shutdown_rx,
            handshake,
        ));
        Ok(session)
    }

    pub fn subscribe_frames(&self) -> broadcast::Receiver<VncFrame> {
        self.frame_tx.subscribe()
    }

    pub fn disconnected_rx(&self) -> watch::Receiver<bool> {
        let mut receiver = self.disconnect_tx.subscribe();
        let terminal = *receiver.borrow();
        if terminal {
            receiver.mark_changed();
        }
        receiver
    }

    pub fn resolution(&self) -> (u16, u16) {
        (
            self.width.load(Ordering::Relaxed).min(u32::from(u16::MAX)) as u16,
            self.height.load(Ordering::Relaxed).min(u32::from(u16::MAX)) as u16,
        )
    }

    pub fn is_connected(&self) -> bool {
        self.lifecycle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .accepts_commands()
    }

    pub async fn request_full_frame(&self) -> Result<(), String> {
        self.send_command(ClientCommand::FullFrame).await
    }

    pub(crate) async fn send_input(
        &self,
        actions: Vec<bmc_backend_model::input::Action>,
        lossy: bool,
    ) -> Result<bool, String> {
        use bmc_backend_model::input::Action;
        if !self.is_connected() {
            return Err("VNC session is disconnected".into());
        }
        let mut bytes = Vec::new();
        for action in actions {
            match action {
                Action::Key(keysym, pressed) => {
                    bytes.extend_from_slice(&key_event(keysym, pressed))
                }
                Action::Pointer(x, y, buttons) => {
                    bytes.extend_from_slice(&pointer_event(x, y, buttons))
                }
                Action::Move(..) | Action::Report(..) | Action::Mode(..) => {
                    return Err("invalid VNC model output".into())
                }
            }
        }
        if bytes.is_empty() {
            return Ok(true);
        }
        if lossy {
            return match self.command_tx.try_send(ClientCommand::Input(bytes)) {
                Ok(()) => Ok(true),
                Err(mpsc::error::TrySendError::Full(_)) => Ok(false),
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    Err("VNC command channel is closed".into())
                }
            };
        }
        self.send_command(ClientCommand::Input(bytes)).await?;
        Ok(true)
    }

    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(true);
    }

    pub fn trace_snapshot(&self) -> serde_json::Value {
        let info = self
            .info
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        serde_json::json!({
            "transport": "vnc",
            "connected": self.is_connected(),
            "rfb_version": info.version,
            "security": info.security,
            "desktop_name": info.desktop_name,
            "resolution": {
                "width": self.width.load(Ordering::Relaxed),
                "height": self.height.load(Ordering::Relaxed),
            },
            "frames": self.frames.load(Ordering::Relaxed),
            "bytes": self.bytes.load(Ordering::Relaxed),
            "errors": self.errors.load(Ordering::Relaxed),
            "last_error": info.last_error,
        })
    }

    async fn send_command(&self, command: ClientCommand) -> Result<(), String> {
        if !self.is_connected() {
            return Err("VNC session is disconnected".to_string());
        }
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            self.command_tx.send(command),
        )
        .await
        .map_err(|_| "VNC command queue is busy".to_string())?
        .map_err(|_| "VNC command channel is closed".to_string())
    }

    fn mark_disconnected(&self, error: Option<String>) {
        let mut state = self.lifecycle.lock().unwrap_or_else(|e| e.into_inner());
        *state = state.step(if error.is_some() {
            SessionEvent::Fault
        } else {
            SessionEvent::Stop
        });
        drop(state);
        if let Some(error) = error {
            self.errors.fetch_add(1, Ordering::Relaxed);
            self.info
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .last_error = Some(error);
        }
        let _ = self.disconnect_tx.send(true);
    }
}

pub struct VncLease {
    key: VncPoolKey,
    session: Arc<VncSession>,
}

impl VncLease {
    pub fn session(&self) -> &Arc<VncSession> {
        &self.session
    }
}

impl Drop for VncLease {
    fn drop(&mut self) {
        // Lease changes and removal share the same lock with acquisition.
        let mut pool = lock_pool();
        if self.session.lease_count.fetch_sub(1, Ordering::SeqCst) == 1 {
            if matches!(pool.get(&self.key), Some(PoolSlot::Ready(current)) if Arc::ptr_eq(current, &self.session))
            {
                pool.remove(&self.key);
            }
            self.session.shutdown();
        }
    }
}

struct ConnectingGuard {
    key: VncPoolKey,
    completion: Arc<watch::Sender<bool>>,
}
impl Drop for ConnectingGuard {
    fn drop(&mut self) {
        let mut pool = lock_pool();
        if matches!(pool.get(&self.key), Some(PoolSlot::Connecting(current)) if Arc::ptr_eq(current, &self.completion))
        {
            pool.remove(&self.key);
        }
        self.completion.send_replace(true);
    }
}

pub async fn get_or_connect(config: &VncConfig) -> Result<VncLease, String> {
    let key = VncPoolKey::from_config(config);
    loop {
        enum Next {
            Ready(Arc<VncSession>),
            Wait(watch::Receiver<bool>),
            Connect(ConnectingGuard),
        }
        let next = {
            let mut pool = lock_pool();
            let slot = match pool.get(&key) {
                Some(PoolSlot::Ready(session)) => {
                    if session.is_connected() {
                        Slot::Online
                    } else {
                        Slot::Disconnected
                    }
                }
                Some(PoolSlot::Connecting(_)) => Slot::Connecting,
                None => Slot::Vacant,
            };
            match acquire(slot) {
                Acquire::Lease => {
                    let Some(PoolSlot::Ready(session)) = pool.get(&key) else {
                        unreachable!()
                    };
                    session.lease_count.fetch_add(1, Ordering::SeqCst);
                    Next::Ready(session.clone())
                }
                Acquire::Wait => {
                    let Some(PoolSlot::Connecting(completion)) = pool.get(&key) else {
                        unreachable!()
                    };
                    Next::Wait(completion.subscribe())
                }
                Acquire::Connect => {
                    let (completion, _) = watch::channel(false);
                    let completion = Arc::new(completion);
                    pool.insert(key.clone(), PoolSlot::Connecting(completion.clone()));
                    Next::Connect(ConnectingGuard {
                        key: key.clone(),
                        completion,
                    })
                }
            }
        };
        match next {
            Next::Ready(session) => return Ok(VncLease { key, session }),
            Next::Wait(mut completion) => {
                if !*completion.borrow_and_update() {
                    let _ = completion.changed().await;
                }
            }
            Next::Connect(guard) => {
                let session = VncSession::connect(config).await?;
                {
                    let mut pool = lock_pool();
                    session.lease_count.fetch_add(1, Ordering::SeqCst);
                    pool.insert(key.clone(), PoolSlot::Ready(session.clone()));
                }
                drop(guard);
                return Ok(VncLease { key, session });
            }
        }
    }
}

async fn run_session(
    mut stream: tokio::net::TcpStream,
    session: Arc<VncSession>,
    mut command_rx: mpsc::Receiver<ClientCommand>,
    mut shutdown_rx: watch::Receiver<bool>,
    handshake: HandshakeInfo,
) {
    let mut framebuffer = match Framebuffer::new(handshake.width, handshake.height) {
        Ok(framebuffer) => framebuffer,
        Err(error) => {
            session.mark_disconnected(Some(error));
            return;
        }
    };
    let mut read_buffer = BytesMut::with_capacity(64 * 1024);
    let origin = tokio::time::Instant::now();
    let mut record =
        bmc_backend_model::stream::Record::new(std::time::Duration::from_secs(10), MAX_READ_BUFFER);
    let result: Result<(), String> = 'session: loop {
        record = record.step(read_buffer.len(), origin.elapsed());
        if record.state() == bmc_backend_model::stream::State::Failed {
            break Err("VNC partial record timed out or exceeded limit".into());
        }
        let deadline = record.deadline();
        tokio::select! {
            _ = async { match deadline {
                Some(deadline) => tokio::time::sleep_until(origin + deadline).await,
                None => core::future::pending().await,
            } } => break Err("VNC partial record timed out".into()),
            changed = shutdown_rx.changed() => {
                if changed.is_err() || *shutdown_rx.borrow() {
                    break Ok(());
                }
            }
            command = command_rx.recv() => match command {
                Some(command) => match tokio::time::timeout(std::time::Duration::from_secs(5), write_command(&mut stream, &session, command)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => break 'session Err(error),
                    Err(_) => break 'session Err("VNC write timed out; input result unknown".into()),
                },
                None => break 'session Ok(()),
            },
            read = stream.read_buf(&mut read_buffer) => {
                let read = match read {
                    Ok(read) => read,
                    Err(error) => break 'session Err(format!("VNC read failed: {error}")),
                };
                if read == 0 {
                    break 'session Err("VNC server closed the connection".to_string());
                }
                record = record.step(read_buffer.len(), origin.elapsed());
                if record.state() == bmc_backend_model::stream::State::Failed { break 'session Err("VNC record budget exceeded".into()); }
                if read_buffer.len() > MAX_READ_BUFFER {
                    break 'session Err("VNC read buffer exceeds limit".to_string());
                }
                loop {
                    let parsed = match try_parse_server_message(&read_buffer) {
                        Ok(parsed) => parsed,
                        Err(error) => break 'session Err(error),
                    };
                    let Some((consumed, message)) = parsed else { break; };
                    let _ = read_buffer.split_to(consumed);
                    record = record.step(0, origin.elapsed()).step(read_buffer.len(), origin.elapsed());
                    let frame = match apply_server_message(&mut framebuffer, &session, message) {
                        Ok(frame) => frame,
                        Err(error) => break 'session Err(error),
                    };
                    let Some(frame) = frame else { continue; };
                    let _ = session.frame_tx.send(frame);
                    let (width, height) = framebuffer.dimensions();
                    let request = framebuffer_update_request(true, 0, 0, width, height);
                    let write = tokio::time::timeout(std::time::Duration::from_secs(5), async {
                        stream.write_all(&request).await?;
                        stream.flush().await
                    });
                    tokio::select! {
                        _ = shutdown_rx.changed() => break 'session Ok(()),
                        result = write => match result {
                            Ok(Ok(())) => {},
                            Ok(Err(error)) => break 'session Err(format!("VNC framebuffer request failed: {error}")),
                            Err(_) => break 'session Err("VNC framebuffer request timed out".into()),
                        }
                    }
                }
            }
        }
    };
    match result {
        Ok(()) => session.mark_disconnected(None),
        Err(error) => session.mark_disconnected(Some(error)),
    }
}

async fn write_command(
    stream: &mut tokio::net::TcpStream,
    session: &VncSession,
    command: ClientCommand,
) -> Result<(), String> {
    let bytes: Vec<u8> = match command {
        ClientCommand::FullFrame => {
            let (width, height) = session.resolution();
            framebuffer_update_request(false, 0, 0, width, height)
        }
        ClientCommand::Input(bytes) => bytes,
    };
    stream
        .write_all(&bytes)
        .await
        .map_err(|error| format!("VNC command write failed: {error}"))?;
    stream
        .flush()
        .await
        .map_err(|error| format!("VNC command flush failed: {error}"))
}

fn apply_server_message(
    framebuffer: &mut Framebuffer,
    session: &VncSession,
    message: ServerMessage,
) -> Result<Option<VncFrame>, String> {
    let ServerMessage::FramebufferUpdate(rectangles) = message else {
        return Ok(None);
    };
    for rectangle in rectangles {
        framebuffer.apply(rectangle)?;
    }
    let (width, height) = framebuffer.dimensions();
    session.width.store(u32::from(width), Ordering::Relaxed);
    session.height.store(u32::from(height), Ordering::Relaxed);
    let buffer = framebuffer.snapshot();
    session.frames.fetch_add(1, Ordering::Relaxed);
    session
        .bytes
        .fetch_add(framebuffer.byte_len() as u64, Ordering::Relaxed);
    Ok(Some(VncFrame {
        buffer,
        width: u32::from(width),
        height: u32::from(height),
        pixel_format: RawPixelFormat::Rgba,
    }))
}

struct Framebuffer {
    width: u16,
    height: u16,
    buffer: gst::Buffer,
}

impl Framebuffer {
    fn new(width: u16, height: u16) -> Result<Self, String> {
        validate_dimensions(width, height)?;
        let bytes = usize::from(width)
            .checked_mul(usize::from(height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "VNC framebuffer size overflow".to_string())?;
        gst::init().map_err(|error| format!("GStreamer initialization failed: {error}"))?;
        let mut buffer = gst::Buffer::with_size(bytes)
            .map_err(|error| format!("VNC framebuffer allocation failed: {error}"))?;
        {
            let buffer = buffer
                .get_mut()
                .ok_or_else(|| "new VNC framebuffer is not writable".to_string())?;
            let mut map = buffer
                .map_writable()
                .map_err(|error| format!("VNC framebuffer map failed: {error}"))?;
            let pixels = map.as_mut_slice();
            pixels.fill(0);
            for pixel in pixels.chunks_exact_mut(4) {
                pixel[3] = 0xff;
            }
        }
        Ok(Self {
            width,
            height,
            buffer,
        })
    }

    fn dimensions(&self) -> (u16, u16) {
        (self.width, self.height)
    }

    fn byte_len(&self) -> usize {
        self.buffer.size()
    }

    /// Publish a ref-counted snapshot. On the next mutation `writable()`
    /// performs copy-on-write only when a downstream consumer still owns this
    /// buffer, avoiding the former VNC-frame -> Vec -> GstBuffer copy.
    fn snapshot(&self) -> gst::Buffer {
        self.buffer.clone()
    }

    fn apply(&mut self, rectangle: Rectangle) -> Result<(), String> {
        match rectangle.encoding {
            -223 => {
                *self = Self::new(rectangle.width, rectangle.height)?;
                Ok(())
            }
            0 => self.copy_raw(rectangle),
            other => Err(format!("unsupported VNC framebuffer encoding {other}")),
        }
    }

    fn copy_raw(&mut self, rectangle: Rectangle) -> Result<(), String> {
        let right = rectangle
            .x
            .checked_add(rectangle.width)
            .ok_or_else(|| "VNC rectangle x overflow".to_string())?;
        let bottom = rectangle
            .y
            .checked_add(rectangle.height)
            .ok_or_else(|| "VNC rectangle y overflow".to_string())?;
        if right > self.width || bottom > self.height {
            return Err("VNC rectangle is outside framebuffer".to_string());
        }
        let expected = usize::from(rectangle.width)
            .checked_mul(usize::from(rectangle.height))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "VNC rectangle size overflow".to_string())?;
        if rectangle.data.len() != expected {
            return Err("VNC raw rectangle has invalid length".to_string());
        }
        let framebuffer_width = usize::from(self.width);
        let buffer = self.writable()?;
        let mut map = buffer
            .get_mut()
            .ok_or_else(|| "VNC framebuffer is not writable".to_string())?
            .map_writable()
            .map_err(|error| format!("VNC framebuffer map failed: {error}"))?;
        let pixels = map.as_mut_slice();
        for row in 0..usize::from(rectangle.height) {
            for column in 0..usize::from(rectangle.width) {
                let source = (row * usize::from(rectangle.width) + column) * 4;
                let destination = ((usize::from(rectangle.y) + row) * framebuffer_width
                    + usize::from(rectangle.x)
                    + column)
                    * 4;
                // The requested RFB true-colour format is little-endian XRGB,
                // so source bytes arrive as B, G, R, X.
                pixels[destination] = rectangle.data[source + 2];
                pixels[destination + 1] = rectangle.data[source + 1];
                pixels[destination + 2] = rectangle.data[source];
                pixels[destination + 3] = 0xff;
            }
        }
        Ok(())
    }

    fn writable(&mut self) -> Result<&mut gst::Buffer, String> {
        if self.buffer.get_mut().is_none() {
            self.buffer = copy_buffer(&self.buffer)?;
        }
        Ok(&mut self.buffer)
    }
}

fn copy_buffer(source: &gst::Buffer) -> Result<gst::Buffer, String> {
    let mut destination = gst::Buffer::with_size(source.size())
        .map_err(|error| format!("VNC framebuffer copy allocation failed: {error}"))?;
    {
        let source_map = source
            .map_readable()
            .map_err(|error| format!("VNC framebuffer read map failed: {error}"))?;
        let destination_ref = destination
            .get_mut()
            .ok_or_else(|| "VNC framebuffer copy is not writable".to_string())?;
        let mut destination_map = destination_ref
            .map_writable()
            .map_err(|error| format!("VNC framebuffer write map failed: {error}"))?;
        destination_map
            .as_mut_slice()
            .copy_from_slice(source_map.as_slice());
    }
    Ok(destination)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancelled_creator_removes_slot_and_wakes_existing_and_late_waiters() {
        let config = VncConfig::new(
            "cancelled-creator.example.test".into(),
            5900,
            None,
            Some("none"),
        )
        .unwrap();
        let key = VncPoolKey::from_config(&config);
        let (completion, _) = watch::channel(false);
        let completion = Arc::new(completion);
        let mut waiter = completion.subscribe();
        lock_pool().insert(key.clone(), PoolSlot::Connecting(completion.clone()));
        let guard = ConnectingGuard {
            key: key.clone(),
            completion: completion.clone(),
        };
        let (started, ready) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _guard = guard;
            started.send(()).unwrap();
            core::future::pending::<()>().await;
        });
        ready.await.unwrap();
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        waiter.changed().await.unwrap();
        assert!(*waiter.borrow());
        assert!(*completion.subscribe().borrow());
        assert!(!lock_pool().contains_key(&key));
    }

    #[test]
    fn config_uses_device_password_when_vnc_password_is_not_overridden() {
        let config = VncConfig::new(
            "bmc.example.test".into(),
            5900,
            Some("device-password".into()),
            Some("vnc-auth"),
        )
        .unwrap();

        assert_eq!(config.password.as_deref(), Some("device-password"));
    }

    #[test]
    fn config_rejects_missing_vnc_auth_password() {
        assert!(VncConfig::new("bmc.example.test".into(), 5900, None, Some("vnc-auth")).is_err());
    }

    #[test]
    fn framebuffer_converts_requested_bgrx_to_rgba() {
        let mut framebuffer = Framebuffer::new(1, 1).unwrap();
        framebuffer
            .copy_raw(Rectangle {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
                encoding: 0,
                data: vec![0x11, 0x22, 0x33, 0],
            })
            .unwrap();
        let map = framebuffer.buffer.map_readable().unwrap();
        assert_eq!(map.as_slice(), &[0x33, 0x22, 0x11, 0xff]);
    }

    #[test]
    fn framebuffer_rejects_out_of_bounds_rectangle() {
        let mut framebuffer = Framebuffer::new(1, 1).unwrap();
        assert!(framebuffer
            .copy_raw(Rectangle {
                x: 1,
                y: 0,
                width: 1,
                height: 1,
                encoding: 0,
                data: vec![0; 4],
            })
            .is_err());
    }
}
