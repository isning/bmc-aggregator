use std::sync::atomic::{AtomicU8, AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::RwLock as SyncRwLock;
use std::time::Duration;

use bytes::{Buf, BytesMut};
use futures::stream::{BoxStream, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{broadcast, mpsc, Mutex, RwLock, watch};
use tokio::time::timeout;
use tracing::{info, debug, warn, error, trace};

use crate::bmc::backends::general::mahogany::framebuffer::Framebuffer;
use crate::bmc::stream::pipeline::SharedVideoPipeline;
use crate::bmc::backends::general::mahogany::hid;
use crate::bmc::error::SessionError;
use crate::bmc::traits::{BmcInputSession, BmcMsdSession, BmcSession, DeviceInfo};
use crate::config::DeviceConfig;
use crate::bmc::backends::general::mahogany::msd as msd_proto;

use super::protocol::*;

// ── Session ───────────────────────────────────────────────────────

pub struct MahoganySession {
    tcp_tx: mpsc::Sender<Vec<u8>>,
    #[allow(dead_code)]
    fb: Arc<Mutex<Framebuffer>>,
    jpeg_tx: broadcast::Sender<Vec<u8>>,
    info: Arc<RwLock<DeviceInfoInner>>,
    _shutdown_tx: watch::Sender<bool>,
    disconnect_tx: watch::Sender<bool>,
    pub cfg: DeviceConfig,
    last_buttons: AtomicU8,
    fps_value: Arc<AtomicU32>,
    perf: Arc<PerfCounters>,
    pub msd: crate::bmc::backends::general::mahogany::msd::MsdBackend,
    pub pipeline: Arc<SyncRwLock<Option<Arc<SharedVideoPipeline>>>>,
    last_frame: Arc<SyncRwLock<Option<Vec<u8>>>>,
    local_addr: std::net::SocketAddr,
}

struct DeviceInfoInner {
    online: bool,
    width: u16,
    height: u16,
}

use super::perf::PerfCounters;

// ── Session context (shared state for spawned background tasks) ──

struct SessionContext {
    tcp_tx: mpsc::Sender<Vec<u8>>,
    jpeg_tx: broadcast::Sender<Vec<u8>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    disconnect_tx: watch::Sender<bool>,
    fb: Arc<Mutex<Framebuffer>>,
    info: Arc<RwLock<DeviceInfoInner>>,
    video_pipeline: Arc<SyncRwLock<Option<Arc<SharedVideoPipeline>>>>,
    fps_counter: Arc<AtomicU32>,
    fps_value: Arc<AtomicU32>,
    perf: Arc<PerfCounters>,
    last_frame: Arc<SyncRwLock<Option<Vec<u8>>>>,
    active_encoder: crate::bmc::encoder::backend::SharedEncoder,
    frame_interval: Duration,
    device_id: String,
    local_addr: std::net::SocketAddr,
}

impl SessionContext {
    fn new(
        cfg: &DeviceConfig,
        encoder: crate::bmc::encoder::backend::SharedEncoder,
        fb: Arc<Mutex<Framebuffer>>,
        info: Arc<RwLock<DeviceInfoInner>>,
        local_addr: std::net::SocketAddr,
    ) -> (Self, mpsc::Receiver<Vec<u8>>) {
        let (tcp_tx, tcp_rx) = mpsc::channel(32);
        let (jpeg_tx, _) = broadcast::channel(16);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (disconnect_tx, _) = watch::channel(false);
        let frame_interval = Duration::from_millis((1000 / cfg.max_fps.max(1)) as u64);
        (
            Self {
                tcp_tx,
                jpeg_tx,
                shutdown_tx,
                shutdown_rx,
                disconnect_tx,
                fb,
                info,
                video_pipeline: Arc::new(SyncRwLock::new(None)),
                fps_counter: Arc::new(AtomicU32::new(0)),
                fps_value: Arc::new(AtomicU32::new(0)),
                perf: Arc::new(PerfCounters::new()),
                last_frame: Arc::new(SyncRwLock::new(None)),
                active_encoder: encoder,
                frame_interval,
                device_id: cfg.id.clone(),
                local_addr,
            },
            tcp_rx,
        )
    }

    fn build_session(self, cfg: DeviceConfig) -> MahoganySession {
        MahoganySession {
            tcp_tx: self.tcp_tx,
            fb: self.fb,
            jpeg_tx: self.jpeg_tx,
            _shutdown_tx: self.shutdown_tx,
            disconnect_tx: self.disconnect_tx,
            info: self.info,
            cfg,
            last_buttons: AtomicU8::new(0),
            fps_value: self.fps_value,
            perf: self.perf,
            msd: crate::bmc::backends::general::mahogany::msd::MsdBackend::new(),
            pipeline: self.video_pipeline,
            last_frame: self.last_frame,
            local_addr: self.local_addr,
        }
    }
}

// ── Spawned background tasks ──

fn spawn_writer(
    mut tcp_rx: mpsc::Receiver<Vec<u8>>,
    mut write_half: tokio::net::tcp::OwnedWriteHalf,
    ctx: &SessionContext,
) {
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    let disconnect_tx = ctx.disconnect_tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = tcp_rx.recv() => {
                    match msg {
                        Some(data) => {
                            if let Err(e) = write_half.write_all(&data).await {
                                warn!("tcp write error: {}", e);
                                let _ = disconnect_tx.send(true);
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = shutdown_rx.changed() => break,
            }
        }
        debug!("writer task exited");
    });
}

fn spawn_encoder_task(ctx: &SessionContext) {
    let fb_enc = ctx.fb.clone();
    let jpeg_enc = ctx.jpeg_tx.clone();
    let last_frame_enc = ctx.last_frame.clone();
    let fps_enc = ctx.fps_counter.clone();
    let perf_enc = ctx.perf.clone();
    let active_enc = ctx.active_encoder.clone();
    let mut shutdown_enc = ctx.shutdown_rx.clone();
    let pipeline_enc = ctx.video_pipeline.clone();
    let frame_interval = ctx.frame_interval;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(frame_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let mut f = fb_enc.lock().await;
                    if f.dirty {
                        f.dirty = false;
                        f.frame_count += 1;
                        let pipeline_us = f.frame_start.take()
                            .map(|t| t.elapsed().as_micros() as u64)
                            .unwrap_or(0);
                        let encode_start = std::time::Instant::now();
                        let encode_result = {
                            let encoder = active_enc.read().await;
                            encoder.encode(&f.pixels, f.width, f.height, 85)
                        };
                        match encode_result {
                            Ok(jpeg) => {
                                let encode_us = encode_start.elapsed().as_micros() as u64;
                                *last_frame_enc.write().unwrap() = Some(jpeg.clone());
                                let _ = jpeg_enc.send(jpeg);
                                fps_enc.fetch_add(1, Ordering::Relaxed);
                                perf_enc.encode_count.fetch_add(1, Ordering::Relaxed);
                                perf_enc.encode_time_total_us.fetch_add(encode_us, Ordering::Relaxed);
                                if encode_us > perf_enc.encode_time_max_us.load(Ordering::Relaxed) {
                                    perf_enc.encode_time_max_us.store(encode_us, Ordering::Relaxed);
                                }
                                if pipeline_us > 0 {
                                    perf_enc.pipeline_total_us.fetch_add(pipeline_us, Ordering::Relaxed);
                                    perf_enc.pipeline_count.fetch_add(1, Ordering::Relaxed);
                                    if pipeline_us > perf_enc.pipeline_max_us.load(Ordering::Relaxed) {
                                        perf_enc.pipeline_max_us.store(pipeline_us, Ordering::Relaxed);
                                    }
                                }
                            }
                            Err(e) => warn!("encode error: {}", e),
                        }

                        if let Some(p) = pipeline_enc.read().unwrap().as_ref() {
                            p.push_frame(&f.pixels, f.width.into(), f.height.into());
                        }
                    }
                }
                _ = shutdown_enc.changed() => break,
            }
        }
        debug!("encoder task exited");
    });
}

