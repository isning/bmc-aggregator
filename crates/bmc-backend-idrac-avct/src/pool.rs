//! AVCT connection lifecycle and shared video/HID pool.

use std::collections::HashMap;
use std::future::Future;
use std::hash::{Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use futures::FutureExt;
use native_tls::TlsConnector as NativeTlsConnector;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, watch, Notify};
use tokio_native_tls::{TlsConnector, TlsStream};

use super::apcp::{
    read_server_setup, write_request, ApcpChannel, ApcpLauncherProfile, ApcpRequestKind, ApcpSetup,
};
use super::avct::{
    read_packet, write_packet, AvctPacket, IDRAC6_MAX_AVCT_PACKET_BYTES,
    IDRAC8_MAX_AVCT_PACKET_BYTES,
};
use super::block_video::{is_block_video_packet_type, BlockVideoPacket};
use super::decoder::{AvctDisplayDecoder, DecodedFrame};
use super::display::{is_display_packet_type, DisplayPacket};
use super::dvc::{
    color_depth_profile, color_depth_request, default_color_depth_request,
    parse_color_mode_response, ColorDepthProfile, ColorModeResponse, IDRAC6_COLOR_DEPTH_PROFILES,
    IDRAC8_COLOR_DEPTH_PROFILES,
};
use super::input::{
    delta_pointer_packet, focus_control_packet, keyboard_led_request_packet, keyboard_packet,
    mouse_reset_packet, pointer_packet,
};
use super::login::{
    build_login_request, parse_login_response, LoginProfile, LoginResponse, LoginResult,
};

// The iDRAC8 used for integration validation completes a healthy AVCT setup
// in roughly twenty seconds.  Keep a single deadline around the whole setup
// as a final safety net, even though each blocking protocol stage also has a
// narrower timeout below.  A peer that accepts TCP but then stops responding
// must never leave a feature lifecycle in `Connecting` indefinitely.
const SESSION_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
const CONNECT_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const APCP_SETUP_TIMEOUT: Duration = Duration::from_secs(10);
const GRACEFUL_CLOSE_RELEASE_DELAY: Duration = Duration::from_millis(100);
/// One absolute deadline for a requested target chain or its DVC15 recovery
/// chain. Partial packets never extend it.
const IDRAC6_DVC_SWITCH_TIMEOUT: Duration = Duration::from_secs(10);
/// iDRAC8 reports the applied raw colour mode through `0x8204`. Do not claim a
/// profile change until a response newer than the request confirms both raw
/// fields.
const IDRAC8_DVC_APPLY_TIMEOUT: Duration = Duration::from_secs(3);

/// iDRAC6's supplied launcher defaults `USE_APCP=false`: its control socket
/// is TLS directly on KMPORT and its display socket is a plain AVCT stream on
/// VPORT.  iDRAC8's supplied launcher uses APCP before TLS on both channels.
/// These are transport profiles, not a generation-derived packet heuristic.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
enum AvctTransportProfile {
    Idrac6Apcp,
    Idrac8Apcp,
}

use bmc_idrac_model::avct::native_reconnect_eligible;
pub use bmc_idrac_model::avct::AvctTerminalReason;

// Runtime adapters only translate monotonic timestamps and dispatch pure events.
use bmc_idrac_model::avct::{
    self as model, DvcDisplayAction, DvcProfileSwitchPhase, DvcStartupPhase, DvcSwitchAction,
    DvcSwitchTimeout, StartupEffect, StartupEvent, SwitchEffect, SwitchEvent,
};

struct DvcStartup {
    state: model::DvcStartup,
    origin: tokio::time::Instant,
}
impl DvcStartup {
    fn new(profile: AvctTransportProfile, now: tokio::time::Instant) -> Self {
        Self {
            state: model::DvcStartup::new(
                profile == AvctTransportProfile::Idrac6Apcp,
                Duration::ZERO,
            ),
            origin: now,
        }
    }
    fn dispatch(&mut self, event: StartupEvent) -> StartupEffect {
        let transition = std::mem::take(&mut self.state).step(event);
        self.state = transition.state;
        transition.effect
    }
    fn phase(&self) -> DvcStartupPhase {
        self.state.phase()
    }
    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.state.deadline().map(|time| self.origin + time)
    }
    fn mark_bootstrap_complete(&mut self) {
        self.dispatch(StartupEvent::BootstrapWritten);
    }
    fn suspend_for_no_signal(&mut self) {
        self.dispatch(StartupEvent::NoSignal);
    }
    fn resume_for_video_ready(&mut self, now: tokio::time::Instant) {
        self.dispatch(StartupEvent::VideoReady {
            now: now.saturating_duration_since(self.origin),
        });
    }
    fn observe_display(&mut self, message_type: u16, flags: u8) -> DvcDisplayAction {
        let StartupEffect::Display(action) = self.dispatch(StartupEvent::Display {
            message_type,
            flags,
        }) else {
            unreachable!("display event effect")
        };
        action
    }
    fn finish_refreshed_chain(&mut self) -> bool {
        let StartupEffect::ChainFinished(finished) = self.dispatch(StartupEvent::ChainDecoded)
        else {
            unreachable!("decoded event effect")
        };
        finished
    }
    fn fail_if_expired(&mut self, now: tokio::time::Instant) -> bool {
        let StartupEffect::Expired(expired) = self.dispatch(StartupEvent::Tick {
            now: now.saturating_duration_since(self.origin),
        }) else {
            unreachable!("tick event effect")
        };
        expired
    }
    fn fail(&mut self) {
        self.dispatch(StartupEvent::Failure);
    }
}
struct DvcProfileSwitch {
    state: model::DvcProfileSwitch,
    origin: tokio::time::Instant,
}
impl Default for DvcProfileSwitch {
    fn default() -> Self {
        Self {
            state: Default::default(),
            origin: tokio::time::Instant::now(),
        }
    }
}
impl DvcProfileSwitch {
    fn dispatch(&mut self, event: SwitchEvent) -> SwitchEffect {
        let transition = std::mem::take(&mut self.state).step(event);
        self.state = transition.state;
        transition.effect
    }
    fn phase(&self) -> DvcProfileSwitchPhase {
        self.state.phase()
    }
    fn requested(&self) -> Option<ColorDepthProfile> {
        self.state.requested()
    }
    fn generation(&self) -> u64 {
        self.state.generation()
    }
    fn active_profile(&self) -> Option<ColorDepthProfile> {
        self.state.active_profile()
    }
    fn confirm_profile(&mut self, profile: ColorDepthProfile) {
        self.dispatch(SwitchEvent::ConfirmProfile(profile));
    }
    fn queue(&mut self, profile: ColorDepthProfile) -> Result<u64, String> {
        let SwitchEffect::Queued(result) = self.dispatch(SwitchEvent::Queue(profile)) else {
            unreachable!("queue event effect")
        };
        result
    }
    fn mark_request_written(&mut self, generation: u64, now: tokio::time::Instant) -> bool {
        if self.state.deadline().is_none() {
            self.origin = now;
        }
        let SwitchEffect::Written(written) = self.dispatch(SwitchEvent::Written {
            generation,
            now: now.saturating_duration_since(self.origin),
        }) else {
            unreachable!("write event effect")
        };
        written
    }
    fn cancel_queued(&mut self, generation: u64) {
        self.dispatch(SwitchEvent::Cancel { generation });
    }
    fn abort(&mut self, error: &str) -> bool {
        let SwitchEffect::Aborted(aborted) = self.dispatch(SwitchEvent::Abort(error.to_string()))
        else {
            unreachable!("abort event effect")
        };
        aborted
    }
    fn observe_display(&mut self, message_type: u16, flags: u8) -> DvcSwitchAction {
        let SwitchEffect::Display(action) = self.dispatch(SwitchEvent::Display {
            message_type,
            flags,
        }) else {
            unreachable!("display event effect")
        };
        action
    }
    fn begin_fallback(&mut self, error: &str, now: tokio::time::Instant) -> bool {
        let SwitchEffect::FallbackStarted(started) = self.dispatch(SwitchEvent::Fallback {
            error: error.to_string(),
            now: now.saturating_duration_since(self.origin),
        }) else {
            unreachable!("fallback event effect")
        };
        started
    }
    fn finish_target_chain(&mut self) -> bool {
        let SwitchEffect::TargetFinished(finished) = self.dispatch(SwitchEvent::TargetDecoded)
        else {
            unreachable!("target event effect")
        };
        finished
    }
    fn finish_fallback_chain(&mut self) -> bool {
        let SwitchEffect::FallbackFinished(finished) = self.dispatch(SwitchEvent::FallbackDecoded)
        else {
            unreachable!("fallback event effect")
        };
        finished
    }
    fn take_completion(&mut self, generation: u64) -> Option<Result<(), String>> {
        let SwitchEffect::Completion(result) =
            self.dispatch(SwitchEvent::TakeCompletion { generation })
        else {
            unreachable!("completion event effect")
        };
        result
    }
    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.state.deadline().map(|time| self.origin + time)
    }
    #[cfg(test)]
    fn is_expired(&self, now: tokio::time::Instant) -> bool {
        self.state
            .is_expired(now.saturating_duration_since(self.origin))
    }
    fn expired_phase(&self, now: tokio::time::Instant) -> Option<DvcSwitchTimeout> {
        self.state
            .expired_phase(now.saturating_duration_since(self.origin))
    }
}

trait AvctIo: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> AvctIo for T {}
type BoxedAvctIo = Box<dyn AvctIo>;

#[derive(Clone, Debug)]
pub struct IdracAvctConfig {
    pub host: String,
    pub port: u16,
    username: String,
    password: String,
    virtual_media_privilege: bool,
    apcp_profile: ApcpLauncherProfile,
    transport_profile: AvctTransportProfile,
}

#[derive(Clone, Debug)]
pub struct IdracAvctOptions {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    pub generation: u8,
    pub virtual_media_privilege: bool,
    pub apcp_selector: u32,
    pub reconnect_capability: bool,
    pub reconnect_version: i16,
    pub reconnect_preference: u16,
}

impl IdracAvctConfig {
    pub fn new(options: IdracAvctOptions) -> Result<Self, String> {
        if options.host.trim().is_empty() {
            return Err("iDRAC AVCT host is empty".to_string());
        }
        if options.username.is_empty() {
            return Err("iDRAC AVCT requires the device username".to_string());
        }
        if options.password.is_empty() {
            return Err("iDRAC AVCT requires the device password".to_string());
        }
        let transport_profile = match options.generation {
            6 => AvctTransportProfile::Idrac6Apcp,
            7 => return Err("iDRAC7 AVCT is not yet source-profiled".to_string()),
            8 => AvctTransportProfile::Idrac8Apcp,
            other => return Err(format!("unsupported iDRAC generation {other}")),
        };
        Ok(Self {
            host: options.host,
            port: options.port,
            username: options.username,
            password: options.password,
            // Dell's normal KVM `ib` login path passes `false` for the final
            // direct-login flag.  `vmprivilege` in the launch arguments is
            // not evidence that this wire bit must be one.
            virtual_media_privilege: options.virtual_media_privilege,
            apcp_profile: ApcpLauncherProfile {
                selector: options.apcp_selector,
                reconnect_capability: options.reconnect_capability,
                reconnect_version: options.reconnect_version,
                reconnect_preference: options.reconnect_preference,
            },
            transport_profile,
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash)]
struct PoolKey {
    host: String,
    port: u16,
    transport_profile: AvctTransportProfile,
    credential_fingerprint: u64,
}

impl PoolKey {
    fn from_config(config: &IdracAvctConfig) -> Self {
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        config.username.hash(&mut hash);
        config.password.hash(&mut hash);
        config.virtual_media_privilege.hash(&mut hash);
        config.apcp_profile.hash(&mut hash);
        Self {
            host: config.host.clone(),
            port: config.port,
            transport_profile: config.transport_profile,
            credential_fingerprint: hash.finish(),
        }
    }
}

enum PoolSlot {
    Connecting(Arc<watch::Sender<bool>>),
    Ready(Arc<AvctSession>),
    /// The final feature lease has been released. Keep this slot until the
    /// AVCT task has dropped both TLS sockets, so a new login cannot race the
    /// appliance's single-session cleanup.
    Closing(Arc<AvctSession>),
}
static POOL: LazyLock<Mutex<HashMap<PoolKey, PoolSlot>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn pool() -> MutexGuard<'static, HashMap<PoolKey, PoolSlot>> {
    POOL.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A `Closing` entry is normally reclaimed only after the AVCT task has
/// dropped both TLS sockets.  The logical disconnect is a bounded fallback:
/// it is set only after the official close record was written (or the peer
/// ended the session), and avoids stranding every later browser connection if
/// the detached task never publishes its final watch notification.
#[cfg(test)]
fn closing_session_can_be_reclaimed(session: &AvctSession) -> bool {
    *session.transport_closed_rx().borrow() || !session.is_connected()
}

#[derive(Clone, Debug)]
pub enum HidCommand {
    Keyboard {
        scancode: u16,
        pressed: bool,
    },
    Consumer {
        usage: u16,
    },
    MouseMove {
        x: i16,
        y: i16,
        buttons: u8,
    },
    MouseButton {
        x: i16,
        y: i16,
        buttons: u8,
    },
    MouseScroll {
        x: i16,
        y: i16,
        buttons: u8,
        scroll: i8,
    },
    Reset,
}

pub struct AvctSession {
    shutdown: watch::Sender<bool>,
    disconnected: watch::Sender<bool>,
    transport_closed: watch::Sender<bool>,
    connected: AtomicBool,
    accepting_commands: AtomicBool,
    control_epoch: AtomicU64,
    leases: AtomicUsize,
    packets_in: AtomicU64,
    packets_out: AtomicU64,
    display_packets: AtomicU64,
    decoded_frames: AtomicU64,
    unchanged_display_packets: AtomicU64,
    suppressed_startup_display_packets: AtomicU64,
    display_decode_errors: AtomicU64,
    last_display_message_type: AtomicU32,
    last_display_codec: AtomicU32,
    last_display_flags: AtomicU32,
    terminal_reason: AtomicU32,
    bootstrap_complete: AtomicBool,
    bootstrap: Mutex<model::Bootstrap>,
    display_running: AtomicBool,
    video_width: AtomicU32,
    video_height: AtomicU32,
    negotiated_width: AtomicU32,
    negotiated_height: AtomicU32,
    negotiated_display_format: AtomicU32,
    last_control_message_type: AtomicU32,
    generic_control_messages: AtomicU64,
    apcp_server_marker: u16,
    apcp_protocol_version: u16,
    apcp_transports: u32,
    apcp_redirect_port: u16,
    transport_profile: AvctTransportProfile,
    /// The only source-confirmed AVCT reconnect handle.  It is assigned by
    /// inbound `0x8102`, not inferred from APCP tails or login randoms.
    /// A zero value means the server has not made this session reconnectable.
    reconnect_session: AtomicU32,
    dvc_startup: Mutex<DvcStartup>,
    dvc_mode: Mutex<Option<ColorModeResponse>>,
    dvc_mode_generation: AtomicU64,
    dvc_mode_notify: Notify,
    dvc_switch: Mutex<DvcProfileSwitch>,
    dvc_switch_notify: Notify,
    dvc_switch_lock: tokio::sync::Mutex<()>,
    keyboard_led_sync_at: tokio::sync::Mutex<Option<tokio::time::Instant>>,
    control_tx: mpsc::Sender<ControlWrite>,
    frame_tx: tokio::sync::broadcast::Sender<DecodedFrame>,
    last_frame: Mutex<Option<DecodedFrame>>,
    video_state: watch::Sender<AvctVideoState>,
}

enum ControlWrite {
    Single(u64, AvctPacket),
    Pair(u64, AvctPacket, AvctPacket),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AvctVideoState {
    Negotiating,
    Running,
    Stopped,
}

impl AvctSession {
    async fn connect(config: &IdracAvctConfig) -> Result<Arc<Self>, String> {
        bounded_session_setup(Self::connect_inner(config)).await
    }

    async fn connect_inner(config: &IdracAvctConfig) -> Result<Arc<Self>, String> {
        let initial_request = ApcpRequestKind::initial(config.apcp_profile)?;
        let (control, video, apcp, packet_limit) =
            establish_console_transport(config, initial_request).await?;

        let (shutdown, shutdown_rx) = watch::channel(false);
        let (disconnected, _) = watch::channel(false);
        let (transport_closed, _) = watch::channel(false);
        // Control events are bounded so a stopped or slow BMC cannot grow
        // unbounded memory. Mouse movement is deliberately coalesced by the
        // HID adapter before it reaches this queue.
        let (control_tx, control_rx) = mpsc::channel(128);
        let (frame_tx, _) = tokio::sync::broadcast::channel(16);
        let (video_state, _) = watch::channel(AvctVideoState::Negotiating);
        let session = Arc::new(Self {
            shutdown,
            disconnected,
            transport_closed,
            connected: AtomicBool::new(true),
            accepting_commands: AtomicBool::new(true),
            control_epoch: AtomicU64::new(0),
            leases: AtomicUsize::new(0),
            packets_in: AtomicU64::new(0),
            // login + video-channel authorisation.
            packets_out: AtomicU64::new(2),
            display_packets: AtomicU64::new(0),
            decoded_frames: AtomicU64::new(0),
            unchanged_display_packets: AtomicU64::new(0),
            suppressed_startup_display_packets: AtomicU64::new(0),
            display_decode_errors: AtomicU64::new(0),
            last_display_message_type: AtomicU32::new(0),
            last_display_codec: AtomicU32::new(0),
            last_display_flags: AtomicU32::new(0),
            terminal_reason: AtomicU32::new(0),
            bootstrap_complete: AtomicBool::new(false),
            bootstrap: Mutex::new(model::Bootstrap::default()),
            display_running: AtomicBool::new(false),
            video_width: AtomicU32::new(0),
            video_height: AtomicU32::new(0),
            negotiated_width: AtomicU32::new(0),
            negotiated_height: AtomicU32::new(0),
            negotiated_display_format: AtomicU32::new(0),
            last_control_message_type: AtomicU32::new(0),
            generic_control_messages: AtomicU64::new(0),
            apcp_server_marker: apcp.as_ref().map_or(0, |setup| setup.server_marker),
            apcp_protocol_version: apcp.as_ref().map_or(0, |setup| setup.protocol_version),
            apcp_transports: apcp.as_ref().map_or(0, |setup| setup.transports),
            apcp_redirect_port: apcp.as_ref().map_or(0, |setup| setup.redirect_port),
            transport_profile: config.transport_profile,
            reconnect_session: AtomicU32::new(0),
            dvc_startup: Mutex::new(DvcStartup::new(
                config.transport_profile,
                tokio::time::Instant::now(),
            )),
            dvc_mode: Mutex::new(None),
            dvc_mode_generation: AtomicU64::new(0),
            dvc_mode_notify: Notify::new(),
            dvc_switch: Mutex::new(DvcProfileSwitch::default()),
            dvc_switch_notify: Notify::new(),
            dvc_switch_lock: tokio::sync::Mutex::new(()),
            keyboard_led_sync_at: tokio::sync::Mutex::new(None),
            control_tx,
            frame_tx,
            last_frame: Mutex::new(None),
            video_state,
        });
        tokio::spawn(supervise_session_task(
            session.clone(),
            run(
                control,
                video,
                session.clone(),
                config.clone(),
                shutdown_rx,
                control_rx,
                packet_limit,
            ),
        ));
        Ok(session)
    }

    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }
    pub fn disconnected_rx(&self) -> watch::Receiver<bool> {
        self.disconnected.subscribe()
    }
    fn transport_closed_rx(&self) -> watch::Receiver<bool> {
        self.transport_closed.subscribe()
    }
    pub fn subscribe_frames(&self) -> tokio::sync::broadcast::Receiver<DecodedFrame> {
        self.frame_tx.subscribe()
    }
    pub fn last_frame(&self) -> Option<DecodedFrame> {
        self.last_frame
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
    pub fn video_state_rx(&self) -> watch::Receiver<AvctVideoState> {
        self.video_state.subscribe()
    }
    pub fn shutdown(&self) {
        let _ = self.shutdown.send(true);
    }

    pub async fn request_full_frame(&self) -> Result<(), String> {
        if !self.display_running.load(Ordering::SeqCst) {
            return Err("iDRAC AVCT display is not running; full-frame requests are unavailable before the first display packet".to_string());
        }
        // Dell's client uses this fixed 0x0301 control packet only after the
        // display channel is running.
        self.send_control(AvctPacket::beef(0x0301, Bytes::from_static(&[0; 8])))
            .await
    }

    pub async fn send_relative_pointer(&self, x: i16, y: i16, buttons: u8) -> Result<(), String> {
        self.send_relative_pointer_with_scroll(x, y, buttons, 0)
            .await
    }

    pub async fn send_relative_pointer_with_scroll(
        &self,
        x: i16,
        y: i16,
        buttons: u8,
        scroll: i8,
    ) -> Result<(), String> {
        if !self.is_connected() {
            return Err("iDRAC AVCT session is disconnected".to_string());
        }
        self.send_control(delta_pointer_packet(x, y, buttons, scroll))
            .await
    }

    /// Set an official-client DVC colour-depth slider profile on the control
    /// socket. Unknown profile IDs are rejected before any protocol write.
    pub async fn set_color_depth(&self, profile_id: &str) -> Result<(), String> {
        let profile = color_depth_profile(profile_id)
            .ok_or_else(|| format!("unsupported iDRAC DVC color-depth profile '{profile_id}'"))?;
        if !supported_color_depth_profiles(self.transport_profile).contains(&profile) {
            return Err(format!(
                "iDRAC DVC color-depth profile '{profile_id}' is not interoperable with this transport profile"
            ));
        }
        let _operation = self.dvc_switch_lock.lock().await;
        let operation_epoch = self.control_epoch.load(Ordering::Acquire);
        if matches!(self.transport_profile, AvctTransportProfile::Idrac8Apcp) {
            let started = tokio::time::Instant::now();
            let mode_generation = self.dvc_mode_generation.load(Ordering::Acquire);
            self.send_control_pair(color_depth_request(profile), screen_refresh())
                .await?;
            self.await_idrac8_dvc_mode(profile, mode_generation, started, operation_epoch)
                .await?;
            let mut switch = self
                .dvc_switch
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !model::command_is_current(
                self.control_epoch.load(Ordering::Acquire),
                operation_epoch,
            ) {
                return Err(
                    "iDRAC color-depth transport epoch changed; remote result is unknown"
                        .to_string(),
                );
            }
            switch.confirm_profile(profile);
            return Ok(());
        }
        if !self.display_running.load(Ordering::SeqCst)
            || self.dvc_startup_state().phase() != DvcStartupPhase::Ready
        {
            return Err(
                "iDRAC6 color depth is unavailable until the baseline DVC15 frame is ready"
                    .to_string(),
            );
        }

        let generation = {
            self.dvc_switch
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .queue(profile)?
        };
        if let Err(error) = self.send_control(color_depth_request(profile)).await {
            self.dvc_switch
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .cancel_queued(generation);
            return Err(error);
        }

        let mut disconnected = self.disconnected_rx();
        tokio::time::timeout(IDRAC6_DVC_SWITCH_TIMEOUT * 2 + Duration::from_secs(2), async {
            loop {
                let notified = self.dvc_switch_notify.notified();
                if !model::command_is_current(self.control_epoch.load(Ordering::Acquire), operation_epoch) {
                    return Err("iDRAC color-depth transport epoch changed; remote result is unknown".to_string());
                }
                if let Some(result) = self
                    .dvc_switch
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take_completion(generation)
                {
                    return result;
                }
                tokio::select! {
                    _ = notified => {}
                    changed = disconnected.changed() => {
                        if changed.is_err() || *disconnected.borrow() {
                            return Err("iDRAC AVCT session ended during color-depth transition".to_string());
                        }
                    }
                }
            }
        })
        .await
        .map_err(|_| "iDRAC6 color-depth transition and DVC15 recovery timed out".to_string())?
    }

    fn record_dvc_mode(&self, mode: ColorModeResponse) {
        let mut current = self
            .dvc_mode
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // The mode lock serializes the observation and generation commit.
        let Some(next) = self
            .dvc_mode_generation
            .load(Ordering::Acquire)
            .checked_add(1)
        else {
            self.set_terminal_reason(AvctTerminalReason::Unknown);
            self.shutdown();
            return;
        };
        *current = Some(mode);
        self.dvc_mode_generation.store(next, Ordering::Release);
        drop(current);
        self.dvc_mode_notify.notify_waiters();
    }

    async fn await_idrac8_dvc_mode(
        &self,
        profile: ColorDepthProfile,
        baseline_generation: u64,
        started: tokio::time::Instant,
        operation_epoch: u64,
    ) -> Result<(), String> {
        use bmc_idrac_model::operation::{Operation, OperationEffect, OperationEvent};
        let mut model = Operation::new(Duration::ZERO, IDRAC8_DVC_APPLY_TIMEOUT)
            .step(OperationEvent::WriteFinished {
                now: started.elapsed(),
                accepted: true,
            })
            .state;
        let deadline = started + model.deadline();
        let mut disconnected = self.disconnected_rx();
        let mut observed = None;
        loop {
            let notified = self.dvc_mode_notify.notified();
            if !model::command_is_current(
                self.control_epoch.load(Ordering::Acquire),
                operation_epoch,
            ) {
                let _ = model.step(OperationEvent::TransportLost);
                return Err(
                    "iDRAC color-depth transport epoch changed; remote result is unknown"
                        .to_string(),
                );
            }
            if self.dvc_mode_generation.load(Ordering::Acquire) > baseline_generation {
                observed = *self
                    .dvc_mode
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let transition = model.step(OperationEvent::Observation {
                    now: started.elapsed(),
                    matches: observed.is_some_and(|mode| {
                        mode.mode == profile.depth_bpp && mode.flag_1 == profile.variant
                    }),
                });
                model = transition.state;
                if transition.effect == OperationEffect::Confirm {
                    // Read the epoch after acquiring the mode observation, so
                    // a new socket's status cannot satisfy an old operation.
                    if !model::command_is_current(
                        self.control_epoch.load(Ordering::Acquire),
                        operation_epoch,
                    ) {
                        return Err(
                            "iDRAC color-depth transport epoch changed; remote result is unknown"
                                .to_string(),
                        );
                    }
                    return Ok(());
                }
            }
            tokio::select! {
                _ = notified => {}
                _ = tokio::time::sleep_until(deadline) => {
                    let _ = model.step(OperationEvent::Tick { now: started.elapsed() });
                    return Err(match observed {
                        Some(mode) => format!(
                            "iDRAC DVC status did not confirm profile '{}': mode={} flag_1={} flag_2={}",
                            profile.id, mode.mode, mode.flag_1, mode.flag_2
                        ),
                        None => format!(
                            "iDRAC DVC status did not report profile '{}' within {} seconds",
                            profile.id,
                            IDRAC8_DVC_APPLY_TIMEOUT.as_secs()
                        ),
                    });
                }
                changed = disconnected.changed() => {
                    if changed.is_err() || *disconnected.borrow() {
                        let _ = model.step(OperationEvent::TransportLost);
                        return Err("iDRAC AVCT session ended during color-depth confirmation; remote result is unknown".to_string());
                    }
                }
            }
        }
    }

    pub(crate) fn color_depth_profiles(&self) -> &'static [ColorDepthProfile] {
        supported_color_depth_profiles(self.transport_profile)
    }
    pub(crate) fn active_color_depth_profile_id(&self) -> Option<&'static str> {
        self.dvc_switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .active_profile()
            .map(|profile| profile.id)
    }
    pub async fn send_hid(&self, command: HidCommand) -> Result<(), String> {
        let (width, height) = (
            self.video_width.load(Ordering::Relaxed),
            self.video_height.load(Ordering::Relaxed),
        );
        let pointer = |x: i16, y: i16, buttons: u8, scroll: i8| {
            let x = clamp_coordinate(x, width);
            let y = clamp_coordinate(y, height);
            pointer_packet(x, y, buttons, scroll)
        };
        match command {
            HidCommand::Keyboard { scancode, pressed } => {
                self.send_control(keyboard_packet(scancode, pressed)).await
            }
            HidCommand::Consumer { usage } => {
                self.send_control_pair(keyboard_packet(usage, true), keyboard_packet(usage, false))
                    .await
            }
            HidCommand::MouseMove { x, y, buttons } => {
                self.send_control(pointer(x, y, buttons, 0)).await
            }
            HidCommand::MouseButton { x, y, buttons } => {
                self.send_control(pointer(x, y, buttons, 0)).await
            }
            HidCommand::MouseScroll {
                x,
                y,
                buttons,
                scroll,
            } => {
                if scroll == 0 {
                    return Ok(());
                }
                self.send_control(pointer(x, y, buttons, scroll)).await
            }
            HidCommand::Reset => self.send_control(mouse_reset_packet()).await,
        }
    }

    /// Mirror the official viewer's focus-gained façade operation before the
    /// browser starts delivering keyboard and pointer events.
    pub async fn activate_hid_focus(&self) -> Result<(), String> {
        self.send_control(focus_control_packet(true)).await?;
        self.synchronize_keyboard_leds().await
    }

    pub async fn synchronize_keyboard_leds(&self) -> Result<(), String> {
        const DEBOUNCE: Duration = Duration::from_millis(250);
        let now = tokio::time::Instant::now();
        let mut last = self.keyboard_led_sync_at.lock().await;
        if last.is_some_and(|instant| now.duration_since(instant) < DEBOUNCE) {
            return Ok(());
        }
        self.send_control(keyboard_led_request_packet()).await?;
        *last = Some(now);
        Ok(())
    }

    async fn send_control(&self, packet: AvctPacket) -> Result<(), String> {
        let epoch = self.control_epoch.load(Ordering::Acquire);
        if !self.accepting_commands.load(Ordering::Acquire) {
            return Err("iDRAC AVCT input is unavailable during transport recovery".to_string());
        }
        if !self.is_connected() {
            return Err("iDRAC AVCT session is disconnected".to_string());
        }
        tokio::time::timeout(
            Duration::from_secs(5),
            self.control_tx.send(ControlWrite::Single(epoch, packet)),
        )
        .await
        .map_err(|_| "iDRAC AVCT control queue timed out".to_string())?
        .map_err(|_| "iDRAC AVCT control channel is closed".to_string())
    }

    async fn send_control_pair(&self, first: AvctPacket, second: AvctPacket) -> Result<(), String> {
        let epoch = self.control_epoch.load(Ordering::Acquire);
        if !self.accepting_commands.load(Ordering::Acquire) {
            return Err("iDRAC AVCT input is unavailable during transport recovery".to_string());
        }
        if !self.is_connected() {
            return Err("iDRAC AVCT session is disconnected".to_string());
        }
        tokio::time::timeout(
            Duration::from_secs(5),
            self.control_tx
                .send(ControlWrite::Pair(epoch, first, second)),
        )
        .await
        .map_err(|_| "iDRAC AVCT control queue timed out".to_string())?
        .map_err(|_| "iDRAC AVCT control channel is closed".to_string())
    }

    fn request_default_dvc(&self) -> bool {
        let mut state = self
            .bootstrap
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let transition = state.step(model::BootstrapEvent::BaselineNeeded);
        *state = transition.state;
        transition.effect == model::BootstrapEffect::RequestBaseline
    }

    fn dvc_startup_state(&self) -> MutexGuard<'_, DvcStartup> {
        self.dvc_startup
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn dvc_startup_deadline(&self) -> Option<tokio::time::Instant> {
        self.dvc_startup_state().deadline()
    }

    fn observe_dvc_display(&self, message_type: u16, flags: u8) -> DvcDisplayAction {
        self.dvc_startup_state()
            .observe_display(message_type, flags)
    }

    fn finish_refreshed_dvc_chain(&self) -> bool {
        let finished = self.dvc_startup_state().finish_refreshed_chain();
        if finished {
            self.dvc_switch
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .confirm_profile(
                    color_depth_profile("variant-1-15").expect("built-in fallback profile"),
                );
        }
        finished
    }

    fn dvc_switch_deadline(&self) -> Option<tokio::time::Instant> {
        self.dvc_switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .deadline()
    }

    fn expired_dvc_switch(&self, now: tokio::time::Instant) -> Option<DvcSwitchTimeout> {
        self.dvc_switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .expired_phase(now)
    }

    fn activate_queued_dvc_switch(&self, packet: &AvctPacket) -> bool {
        let mut switch = self
            .dvc_switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(profile) = switch.requested() else {
            return false;
        };
        if switch.phase() != DvcProfileSwitchPhase::Queued
            || packet.message_type != super::dvc::SET_DVC_COLOR_DEPTH_TYPE
            || packet.payload != color_depth_request(profile).payload
        {
            return false;
        }
        let generation = switch.generation();
        switch.mark_request_written(generation, tokio::time::Instant::now())
    }

    fn observe_dvc_switch(&self, message_type: u16, flags: u8) -> DvcSwitchAction {
        let mut switch = self
            .dvc_switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        switch.observe_display(message_type, flags)
    }

    fn begin_dvc_switch_fallback(&self, error: &str) -> bool {
        self.dvc_switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .begin_fallback(error, tokio::time::Instant::now())
    }

    fn finish_dvc_target_chain(&self) -> bool {
        let finished = self
            .dvc_switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish_target_chain();
        if finished {
            self.dvc_switch_notify.notify_waiters();
        }
        finished
    }

    fn finish_dvc_fallback_chain(&self) -> bool {
        let finished = self
            .dvc_switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish_fallback_chain();
        if finished {
            self.dvc_switch_notify.notify_waiters();
        }
        finished
    }

    fn abort_dvc_switch(&self, error: &str) {
        if self
            .dvc_switch
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .abort(error)
        {
            self.dvc_switch_notify.notify_waiters();
        }
    }

    fn suspend_dvc_for_no_signal(&self) {
        self.dvc_startup_state().suspend_for_no_signal();
    }

    fn resume_dvc_for_video_ready(&self, now: tokio::time::Instant) {
        self.dvc_startup_state().resume_for_video_ready(now);
    }

    fn fail_dvc_startup(&self) {
        self.dvc_startup_state().fail();
    }

    fn expire_dvc_startup(&self, now: tokio::time::Instant) -> bool {
        self.dvc_startup_state().fail_if_expired(now)
    }

    fn set_terminal_reason(&self, reason: AvctTerminalReason) {
        self.terminal_reason.store(reason as u32, Ordering::Relaxed);
    }

    pub fn trace_snapshot(&self) -> serde_json::Value {
        let (dvc_startup_phase, dvc_deadline_remaining_ms) = {
            let startup = self.dvc_startup_state();
            let remaining = startup.deadline().map(|deadline| {
                deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            });
            (startup.phase().as_str(), remaining)
        };
        let (dvc_switch_phase, dvc_switch_deadline_remaining_ms, active_profile, requested_profile) = {
            let state = self
                .dvc_switch
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let remaining = state.deadline().map(|deadline| {
                deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .as_millis()
                    .min(u128::from(u64::MAX)) as u64
            });
            (
                state.phase().as_str(),
                remaining,
                state.active_profile().map(|profile| profile.id),
                state.requested().map(|profile| profile.id),
            )
        };
        serde_json::json!({
            "transport": "idrac-avct",
            "transport_profile": match self.transport_profile {
                AvctTransportProfile::Idrac6Apcp => "idrac6-apcp",
                AvctTransportProfile::Idrac8Apcp => "idrac8-apcp",
            },
            "connected": self.is_connected(),
            "apcp": {
                "server_marker": self.apcp_server_marker,
                "protocol_version": self.apcp_protocol_version,
                "transports": self.apcp_transports,
                "redirect_port": self.apcp_redirect_port,
                "reconnect_session_available": self.reconnect_session.load(Ordering::Relaxed) != 0,
            },
            "dvc": {
                "startup_phase": dvc_startup_phase,
                "startup_deadline_remaining_ms": dvc_deadline_remaining_ms,
                "switch_phase": dvc_switch_phase,
                "switch_deadline_remaining_ms": dvc_switch_deadline_remaining_ms,
                "active_profile": active_profile,
                "requested_profile": requested_profile,
                "idrac8_default_request_sent": self.bootstrap.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).baseline_requested(),
                "mode": self.dvc_mode.lock().ok().and_then(|state| *state).map(|mode| serde_json::json!({
                    "mode": mode.mode,
                    "flag_1": mode.flag_1,
                    "flag_2": mode.flag_2,
                })),
            },
            "packets": {
                "in": self.packets_in.load(Ordering::Relaxed),
                "out": self.packets_out.load(Ordering::Relaxed),
                "display": self.display_packets.load(Ordering::Relaxed),
                "decoded_frames": self.decoded_frames.load(Ordering::Relaxed),
                "unchanged_display": self.unchanged_display_packets.load(Ordering::Relaxed),
                "suppressed_startup_display": self.suppressed_startup_display_packets.load(Ordering::Relaxed),
                "decode_errors": self.display_decode_errors.load(Ordering::Relaxed),
                "last_display_message_type": self.last_display_message_type.load(Ordering::Relaxed),
                "last_display_codec": self.last_display_codec.load(Ordering::Relaxed),
                "last_display_flags": self.last_display_flags.load(Ordering::Relaxed),
            },
            "bootstrap_complete": self.bootstrap_complete.load(Ordering::Relaxed),
            "display_running": self.display_running.load(Ordering::Relaxed),
            "terminal_reason": self.terminal_reason().as_str(),
            "video": {
                "width": self.video_width.load(Ordering::Relaxed),
                "height": self.video_height.load(Ordering::Relaxed),
                "negotiated_width": self.negotiated_width.load(Ordering::Relaxed),
                "negotiated_height": self.negotiated_height.load(Ordering::Relaxed),
                "negotiated_display_format": self.negotiated_display_format.load(Ordering::Relaxed),
            },
            "control": {
                "last_message_type": self.last_control_message_type.load(Ordering::Relaxed),
                "generic_messages": self.generic_control_messages.load(Ordering::Relaxed),
            },
        })
    }