fn spawn_fps_task(ctx: &SessionContext) {
    let fps_counter = ctx.fps_counter.clone();
    let fps_value = ctx.fps_value.clone();
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let count = fps_counter.swap(0, Ordering::Relaxed);
                    fps_value.store(count, Ordering::Relaxed);
                }
                _ = shutdown_rx.changed() => break,
            }
        }
    });
}

fn spawn_keepalive_task(ctx: &SessionContext) {
    let tx_keep = ctx.tcp_tx.clone();
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(Duration::from_secs(5));
        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let _ = tx_keep.try_send(hid::encode_nop());
                }
                _ = shutdown_rx.changed() => break,
            }
        }
    });
}

fn spawn_reader(
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    ctx: &SessionContext,
) {
    let fb = ctx.fb.clone();
    let info = ctx.info.clone();
    let perf = ctx.perf.clone();
    let device_id = ctx.device_id.clone();
    let mut shutdown_rx = ctx.shutdown_rx.clone();
    let disconnect_tx = ctx.disconnect_tx.clone();
    tokio::spawn(async move {
        let mut read_buf = BytesMut::with_capacity(65536);
        loop {
            tokio::select! {
                result = read_half.read_buf(&mut read_buf) => {
                    match result {
                        Ok(0) => {
                            warn!("[{}] BMC connection closed", device_id);
                            break;
                        }
                        Ok(_) => {
                            loop {
                                let buf_len = read_buf.len();
                                if buf_len == 0 { break; }
                                match try_parse_message(&read_buf[..buf_len]) {
                                    Ok(Some((msg, consumed))) => {
                                        read_buf.advance(consumed);
                                        if let Err(e) = MahoganySession::handle_message(
                                            msg, &fb, &info, &perf,
                                        ).await {
                                            warn!("handle_message error: {}", e);
                                        }
                                    }
                                    Ok(None) => break,
                                    Err(e) => {
                                        warn!("parse error: {}, skipping byte", e);
                                        read_buf.advance(1);
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
        let mut i = info.write().await;
        i.online = false;
        let _ = disconnect_tx.send(true);
        debug!("[{}] read loop exited", device_id);
    });
}

/// Enable TCP keepalive on the stream with custom linux parameters.
fn set_tcp_keepalive(stream: &TcpStream) -> std::io::Result<()> {
    let sock_ref = socket2::SockRef::from(stream);
    sock_ref.set_keepalive(true)?;
    sock_ref.set_tcp_keepalive(&socket2::TcpKeepalive::new()
        .with_time(std::time::Duration::from_secs(5))
        .with_interval(std::time::Duration::from_secs(3))
    )?;
    Ok(())
}

impl MahoganySession {
    /// Fetch httpdata from the BMC's HTTP page.
    /// Returns (httpdata, kvm_port) where kvm_port is the actual KVM port to use.
    async fn fetch_httpdata_and_port(cfg: &DeviceConfig) -> Result<(String, u16), anyhow::Error> {
        let body = super::auth::fetch_bmc_page(cfg).await?;
        let httpdata = extract_httpdata(&body)
            .ok_or_else(|| anyhow::anyhow!("httpdata not found in BMC response"))?;
        info!("[{}] httpdata obtained ({} chars)", cfg.id, httpdata.len());
        let kvm_port = cfg.kvm_port
            .or_else(|| extract_kvm_port(&body))
            .unwrap_or(cfg.port);
        info!("[{}] using KVM port {}", cfg.id, kvm_port);
        Ok((httpdata, kvm_port))
    }

    /// TCP connect + protocol handshake (NOP → ServerHandshake → ClientHandshake + control msgs).
    /// Returns (read_half, write_half) for the caller to set up the writer task.
    async fn kvm_connect_and_handshake(
        cfg: &DeviceConfig,
        httpdata: &str,
        kvm_port: u16,
    ) -> Result<(tokio::net::tcp::OwnedReadHalf, tokio::net::tcp::OwnedWriteHalf, std::net::SocketAddr), anyhow::Error> {
        let kvm_addr = format!("{}:{}", cfg.host, kvm_port);
        let stream = TcpStream::connect(&kvm_addr).await?;
        info!("[{}] TCP connected to {}", cfg.id, kvm_addr);
        let _ = set_tcp_keepalive(&stream);
        let local_addr = stream.local_addr()?;
        let (mut read_half, mut write_half) = stream.into_split();

        // NOP
        write_half.write_all(&hid::encode_nop()).await?;
        info!("[{}] NOP (0xD2) sent...", cfg.id);

        // Read first message
        let mut type_buf = [0u8; 1];
        timeout(Duration::from_secs(10), read_half.read_exact(&mut type_buf))
            .await
            .map_err(|_| anyhow::anyhow!("timeout waiting for server response"))?
            .map_err(|e| anyhow::anyhow!("read type byte error: {}", e))?;

        match type_buf[0] {
            msg::SERVER_HANDSHAKE => {
                let mut rest = [0u8; 32];
                read_half.read_exact(&mut rest).await
                    .map_err(|e| anyhow::anyhow!("read ServerHandshake payload error: {}", e))?;
                let priv_str = String::from_utf8_lossy(&rest);
                info!("[{}] ServerHandshake: privilege='{}'", cfg.id, priv_str.trim_matches(char::from(0)));
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
                anyhow::bail!("server disconnected: reason={}, message='{}'", reason, String::from_utf8_lossy(&msg_body).trim_matches(char::from(0)));
            }
            other => {
                let mut buf = vec![other];
                let mut more = [0u8; 255];
                let n = read_half.read(&mut more).await?;
                buf.extend_from_slice(&more[..n]);
                anyhow::bail!("unexpected first message type 0x{:02x} ({} bytes): {:02x?}", other, buf.len(), &buf);
            }
        }

        // ClientHandshake
        let handshake = build_client_handshake(&cfg.auth.username, httpdata);
        write_half.write_all(&handshake).await?;
        info!("[{}] ClientHandshake sent ({} bytes)", cfg.id, handshake.len());

        // Control messages
        write_half.write_all(&build_request_primary_control()).await?;
        write_half.write_all(&build_absolute_mode(true)).await?;
        write_half.write_all(&build_inform_hlevel_compression(false)).await?;
        info!("[{}] control messages sent", cfg.id);

        Ok((read_half, write_half, local_addr))
    }

    pub async fn connect(cfg: DeviceConfig, encoder: crate::bmc::encoder::backend::SharedEncoder) -> Result<Self, anyhow::Error> {
        info!("[{}] connecting to {}:{} ...", cfg.id, cfg.host, cfg.port);

        let (httpdata, kvm_port) = Self::fetch_httpdata_and_port(&cfg).await?;
        let (read_half, write_half, local_addr) = Self::kvm_connect_and_handshake(&cfg, &httpdata, kvm_port).await?;

        let fb = Arc::new(Mutex::new(Framebuffer::new(1024, 768)));
        let info = Arc::new(RwLock::new(DeviceInfoInner { online: true, width: 1024, height: 768 }));

        let (ctx, tcp_rx) = SessionContext::new(&cfg, encoder, fb, info, local_addr);

        spawn_writer(tcp_rx, write_half, &ctx);
        spawn_encoder_task(&ctx);
        spawn_fps_task(&ctx);
        spawn_keepalive_task(&ctx);
        spawn_reader(read_half, &ctx);

        Ok(ctx.build_session(cfg))
    }

    /// Send StorageClientConnect to the BMC over the KVM control channel.
    pub async fn msd_connect(&self, ip_type: u8) -> Result<(), anyhow::Error> {
        let msg = msd_proto::build_storage_client_connect_default(ip_type);
        self.tcp_tx.send(msg).await?;
        Ok(())
    }

    /// Send StorageClientDisconnect to the BMC over the KVM control channel.
    pub async fn msd_disconnect(&self) -> Result<(), anyhow::Error> {
        let msg = msd_proto::build_storage_client_disconnect();
        self.tcp_tx.send(msg).await?;
        Ok(())
    }

    async fn handle_message(
        msg: ServerMessage,
        fb: &Arc<Mutex<Framebuffer>>,
        info: &Arc<RwLock<DeviceInfoInner>>,
        perf: &PerfCounters,
    ) -> Result<(), anyhow::Error> {
        match msg {
            ServerMessage::Nop => {}
            ServerMessage::MultiUserState => {
                debug!("MultiUserState (ignored)");
            }
            ServerMessage::DisplayParameters { width, height } => {
                info!("display parameters: {}x{}", width, height);
                let mut f = fb.lock().await;
                f.resize(width, height);
                let mut i = info.write().await;
                i.width = width;
                i.height = height;
            }
            ServerMessage::StorageStatus(status) => {
                debug!("StorageStatus: {:?}", status);
            }
            ServerMessage::FramebufferUpdate { sub_type, rects } => {
                let mut f = fb.lock().await;
                debug!("FramebufferUpdate: sub_type=0x{:04x}, {} rects", sub_type, rects.len());
                perf.tile_count.fetch_add(rects.len() as u64, Ordering::Relaxed);
                for rect in &rects {
                    f.blit_tile(rect.x, rect.y, &rect.raw_data, rect.w, rect.h);
                }
            }
            ServerMessage::EnhanceBltRaw { bilt_type, tile_w, tile_h, snoop_data, scrunch_data, raw_size, scrunch_size: _ } => {
                let f = fb.lock().await;
                let cw = f.width;
                let ch = f.height;
                drop(f);
                let rects = decode_enhance_bitblt_tiles(
                    bilt_type, tile_w, tile_h,
                    &snoop_data, &scrunch_data, raw_size,
                    cw, ch,
                )?;
                let mut f = fb.lock().await;
                debug!("EnhanceBitBlt: sub_type=0x{:04x}, {} rects (tiles {}x{}, canvas {}x{}, scrunch {}B)",
                    bilt_type, rects.len(), tile_w, tile_h, cw, ch, scrunch_data.len());
                perf.tile_count.fetch_add(rects.len() as u64, Ordering::Relaxed);
                for rect in &rects {
                    f.blit_tile(rect.x, rect.y, &rect.raw_data, rect.w, rect.h);
                }
            }
            ServerMessage::Other { type_id, .. } => {
                if type_id != 0x89 && type_id != 0xFF {
                    trace!("unhandled message type 0x{:02x}", type_id);
                }
            }
        }
        Ok(())
    }

    /// Request the BMC to resend the full framebuffer via INVALIDATE (0xF2) message.
    pub fn send_invalidate(&self) {
        if let Ok(info) = self.info.try_read() {
            let w = (info.width as u16).to_le_bytes();
            let h = (info.height as u16).to_le_bytes();
            drop(info);
            let mut msg = Vec::with_capacity(11);
            msg.push(0xF2);
            msg.push(0x01);
            msg.push(0x00);
            msg.push(0x00);
            msg.push(0x00);
            msg.push(0x00);
            msg.push(0x00);
            msg.extend_from_slice(&w);
            msg.extend_from_slice(&h);
            let _ = self.tcp_tx.try_send(msg);
        }
    }

    pub fn on_disconnect_rx(&self) -> watch::Receiver<bool> {
        self.disconnect_tx.subscribe()
    }
}

// ── Connect wrapper (standalone, not a method) ─────────────────────

/// Wrapper for ConnectionTemplate::connect — calls MahoganySession::connect and wraps in DeviceSession.
pub fn mahogany_kvm_connect(
    cfg: &crate::config::DeviceConfig,
    encoder: crate::bmc::encoder::backend::SharedEncoder,
    _progress: &crate::bmc::connection::progress::ConnectProgress,
) -> futures::future::BoxFuture<'static, Result<crate::bmc::session::types::DeviceSession, anyhow::Error>> {
    
    let cfg = cfg.clone();
    Box::pin(async move {
        let session = MahoganySession::connect(cfg.clone(), encoder).await?;
        let arc_session = Arc::new(session);
        Ok(crate::bmc::session::types::DeviceSession {
            base: Some(arc_session.clone()),
            input: Some(arc_session.clone()),
            power: None,
            msd: Some(arc_session),
        })
    })
}

// ── BmcSession impl ───────────────────────────────────────────────

#[async_trait::async_trait]
impl BmcSession for MahoganySession {
    fn subscribe_encoded_frames(&self) -> BoxStream<'static, Vec<u8>> {
        use futures::future::Either;
        use tokio_stream::wrappers::errors::BroadcastStreamRecvError;

        self.send_invalidate();

        let rx = self.jpeg_tx.subscribe();
        let last = self.last_frame.read().unwrap().clone();
        let mut disconnect_rx = self.disconnect_tx.subscribe();

        let stream = tokio_stream::wrappers::BroadcastStream::new(rx)
            .filter_map(|result| async move {
                match result {
                    Ok(frame) => Some(frame),
                    Err(BroadcastStreamRecvError::Lagged(_)) => None,
                }
            });

        // End the stream when the TCP connection drops.
        // select() interleaves the disconnect signal with frame stream.
        // take_while stops when disconnect fires, discarding the signal item.
        let disconnect_signal = async move {
            if *disconnect_rx.borrow_and_update() {
                return;
            }
            let _ = disconnect_rx.changed().await;
        };
        let stream = futures::stream::select(
            stream.map(Either::Left),
            futures::stream::once(disconnect_signal).map(|()| Either::Right(())),
        )
        .take_while(|item| futures::future::ready(matches!(item, Either::Left(_))))
        .map(|item| match item {
            Either::Left(frame) => frame,
            Either::Right(()) => unreachable!(),
        });

        if let Some(frame) = last {
            Box::pin(futures::stream::once(async move { frame }).chain(stream))
        } else {
            Box::pin(stream)
        }
    }

    async fn info(&self) -> DeviceInfo {
        let i = self.info.read().await;
        DeviceInfo {
            id: String::new(),
            name: String::new(),
            width: i.width,
            height: i.height,
            online: i.online,
        }
    }

    async fn close(&self) -> Result<(), SessionError> {
        let _ = self._shutdown_tx.send(true);
        Ok(())
    }

    fn fps(&self) -> u32 {
        self.fps_value.load(Ordering::Relaxed)
    }

    fn clients(&self) -> usize {
        self.jpeg_tx.receiver_count()
    }

    fn perf_snapshot(&self) -> serde_json::Value {
        self.perf.snapshot()
    }

    fn set_pipeline(&self, pipeline: Arc<SharedVideoPipeline>) {
        *self.pipeline.write().unwrap() = Some(pipeline);
    }

    fn on_disconnect(&self) -> Option<watch::Receiver<bool>> {
        Some(self.disconnect_tx.subscribe())
    }
}

#[async_trait::async_trait]
impl BmcInputSession for MahoganySession {
    async fn send_keyboard(&self, scancode: u16, pressed: bool) -> Result<(), SessionError> {
        let msg = hid::encode_keyboard_event(scancode, pressed);
        self.tcp_tx.send(msg).await
            .map_err(|e| SessionError::Other(format!("send_keyboard: {}", e)))
    }

    async fn send_mouse(
        &self, x: i16, y: i16, button_mask: u8, _wheel_delta: i8,
    ) -> Result<(), SessionError> {
        let move_msg = hid::encode_mouse_move(x, y);
        self.tcp_tx.send(move_msg).await
            .map_err(|e| SessionError::Other(format!("send_mouse move: {}", e)))?;

        let prev = self.last_buttons.load(Ordering::Relaxed);
        if button_mask != prev {
            let btn_msg = hid::encode_button_state(x, y, button_mask);
            self.tcp_tx.send(btn_msg).await
                .map_err(|e| SessionError::Other(format!("send_mouse button: {}", e)))?;
            self.last_buttons.store(button_mask, Ordering::Relaxed);
        }

        Ok(())
    }
}

#[async_trait::async_trait]
impl BmcMsdSession for MahoganySession {
    async fn msd_status(&self) -> crate::bmc::traits::MsdStatusInfo {
        let status = self.msd.status().await;
        crate::bmc::traits::MsdStatusInfo {
            connected: status.connected,
            enabled: status.enabled,
            image: status.image,
            image_size: status.image_size,
        }
    }

    async fn msd_connect(&self) -> Result<(), SessionError> {
        self.msd.set_connected(true).await;
        let port = {
            if self.msd.storage_server.local_port().await.is_none() {
                self.msd.storage_server.start(0).await
                    .map_err(|e| SessionError::Other(format!("storage server start: {}", e)))?;
                let ss = self.msd.storage_server.clone();
                tokio::spawn(async move { ss.accept_loop().await });
            }
            self.msd.storage_server.local_port().await
                .ok_or_else(|| SessionError::Other("no storage port".into()))?
        };
        let (ip_bytes, ip_type) = match self.local_addr {
            std::net::SocketAddr::V4(v4) => {
                let mut bytes = [0u8; 16];
                bytes[10..12].copy_from_slice(&[0xFF, 0xFF]);
                bytes[12..16].copy_from_slice(&v4.ip().octets());
                (bytes, 4u8)
            }
            std::net::SocketAddr::V6(v6) => {
                (v6.ip().octets(), 6u8)
            }
        };
        let msg = msd_proto::build_storage_client_connect(
            &ip_bytes,
            port,
            "",
            "",
            0xFF,
            0xFF,
            ip_type,
        );
        self.tcp_tx.send(msg).await
            .map_err(|e| SessionError::Other(format!("msd_connect: {}", e)))
    }

    async fn msd_disconnect(&self) -> Result<(), SessionError> {
        self.msd.set_connected(false).await;
        let msg = msd_proto::build_storage_client_disconnect();
        self.tcp_tx.send(msg).await
            .map_err(|e| SessionError::Other(format!("msd_disconnect: {}", e)))
    }

    async fn msd_set_image(&self, path: String, size: u64) {
        self.msd.set_image(path, size).await;
    }
}