    fn mark_disconnected(&self) {
        self.accepting_commands.store(false, Ordering::Release);
        self.connected.store(false, Ordering::SeqCst);
        self.disconnected.send_replace(true);
    }

    fn mark_transport_closed(&self) {
        self.transport_closed.send_replace(true);
    }

    pub fn terminal_reason(&self) -> AvctTerminalReason {
        AvctTerminalReason::from_raw(self.terminal_reason.load(Ordering::Relaxed))
    }
}

async fn supervise_session_task<F>(session: Arc<AvctSession>, task: F)
where
    F: Future<Output = ()>,
{
    if AssertUnwindSafe(task).catch_unwind().await.is_err() {
        session.set_terminal_reason(AvctTerminalReason::SessionTaskPanic);
        session.fail_dvc_startup();
        session.display_running.store(false, Ordering::SeqCst);
        session.mark_disconnected();
        session.mark_transport_closed();
        tracing::error!("iDRAC AVCT session task panicked; published terminal disconnect");
    }
}

/// Build both AVCT sockets in the exact official order.  `control_request`
/// is explicit so a reconnect can use its retained-session writer while the
/// display channel keeps the normal post-login attach flow.
async fn establish_console_transport(
    config: &IdracAvctConfig,
    control_request: ApcpRequestKind,
) -> Result<(BoxedAvctIo, BoxedAvctIo, Option<ApcpSetup>, usize), String> {
    let is_idrac6 = config.transport_profile == AvctTransportProfile::Idrac6Apcp;
    let control_request = if is_idrac6 {
        ApcpRequestKind::Idrac6Initial53
    } else {
        control_request
    };
    let (mut control, apcp) =
        open_channel_with_request(config, ApcpChannel::Control, control_request).await?;
    let login = build_login_request(
        if is_idrac6 {
            LoginProfile::Compatibility
        } else {
            LoginProfile::from_apcp_pair(apcp.generation_major, apcp.generation_minor)
        },
        &config.username,
        &config.password,
        config.virtual_media_privilege,
    )?;
    write_connect_packet(&mut control, &login.packet, "login request").await?;
    let packet_limit = avct_packet_limit(&apcp);
    let login_response = wait_for_login(&mut control, packet_limit).await?;
    let (mut video, _) = if is_idrac6 {
        open_channel_with_request(config, ApcpChannel::Video, ApcpRequestKind::Idrac6Initial53)
            .await?
    } else {
        open_channel(config, ApcpChannel::Video).await?
    };
    write_connect_packet(
        &mut video,
        &if is_idrac6 {
            idrac6_video_session_request(login.client_random, login_response.appliance_random)
        } else {
            video_session_request(login.client_random, login_response.appliance_random)
        },
        "video attach request",
    )
    .await?;
    Ok((control, video, Some(apcp), packet_limit))
}

async fn open_channel(
    config: &IdracAvctConfig,
    channel: ApcpChannel,
) -> Result<(BoxedAvctIo, ApcpSetup), String> {
    open_channel_with_request(
        config,
        channel,
        ApcpRequestKind::initial(config.apcp_profile)?,
    )
    .await
}

/// Establish one fresh APCP/TLS channel with the caller-selected official
/// request writer. Initial console setup obtains its kind from the launcher
/// profile; the native reconnect supervisor must explicitly provide
/// `ReconnectWithRetainedSession` and cannot accidentally fall back to an
/// initial writer.
async fn open_channel_with_request(
    config: &IdracAvctConfig,
    channel: ApcpChannel,
    request_kind: ApcpRequestKind,
) -> Result<(BoxedAvctIo, ApcpSetup), String> {
    let mut tcp = open_tcp(config, "APCP").await?;
    let mut random = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut random);
    tokio::time::timeout(
        APCP_SETUP_TIMEOUT,
        write_request(&mut tcp, request_kind, channel, config.apcp_profile, random),
    )
    .await
    .map_err(|_| "iDRAC AVCT APCP client setup write timed out".to_string())??;
    let apcp = tokio::time::timeout(
        APCP_SETUP_TIMEOUT,
        read_server_setup(&mut tcp, request_kind),
    )
    .await
    .map_err(|_| "iDRAC AVCT APCP server setup timed out".to_string())??;
    let idrac6_legacy = matches!(request_kind, ApcpRequestKind::Idrac6Initial53);
    let plain_transport = idrac6_legacy && apcp.transports & 0x0000_0001 != 0;
    let tls_transport = apcp.transports & 0x0000_0004 != 0;
    if !plain_transport && !tls_transport {
        return Err(format!(
            "iDRAC AVCT APCP server does not advertise an official transport (transports=0x{:08x})",
            apcp.transports
        ));
    }
    if apcp.redirect_port != 0 {
        let redirected_address =
            bmc_backend_support::endpoint::authority(&config.host, apcp.redirect_port)?;
        tcp = tokio::time::timeout(
            Duration::from_secs(10),
            TcpStream::connect(&redirected_address),
        )
        .await
        .map_err(|_| format!("iDRAC AVCT redirect to {redirected_address} timed out"))?
        .map_err(|error| format!("iDRAC AVCT redirect to {redirected_address} failed: {error}"))?;
        tcp.set_nodelay(true)
            .map_err(|error| format!("iDRAC AVCT redirected TCP setup failed: {error}"))?;
    }
    if plain_transport {
        return Ok((Box::new(tcp), apcp));
    }
    let tls = tls_connect(tcp, &config.host).await?;
    Ok((Box::new(tls), apcp))
}

async fn open_tcp(config: &IdracAvctConfig, channel: &str) -> Result<TcpStream, String> {
    let address = bmc_backend_support::endpoint::authority(&config.host, config.port)?;
    let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect(&address))
        .await
        .map_err(|_| format!("iDRAC AVCT {channel} connection to {address} timed out"))?
        .map_err(|error| format!("iDRAC AVCT {channel} connection to {address} failed: {error}"))?;
    tcp.set_nodelay(true)
        .map_err(|error| format!("iDRAC AVCT {channel} TCP setup failed: {error}"))?;
    Ok(tcp)
}

async fn bounded_session_setup<T>(
    operation: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    bounded_session_setup_with_timeout(SESSION_CONNECT_TIMEOUT, operation).await
}

async fn bounded_session_setup_with_timeout<T>(
    deadline: Duration,
    operation: impl Future<Output = Result<T, String>>,
) -> Result<T, String> {
    tokio::time::timeout(deadline, operation)
        .await
        .map_err(|_| {
            format!(
                "iDRAC AVCT session setup timed out after {}s",
                deadline.as_secs()
            )
        })?
}

async fn write_connect_packet<W>(
    stream: &mut W,
    packet: &AvctPacket,
    operation: &'static str,
) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(CONNECT_WRITE_TIMEOUT, write_packet(stream, packet))
        .await
        .map_err(|_| format!("iDRAC AVCT {operation} write timed out"))?
}

fn video_session_request(client_random: u32, appliance_random: u32) -> AvctPacket {
    // `ic` in the reference client writes a generic (non-BEEF) 0x0101
    // request with two zero u32 parameters.  It is the display-channel
    // attach request, not an authentication packet.
    let mut payload = [0u8; 8];
    payload[..4].copy_from_slice(&client_random.to_be_bytes());
    payload[4..].copy_from_slice(&appliance_random.to_be_bytes());
    AvctPacket {
        magic: [0, 0, 0, 0],
        message_type: 0x0101,
        payload: Bytes::copy_from_slice(&payload),
    }
}

fn idrac6_video_session_request(client_random: u32, appliance_random: u32) -> AvctPacket {
    // iDRAC6 `com.avocent.kvm.b.a.yb` extends the normal BEEF packet header,
    // has type 1, and serialises exactly these two u32s.  Unlike iDRAC8 `ic`,
    // its magic is not four zero bytes.
    let mut payload = [0u8; 8];
    payload[..4].copy_from_slice(&client_random.to_be_bytes());
    payload[4..].copy_from_slice(&appliance_random.to_be_bytes());
    AvctPacket::beef(0x0001, Bytes::copy_from_slice(&payload))
}

/// APCP keepalive sent by Dell's `com.avocent.d.a.d` after the control-side
/// 0x8302 readiness record. Although it has the same outer shape as an AVCT
/// packet, its magic is `APCP` and its four-byte payload is fixed by the
/// official writer: `u16(1024), u16(0)`.
fn apcp_keepalive() -> AvctPacket {
    AvctPacket {
        magic: *b"APCP",
        message_type: 0,
        payload: Bytes::from_static(&[0x04, 0x00, 0x00, 0x00]),
    }
}

fn rejected_login_close() -> [AvctPacket; 2] {
    // The failed-login trace shows the official client sending AVCT 0x0400,
    // followed by `com.avocent.c.f.a`'s 16-byte APCP record. This is not the
    // `c.f.f` record used during successful display-session startup.
    [
        AvctPacket::beef(0x0400, Bytes::from_static(&[0; 8])),
        graceful_session_close_record(),
    ]
}

/// Normal client shutdown record from `com.avocent.c.f.a`. Dell's
/// `com.avocent.a.b.w#a()` sends it to both control and video writers, waits
/// 50ms, then closes their sockets. It is distinct from the failed-login
/// `0x0400` packet, which is not part of normal session release.
fn graceful_session_close_record() -> AvctPacket {
    let mut cleanup = [0u8; 8];
    cleanup[..2].copy_from_slice(&0x01ffu16.to_be_bytes());
    AvctPacket {
        magic: *b"APCP",
        message_type: 0,
        payload: Bytes::copy_from_slice(&cleanup),
    }
}

fn display_ack() -> AvctPacket {
    // The reference client's `jb` acknowledgement is emitted on the display
    // writer after each decoded display packet.  A single packet is
    // acknowledged at a time, hence sequence value one.
    AvctPacket::beef(0x0000, Bytes::from_static(&[1, 0, 0, 0, 0, 0, 0, 0]))
}

fn screen_refresh() -> AvctPacket {
    AvctPacket::beef(0x0301, Bytes::from_static(&[0; 8]))
}

async fn write_dvc15_fallback<W>(
    control_write: &mut W,
    session: &AvctSession,
    decoder: &mut AvctDisplayDecoder,
    error: &str,
) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    if !session.begin_dvc_switch_fallback(error) {
        return Ok(());
    }
    decoder.discard_pending_commands();
    let dvc15 = color_depth_profile("variant-1-15")
        .ok_or_else(|| "source-confirmed DVC15 fallback profile is unavailable".to_string())?;
    write_session_packet(
        control_write,
        &color_depth_request(dvc15),
        "iDRAC6 DVC15 profile fallback",
    )
    .await?;
    session.packets_out.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

fn startup_dvc_request(profile: AvctTransportProfile) -> Option<AvctPacket> {
    matches!(profile, AvctTransportProfile::Idrac6Apcp)
        .then(|| color_depth_profile("variant-1-15").map(color_depth_request))
        .flatten()
}

fn supported_color_depth_profiles(profile: AvctTransportProfile) -> &'static [ColorDepthProfile] {
    match profile {
        AvctTransportProfile::Idrac6Apcp => IDRAC6_COLOR_DEPTH_PROFILES,
        AvctTransportProfile::Idrac8Apcp => IDRAC8_COLOR_DEPTH_PROFILES,
    }
}

fn display_bootstrap() -> [AvctPacket; 3] {
    let mut resolution = [0u8; 8];
    resolution[..2].copy_from_slice(&1024u16.to_be_bytes());
    resolution[2..4].copy_from_slice(&768u16.to_be_bytes());
    [
        // 0x030e: enable the display path.
        AvctPacket::beef(0x030e, Bytes::from_static(&[1, 1, 0, 0, 0, 0, 0, 0])),
        // 0x0302: request the reference client's initial 1024x768 mode.
        AvctPacket::beef(0x0302, Bytes::from(resolution.to_vec())),
        // 0x0304: finish display-mode negotiation.
        AvctPacket::beef(0x0304, Bytes::from_static(&[0; 8])),
    ]
}

/// Dell's `com.avocent.d.g.a.b#a(fc)` treats only a `0x0084` whose first
/// payload byte is zero as the direct start-video state. Its `0x8203` control
/// handler is unrelated, so requiring both notifications adds a race that the
/// reference client does not have.  The remaining payload bytes are opaque;
/// do not promote a generic packet type into permission to start video.
fn is_display_bootstrap_trigger(packet: &AvctPacket) -> bool {
    packet.message_type == 0x0084 && packet.payload.first() == Some(&0)
}

async fn write_session_packet<W>(
    stream: &mut W,
    packet: &AvctPacket,
    operation: &'static str,
) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(5), write_packet(stream, packet))
        .await
        .map_err(|_| format!("iDRAC AVCT {operation} write timed out"))?
}

async fn write_queued_control<W>(
    control_write: &mut W,
    session: &AvctSession,
    decoder: &mut AvctDisplayDecoder,
    packet: &AvctPacket,
) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    write_session_packet(control_write, packet, "control").await?;
    session.packets_out.fetch_add(1, Ordering::Relaxed);
    if session.activate_queued_dvc_switch(packet) {
        decoder.discard_pending_commands();
    }
    Ok(())
}

async fn try_write_graceful_close<W>(stream: &mut W, packet: &AvctPacket) -> bool
where
    W: tokio::io::AsyncWrite + Unpin,
{
    matches!(
        tokio::time::timeout(Duration::from_millis(200), write_packet(stream, packet)).await,
        Ok(Ok(()))
    )
}

async fn send_display_bootstrap<W>(
    control_write: &mut W,
    session: &Arc<AvctSession>,
) -> Result<(), String>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    // The video-ready record is occasionally repeated by firmware.  The
    // bootstrap sequence changes server state, so replaying it is not an
    // idempotent wire operation even though handling the notification must be
    // idempotent locally.
    let transition = {
        let mut state = session
            .bootstrap
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let transition = state.step(model::BootstrapEvent::Requested);
        *state = transition.state;
        transition
    };
    match transition.effect {
        model::BootstrapEffect::Write => {}
        model::BootstrapEffect::Wait => return Ok(()),
        model::BootstrapEffect::Ready
        | model::BootstrapEffect::RequestBaseline
        | model::BootstrapEffect::Reset
        | model::BootstrapEffect::Reject
        | model::BootstrapEffect::IgnoreTerminal => {
            return Err("iDRAC display bootstrap is terminal".to_string())
        }
    }
    let result = async {
        for bootstrap in display_bootstrap() {
            write_session_packet(control_write, &bootstrap, "display bootstrap").await?;
            session.packets_out.fetch_add(1, Ordering::Relaxed);
        }
        // This is a product startup policy, not an inferred handshake field:
        // iDRAC6 hardware interop validates DVC15 as the stable fresh-session
        // family, while a persisted DVC7/GRAY selection begins with retained
        // deltas that cannot reconstruct a new local framebuffer.
        if let Some(request) = startup_dvc_request(session.transport_profile) {
            write_session_packet(control_write, &request, "iDRAC6 startup DVC15 request").await?;
            session.packets_out.fetch_add(1, Ordering::Relaxed);
            session.dvc_startup_state().mark_bootstrap_complete();
        }
        Ok::<_, String>(())
    }
    .await;
    let transition = {
        let mut state = session
            .bootstrap
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let transition = state.step(model::BootstrapEvent::Finished {
            success: result.is_ok(),
        });
        *state = transition.state;
        transition
    };
    session.bootstrap_complete.store(
        transition.effect == model::BootstrapEffect::Ready,
        Ordering::SeqCst,
    );
    result
}

async fn tls_connect(tcp: TcpStream, host: &str) -> Result<TlsStream<TcpStream>, String> {
    // This accepts the appliance certificate only for the AVCT port. Native TLS
    // delegates static RSA/CBC negotiation to OpenSSL/system TLS.
    let mut builder = NativeTlsConnector::builder();
    builder.danger_accept_invalid_certs(true);
    builder.danger_accept_invalid_hostnames(true);
    let connector = TlsConnector::from(
        builder
            .build()
            .map_err(|e| format!("iDRAC AVCT TLS setup failed: {e}"))?,
    );
    tokio::time::timeout(
        Duration::from_secs(10),
        connector.connect(host.trim().trim_matches(['[', ']']), tcp),
    )
    .await
    .map_err(|_| "iDRAC AVCT TLS handshake timed out".to_string())?
    .map_err(|e| format!("iDRAC AVCT TLS handshake failed: {e}"))
}

fn avct_packet_limit(apcp: &ApcpSetup) -> usize {
    if apcp.generation_major <= 1 {
        IDRAC6_MAX_AVCT_PACKET_BYTES
    } else {
        IDRAC8_MAX_AVCT_PACKET_BYTES
    }
}

async fn wait_for_login<S>(stream: &mut S, packet_limit: usize) -> Result<LoginResponse, String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let packet = read_packet(stream, packet_limit).await?;
            if matches!(packet.message_type, 0x8300 | 0x8305) {
                return match parse_login_response(&packet)? {
                    LoginResult::Accepted(response) => Ok(response),
                    LoginResult::Rejected(response) => {
                        for close in rejected_login_close() {
                            write_packet(stream, &close).await?;
                        }
                        Err(format!(
                            "iDRAC AVCT login was rejected with status {}",
                            response.status
                        ))
                    }
                };
            }
        }
    })
    .await
    .map_err(|_| "iDRAC AVCT login timed out".to_string())?
}

async fn run(
    control: BoxedAvctIo,
    video: BoxedAvctIo,
    session: Arc<AvctSession>,
    config: IdracAvctConfig,
    mut shutdown: watch::Receiver<bool>,
    mut control_rx: mpsc::Receiver<ControlWrite>,
    packet_limit: usize,
) {
    // `run_session_loop` owns the TLS streams. Signal pool waiters only after
    // it returns, which guarantees both sockets have been dropped locally.
    run_session_loop(
        control,
        video,
        session.clone(),
        &mut shutdown,
        &mut control_rx,
        packet_limit,
        config.transport_profile == AvctTransportProfile::Idrac8Apcp,
    )
    .await;
    // Inputs queued for the old transport must not replay on a new epoch.
    session.accepting_commands.store(false, Ordering::Release);
    let Some(next_epoch) = session.control_epoch.load(Ordering::Acquire).checked_add(1) else {
        session.set_terminal_reason(AvctTerminalReason::Unknown);
        session.mark_disconnected();
        session.mark_transport_closed();
        return;
    };
    session.control_epoch.store(next_epoch, Ordering::Release);
    session.dvc_mode_notify.notify_waiters();
    session.dvc_switch_notify.notify_waiters();
    while control_rx.try_recv().is_ok() {}
    use bmc_idrac_model::session::{Session, SessionEffect, SessionEvent};
    let mut lifecycle = Session::default()
        .step(SessionEvent::Established { epoch: 0 })
        .state;
    let reconnect_handle = session.reconnect_session.load(Ordering::Relaxed);
    let terminal_reason = session.terminal_reason();
    let transition = lifecycle.step(if *shutdown.borrow() {
        SessionEvent::Stop
    } else {
        SessionEvent::TransportLost {
            epoch: lifecycle.epoch(),
            recoverable: config.transport_profile == AvctTransportProfile::Idrac8Apcp
                && native_reconnect_eligible(false, reconnect_handle, terminal_reason),
        }
    });
    lifecycle = transition.state;
    if let SessionEffect::Recover { epoch } = transition.effect {
        match reconnect_console_transport(&config, reconnect_handle, &mut shutdown).await {
            Ok((control, video, _, packet_limit)) => {
                let transition = lifecycle.step(if *shutdown.borrow() {
                    SessionEvent::Stop
                } else {
                    SessionEvent::Established { epoch }
                });
                lifecycle = transition.state;
                if transition.effect != SessionEffect::Online {
                    session.mark_disconnected();
                    session.mark_transport_closed();
                    return;
                }
                session.bootstrap_complete.store(false, Ordering::SeqCst);
                *session
                    .bootstrap
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = model::Bootstrap::default();
                *session
                    .dvc_mode
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                *session
                    .dvc_switch
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = DvcProfileSwitch::default();
                session.dvc_switch_notify.notify_waiters();
                // No new commands were admitted during recovery. Drain a
                // sender that was already awaiting queue capacity at failure.
                while control_rx.try_recv().is_ok() {}
                session.accepting_commands.store(true, Ordering::Release);
                session.connected.store(true, Ordering::SeqCst);
                session.set_terminal_reason(AvctTerminalReason::None);
                session.display_running.store(false, Ordering::SeqCst);
                session
                    .video_state
                    .send_replace(AvctVideoState::Negotiating);
                *session
                    .last_frame
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                run_session_loop(
                    control,
                    video,
                    session.clone(),
                    &mut shutdown,
                    &mut control_rx,
                    packet_limit,
                    true,
                )
                .await;
            }
            Err(error) => {
                lifecycle = lifecycle
                    .step(if *shutdown.borrow() {
                        SessionEvent::Stop
                    } else {
                        SessionEvent::RecoveryFailed { epoch }
                    })
                    .state;
                tracing::debug!("iDRAC AVCT native reconnect failed: {error}");
            }
        }
    }

    let _ = lifecycle.step(if *shutdown.borrow() {
        SessionEvent::Stop
    } else {
        SessionEvent::TransportLost {
            epoch: lifecycle.epoch(),
            recoverable: false,
        }
    });
    // Only the supervisor publishes the terminal transition. The reader
    // loop records a reason and releases its socket halves, which lets a
    // native reconnect supervisor replace both transports before a feature
    // lifecycle observes a terminal disconnect.
    session.mark_disconnected();
    session.mark_transport_closed();
}

/// `AVSPKVMSession::jb()` retries a failed APCP helper after two five-second
/// sleeps while no control socket exists. Keep that bounded Java-console rule
/// separate from the AVMP reconnect scheduler.
async fn reconnect_console_transport(
    config: &IdracAvctConfig,
    session: u32,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(BoxedAvctIo, BoxedAvctIo, Option<ApcpSetup>, usize), String> {
    use bmc_idrac_model::recovery::{Recovery, RecoveryEffect, RecoveryEvent};
    let origin = tokio::time::Instant::now();
    let mut model = Recovery::new(Duration::ZERO);
    let mut event = RecoveryEvent::Tick {
        now: Duration::ZERO,
    };
    let mut result = None;
    loop {
        let transition = model.step(event);
        model = transition.state;
        if *shutdown.borrow() {
            model = model.step(RecoveryEvent::Stop).state;
            debug_assert_eq!(
                model.state(),
                bmc_idrac_model::recovery::RecoveryState::Cancelled
            );
            return Err("iDRAC AVCT reconnect cancelled".to_string());
        }
        event = match transition.effect {
            RecoveryEffect::Connect { remaining } => {
                result = Some(tokio::select! {
                    biased;
                    _ = shutdown.changed() => {
                        model = model.step(RecoveryEvent::Stop).state;
                        debug_assert_eq!(model.state(), bmc_idrac_model::recovery::RecoveryState::Cancelled);
                        return Err("iDRAC AVCT reconnect cancelled".to_string());
                    }
                    result = tokio::time::timeout(remaining, establish_console_transport(config, ApcpRequestKind::ReconnectWithRetainedSession { session })) => {
                        result.unwrap_or_else(|_| Err("iDRAC AVCT reconnect attempt timed out".to_string()))
                    }
                });
                RecoveryEvent::Finished {
                    now: origin.elapsed(),
                    success: result.as_ref().is_some_and(Result::is_ok),
                }
            }
            RecoveryEffect::Sleep(delay) => {
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => { RecoveryEvent::Stop }
                    _ = tokio::time::sleep(delay) => { RecoveryEvent::Tick { now: origin.elapsed() } }
                }
            }
            RecoveryEffect::Complete => {
                return result.take().expect("recovery completed after an attempt")
            }
            RecoveryEffect::Fail | RecoveryEffect::Cancel | RecoveryEffect::IgnoreTerminal => {
                return Err(result.take().and_then(Result::err).unwrap_or_else(|| {
                    "iDRAC AVCT recovery ended without a transport".to_string()
                }))
            }
        };
    }
}

async fn run_session_loop(
    control: BoxedAvctIo,
    video: BoxedAvctIo,
    session: Arc<AvctSession>,
    shutdown: &mut watch::Receiver<bool>,
    control_rx: &mut mpsc::Receiver<ControlWrite>,
    packet_limit: usize,
    apcp_enabled: bool,
) {
    let (control_read, mut control_write) = tokio::io::split(control);
    let (video_read, mut video_write) = tokio::io::split(video);
    // `read_exact` is not cancellation-safe.  Do not place reads from both
    // sockets directly in this select: when one branch wins, Tokio would
    // discard any partially consumed header/body from the other socket and
    // permanently desynchronise AVCT framing.  The reference client likewise
    // owns independent control/video reader threads.  These pumps preserve
    // that ownership and forward only complete records here.
    let mut inbound = InboundReaders::spawn(control_read, video_read, packet_limit, apcp_enabled);
    let mut decoder = if matches!(session.transport_profile, AvctTransportProfile::Idrac6Apcp) {
        AvctDisplayDecoder::idrac6_legacy()
    } else {
        AvctDisplayDecoder::default()
    };
    let assembly_origin = tokio::time::Instant::now();
    let mut apcp_keepalive_timer = tokio::time::interval_at(
        tokio::time::Instant::now() + Duration::from_secs(10),
        Duration::from_secs(10),
    );
    loop {
        let dvc_startup_deadline = session.dvc_startup_deadline();
        let dvc_deadline = match (dvc_startup_deadline, session.dvc_switch_deadline()) {
            (Some(startup), Some(profile_switch)) => Some(startup.min(profile_switch)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
        let dvc_deadline = match (
            dvc_deadline,
            decoder
                .assembly_deadline()
                .map(|deadline| assembly_origin + deadline),
        ) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(deadline), None) | (None, Some(deadline)) => Some(deadline),
            (None, None) => None,
        };
        let event = tokio::select! {
            _ = wait_for_optional_deadline(dvc_deadline) => Ok(SessionEvent::DvcDeadline),
            _ = apcp_keepalive_timer.tick(), if apcp_enabled => Ok(SessionEvent::ApcpKeepalive),
            changed = shutdown.changed() => changed
                .map(|_| shutdown_event(*shutdown.borrow()))
                .map_err(|_| "iDRAC AVCT shutdown channel closed".to_string()),
            packet = control_rx.recv() => packet
                .map(SessionEvent::OutgoingControl)
                .ok_or_else(|| "iDRAC AVCT control sender closed".to_string()),
            packet = inbound.rx.recv() => packet
                .ok_or_else(|| "iDRAC AVCT inbound reader stopped".to_string())
                .and_then(|packet| packet),
        };
        match event {
            Ok(SessionEvent::DvcDeadline) => {
                let now = tokio::time::Instant::now();
                if decoder.expire_assembly(now.saturating_duration_since(assembly_origin)) {
                    session.set_terminal_reason(AvctTerminalReason::DisplayPacketRejected);
                    return;
                }
                if session.expire_dvc_startup(now) {
                    session.set_terminal_reason(AvctTerminalReason::Idrac6Dvc15NegotiationFailed);
                    tracing::debug!(
                        "iDRAC6 DVC15 startup exceeded its absolute negotiation deadline"
                    );
                    return;
                }
                match session.expired_dvc_switch(now) {
                    Some(DvcSwitchTimeout::Target) => {
                        if let Err(error) = write_dvc15_fallback(
                            &mut control_write,
                            &session,
                            &mut decoder,
                            "requested DVC profile did not produce a complete reset-marked chain",
                        )
                        .await
                        {
                            session.abort_dvc_switch(&error);
                            session.set_terminal_reason(AvctTerminalReason::ControlWrite);
                            tracing::debug!("iDRAC6 DVC15 fallback write failed: {error}");
                            return;
                        }
                    }
                    Some(DvcSwitchTimeout::Fallback) => {
                        session.abort_dvc_switch(
                            "iDRAC6 DVC15 fallback did not produce a complete reset-marked chain",
                        );
                        session.set_terminal_reason(
                            AvctTerminalReason::Idrac6DvcProfileRecoveryFailed,
                        );
                        tracing::debug!("iDRAC6 DVC15 profile recovery deadline elapsed");
                        return;
                    }
                    None => {}
                }
            }
            Ok(SessionEvent::Shutdown(false)) => {}
            Ok(SessionEvent::Shutdown(true)) => {
                if apcp_enabled {
                    let close = graceful_session_close_record();
                    if try_write_graceful_close(&mut control_write, &close).await {
                        session.packets_out.fetch_add(1, Ordering::Relaxed);
                    }
                    if try_write_graceful_close(&mut video_write, &close).await {
                        session.packets_out.fetch_add(1, Ordering::Relaxed);
                    }
                }
                // The reference client gives the appliance's two writer
                // threads 50ms to flush the APCP close record before it
                // releases either TLS socket.
                tokio::time::sleep(Duration::from_millis(50)).await;
                session.set_terminal_reason(AvctTerminalReason::Shutdown);
                return;
            }
            Ok(SessionEvent::ApcpKeepalive) => {
                if let Err(error) =
                    write_session_packet(&mut control_write, &apcp_keepalive(), "control keepalive")
                        .await
                {
                    session.set_terminal_reason(AvctTerminalReason::ControlWrite);
                    tracing::debug!("iDRAC AVCT APCP keepalive failed: {error}");
                    return;
                }
                session.packets_out.fetch_add(1, Ordering::Relaxed);
                if let Err(error) =
                    write_session_packet(&mut video_write, &apcp_keepalive(), "video keepalive")
                        .await
                {
                    session.set_terminal_reason(AvctTerminalReason::DisplayAckWrite);
                    tracing::debug!("iDRAC AVCT video APCP keepalive failed: {error}");
                    return;
                }
                session.packets_out.fetch_add(1, Ordering::Relaxed);
            }
            Ok(SessionEvent::Control(packet)) => {
                session.packets_in.fetch_add(1, Ordering::Relaxed);
                let apcp_kind = if packet.magic == *b"APCP" {
                    Some(
                        packet
                            .payload
                            .get(..2)
                            .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
                            .unwrap_or(u16::MAX),
                    )
                } else {
                    None
                };
                match model::control_action(
                    packet.message_type,
                    packet.payload.len(),
                    apcp_kind,
                    apcp_enabled,
                ) {
                    model::ControlAction::Reject => {
                        session.set_terminal_reason(AvctTerminalReason::UnsupportedControlPacket);
                        tracing::debug!(
                            message_type = packet.message_type,
                            "iDRAC AVCT unsupported or malformed control record"
                        );
                        return;
                    }
                    model::ControlAction::RemoteClose => {
                        session.set_terminal_reason(AvctTerminalReason::Shutdown);
                        return;
                    }
                    model::ControlAction::Observe
                    | model::ControlAction::ColorMode
                    | model::ControlAction::ReconnectHandle => {
                        observe_control_packet(&session, &packet)
                    }
                }
                if packet.message_type == super::dvc::DVC_COLOR_MODE_RESPONSE_TYPE {
                    match parse_color_mode_response(&packet) {
                        Ok(mode) => session.record_dvc_mode(mode),
                        Err(error) => {
                            session.set_terminal_reason(AvctTerminalReason::DisplayPacketRejected);
                            tracing::warn!("iDRAC AVCT DVC response rejected: {error}");
                            return;
                        }
                    }
                }
            }
            Ok(SessionEvent::OutgoingControl(write)) => {
                let epoch = match &write {
                    ControlWrite::Single(epoch, _) | ControlWrite::Pair(epoch, _, _) => *epoch,
                };
                if !model::command_is_current(session.control_epoch.load(Ordering::Acquire), epoch)
                {
                    continue;
                }
                let result = match write {
                    ControlWrite::Single(_, packet) => {
                        write_queued_control(&mut control_write, &session, &mut decoder, &packet)
                            .await
                    }
                    ControlWrite::Pair(_, first, second) => {
                        if let Err(error) =
                            write_queued_control(&mut control_write, &session, &mut decoder, &first)
                                .await
                        {
                            Err(error)
                        } else {
                            write_queued_control(
                                &mut control_write,
                                &session,
                                &mut decoder,
                                &second,
                            )
                            .await
                        }
                    }
                };
                if let Err(error) = result {
                    session.set_terminal_reason(AvctTerminalReason::ControlWrite);
                    tracing::debug!("iDRAC AVCT control write failed: {error}");
                    return;
                }
            }
            Ok(SessionEvent::Video(packet)) => {
                session.packets_in.fetch_add(1, Ordering::Relaxed);
                if is_video_stopped_packet_type(packet.message_type) {
                    // The official response handler publishes `video_stopped`,
                    // resets display state, and participates in normal video
                    // acknowledgement batching for this record.
                    session.display_running.store(false, Ordering::SeqCst);
                    {
                        let mut state = session
                            .bootstrap
                            .lock()
                            .unwrap_or_else(|poisoned| poisoned.into_inner());
                        *state = state.step(model::BootstrapEvent::NoSignal).state;
                    }
                    session.bootstrap_complete.store(false, Ordering::SeqCst);
                    session.suspend_dvc_for_no_signal();
                    session.abort_dvc_switch(
                        "the iDRAC video signal stopped during the color-depth transition",
                    );
                    decoder =
                        if matches!(session.transport_profile, AvctTransportProfile::Idrac6Apcp) {
                            AvctDisplayDecoder::idrac6_legacy()
                        } else {
                            AvctDisplayDecoder::default()
                        };
                    session.video_state.send_replace(AvctVideoState::Stopped);
                    *session
                        .last_frame
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
                    if let Err(error) = write_session_packet(
                        &mut video_write,
                        &display_ack(),
                        "video-stopped acknowledgement",
                    )
                    .await
                    {
                        session.set_terminal_reason(AvctTerminalReason::DisplayAckWrite);
                        tracing::debug!("iDRAC AVCT video-stopped acknowledgement failed: {error}");
                        return;
                    }
                    session.packets_out.fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                if is_video_control_packet(packet.message_type) {
                    // iDRAC6 sends its no-payload `rb` Scaling record
                    // (`0x8203`) on the display reader before any frame. The
                    // official receiver constructs it and performs no reply
                    // or state mutation. Keep all known display-control
                    // records out of the frame decoder for the same reason.
                    observe_control_packet(&session, &packet);
                    continue;
                }
                if is_display_bootstrap_trigger(&packet) {
                    session.resume_dvc_for_video_ready(tokio::time::Instant::now());
                    let result = match session.dvc_startup_deadline() {
                        Some(deadline) => match tokio::time::timeout_at(
                            deadline,
                            send_display_bootstrap(&mut control_write, &session),
                        )
                        .await
                        {
                            Ok(result) => result,
                            Err(_) => {
                                session.fail_dvc_startup();
                                session.set_terminal_reason(
                                    AvctTerminalReason::Idrac6Dvc15NegotiationFailed,
                                );
                                tracing::debug!(
                                    "iDRAC6 DVC15 startup deadline elapsed during display bootstrap"
                                );
                                return;
                            }
                        },
                        None => send_display_bootstrap(&mut control_write, &session).await,
                    };
                    if let Err(error) = result {
                        session.set_terminal_reason(AvctTerminalReason::DisplayBootstrapWrite);
                        tracing::debug!("iDRAC AVCT display bootstrap failed: {error}");
                        return;
                    }
                    continue;
                }
                match packet.message_type {
                    message_type if is_block_video_packet_type(message_type) => {
                        let block = match BlockVideoPacket::parse(&packet) {
                            Ok(block) => block,
                            Err(error) => {
                                session
                                    .set_terminal_reason(AvctTerminalReason::DisplayPacketRejected);
                                tracing::debug!("iDRAC AVCT block-video packet rejected: {error}");
                                return;
                            }
                        };
                        session.display_packets.fetch_add(1, Ordering::Relaxed);
                        session
                            .last_display_message_type
                            .store(u32::from(packet.message_type), Ordering::Relaxed);
                        session
                            .last_display_codec
                            .store(u32::from(block.selector), Ordering::Relaxed);
                        session
                            .last_display_flags
                            .store(u32::from(block.flags), Ordering::Relaxed);
                        session
                            .video_width
                            .store(u32::from(block.first_dimension), Ordering::Relaxed);
                        session
                            .video_height
                            .store(u32::from(block.second_dimension), Ordering::Relaxed);
                        match session.observe_dvc_display(packet.message_type, block.flags) {
                            DvcDisplayAction::Passthrough => {
                                match decoder
                                    .decode_block_video_at(&block, assembly_origin.elapsed())
                                {
                                    Ok(Some(frame)) => publish_decoded_frame(&session, frame),
                                    Ok(None) => {
                                        session
                                            .unchanged_display_packets
                                            .fetch_add(1, Ordering::Relaxed);
                                    }
                                    Err(error) => {
                                        session
                                            .display_decode_errors
                                            .fetch_add(1, Ordering::Relaxed);
                                        session.set_terminal_reason(
                                            AvctTerminalReason::DisplayPacketRejected,
                                        );
                                        tracing::debug!(
                                            "iDRAC AVCT block-video decode failed: {error}"
                                        );
                                        return;
                                    }
                                }
                            }
                            DvcDisplayAction::Suppress => {
                                session
                                    .suppressed_startup_display_packets
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            DvcDisplayAction::SuppressAndRequestRefresh
                            | DvcDisplayAction::DecodeRefreshed { .. }
                            | DvcDisplayAction::RejectUnexpectedFamily => {
                                session.fail_dvc_startup();
                                session.set_terminal_reason(
                                    AvctTerminalReason::Idrac6Dvc15NegotiationFailed,
                                );
                                tracing::debug!(
                                    "iDRAC6 DVC15 startup received block-video after its DVC15 chain began"
                                );
                                return;
                            }
                        }
                        // The official receiver acknowledges every block-video
                        // record, including retained continuation fragments.
                        if let Err(error) = write_session_packet(
                            &mut video_write,
                            &display_ack(),
                            "block-video acknowledgement",
                        )
                        .await
                        {
                            session.set_terminal_reason(AvctTerminalReason::DisplayAckWrite);
                            tracing::debug!(
                                "iDRAC AVCT block-video acknowledgement failed: {error}"
                            );
                            return;
                        }
                        session.packets_out.fetch_add(1, Ordering::Relaxed);
                    }
                    message_type
                        if is_display_packet_type(message_type)
                            && (apcp_enabled || !matches!(message_type, 0x008a | 0x860a)) =>
                    {
                        let display = match DisplayPacket::parse(&packet) {
                            Ok(display) => display,
                            Err(error) => {
                                session
                                    .set_terminal_reason(AvctTerminalReason::DisplayPacketRejected);
                                tracing::debug!("iDRAC AVCT display packet rejected: {error}");
                                return;
                            }
                        };
                        session.display_packets.fetch_add(1, Ordering::Relaxed);
                        session
                            .last_display_message_type
                            .store(u32::from(packet.message_type), Ordering::Relaxed);
                        session
                            .last_display_codec
                            .store(u32::from(display.codec), Ordering::Relaxed);
                        session
                            .last_display_flags
                            .store(u32::from(display.flags), Ordering::Relaxed);
                        session
                            .video_width
                            .store(display.width as u32, Ordering::Relaxed);
                        session
                            .video_height
                            .store(display.height as u32, Ordering::Relaxed);
                        let startup_action =
                            session.observe_dvc_display(packet.message_type, display.flags);
                        let mut requested_profile_failure = None;
                        match startup_action {
                            DvcDisplayAction::Suppress => {
                                session
                                    .suppressed_startup_display_packets
                                    .fetch_add(1, Ordering::Relaxed);
                            }
                            DvcDisplayAction::SuppressAndRequestRefresh => {
                                session
                                    .suppressed_startup_display_packets
                                    .fetch_add(1, Ordering::Relaxed);
                                let refresh_result = match session.dvc_startup_deadline() {
                                    Some(deadline) => match tokio::time::timeout_at(
                                        deadline,
                                        write_session_packet(
                                            &mut control_write,
                                            &screen_refresh(),
                                            "initial iDRAC6 screen refresh",
                                        ),
                                    )
                                    .await
                                    {
                                        Ok(result) => result,
                                        Err(_) => {
                                            session.fail_dvc_startup();
                                            session.set_terminal_reason(
                                                AvctTerminalReason::Idrac6Dvc15NegotiationFailed,
                                            );
                                            tracing::debug!(
                                                "iDRAC6 DVC15 startup deadline elapsed while requesting the refreshed chain"
                                            );
                                            return;
                                        }
                                    },
                                    None => Err("iDRAC6 DVC15 refresh lost its startup deadline"
                                        .to_string()),
                                };
                                if let Err(error) = refresh_result {
                                    session.set_terminal_reason(AvctTerminalReason::ControlWrite);
                                    tracing::debug!(
                                        "iDRAC AVCT initial iDRAC6 screen refresh failed: {error}"
                                    );
                                    return;
                                }
                                session.packets_out.fetch_add(1, Ordering::Relaxed);
                            }
                            DvcDisplayAction::DecodeRefreshed {
                                command_chain_boundary,
                            } => {
                                if let Err(error) =
                                    decoder.decode_at(&display, assembly_origin.elapsed())
                                {
                                    session
                                        .display_decode_errors
                                        .fetch_add(1, Ordering::Relaxed);
                                    session.fail_dvc_startup();
                                    session.set_terminal_reason(
                                        AvctTerminalReason::DisplayPacketRejected,
                                    );
                                    tracing::debug!(
                                        "iDRAC6 refreshed DVC15 chain could not be decoded: {error}"
                                    );
                                    return;
                                }
                                if command_chain_boundary {
                                    if !session.finish_refreshed_dvc_chain() {
                                        session.fail_dvc_startup();
                                        session.set_terminal_reason(
                                            AvctTerminalReason::Idrac6Dvc15NegotiationFailed,
                                        );
                                        tracing::debug!(
                                            "iDRAC6 refreshed DVC15 chain ended outside its startup phase"
                                        );
                                        return;
                                    }
                                    let Some(frame) = decoder.snapshot_frame() else {
                                        session.fail_dvc_startup();
                                        session.set_terminal_reason(
                                            AvctTerminalReason::Idrac6Dvc15NegotiationFailed,
                                        );
                                        tracing::debug!(
                                            "iDRAC6 refreshed DVC15 chain ended without a framebuffer"
                                        );
                                        return;
                                    };
                                    publish_decoded_frame(&session, frame);
                                } else {
                                    session
                                        .suppressed_startup_display_packets
                                        .fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            DvcDisplayAction::Passthrough => {
                                session.display_running.store(true, Ordering::SeqCst);
                                // The iDRAC8 client sends this transition request after
                                // the first DVC packet. iDRAC6 uses the typed startup
                                // state above and never receives this iDRAC8 default.
                                if matches!(
                                    session.transport_profile,
                                    AvctTransportProfile::Idrac8Apcp
                                ) && session.request_default_dvc()
                                {
                                    if let Err(error) = write_session_packet(
                                        &mut control_write,
                                        &default_color_depth_request(),
                                        "color-depth request",
                                    )
                                    .await
                                    {
                                        session
                                            .set_terminal_reason(AvctTerminalReason::ControlWrite);
                                        tracing::debug!(
                                            "iDRAC AVCT automatic DVC request failed: {error}"
                                        );
                                        return;
                                    }
                                    session.packets_out.fetch_add(1, Ordering::Relaxed);
                                }
                                match session.observe_dvc_switch(packet.message_type, display.flags)
                                {
                                    DvcSwitchAction::Suppress => {
                                        session
                                            .suppressed_startup_display_packets
                                            .fetch_add(1, Ordering::Relaxed);
                                    }
                                    DvcSwitchAction::RequestTargetRefresh => {
                                        session
                                            .suppressed_startup_display_packets
                                            .fetch_add(1, Ordering::Relaxed);
                                        decoder.discard_pending_commands();
                                        if let Err(error) = write_session_packet(
                                            &mut control_write,
                                            &screen_refresh(),
                                            "target DVC transition refresh",
                                        )
                                        .await
                                        {
                                            session.abort_dvc_switch(&format!(
                                                "target DVC transition refresh failed: {error}"
                                            ));
                                            session.set_terminal_reason(
                                                AvctTerminalReason::ControlWrite,
                                            );
                                            return;
                                        }
                                        session.packets_out.fetch_add(1, Ordering::Relaxed);
                                    }
                                    DvcSwitchAction::DecodeTarget {
                                        command_chain_boundary,
                                    } => match decoder
                                        .decode_at(&display, assembly_origin.elapsed())
                                    {
                                        Ok(_) if command_chain_boundary => {
                                            if let Some(frame) = decoder.snapshot_frame() {
                                                if session.finish_dvc_target_chain() {
                                                    publish_decoded_frame(&session, frame);
                                                } else {
                                                    requested_profile_failure = Some(
                                                        "target DVC chain ended outside its transition phase"
                                                            .to_string(),
                                                    );
                                                }
                                            } else {
                                                requested_profile_failure = Some(
                                                    "target DVC chain ended without a framebuffer"
                                                        .to_string(),
                                                );
                                            }
                                        }
                                        Ok(_) => {
                                            session
                                                .suppressed_startup_display_packets
                                                .fetch_add(1, Ordering::Relaxed);
                                        }
                                        Err(error) => {
                                            session
                                                .display_decode_errors
                                                .fetch_add(1, Ordering::Relaxed);
                                            requested_profile_failure = Some(format!(
                                                "target DVC chain could not be decoded: {error}"
                                            ));
                                        }
                                    },
                                    DvcSwitchAction::DecodeFallback {
                                        command_chain_boundary,
                                    } => match decoder
                                        .decode_at(&display, assembly_origin.elapsed())
                                    {
                                        Ok(_) if command_chain_boundary => {
                                            let Some(frame) = decoder.snapshot_frame() else {
                                                session.abort_dvc_switch(
                                                    "DVC15 fallback ended without a framebuffer",
                                                );
                                                session.set_terminal_reason(
                                                    AvctTerminalReason::Idrac6DvcProfileRecoveryFailed,
                                                );
                                                return;
                                            };
                                            if !session.finish_dvc_fallback_chain() {
                                                session.abort_dvc_switch(
                                                    "DVC15 fallback ended outside its transition phase",
                                                );
                                                session.set_terminal_reason(
                                                    AvctTerminalReason::Idrac6DvcProfileRecoveryFailed,
                                                );
                                                return;
                                            }
                                            publish_decoded_frame(&session, frame);
                                        }
                                        Ok(_) => {
                                            session
                                                .suppressed_startup_display_packets
                                                .fetch_add(1, Ordering::Relaxed);
                                        }
                                        Err(error) => {
                                            session
                                                .display_decode_errors
                                                .fetch_add(1, Ordering::Relaxed);
                                            session.abort_dvc_switch(&format!(
                                                "DVC15 fallback could not be decoded: {error}"
                                            ));
                                            session.set_terminal_reason(
                                                AvctTerminalReason::Idrac6DvcProfileRecoveryFailed,
                                            );
                                            return;
                                        }
                                    },
                                    DvcSwitchAction::RequestFallbackRefresh => {
                                        session
                                            .suppressed_startup_display_packets
                                            .fetch_add(1, Ordering::Relaxed);
                                        decoder.discard_pending_commands();
                                        if let Err(error) = write_session_packet(
                                            &mut control_write,
                                            &screen_refresh(),
                                            "fallback DVC transition refresh",
                                        )
                                        .await
                                        {
                                            session.abort_dvc_switch(&format!(
                                                "fallback DVC transition refresh failed: {error}"
                                            ));
                                            session.set_terminal_reason(
                                                AvctTerminalReason::ControlWrite,
                                            );
                                            return;
                                        }
                                        session.packets_out.fetch_add(1, Ordering::Relaxed);
                                    }
                                    DvcSwitchAction::RejectTarget => {
                                        requested_profile_failure = Some(
                                            "target DVC family changed inside its command chain"
                                                .to_string(),
                                        );
                                    }
                                    DvcSwitchAction::RejectFallback => {
                                        session.abort_dvc_switch(
                                            "DVC15 fallback changed family inside its command chain",
                                        );
                                        session.set_terminal_reason(
                                            AvctTerminalReason::Idrac6DvcProfileRecoveryFailed,
                                        );
                                        return;
                                    }
                                    DvcSwitchAction::Passthrough => {
                                        match decoder.decode_at(&display, assembly_origin.elapsed())
                                        {
                                            Ok(Some(frame)) => {
                                                publish_decoded_frame(&session, frame)
                                            }
                                            Ok(None) => {
                                                session
                                                    .unchanged_display_packets
                                                    .fetch_add(1, Ordering::Relaxed);
                                            }
                                            Err(error) => {
                                                session
                                                    .display_decode_errors
                                                    .fetch_add(1, Ordering::Relaxed);
                                                if matches!(
                                                    session.transport_profile,
                                                    AvctTransportProfile::Idrac6Apcp
                                                ) {
                                                    requested_profile_failure = Some(format!(
                                                    "active DVC stream could not be decoded: {error}"
                                                ));
                                                } else {
                                                    // iDRAC8 can recover from an individual bad
                                                    // retained tile after it has ACKed that packet.
                                                    tracing::warn!(
                                                    "iDRAC AVCT display tile could not be decoded: {error}"
                                                );
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            DvcDisplayAction::RejectUnexpectedFamily => {
                                session.set_terminal_reason(
                                    AvctTerminalReason::Idrac6Dvc15NegotiationFailed,
                                );
                                tracing::debug!(
                                    message_type = format_args!("0x{:04x}", packet.message_type),
                                    "iDRAC6 DVC15 stream changed family during startup or after readiness"
                                );
                                return;
                            }
                        }
                        if let Some(error) = requested_profile_failure {
                            // Acknowledge the rejected target record before asking the
                            // independent control socket to restore DVC15. This mirrors
                            // the official receiver's per-record acknowledgement order.
                            if let Err(ack_error) = write_session_packet(
                                &mut video_write,
                                &display_ack(),
                                "display acknowledgement before DVC15 fallback",
                            )
                            .await
                            {
                                session.abort_dvc_switch(&ack_error);
                                session.set_terminal_reason(AvctTerminalReason::DisplayAckWrite);
                                return;
                            }
                            session.packets_out.fetch_add(1, Ordering::Relaxed);
                            if let Err(fallback_error) = write_dvc15_fallback(
                                &mut control_write,
                                &session,
                                &mut decoder,
                                &error,
                            )
                            .await
                            {
                                session.abort_dvc_switch(&fallback_error);
                                session.set_terminal_reason(AvctTerminalReason::ControlWrite);
                                return;
                            }
                            continue;
                        }
                        if let Err(error) = write_session_packet(
                            &mut video_write,
                            &display_ack(),
                            "display acknowledgement",
                        )
                        .await
                        {
                            session.set_terminal_reason(AvctTerminalReason::DisplayAckWrite);
                            tracing::debug!("iDRAC AVCT display acknowledgement failed: {error}");
                            return;
                        }
                        session.packets_out.fetch_add(1, Ordering::Relaxed);
                    }
                    unsupported => {
                        // The official receive factory distinguishes several
                        // stateful video families (including 0x0086). A
                        // backend without that complete decoder must fail the
                        // feature deterministically instead of waiting for a
                        // frame that can never arrive.
                        session.set_terminal_reason(AvctTerminalReason::UnsupportedVideoPacket);
                        tracing::debug!("iDRAC AVCT unsupported video packet 0x{unsupported:04x}");
                        return;
                    }
                }
            }
            Err(error) => {
                session.set_terminal_reason(AvctTerminalReason::SocketOrControlQueueClosed);
                tracing::debug!("iDRAC AVCT session ended: {error}");
                return;
            }
        }
    }
}

fn publish_decoded_frame(session: &AvctSession, frame: DecodedFrame) {
    session.display_running.store(true, Ordering::SeqCst);
    session.video_state.send_replace(AvctVideoState::Running);
    session.decoded_frames.fetch_add(1, Ordering::Relaxed);
    *session
        .last_frame
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(frame.clone());
    let _ = session.frame_tx.send(frame);
}

async fn wait_for_optional_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending::<()>().await,
    }
}

fn is_video_control_packet(message_type: u16) -> bool {
    matches!(message_type, 0x8200..=0x8204 | 0x8206)
}

fn is_video_stopped_packet_type(message_type: u16) -> bool {
    matches!(message_type, 0x0085 | 0x8605)
}

/// The iDRAC6 `kvm.b.a.a` factory rejects AVCT records below 16 bytes before
/// dispatch and does not contain DVC23 (`0x008a/0x860a`) cases.  Keep those
/// bounds at the receive boundary so a modern-only record cannot accidentally
/// reach the legacy decoder just because both generations use BEEF framing.
fn validate_inbound_packet(packet: AvctPacket, apcp_enabled: bool) -> Result<AvctPacket, String> {
    if !apcp_enabled && packet.total_len() < 16 {
        return Err(format!(
            "iDRAC6 AVCT record length {} is below the official 16-byte minimum",
            packet.total_len()
        ));
    }
    Ok(packet)
}

/// Record the control-plane information that Dell's dispatcher consumes.
/// These packet types do not require an unconditional response in the official
/// iDRAC8 trace, so parsing them here keeps state complete without inventing a
/// reply for an appliance-specific optional mode.
fn observe_control_packet(session: &AvctSession, packet: &AvctPacket) {
    session
        .last_control_message_type
        .store(u32::from(packet.message_type), Ordering::Relaxed);
    if !packet.is_beef() && packet.magic != *b"APCP" {
        session
            .generic_control_messages
            .fetch_add(1, Ordering::Relaxed);
    }
    match packet.message_type {
        // `com.avocent.d.g.a.a` reads the reconnect scalar at packet offset
        // four and passes it to `AVSPKVMSession::j(int)`.  It is the value
        // later supplied to the official APCP `SessionRequestEx` writer.
        // Do not treat any APCP setup-tail field as interchangeable.
        0x8102 => {
            if packet.payload.len() >= 8 {
                let reconnect_handle = u32::from_be_bytes(
                    packet.payload[4..8]
                        .try_into()
                        .expect("bounded reconnect scalar"),
                );
                session
                    .reconnect_session
                    .store(reconnect_handle, Ordering::Relaxed);
            } else {
                session.reconnect_session.store(0, Ordering::Relaxed);
                tracing::warn!(
                    "iDRAC AVCT reconnect record 0x8102 is truncated ({} bytes)",
                    packet.payload.len()
                );
            }
        }
        // `com.avocent.c.d.qb`: negotiated display resolution.
        0x8201 if packet.payload.len() >= 4 => {
            session.negotiated_width.store(
                u32::from(u16::from_be_bytes([packet.payload[0], packet.payload[1]])),
                Ordering::Relaxed,
            );
            session.negotiated_height.store(
                u32::from(u16::from_be_bytes([packet.payload[2], packet.payload[3]])),
                Ordering::Relaxed,
            );
        }
        // `com.avocent.c.d.tb`: resolution plus the server display family.
        0x8206 if packet.payload.len() >= 6 => {
            session.negotiated_width.store(
                u32::from(u16::from_be_bytes([packet.payload[0], packet.payload[1]])),
                Ordering::Relaxed,
            );
            session.negotiated_height.store(
                u32::from(u16::from_be_bytes([packet.payload[2], packet.payload[3]])),
                Ordering::Relaxed,
            );
            session.negotiated_display_format.store(
                u32::from(u16::from_be_bytes([packet.payload[4], packet.payload[5]])),
                Ordering::Relaxed,
            );
        }
        _ => {}
    }
}

fn shutdown_event(requested: bool) -> SessionEvent {
    SessionEvent::Shutdown(requested)
}

struct InboundReaders {
    rx: mpsc::Receiver<Result<SessionEvent, String>>,
    control: tokio::task::JoinHandle<()>,
    video: tokio::task::JoinHandle<()>,
}

impl InboundReaders {
    fn spawn(
        control: tokio::io::ReadHalf<BoxedAvctIo>,
        video: tokio::io::ReadHalf<BoxedAvctIo>,
        packet_limit: usize,
        apcp_enabled: bool,
    ) -> Self {
        let (tx, rx) = mpsc::channel(32);
        let control_tx = tx.clone();
        let control = tokio::spawn(pump_inbound_packets(
            control,
            control_tx,
            packet_limit,
            apcp_enabled,
            true,
        ));
        let video = tokio::spawn(pump_inbound_packets(
            video,
            tx,
            packet_limit,
            apcp_enabled,
            false,
        ));
        Self { rx, control, video }
    }
}

impl Drop for InboundReaders {
    fn drop(&mut self) {
        self.control.abort();
        self.video.abort();
    }
}

async fn read_bounded_inbound_packet<R: AsyncRead + Unpin>(
    read: &mut R,
    packet_limit: usize,
) -> Result<AvctPacket, String> {
    use tokio::io::AsyncReadExt;
    let mut first = [0u8; 1];
    read.read_exact(&mut first)
        .await
        .map_err(|error| format!("AVCT record start failed: {error}"))?;
    let prefix = std::io::Cursor::new(first);
    let mut record = prefix.chain(read);
    tokio::time::timeout(
        Duration::from_secs(10),
        read_packet(&mut record, packet_limit),
    )
    .await
    .map_err(|_| "AVCT partial record exceeded its absolute deadline".to_string())?
}

async fn pump_inbound_packets(
    mut read: tokio::io::ReadHalf<BoxedAvctIo>,
    tx: mpsc::Sender<Result<SessionEvent, String>>,
    packet_limit: usize,
    apcp_enabled: bool,
    control: bool,
) {
    loop {
        let packet = read_bounded_inbound_packet(&mut read, packet_limit)
            .await
            .and_then(|packet| validate_inbound_packet(packet, apcp_enabled))
            .map(|packet| {
                if control || packet.magic == *b"APCP" {
                    SessionEvent::Control(packet)
                } else {
                    SessionEvent::Video(packet)
                }
            })
            .map_err(|error| {
                format!(
                    "{} socket read failed: {error}",
                    if control { "control" } else { "video" }
                )
            });
        let terminal = packet.is_err();
        if tx.send(packet).await.is_err() || terminal {
            return;
        }
    }
}

enum SessionEvent {
    ApcpKeepalive,
    DvcDeadline,
    Shutdown(bool),
    Control(AvctPacket),
    OutgoingControl(ControlWrite),
    Video(AvctPacket),
}

fn clamp_coordinate(value: i16, dimension: u32) -> u16 {
    let value = value.max(0) as u32;
    let maximum = if dimension == 0 {
        u16::MAX as u32
    } else {
        dimension.saturating_sub(1)
    };
    value.min(maximum).min(u16::MAX as u32) as u16
}

pub struct AvctLease {
    key: PoolKey,
    session: Arc<AvctSession>,
}
impl AvctLease {
    pub fn session(&self) -> &Arc<AvctSession> {
        &self.session
    }
}

impl Drop for AvctLease {
    fn drop(&mut self) {
        let should_shutdown = {
            let mut entries = pool();
            if self.session.leases.fetch_sub(1, Ordering::SeqCst) != 1 {
                return;
            }
            match entries.get(&self.key) {
                Some(PoolSlot::Ready(current)) if Arc::ptr_eq(current, &self.session) => {
                    use bmc_idrac_model::pool::{Pool, PoolEffect, PoolEvent, PoolState};
                    let transition =
                        Pool::new(PoolState::Online).step(PoolEvent::LastLeaseReleased);
                    debug_assert_eq!(transition.effect, PoolEffect::Close);
                    entries.insert(self.key.clone(), PoolSlot::Closing(self.session.clone()));
                    true
                }
                _ => false,
            }
        };
        if should_shutdown {
            self.session.shutdown();
        }
    }
}

/// A cancelled creator cannot leave a permanent Connecting slot. Sticky watch
/// completion also closes the notify-before-wait race for concurrent callers.
struct ConnectingGuard {
    key: PoolKey,
    completion: Arc<watch::Sender<bool>>,
}
impl Drop for ConnectingGuard {
    fn drop(&mut self) {
        use bmc_idrac_model::pool::{Pool, PoolEffect, PoolEvent, PoolState};
        let mut entries = pool();
        if matches!(entries.get(&self.key), Some(PoolSlot::Connecting(current)) if Arc::ptr_eq(current, &self.completion))
        {
            let effect = Pool::new(PoolState::Connecting)
                .step(PoolEvent::ConnectFailed)
                .effect;
            if effect == PoolEffect::Remove {
                entries.remove(&self.key);
            }
        }
        self.completion.send_replace(true);
    }
}

pub async fn get_or_connect(config: &IdracAvctConfig) -> Result<AvctLease, String> {
    use bmc_idrac_model::pool::{Pool, PoolEffect, PoolEvent, PoolState};
    let key = PoolKey::from_config(config);
    loop {
        enum Next {
            Ready(Arc<AvctSession>),
            Wait(watch::Receiver<bool>),
            Connect {
                completion: Arc<watch::Sender<bool>>,
                grace: bool,
            },
        }
        let next = {
            let mut entries = pool();
            let state = match entries.get(&key) {
                None => PoolState::Vacant,
                Some(PoolSlot::Connecting(_)) => PoolState::Connecting,
                Some(PoolSlot::Ready(session)) | Some(PoolSlot::Closing(session))
                    if *session.transport_closed_rx().borrow() =>
                {
                    PoolState::TransportClosed
                }
                Some(PoolSlot::Ready(session)) | Some(PoolSlot::Closing(session))
                    if !session.is_connected() =>
                {
                    PoolState::Disconnected
                }
                Some(PoolSlot::Ready(_)) => PoolState::Online,
                Some(PoolSlot::Closing(_)) => PoolState::Closing,
            };
            match Pool::new(state).step(PoolEvent::Acquire).effect {
                PoolEffect::Connect { grace } => {
                    let (completion, _) = watch::channel(false);
                    let completion = Arc::new(completion);
                    entries.insert(key.clone(), PoolSlot::Connecting(completion.clone()));
                    Next::Connect { completion, grace }
                }
                PoolEffect::Lease => {
                    let Some(PoolSlot::Ready(session)) = entries.get(&key) else {
                        unreachable!("online pool slot owns a session")
                    };
                    // Increment while holding the same lock as final-lease close.
                    session.leases.fetch_add(1, Ordering::SeqCst);
                    Next::Ready(session.clone())
                }
                PoolEffect::WaitForConnect => {
                    let Some(PoolSlot::Connecting(completion)) = entries.get(&key) else {
                        unreachable!("connecting slot owns completion")
                    };
                    Next::Wait(completion.subscribe())
                }
                PoolEffect::WaitForClose => {
                    let Some(PoolSlot::Closing(session)) = entries.get(&key) else {
                        unreachable!("closing slot owns a session")
                    };
                    Next::Wait(session.transport_closed_rx())
                }
                PoolEffect::Close | PoolEffect::Remove | PoolEffect::Ignore => {
                    unreachable!("acquire has a typed acquisition effect")
                }
            }
        };
        match next {
            Next::Ready(session) => return Ok(AvctLease { key, session }),
            Next::Wait(mut completion) => {
                while !*completion.borrow() {
                    if completion.changed().await.is_err() {
                        break;
                    }
                }
            }
            Next::Connect { completion, grace } => {
                let _guard = ConnectingGuard {
                    key: key.clone(),
                    completion,
                };
                if grace {
                    tokio::time::sleep(GRACEFUL_CLOSE_RELEASE_DELAY).await;
                }
                let session = AvctSession::connect(config).await?;
                let transition = Pool::new(PoolState::Connecting).step(PoolEvent::ConnectSucceeded);
                debug_assert_eq!(transition.effect, PoolEffect::Lease);
                {
                    let mut entries = pool();
                    session.leases.fetch_add(1, Ordering::SeqCst);
                    entries.insert(key.clone(), PoolSlot::Ready(session.clone()));
                }
                return Ok(AvctLease { key, session });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{LazyLock, Mutex as StdMutex};

    use super::*;

    static POOL_TEST_LOCK: LazyLock<StdMutex<()>> = LazyLock::new(|| StdMutex::new(()));

    fn test_options(host: &str, generation: u8) -> IdracAvctOptions {
        IdracAvctOptions {
            host: host.into(),
            port: 5900,
            username: "admin".into(),
            password: "not-a-real-secret".into(),
            generation,
            virtual_media_privilege: false,
            apcp_selector: 5,
            reconnect_capability: false,
            reconnect_version: -1,
            reconnect_preference: 0,
        }
    }

    fn test_session(leases: usize) -> Arc<AvctSession> {
        test_session_for_profile(leases, AvctTransportProfile::Idrac8Apcp)
    }

    fn test_session_for_profile(
        leases: usize,
        transport_profile: AvctTransportProfile,
    ) -> Arc<AvctSession> {
        test_session_for_profile_with_control(leases, transport_profile).0
    }

    fn test_session_for_profile_with_control(
        leases: usize,
        transport_profile: AvctTransportProfile,
    ) -> (Arc<AvctSession>, mpsc::Receiver<ControlWrite>) {
        let (shutdown, _) = watch::channel(false);
        let (disconnected, _) = watch::channel(false);
        let (control_tx, control_rx) = mpsc::channel(8);
        let (frame_tx, _) = tokio::sync::broadcast::channel(1);
        let (video_state, _) = watch::channel(AvctVideoState::Negotiating);
        let session = Arc::new(AvctSession {
            shutdown,
            disconnected,
            transport_closed: watch::channel(false).0,
            connected: AtomicBool::new(true),
            accepting_commands: AtomicBool::new(true),
            control_epoch: AtomicU64::new(0),
            leases: AtomicUsize::new(leases),
            packets_in: AtomicU64::new(0),
            packets_out: AtomicU64::new(0),
            display_packets: AtomicU64::new(0),
            decoded_frames: AtomicU64::new(0),
            unchanged_display_packets: AtomicU64::new(0),
            suppressed_startup_display_packets: AtomicU64::new(0),
            display_decode_errors: AtomicU64::new(0),
            last_display_message_type: AtomicU32::new(0),
            last_display_codec: AtomicU32::new(0),
            last_display_flags: AtomicU32::new(0),
            terminal_reason: AtomicU32::new(0),
            bootstrap_complete: AtomicBool::new(false),
            bootstrap: Mutex::new(model::Bootstrap::default()),
            display_running: AtomicBool::new(false),
            video_width: AtomicU32::new(0),
            video_height: AtomicU32::new(0),
            negotiated_width: AtomicU32::new(0),
            negotiated_height: AtomicU32::new(0),
            negotiated_display_format: AtomicU32::new(0),
            last_control_message_type: AtomicU32::new(0),
            generic_control_messages: AtomicU64::new(0),
            apcp_server_marker: 0,
            apcp_protocol_version: 0,
            apcp_transports: 0,
            apcp_redirect_port: 0,
            transport_profile,
            reconnect_session: AtomicU32::new(0),
            dvc_startup: Mutex::new(DvcStartup::new(
                transport_profile,
                tokio::time::Instant::now(),
            )),
            dvc_mode: Mutex::new(None),
            dvc_mode_generation: AtomicU64::new(0),
            dvc_mode_notify: Notify::new(),
            dvc_switch: Mutex::new(DvcProfileSwitch::default()),
            dvc_switch_notify: Notify::new(),
            dvc_switch_lock: tokio::sync::Mutex::new(()),
            keyboard_led_sync_at: tokio::sync::Mutex::new(None),
            control_tx,
            frame_tx,
            last_frame: Mutex::new(None),
            video_state,
        });
        (session, control_rx)
    }

    #[tokio::test]
    async fn a_new_transport_status_cannot_confirm_an_old_color_request() {
        let (session, mut commands) =
            test_session_for_profile_with_control(0, AvctTransportProfile::Idrac8Apcp);
        let requesting = session.clone();
        let request = tokio::spawn(async move { requesting.set_color_depth("variant-1-4").await });
        let _ = commands.recv().await.unwrap();
        session.control_epoch.store(1, Ordering::Release);
        session.record_dvc_mode(ColorModeResponse {
            mode: 4,
            flag_1: true,
            flag_2: false,
        });
        let error = tokio::time::timeout(Duration::from_secs(1), request)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(error.contains("epoch changed"));
        assert_eq!(session.active_color_depth_profile_id(), None);
    }

    #[tokio::test]
    async fn stale_input_queue_records_cannot_cross_transport_epochs() {
        let (session, mut commands) =
            test_session_for_profile_with_control(0, AvctTransportProfile::Idrac8Apcp);
        session.control_epoch.store(1, Ordering::Release);
        session
            .control_tx
            .send(ControlWrite::Single(0, keyboard_packet(4, true)))
            .await
            .unwrap();
        session
            .send_hid(HidCommand::Keyboard {
                scancode: 5,
                pressed: true,
            })
            .await
            .unwrap();
        let (control, mut peer) = tokio::io::duplex(1024);
        let (video, _video_peer) = tokio::io::duplex(1024);
        let running = session.clone();
        let mut shutdown = session.shutdown.subscribe();
        let worker = tokio::spawn(async move {
            run_session_loop(
                Box::new(control),
                Box::new(video),
                running,
                &mut shutdown,
                &mut commands,
                4096,
                false,
            )
            .await;
        });
        let written = tokio::time::timeout(Duration::from_secs(1), read_packet(&mut peer, 4096))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(written.payload, keyboard_packet(5, true).payload);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), read_packet(&mut peer, 4096))
                .await
                .is_err()
        );
        session.shutdown();
        tokio::time::timeout(Duration::from_secs(1), worker)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn absolute_hid_motion_and_release_emit_packets_even_without_scroll() {
        use bmc_feature_api::hid::HidBackend;
        let (session, mut control) =
            test_session_for_profile_with_control(1, AvctTransportProfile::Idrac8Apcp);
        let key = PoolKey::from_config(
            &IdracAvctConfig::new(test_options("hid-intent.example.test", 8)).unwrap(),
        );
        let backend = crate::hid::IdracAvctHidBackend::new(AvctLease { key, session });
        backend.mouse_move_absolute(10, 20).await.unwrap();
        assert!(
            matches!(control.recv().await, Some(ControlWrite::Single(_, packet)) if packet.message_type == 0x0201)
        );
        backend
            .mouse_button_transition(10, 20, 0, true)
            .await
            .unwrap();
        assert!(
            matches!(control.recv().await, Some(ControlWrite::Single(_, packet)) if packet.message_type == 0x0201)
        );
        backend.reset().await.unwrap();
        assert!(
            matches!(control.recv().await, Some(ControlWrite::Single(_, packet)) if packet.message_type == 0x0201)
        );
    }

    #[tokio::test]
    async fn cancelled_pool_creator_releases_slot_and_sticky_waiters() {
        let key = PoolKey {
            host: "cancelled-creator.example.test".to_string(),
            port: 5900,
            transport_profile: AvctTransportProfile::Idrac8Apcp,
            credential_fingerprint: 0,
        };
        let (completion, mut waiter) = watch::channel(false);
        let completion = Arc::new(completion);
        pool().insert(key.clone(), PoolSlot::Connecting(completion.clone()));
        let guard = ConnectingGuard {
            key: key.clone(),
            completion,
        };
        let (started, ready) = tokio::sync::oneshot::channel();
        let worker = tokio::spawn(async move {
            let _guard = guard;
            let _ = started.send(());
            std::future::pending::<()>().await;
        });
        ready.await.unwrap();
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        // Subscribe/observe after completion: watch retains the notification.
        if !*waiter.borrow() {
            tokio::time::timeout(Duration::from_millis(100), waiter.changed())
                .await
                .unwrap()
                .unwrap();
        }
        assert!(*waiter.borrow());
        assert!(!pool().contains_key(&key));
    }

    #[tokio::test]
    async fn control_pairs_are_one_non_interleavable_queue_command() {
        let (session, mut control_rx) =
            test_session_for_profile_with_control(0, AvctTransportProfile::Idrac6Apcp);
        let target = color_depth_request(color_depth_profile("base-4").unwrap());
        let refresh = screen_refresh();

        session
            .send_control_pair(target.clone(), refresh.clone())
            .await
            .unwrap();

        match control_rx.recv().await.unwrap() {
            ControlWrite::Pair(_, first, second) => {
                assert_eq!(first.magic, target.magic);
                assert_eq!(first.message_type, target.message_type);
                assert_eq!(first.payload, target.payload);
                assert_eq!(second.magic, refresh.magic);
                assert_eq!(second.message_type, refresh.message_type);
                assert_eq!(second.payload, refresh.payload);
            }
            ControlWrite::Single(_, _) => panic!("control pair was split into a single write"),
        }
        assert!(control_rx.try_recv().is_err());
    }

    #[test]
    fn final_lease_keeps_the_pool_slot_until_the_transport_has_closed() {
        let _serial = POOL_TEST_LOCK.lock().unwrap();
        let key = PoolKey {
            host: "bmc.example.test".to_string(),
            port: 5900,
            transport_profile: AvctTransportProfile::Idrac8Apcp,
            credential_fingerprint: 1,
        };
        let session = test_session(1);
        pool().remove(&key);
        pool().insert(key.clone(), PoolSlot::Ready(Arc::clone(&session)));

        drop(AvctLease {
            key: key.clone(),
            session,
        });

        assert!(pool().contains_key(&key));
        pool().remove(&key);
    }

    #[test]
    fn disconnected_closing_session_can_recover_if_transport_notification_is_lost() {
        let session = test_session(0);
        assert!(!closing_session_can_be_reclaimed(&session));

        session.mark_disconnected();
        assert!(closing_session_can_be_reclaimed(&session));
    }

    #[tokio::test]
    async fn full_frame_is_rejected_before_display_running() {
        let session = test_session(0);
        assert!(session
            .request_full_frame()
            .await
            .unwrap_err()
            .contains("display is not running"));
    }

    #[tokio::test]
    async fn panicked_session_task_publishes_a_terminal_disconnect() {
        let session = test_session(0);
        supervise_session_task(session.clone(), async {
            panic!("synthetic AVCT session panic");
        })
        .await;

        assert!(!session.is_connected());
        assert!(*session.disconnected_rx().borrow());
        assert!(*session.transport_closed_rx().borrow());
        assert_eq!(
            session.terminal_reason(),
            AvctTerminalReason::SessionTaskPanic
        );
    }

    #[test]
    fn uses_device_credentials_without_exposing_them_in_pool_key() {
        let config = IdracAvctConfig::new(test_options("bmc.example.test", 8)).unwrap();
        assert_eq!(config.port, 5900);
        assert_ne!(
            format!("{:?}", PoolKey::from_config(&config)),
            format!("{config:?}")
        );
    }

    #[test]
    fn display_attach_reuses_the_login_client_random() {
        let packet = video_session_request(7_654_321, 0x01020304);
        assert_eq!(packet.magic, [0, 0, 0, 0]);
        assert_eq!(packet.message_type, 0x0101);
        assert_eq!(packet.payload.len(), 8);
        assert_eq!(
            u32::from_be_bytes(packet.payload[..4].try_into().unwrap()),
            7_654_321
        );
        assert_eq!(
            u32::from_be_bytes(packet.payload[4..8].try_into().unwrap()),
            0x01020304
        );
    }

    #[test]
    fn idrac6_direct_attach_matches_the_official_yb_packet() {
        let packet = idrac6_video_session_request(7_654_321, 0x01020304);
        assert!(packet.is_beef());
        assert_eq!(packet.message_type, 0x0001);
        assert_eq!(
            packet.payload.as_ref(),
            &[0x00, 0x74, 0xcb, 0xb1, 1, 2, 3, 4]
        );
    }

    #[test]
    fn idrac6_selects_the_direct_profile_and_fixed_login_layout() {
        let config = IdracAvctConfig::new(test_options("idrac6.example.test", 6)).unwrap();
        assert_eq!(config.transport_profile, AvctTransportProfile::Idrac6Apcp);
        let login =
            build_login_request(LoginProfile::Compatibility, "root", "test-only", false).unwrap();
        assert_eq!(login.packet.message_type, 0x0102);
        assert_eq!(login.packet.total_len(), 217);
    }

    #[test]
    fn idrac6_enforces_its_factory_minimum_and_excludes_dvc23() {
        assert!(validate_inbound_packet(
            AvctPacket::beef(0x0082, Bytes::from_static(&[0; 7])),
            false,
        )
        .is_err());
        assert!(validate_inbound_packet(
            AvctPacket::beef(0x0082, Bytes::from_static(&[0; 7])),
            true,
        )
        .is_ok());
    }

    #[test]
    fn official_video_control_records_are_not_sent_to_the_frame_decoder() {
        assert!(is_video_control_packet(0x8203));
        assert!(is_video_control_packet(0x8200));
        assert!(!is_video_control_packet(0x8205));
    }

    #[test]
    fn official_video_stopped_records_are_typed_state_inputs() {
        assert!(is_video_stopped_packet_type(0x0085));
        assert!(is_video_stopped_packet_type(0x8605));
        assert!(!is_video_stopped_packet_type(0x0084));
    }

    #[test]
    fn display_ack_and_bootstrap_match_the_reference_wire_shapes() {
        let ack = display_ack();
        assert_eq!(ack.message_type, 0);
        assert_eq!(&ack.payload[..], &[1, 0, 0, 0, 0, 0, 0, 0]);

        let bootstrap = display_bootstrap();
        assert_eq!(
            bootstrap
                .iter()
                .map(|packet| packet.message_type)
                .collect::<Vec<_>>(),
            vec![0x030e, 0x0302, 0x0304]
        );
        assert_eq!(&bootstrap[1].payload[..4], &[4, 0, 3, 0]);

        let refresh = screen_refresh();
        assert_eq!(refresh.message_type, 0x0301);
        assert_eq!(&refresh.payload[..], &[0; 8]);

        let idrac6_startup =
            startup_dvc_request(AvctTransportProfile::Idrac6Apcp).expect("iDRAC6 startup profile");
        assert_eq!(idrac6_startup.message_type, 0x0403);
        assert_eq!(&idrac6_startup.payload[..], &[0, 15, 1, 0, 0, 0, 0, 0]);
        assert!(startup_dvc_request(AvctTransportProfile::Idrac8Apcp).is_none());
        assert_eq!(
            supported_color_depth_profiles(AvctTransportProfile::Idrac6Apcp).len(),
            IDRAC6_COLOR_DEPTH_PROFILES.len()
        );
        assert_eq!(
            supported_color_depth_profiles(AvctTransportProfile::Idrac8Apcp).len(),
            IDRAC8_COLOR_DEPTH_PROFILES.len()
        );
    }

    #[test]
    fn idrac6_startup_drains_old_data_and_publishes_only_a_refreshed_chain() {
        let now = tokio::time::Instant::now();
        let mut startup = DvcStartup::new(AvctTransportProfile::Idrac6Apcp, now);
        let deadline = startup.deadline().expect("iDRAC6 startup deadline");
        assert_eq!(startup.phase(), DvcStartupPhase::AwaitingBootstrap);

        startup.mark_bootstrap_complete();
        assert_eq!(startup.phase(), DvcStartupPhase::AwaitingDvc15);
        assert_eq!(startup.deadline(), Some(deadline));

        // A persisted DVC7 packet is acknowledged but never decoded.
        assert_eq!(
            startup.observe_display(0x0082, 3),
            DvcDisplayAction::Suppress
        );
        assert_eq!(startup.phase(), DvcStartupPhase::AwaitingDvc15);

        // Drain the first DVC15 command chain. Only its source-confirmed bit-1
        // boundary requests a refresh.
        assert_eq!(
            startup.observe_display(0x0081, 1),
            DvcDisplayAction::Suppress
        );
        assert_eq!(startup.phase(), DvcStartupPhase::DrainingInitialDvc15Chain);
        assert_eq!(
            startup.observe_display(0x0081, 2),
            DvcDisplayAction::SuppressAndRequestRefresh
        );
        assert_eq!(startup.phase(), DvcStartupPhase::AwaitingRefreshedDvc15);

        // In-flight DVC15 data from before the refresh lacks the reset bit and
        // remains suppressed. It cannot trigger a second refresh.
        assert_eq!(
            startup.observe_display(0x0081, 2),
            DvcDisplayAction::Suppress
        );

        assert_eq!(
            startup.observe_display(0x0081, 1),
            DvcDisplayAction::DecodeRefreshed {
                command_chain_boundary: false
            }
        );
        assert_eq!(
            startup.phase(),
            DvcStartupPhase::DecodingRefreshedDvc15Chain
        );
        assert_eq!(
            startup.observe_display(0x0081, 2),
            DvcDisplayAction::DecodeRefreshed {
                command_chain_boundary: true
            }
        );
        assert!(startup.finish_refreshed_chain());
        assert_eq!(startup.phase(), DvcStartupPhase::Ready);
        assert_eq!(startup.deadline(), None);
        assert_eq!(
            startup.observe_display(0x0081, 2),
            DvcDisplayAction::Passthrough
        );
    }

    #[test]
    fn idrac6_startup_rejects_a_family_change_inside_the_baseline_chain() {
        let now = tokio::time::Instant::now();
        let mut startup = DvcStartup::new(AvctTransportProfile::Idrac6Apcp, now);
        startup.mark_bootstrap_complete();
        assert_eq!(
            startup.observe_display(0x0081, 0),
            DvcDisplayAction::Suppress
        );
        assert_eq!(
            startup.observe_display(0x0083, 2),
            DvcDisplayAction::RejectUnexpectedFamily
        );
        assert_eq!(startup.phase(), DvcStartupPhase::Failed);
        assert_eq!(startup.deadline(), None);
    }

    #[test]
    fn idrac6_profile_switch_waits_for_a_complete_reset_marked_target_chain() {
        let now = tokio::time::Instant::now();
        let target = color_depth_profile("base-4").unwrap();
        let mut switch = DvcProfileSwitch::default();

        let generation = switch.queue(target).unwrap();
        assert_eq!(switch.phase(), DvcProfileSwitchPhase::Queued);
        assert_eq!(
            switch.observe_display(0x0081, 2),
            DvcSwitchAction::Passthrough
        );

        assert!(switch.mark_request_written(generation, now));
        assert_eq!(
            switch.phase(),
            DvcProfileSwitchPhase::AwaitingTargetTransitionReset
        );
        assert_eq!(switch.observe_display(0x0081, 3), DvcSwitchAction::Suppress);
        assert_eq!(
            switch.phase(),
            DvcProfileSwitchPhase::AwaitingTargetTransitionReset
        );
        assert_eq!(switch.observe_display(0x0083, 1), DvcSwitchAction::Suppress);
        assert_eq!(
            switch.phase(),
            DvcProfileSwitchPhase::DrainingTargetTransition
        );
        assert_eq!(
            switch.observe_display(0x0083, 2),
            DvcSwitchAction::RequestTargetRefresh
        );
        assert_eq!(switch.phase(), DvcProfileSwitchPhase::AwaitingTargetReset);
        assert_eq!(
            switch.observe_display(0x0083, 1),
            DvcSwitchAction::DecodeTarget {
                command_chain_boundary: false,
            }
        );
        assert_eq!(switch.phase(), DvcProfileSwitchPhase::DecodingTarget);
        assert_eq!(
            switch.observe_display(0x0083, 2),
            DvcSwitchAction::DecodeTarget {
                command_chain_boundary: true,
            }
        );
        assert!(switch.finish_target_chain());
        assert_eq!(switch.take_completion(generation), Some(Ok(())));
        assert_eq!(switch.phase(), DvcProfileSwitchPhase::Idle);
    }

    #[test]
    fn idrac6_profile_switch_recovers_to_dvc15_before_reporting_failure() {
        let now = tokio::time::Instant::now();
        let target = color_depth_profile("variant-1-9").unwrap();
        let mut switch = DvcProfileSwitch::default();
        let generation = switch.queue(target).unwrap();
        assert!(switch.mark_request_written(generation, now));

        switch.begin_fallback("target DVC chain could not be decoded", now);
        assert_eq!(
            switch.phase(),
            DvcProfileSwitchPhase::AwaitingFallbackTransitionReset
        );
        assert_eq!(
            switch.observe_display(0x0081, 3),
            DvcSwitchAction::RequestFallbackRefresh
        );
        assert_eq!(switch.phase(), DvcProfileSwitchPhase::AwaitingFallbackReset);
        assert_eq!(
            switch.observe_display(0x0081, 1),
            DvcSwitchAction::DecodeFallback {
                command_chain_boundary: false,
            }
        );
        assert_eq!(
            switch.observe_display(0x0081, 2),
            DvcSwitchAction::DecodeFallback {
                command_chain_boundary: true,
            }
        );
        assert!(switch.finish_fallback_chain());
        assert_eq!(
            switch.take_completion(generation),
            Some(Err(
                "target DVC chain could not be decoded; restored variant-1-15".to_string()
            ))
        );
        assert_eq!(switch.phase(), DvcProfileSwitchPhase::Idle);
    }

    #[test]
    fn idrac6_profile_switch_deadlines_do_not_extend_with_partial_progress() {
        let now = tokio::time::Instant::now();
        let target = color_depth_profile("variant-1-23").unwrap();
        let mut switch = DvcProfileSwitch::default();
        let generation = switch.queue(target).unwrap();
        assert!(switch.mark_request_written(generation, now));
        let deadline = switch.deadline().unwrap();

        assert_eq!(switch.observe_display(0x0081, 0), DvcSwitchAction::Suppress);
        assert_eq!(switch.deadline(), Some(deadline));
        assert_eq!(
            switch.observe_display(0x0081, 3),
            DvcSwitchAction::RequestTargetRefresh
        );
        assert_eq!(switch.deadline(), Some(deadline));
        assert_eq!(
            switch.observe_display(0x0081, 1),
            DvcSwitchAction::DecodeTarget {
                command_chain_boundary: false,
            }
        );
        assert_eq!(switch.deadline(), Some(deadline));
        assert!(!switch.is_expired(deadline - Duration::from_nanos(1)));
        assert!(switch.is_expired(deadline));
    }

    #[test]
    fn idrac6_startup_uses_one_absolute_deadline_for_all_phases() {
        let now = tokio::time::Instant::now();
        let mut startup = DvcStartup::new(AvctTransportProfile::Idrac6Apcp, now);
        let deadline = startup.deadline().unwrap();
        startup.mark_bootstrap_complete();
        assert_eq!(
            startup.observe_display(0x0081, 2),
            DvcDisplayAction::SuppressAndRequestRefresh
        );
        assert_eq!(startup.deadline(), Some(deadline));
        assert!(!startup.fail_if_expired(deadline - Duration::from_nanos(1)));
        assert!(startup.fail_if_expired(deadline));
        assert_eq!(startup.phase(), DvcStartupPhase::Failed);
        assert_eq!(startup.deadline(), None);
    }

    #[test]
    fn explicit_video_stopped_recovers_on_the_first_official_dvc_record() {
        let now = tokio::time::Instant::now();
        let mut startup = DvcStartup::new(AvctTransportProfile::Idrac6Apcp, now);
        startup.mark_bootstrap_complete();
        startup.suspend_for_no_signal();
        assert_eq!(startup.phase(), DvcStartupPhase::AwaitingSignal);
        assert_eq!(startup.deadline(), None);
        assert!(!startup.fail_if_expired(now + Duration::from_secs(60)));
        assert_eq!(
            startup.observe_display(0x0081, 3),
            DvcDisplayAction::Passthrough
        );
        assert_eq!(startup.phase(), DvcStartupPhase::Ready);
        assert_eq!(startup.deadline(), None);
    }

    #[test]
    fn idrac8_startup_policy_remains_unmanaged() {
        let mut startup = DvcStartup::new(
            AvctTransportProfile::Idrac8Apcp,
            tokio::time::Instant::now(),
        );
        assert_eq!(startup.phase(), DvcStartupPhase::Unmanaged);
        assert_eq!(startup.deadline(), None);
        assert_eq!(
            startup.observe_display(0x008a, 3),
            DvcDisplayAction::Passthrough
        );
    }

    #[tokio::test]
    async fn repeated_idrac6_video_ready_notification_does_not_replay_bootstrap() {
        let session = test_session_for_profile(0, AvctTransportProfile::Idrac6Apcp);
        let (mut writer, mut reader) = tokio::io::duplex(256);

        send_display_bootstrap(&mut writer, &session).await.unwrap();
        send_display_bootstrap(&mut writer, &session).await.unwrap();

        let mut message_types = Vec::new();
        for _ in 0..4 {
            message_types.push(read_packet(&mut reader, 256).await.unwrap().message_type);
        }
        assert_eq!(message_types, vec![0x030e, 0x0302, 0x0304, 0x0403]);
        assert!(
            tokio::time::timeout(Duration::from_millis(10), read_packet(&mut reader, 256))
                .await
                .is_err()
        );
        assert_eq!(session.packets_out.load(Ordering::Relaxed), 4);
        assert_eq!(
            session.dvc_startup_state().phase(),
            DvcStartupPhase::AwaitingDvc15
        );
    }

    #[tokio::test]
    async fn idrac6_rejects_unknown_profiles_before_writing_control_data() {
        let (session, mut control_rx) =
            test_session_for_profile_with_control(0, AvctTransportProfile::Idrac6Apcp);

        let error = session.set_color_depth("not-a-profile").await.unwrap_err();

        assert!(error.contains("unsupported iDRAC DVC color-depth profile"));
        assert!(control_rx.try_recv().is_err());
        assert_eq!(
            session.trace_snapshot()["dvc"]["switch_phase"],
            serde_json::json!("idle")
        );
    }

    #[test]
    fn transport_profiles_expose_only_hardware_confirmed_combinations() {
        let profiles = supported_color_depth_profiles(AvctTransportProfile::Idrac6Apcp);

        assert_eq!(profiles.len(), 11);
        assert!(profiles.contains(&color_depth_profile("base-6").unwrap()));
        assert!(profiles.contains(&color_depth_profile("variant-1-23").unwrap()));
        assert!(!profiles.contains(&color_depth_profile("variant-1-4").unwrap()));

        let profiles = supported_color_depth_profiles(AvctTransportProfile::Idrac8Apcp);
        assert_eq!(profiles.len(), 14);
        assert!(profiles.contains(&color_depth_profile("variant-1-4").unwrap()));
        assert!(color_depth_profile("base-9").is_none());
    }

    #[tokio::test]
    async fn idrac8_commits_a_profile_only_after_matching_raw_status() {
        let (session, mut control_rx) =
            test_session_for_profile_with_control(0, AvctTransportProfile::Idrac8Apcp);
        let set_session = session.clone();
        let operation =
            tokio::spawn(async move { set_session.set_color_depth("variant-1-4").await });

        let write = control_rx.recv().await.expect("queued colour-depth write");
        let ControlWrite::Pair(_, request, refresh) = write else {
            panic!("colour-depth request was not queued atomically");
        };
        let expected = color_depth_request(color_depth_profile("variant-1-4").unwrap());
        assert_eq!(request.message_type, expected.message_type);
        assert_eq!(request.payload, expected.payload);
        let expected = screen_refresh();
        assert_eq!(refresh.message_type, expected.message_type);
        assert_eq!(refresh.payload, expected.payload);
        assert_eq!(session.active_color_depth_profile_id(), None);

        session.record_dvc_mode(ColorModeResponse {
            mode: 4,
            flag_1: true,
            flag_2: false,
        });

        operation.await.unwrap().unwrap();
        assert_eq!(session.active_color_depth_profile_id(), Some("variant-1-4"));
    }

    #[tokio::test]
    async fn idrac6_session_loop_does_not_publish_until_refreshed_dvc15_is_complete() {
        fn dvc_packet(message_type: u16, flags: u8, compressed: &'static [u8]) -> AvctPacket {
            let mut payload = vec![0; 12 + compressed.len()];
            payload[4..6].copy_from_slice(&1u16.to_be_bytes());
            payload[6..8].copy_from_slice(&1u16.to_be_bytes());
            payload[8] = flags;
            payload[12..].copy_from_slice(compressed);
            AvctPacket::beef(message_type, Bytes::from(payload))
        }

        let session = test_session_for_profile(0, AvctTransportProfile::Idrac6Apcp);
        let mut frames = session.subscribe_frames();
        let (client_control, mut bmc_control) = tokio::io::duplex(4096);
        let (client_video, mut bmc_video) = tokio::io::duplex(4096);
        let mut shutdown = session.shutdown.subscribe();
        let (control_tx, mut control_rx) = mpsc::channel(1);
        let task_session = session.clone();
        let task = tokio::spawn(async move {
            run_session_loop(
                Box::new(client_control),
                Box::new(client_video),
                task_session,
                &mut shutdown,
                &mut control_rx,
                4096,
                false,
            )
            .await;
        });

        write_packet(
            &mut bmc_video,
            &AvctPacket::beef(0x0084, Bytes::from_static(&[0; 8])),
        )
        .await
        .unwrap();
        for expected in [0x030e, 0x0302, 0x0304, 0x0403] {
            assert_eq!(
                read_packet(&mut bmc_control, 4096)
                    .await
                    .unwrap()
                    .message_type,
                expected
            );
        }

        // Persisted DVC7 is ACKed but never reaches the decoder or frame bus.
        write_packet(&mut bmc_video, &dvc_packet(0x0082, 3, &[0x81]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        assert!(matches!(
            frames.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(!session.display_running.load(Ordering::SeqCst));

        // The initial DVC15 chain is drained. Its boundary causes exactly one
        // refresh request, while its pixels remain unpublished.
        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_control, 4096)
                .await
                .unwrap()
                .message_type,
            0x0301
        );
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        assert!(matches!(
            frames.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ));
        assert!(!session.display_running.load(Ordering::SeqCst));

        // A pre-refresh packet already in flight lacks cursor-reset and is
        // suppressed without issuing another refresh.
        write_packet(&mut bmc_video, &dvc_packet(0x0081, 2, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            read_packet(&mut bmc_control, 4096)
        )
        .await
        .is_err());
        assert!(!session.display_running.load(Ordering::SeqCst));

        // Only a reset-marked, terminal refreshed chain becomes the first
        // browser-visible framebuffer.
        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        let frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((frame.width, frame.height), (1, 1));
        assert_eq!(
            frame.buffer.map_readable().unwrap().as_slice(),
            &[0, 0, 248, 255]
        );
        assert_eq!(session.dvc_startup_state().phase(), DvcStartupPhase::Ready);
        assert_eq!(session.decoded_frames.load(Ordering::Relaxed), 1);
        assert!(session.display_running.load(Ordering::SeqCst));
        let late_subscriber = session.subscribe_frames();
        assert_eq!(late_subscriber.len(), 0);
        assert_eq!(
            session
                .last_frame()
                .unwrap()
                .buffer
                .map_readable()
                .unwrap()
                .as_slice(),
            &[0, 0, 248, 255]
        );

        session.shutdown();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        drop(control_tx);
    }

    #[tokio::test]
    async fn idrac6_session_loop_confirms_a_profile_only_after_its_complete_frame() {
        fn dvc_packet(message_type: u16, flags: u8, compressed: &'static [u8]) -> AvctPacket {
            let mut payload = vec![0; 12 + compressed.len()];
            payload[4..6].copy_from_slice(&1u16.to_be_bytes());
            payload[6..8].copy_from_slice(&1u16.to_be_bytes());
            payload[8] = flags;
            payload[12..].copy_from_slice(compressed);
            AvctPacket::beef(message_type, Bytes::from(payload))
        }

        let (session, mut control_rx) =
            test_session_for_profile_with_control(0, AvctTransportProfile::Idrac6Apcp);
        {
            let mut startup = session.dvc_startup_state();
            startup.suspend_for_no_signal();
            startup.observe_display(0x0081, 3);
        }
        let mut frames = session.subscribe_frames();
        let (client_control, mut bmc_control) = tokio::io::duplex(4096);
        let (client_video, mut bmc_video) = tokio::io::duplex(4096);
        let mut shutdown = session.shutdown.subscribe();
        let task_session = session.clone();
        let task = tokio::spawn(async move {
            run_session_loop(
                Box::new(client_control),
                Box::new(client_video),
                task_session,
                &mut shutdown,
                &mut control_rx,
                4096,
                false,
            )
            .await;
        });

        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        frames.recv().await.unwrap();

        let set_session = session.clone();
        let set = tokio::spawn(async move { set_session.set_color_depth("base-4").await });
        let request = read_packet(&mut bmc_control, 4096).await.unwrap();
        assert_eq!(request.message_type, 0x0403);
        assert_eq!(&request.payload[..3], &[0, 4, 0]);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                read_packet(&mut bmc_control, 4096)
            )
            .await
            .is_err(),
            "0x0403 must not be followed by an immediate ScreenRefresh"
        );
        assert!(!set.is_finished());

        // Old-family traffic may complete after 0x0403. It is acknowledged,
        // but cannot be mistaken for the target-family transition chain.
        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                read_packet(&mut bmc_control, 4096)
            )
            .await
            .is_err(),
            "old-family traffic must not trigger the target refresh"
        );
        assert!(!set.is_finished());

        // The first target-family chain is the source-confirmed transition
        // payload. Its boundary triggers the only ScreenRefresh.
        write_packet(&mut bmc_video, &dvc_packet(0x0083, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        assert_eq!(
            read_packet(&mut bmc_control, 4096)
                .await
                .unwrap()
                .message_type,
            0x0301
        );
        assert!(!set.is_finished());

        // Only the next reset-marked target-family chain commits the
        // requested profile and publishes its framebuffer.
        write_packet(&mut bmc_video, &dvc_packet(0x0083, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        set.await.unwrap().unwrap();
        frames.recv().await.unwrap();
        assert_eq!(
            session.trace_snapshot()["dvc"]["active_profile"],
            serde_json::json!("base-4")
        );

        session.shutdown();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn idrac6_session_loop_restores_dvc15_before_rejecting_a_bad_profile() {
        fn dvc_packet(message_type: u16, flags: u8, compressed: &'static [u8]) -> AvctPacket {
            let mut payload = vec![0; 12 + compressed.len()];
            payload[4..6].copy_from_slice(&1u16.to_be_bytes());
            payload[6..8].copy_from_slice(&1u16.to_be_bytes());
            payload[8] = flags;
            payload[12..].copy_from_slice(compressed);
            AvctPacket::beef(message_type, Bytes::from(payload))
        }

        let (session, mut control_rx) =
            test_session_for_profile_with_control(0, AvctTransportProfile::Idrac6Apcp);
        {
            let mut startup = session.dvc_startup_state();
            startup.suspend_for_no_signal();
            startup.observe_display(0x0081, 3);
        }
        let (client_control, mut bmc_control) = tokio::io::duplex(4096);
        let (client_video, mut bmc_video) = tokio::io::duplex(4096);
        let mut shutdown = session.shutdown.subscribe();
        let task_session = session.clone();
        let task = tokio::spawn(async move {
            run_session_loop(
                Box::new(client_control),
                Box::new(client_video),
                task_session,
                &mut shutdown,
                &mut control_rx,
                4096,
                false,
            )
            .await;
        });

        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        read_packet(&mut bmc_video, 4096).await.unwrap();

        let set_session = session.clone();
        let set = tokio::spawn(async move { set_session.set_color_depth("variant-1-9").await });
        assert_eq!(
            read_packet(&mut bmc_control, 4096)
                .await
                .unwrap()
                .message_type,
            0x0403
        );
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                read_packet(&mut bmc_control, 4096)
            )
            .await
            .is_err(),
            "0x0403 must not be followed by an immediate ScreenRefresh"
        );

        // The first chain after 0x0403 is transition data. Its boundary
        // requests a clean target frame.
        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        assert_eq!(
            read_packet(&mut bmc_control, 4096)
                .await
                .unwrap()
                .message_type,
            0x0301
        );

        // The following target chain is malformed according to the official
        // DVC15 literal-reader rules, which starts the fallback transition.
        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        let fallback = read_packet(&mut bmc_control, 4096).await.unwrap();
        assert_eq!(fallback.message_type, 0x0403);
        assert_eq!(&fallback.payload[..3], &[0, 15, 1]);
        assert!(
            tokio::time::timeout(
                Duration::from_millis(50),
                read_packet(&mut bmc_control, 4096)
            )
            .await
            .is_err(),
            "DVC15 fallback must also wait for its transition boundary"
        );

        // Fallback also drains its transition chain, then requests a clean
        // DVC15 reset-marked frame.
        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        assert_eq!(
            read_packet(&mut bmc_control, 4096)
                .await
                .unwrap()
                .message_type,
            0x0301
        );

        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        let error = set.await.unwrap().unwrap_err();
        assert!(error.contains("restored variant-1-15"));
        assert!(session.is_connected());
        assert_eq!(
            session.trace_snapshot()["dvc"]["active_profile"],
            serde_json::json!("variant-1-15")
        );

        session.shutdown();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn idrac6_video_stopped_recovers_and_publishes_the_first_dvc_frame() {
        fn dvc_packet(message_type: u16, flags: u8, compressed: &'static [u8]) -> AvctPacket {
            let mut payload = vec![0; 12 + compressed.len()];
            payload[4..6].copy_from_slice(&1u16.to_be_bytes());
            payload[6..8].copy_from_slice(&1u16.to_be_bytes());
            payload[8] = flags;
            payload[12..].copy_from_slice(compressed);
            AvctPacket::beef(message_type, Bytes::from(payload))
        }

        let session = test_session_for_profile(0, AvctTransportProfile::Idrac6Apcp);
        let (client_control, mut bmc_control) = tokio::io::duplex(4096);
        let (client_video, mut bmc_video) = tokio::io::duplex(4096);
        let mut shutdown = session.shutdown.subscribe();
        let (control_tx, mut control_rx) = mpsc::channel(1);
        let task_session = session.clone();
        let task = tokio::spawn(async move {
            run_session_loop(
                Box::new(client_control),
                Box::new(client_video),
                task_session,
                &mut shutdown,
                &mut control_rx,
                4096,
                false,
            )
            .await;
        });

        write_packet(
            &mut bmc_video,
            &AvctPacket::beef(0x0085, Bytes::from_static(&[0; 8])),
        )
        .await
        .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        assert_eq!(
            session.dvc_startup_state().phase(),
            DvcStartupPhase::AwaitingSignal
        );
        assert_eq!(session.dvc_startup_deadline(), None);
        assert!(!session.bootstrap_complete.load(Ordering::SeqCst));
        assert_eq!(*session.video_state_rx().borrow(), AvctVideoState::Stopped);

        let mut frames = session.subscribe_frames();
        write_packet(&mut bmc_video, &dvc_packet(0x0081, 3, &[0xfc, 0x00]))
            .await
            .unwrap();
        assert_eq!(
            read_packet(&mut bmc_video, 4096)
                .await
                .unwrap()
                .message_type,
            0
        );
        let frame = tokio::time::timeout(Duration::from_secs(1), frames.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((frame.width, frame.height), (1, 1));
        assert_eq!(session.dvc_startup_state().phase(), DvcStartupPhase::Ready);
        assert_eq!(session.dvc_startup_deadline(), None);
        assert!(session.display_running.load(Ordering::SeqCst));
        assert_eq!(*session.video_state_rx().borrow(), AvctVideoState::Running);
        assert!(tokio::time::timeout(
            Duration::from_millis(10),
            read_packet(&mut bmc_control, 4096)
        )
        .await
        .is_err());

        session.shutdown();
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        drop(control_tx);
    }

    #[test]
    fn apcp_keepalive_matches_the_official_control_writer() {
        let keepalive = apcp_keepalive();
        assert_eq!(keepalive.magic, *b"APCP");
        assert_eq!(keepalive.message_type, 0);
        assert_eq!(keepalive.total_len(), 12);
        assert_eq!(&keepalive.payload[..], &[0x04, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn rejected_login_cleanup_matches_the_official_sequence() {
        let cleanup = rejected_login_close();
        assert_eq!(cleanup[0].message_type, 0x0400);
        assert!(cleanup[0].is_beef());
        assert_eq!(&cleanup[0].payload[..], &[0; 8]);
        assert_eq!(cleanup[1].magic, *b"APCP");
        assert_eq!(cleanup[1].message_type, 0);
        assert_eq!(&cleanup[1].payload[..], &[0x01, 0xff, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn graceful_shutdown_uses_the_official_apcp_close_record() {
        let close = graceful_session_close_record();
        assert_eq!(close.magic, *b"APCP");
        assert_eq!(close.message_type, 0);
        assert_eq!(close.total_len(), 16);
        assert_eq!(&close.payload[..], &[0x01, 0xff, 0, 0, 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn graceful_shutdown_write_is_bounded_when_the_peer_stops_reading() {
        let (mut writer, _reader) = tokio::io::duplex(1);
        let start = tokio::time::Instant::now();

        assert!(!try_write_graceful_close(&mut writer, &graceful_session_close_record()).await);
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn session_setup_has_a_hard_deadline_when_a_peer_stops_responding() {
        let error = bounded_session_setup_with_timeout(
            Duration::from_millis(20),
            std::future::pending::<Result<(), String>>(),
        )
        .await
        .unwrap_err();

        assert!(error.contains("session setup timed out"));
    }

    #[test]
    fn shutdown_signal_requests_the_official_close_path() {
        assert!(matches!(shutdown_event(true), SessionEvent::Shutdown(true)));
        assert!(matches!(
            shutdown_event(false),
            SessionEvent::Shutdown(false)
        ));
    }

    #[test]
    fn records_official_resolution_and_generic_control_state() {
        let (shutdown, _) = watch::channel(false);
        let (disconnected, _) = watch::channel(false);
        let (control_tx, _) = mpsc::channel(1);
        let (frame_tx, _) = tokio::sync::broadcast::channel(1);
        let (video_state, _) = watch::channel(AvctVideoState::Negotiating);
        let session = AvctSession {
            shutdown,
            disconnected,
            transport_closed: watch::channel(false).0,
            connected: AtomicBool::new(true),
            accepting_commands: AtomicBool::new(true),
            control_epoch: AtomicU64::new(0),
            leases: AtomicUsize::new(0),
            packets_in: AtomicU64::new(0),
            packets_out: AtomicU64::new(0),
            display_packets: AtomicU64::new(0),
            decoded_frames: AtomicU64::new(0),
            unchanged_display_packets: AtomicU64::new(0),
            suppressed_startup_display_packets: AtomicU64::new(0),
            display_decode_errors: AtomicU64::new(0),
            last_display_message_type: AtomicU32::new(0),
            last_display_codec: AtomicU32::new(0),
            last_display_flags: AtomicU32::new(0),
            terminal_reason: AtomicU32::new(0),
            bootstrap_complete: AtomicBool::new(false),
            bootstrap: Mutex::new(model::Bootstrap::default()),
            display_running: AtomicBool::new(false),
            video_width: AtomicU32::new(0),
            video_height: AtomicU32::new(0),
            negotiated_width: AtomicU32::new(0),
            negotiated_height: AtomicU32::new(0),
            negotiated_display_format: AtomicU32::new(0),
            last_control_message_type: AtomicU32::new(0),
            generic_control_messages: AtomicU64::new(0),
            apcp_server_marker: 0x8101,
            apcp_protocol_version: 259,
            apcp_transports: 4,
            apcp_redirect_port: 0,
            transport_profile: AvctTransportProfile::Idrac8Apcp,
            reconnect_session: AtomicU32::new(0),
            dvc_startup: Mutex::new(DvcStartup::new(
                AvctTransportProfile::Idrac8Apcp,
                tokio::time::Instant::now(),
            )),
            dvc_mode: Mutex::new(None),
            dvc_mode_generation: AtomicU64::new(0),
            dvc_mode_notify: Notify::new(),
            dvc_switch: Mutex::new(DvcProfileSwitch::default()),
            dvc_switch_notify: Notify::new(),
            dvc_switch_lock: tokio::sync::Mutex::new(()),
            keyboard_led_sync_at: tokio::sync::Mutex::new(None),
            control_tx,
            frame_tx,
            last_frame: Mutex::new(None),
            video_state,
        };
        observe_control_packet(
            &session,
            &AvctPacket::beef(0x8206, Bytes::from_static(&[4, 0, 3, 0, b'P', b'3', 0, 0])),
        );
        assert_eq!(session.negotiated_width.load(Ordering::Relaxed), 1024);
        assert_eq!(session.negotiated_height.load(Ordering::Relaxed), 768);
        assert_eq!(
            session.negotiated_display_format.load(Ordering::Relaxed),
            0x5033
        );

        // The reconnect value is intentionally at payload offset four.  The
        // first four payload bytes are not a substitute session identifier.
        observe_control_packet(
            &session,
            &AvctPacket::beef(
                0x8102,
                Bytes::from_static(&[0xaa, 0xbb, 0xcc, 0xdd, 1, 2, 3, 4]),
            ),
        );
        assert_eq!(
            session.reconnect_session.load(Ordering::Relaxed),
            0x0102_0304
        );

        observe_control_packet(
            &session,
            &AvctPacket {
                magic: [0, 0, 0, 0],
                message_type: 0x9e10,
                payload: Bytes::new(),
            },
        );
        assert_eq!(session.generic_control_messages.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn display_bootstrap_is_triggered_by_the_official_video_ready_packet() {
        assert!(is_display_bootstrap_trigger(&AvctPacket {
            magic: [0; 4],
            message_type: 0x0084,
            payload: Bytes::from_static(&[0]),
        }));
        assert!(!is_display_bootstrap_trigger(&AvctPacket {
            magic: [0; 4],
            message_type: 0x0084,
            payload: Bytes::from_static(&[1]),
        }));
        assert!(!is_display_bootstrap_trigger(&AvctPacket {
            magic: [0; 4],
            message_type: 0x0084,
            payload: Bytes::new(),
        }));
        assert!(!is_display_bootstrap_trigger(&AvctPacket::beef(
            0x8203,
            Bytes::new(),
        )));
    }

    #[test]
    fn clamps_pointer_coordinates_to_known_or_wire_limits() {
        assert_eq!(clamp_coordinate(-1, 1024), 0);
        assert_eq!(clamp_coordinate(2048, 1024), 1023);
        assert_eq!(clamp_coordinate(i16::MAX, 0), i16::MAX as u16);
    }

    #[test]
    fn native_reconnect_never_retries_protocol_or_decode_terminal_reasons() {
        assert!(native_reconnect_eligible(
            false,
            1,
            AvctTerminalReason::SocketOrControlQueueClosed
        ));
        assert!(!native_reconnect_eligible(
            false,
            0,
            AvctTerminalReason::SocketOrControlQueueClosed
        ));
        assert!(!native_reconnect_eligible(
            true,
            1,
            AvctTerminalReason::SocketOrControlQueueClosed
        ));
        assert!(!native_reconnect_eligible(
            false,
            1,
            AvctTerminalReason::DisplayPacketRejected
        ));
        assert!(!native_reconnect_eligible(
            false,
            1,
            AvctTerminalReason::UnsupportedVideoPacket
        ));
        assert!(!native_reconnect_eligible(
            false,
            1,
            AvctTerminalReason::Idrac6Dvc15NegotiationFailed
        ));
    }
}
