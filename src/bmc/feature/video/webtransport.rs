use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use wtransport::endpoint::IncomingSession;
use wtransport::tls::Sha256DigestFmt;
use wtransport::{Endpoint, Identity, ServerConfig, VarInt};

use crate::bmc::encoder::registry::{probe_webtransport_codec_for_backend, EncoderRegistry};
use crate::bmc::encoder::Codec;
use crate::bmc::feature::connection::{FeatureConnection, FeatureGuard};
use crate::bmc::feature::fault::{fault_response, FeatureFault, FeatureFaultDomain};
use crate::bmc::feature::registered_backends;
use crate::bmc::feature::video::encoding::normalize_stream_backend;
use crate::bmc::feature::video::processor::ActiveWebTransportSession;
use crate::bmc::feature::video::replay_initial_base_frame;
use crate::bmc::feature::video::runtime;
use crate::bmc::stream::encoder::{create_stream_encoder, VideoStreamEncoder};
use crate::bmc::stream::pipeline::{RawFrame, RawPixelFormat};
use crate::config::WebTransportConfig;
use crate::web::state::AppState;

const DEFAULT_BITRATE_KBPS: u32 = 5000;
const DEFAULT_FRAMERATE: u32 = 30;
const CONNECT_TICKET_TTL: Duration = Duration::from_secs(30);
const MAX_PENDING_TICKETS: usize = 1024;
const MAX_PENDING_TICKETS_PER_DEVICE: usize = 64;

#[derive(Clone, Debug)]
struct WebTransportTicket {
    // IDs describe the ticket; only this original connection may consume it.
    connection: Weak<FeatureConnection>,
    bmc_id: String,
    connection_id: String,
    codec: String,
    encoder: String,
    expires_at: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TicketError {
    ConnectionStopped,
    Capacity,
}

#[derive(Clone, Debug)]
pub struct WebTransportRuntimeState {
    pub enabled: bool,
    pub listen: String,
    pub public_url: Option<String>,
    pub cert_hash: Option<Vec<u8>>,
    pub local_addr: Option<SocketAddr>,
    pub error: Option<String>,
    tickets: Arc<tokio::sync::Mutex<HashMap<String, WebTransportTicket>>>,
}

async fn publish_runtime_error(state: &Arc<AppState>, cfg: &WebTransportConfig, error: String) {
    *state.webtransport.write().await = Some(WebTransportRuntimeState {
        enabled: false,
        listen: cfg.listen.clone(),
        public_url: cfg.public_url.clone(),
        cert_hash: None,
        local_addr: None,
        error: Some(error),
        tickets: WebTransportRuntimeState::new_tickets(),
    });
}

impl WebTransportRuntimeState {
    fn new_tickets() -> Arc<tokio::sync::Mutex<HashMap<String, WebTransportTicket>>> {
        Arc::new(tokio::sync::Mutex::new(HashMap::new()))
    }

    async fn issue_ticket(
        &self,
        connection: &Arc<FeatureConnection>,
        codec: &str,
        encoder: &str,
    ) -> Result<String, TicketError> {
        let mut tickets = self.tickets.lock().await;
        let now = Instant::now();
        tickets.retain(|_, value| {
            value.expires_at > now
                && value
                    .connection
                    .upgrade()
                    .is_some_and(|connection| !connection.is_stopped())
        });
        if connection.is_stopped() {
            return Err(TicketError::ConnectionStopped);
        }
        if tickets.len() >= MAX_PENDING_TICKETS
            || tickets
                .values()
                .filter(|ticket| ticket.bmc_id == connection.device_id)
                .count()
                >= MAX_PENDING_TICKETS_PER_DEVICE
        {
            return Err(TicketError::Capacity);
        }
        let ticket = uuid::Uuid::new_v4().to_string();
        tickets.insert(
            ticket.clone(),
            WebTransportTicket {
                connection: Arc::downgrade(connection),
                bmc_id: connection.device_id.clone(),
                connection_id: connection.instance_id.clone(),
                codec: codec.to_string(),
                encoder: encoder.to_string(),
                expires_at: now + CONNECT_TICKET_TTL,
            },
        );
        Ok(ticket)
    }

    async fn consume_ticket(&self, value: &str) -> Option<WebTransportTicket> {
        let mut tickets = self.tickets.lock().await;
        let ticket = tickets.remove(value)?;
        (ticket.expires_at > Instant::now()
            && ticket
                .connection
                .upgrade()
                .is_some_and(|connection| !connection.is_stopped()))
        .then_some(ticket)
    }
}

fn streaming_codec_ids() -> Vec<&'static str> {
    EncoderRegistry::codecs()
        .into_iter()
        .filter_map(|entry| {
            Codec::from_str(entry.id)
                .filter(|codec| codec.supports_webrtc())
                .filter(|codec| {
                    probe_webtransport_codec_for_backend("software", *codec)
                        || probe_webtransport_codec_for_backend("nvenc", *codec)
                })
                .map(|_| entry.id)
        })
        .collect()
}

fn webcodecs_codec_string(codec_id: &str) -> Option<&'static str> {
    match codec_id {
        "h264" => Some("avc1.42E01F"),
        "h265" => Some("hev1.1.6.L93.B0"),
        "vp8" => Some("vp8"),
        "vp9" => Some("vp09.00.10.08"),
        "av1" => Some("av01.0.05M.08"),
        _ => None,
    }
}

fn runtime_url(state: &WebTransportRuntimeState, bmc_id: &str) -> Option<String> {
    let base = state.public_url.clone().or_else(|| {
        state
            .local_addr
            .map(|addr| format!("https://{}:{}/webtransport", "127.0.0.1", addr.port()))
    })?;
    if base.contains('?') {
        Some(format!("{base}&bmc_id={bmc_id}"))
    } else {
        Some(format!("{base}?bmc_id={bmc_id}"))
    }
}

fn capability_value(
    bmc_id: &str,
    configured: bool,
    runtime: Option<WebTransportRuntimeState>,
) -> serde_json::Value {
    let codecs: Vec<serde_json::Value> = streaming_codec_ids()
        .into_iter()
        .filter_map(|id| {
            let entry = EncoderRegistry::find_codec(id)?;
            Some(serde_json::json!({
                "id": entry.id,
                "name": entry.name,
                "webcodecs_codec": webcodecs_codec_string(entry.id),
            }))
        })
        .collect();

    let encoders: Vec<serde_json::Value> = EncoderRegistry::discover()
        .into_iter()
        .filter(|backend| backend.available)
        .filter(|backend| {
            crate::bmc::encoder::codec::codecs_for_backend(backend.id)
                .into_iter()
                .any(|id| webcodecs_codec_string(id).is_some())
        })
        .map(|backend| {
            serde_json::json!({
                "id": backend.id,
                "name": backend.name,
                "is_hardware": backend.is_hardware,
                "available": backend.available,
                "supported_formats": crate::bmc::encoder::codec::codecs_for_backend(backend.id)
                    .into_iter()
                    .filter(|id| webcodecs_codec_string(id).is_some())
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    let enabled = runtime
        .as_ref()
        .is_some_and(|rt| rt.enabled && rt.error.is_none());
    let url = runtime.as_ref().and_then(|rt| runtime_url(rt, bmc_id));
    let cert_hash = runtime.as_ref().and_then(|rt| rt.cert_hash.clone());
    let reason = if !configured {
        Some("webtransport_not_enabled_in_config".to_string())
    } else if runtime.is_none() {
        Some("webtransport_server_not_started".to_string())
    } else if let Some(error) = runtime.as_ref().and_then(|rt| rt.error.clone()) {
        Some(error)
    } else if !enabled {
        Some("webtransport_runtime_disabled".to_string())
    } else if url.is_none() {
        Some("webtransport_url_unavailable".to_string())
    } else {
        None
    };

    serde_json::json!({
        "success": true,
        "transport": "webtransport",
        "decoder": "webcodecs",
        "enabled": enabled,
        "configured": configured,
        "url": url,
        "server_certificate_hashes": cert_hash.map(|hash| vec![serde_json::json!({
            "algorithm": "sha-256",
            "value": hash,
        })]),
        "requires_secure_context": true,
        "codecs": codecs,
        "encoders": encoders,
        "packet_format": {
            "version": 1,
            "header": "u32_be json_len + u32_be payload_len + json_metadata + encoded_payload",
            "metadata": ["connection_id", "codec", "timestamp_us", "duration_us", "keyframe", "width", "height", "trace_id", "pixel_format"]
        },
        "reason": reason,
    })
}

pub async fn capabilities(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    let configured = state.config.read().await.server.webtransport.enabled;
    let runtime = state.webtransport.read().await.clone();
    Json(capability_value(&bmc_id, configured, runtime))
}

#[derive(Debug, Deserialize)]
pub struct ConnectRequest {
    pub connection_id: Option<String>,
    pub codec: Option<String>,
    pub encoder: Option<String>,
}

async fn webtransport_fault_response(
    state: &Arc<AppState>,
    bmc_id: &str,
    status: StatusCode,
    fault: FeatureFault,
) -> Response {
    if let Some(dev) = state.registry.get_dev(bmc_id).await {
        if let Some(fc) = dev.features.get("video") {
            fc.faults.record(fault.clone()).await;
        }
    }
    fault_response(status, fault)
}

pub async fn connect(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(req): Json<ConnectRequest>,
) -> Response {
    let codec = req.codec.as_deref().unwrap_or("h264");
    if webcodecs_codec_string(codec).is_none() {
        return webtransport_fault_response(
            &state,
            &bmc_id,
            StatusCode::BAD_REQUEST,
            FeatureFault::operation(
                FeatureFaultDomain::BrowserTransport,
                "unsupported_webcodecs_codec",
                "The requested WebCodecs codec is unsupported",
                false,
                "webtransport_connect",
            ),
        )
        .await;
    }

    let Some(runtime) = state.webtransport.read().await.clone() else {
        return webtransport_fault_response(
            &state,
            &bmc_id,
            StatusCode::SERVICE_UNAVAILABLE,
            FeatureFault::operation(
                FeatureFaultDomain::BrowserTransport,
                "webtransport_not_started",
                "The WebTransport server is not running",
                true,
                "webtransport_connect",
            ),
        )
        .await;
    };
    if !runtime.enabled || runtime.error.is_some() {
        return webtransport_fault_response(
            &state,
            &bmc_id,
            StatusCode::SERVICE_UNAVAILABLE,
            FeatureFault::operation(
                FeatureFaultDomain::BrowserTransport,
                "webtransport_unavailable",
                "The WebTransport server is unavailable",
                true,
                "webtransport_connect",
            ),
        )
        .await;
    }
    if state.registry.get_dev(&bmc_id).await.is_none() {
        return webtransport_fault_response(
            &state,
            &bmc_id,
            StatusCode::NOT_FOUND,
            FeatureFault::operation(
                FeatureFaultDomain::Lifecycle,
                "device_not_found",
                "The requested BMC device was not found",
                false,
                "webtransport_connect",
            ),
        )
        .await;
    }

    let connection_id = req
        .connection_id
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    if !valid_connection_id(&connection_id) {
        return webtransport_fault_response(
            &state,
            &bmc_id,
            StatusCode::BAD_REQUEST,
            FeatureFault::operation(
                FeatureFaultDomain::BrowserTransport,
                "invalid_connection_id",
                "The connection ID contains unsupported characters or is too long",
                false,
                "webtransport_connect",
            ),
        )
        .await;
    }
    let encoder = req.encoder.unwrap_or_else(|| "software".to_string());
    if !valid_connection_id(&encoder) {
        return webtransport_fault_response(
            &state,
            &bmc_id,
            StatusCode::BAD_REQUEST,
            FeatureFault::operation(
                FeatureFaultDomain::BrowserTransport,
                "invalid_encoder_backend",
                "The encoder backend identifier is invalid",
                false,
                "webtransport_connect",
            ),
        )
        .await;
    }
    let normalized_encoder = normalize_stream_backend(&encoder).to_string();
    let Some(codec_value) = Codec::from_str(codec) else {
        return webtransport_fault_response(
            &state,
            &bmc_id,
            StatusCode::BAD_REQUEST,
            FeatureFault::operation(
                FeatureFaultDomain::BrowserTransport,
                "unsupported_webtransport_codec",
                "The requested WebTransport codec is unavailable",
                false,
                "webtransport_connect",
            ),
        )
        .await;
    };
    if !probe_webtransport_codec_for_backend(&normalized_encoder, codec_value) {
        return webtransport_fault_response(
            &state,
            &bmc_id,
            StatusCode::BAD_REQUEST,
            FeatureFault::operation(
                FeatureFaultDomain::BrowserTransport,
                "encoder_codec_unavailable",
                "The selected encoder cannot produce the requested WebTransport codec",
                false,
                "webtransport_connect",
            ),
        )
        .await;
    }
    let mut url = match runtime_url(&runtime, &bmc_id) {
        Some(url) => url,
        None => {
            return webtransport_fault_response(
                &state,
                &bmc_id,
                StatusCode::SERVICE_UNAVAILABLE,
                FeatureFault::operation(
                    FeatureFaultDomain::BrowserTransport,
                    "webtransport_url_unavailable",
                    "The WebTransport endpoint URL is unavailable",
                    true,
                    "webtransport_connect",
                ),
            )
            .await
        }
    };
    let fc = match state
        .registry
        .get_or_create_feature_instance(&bmc_id, "video", &connection_id, registered_backends())
        .await
    {
        Ok(connection) => connection,
        Err(error) => {
            return webtransport_fault_response(
                &state,
                &bmc_id,
                if error.contains("instance limit") {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                FeatureFault::operation(
                    FeatureFaultDomain::BrowserTransport,
                    "webtransport_connection_unavailable",
                    "The requested video connection is unavailable",
                    true,
                    "webtransport_connect",
                ),
            )
            .await
        }
    };
    let ticket = match runtime.issue_ticket(&fc, codec, &normalized_encoder).await {
        Ok(ticket) => ticket,
        Err(error) => {
            return webtransport_fault_response(
                &state,
                &bmc_id,
                if error == TicketError::Capacity {
                    StatusCode::TOO_MANY_REQUESTS
                } else {
                    StatusCode::SERVICE_UNAVAILABLE
                },
                FeatureFault::operation(
                    FeatureFaultDomain::BrowserTransport,
                    if error == TicketError::Capacity {
                        "webtransport_ticket_limit_reached"
                    } else {
                        "webtransport_connection_unavailable"
                    },
                    "A WebTransport connection ticket is currently unavailable",
                    true,
                    "webtransport_connect",
                ),
            )
            .await
        }
    };
    url.push_str(&format!("&ticket={}", urlencoding::encode(&ticket),));

    Json(serde_json::json!({
        "success": true,
        "transport": "webtransport",
        "decoder": "webcodecs",
        "connection_id": connection_id,
        "codec": codec,
        "encoder": normalized_encoder,
        "ticket_expires_in_seconds": CONNECT_TICKET_TTL.as_secs(),
        "url": url,
        "server_certificate_hashes": runtime.cert_hash.map(|hash| vec![serde_json::json!({
            "algorithm": "sha-256",
            "value": hash,
        })]),
    }))
    .into_response()
}

pub async fn serve(state: Arc<AppState>) -> Result<(), String> {
    let cfg = state.config.read().await.server.webtransport.clone();
    if !cfg.enabled {
        *state.webtransport.write().await = Some(WebTransportRuntimeState {
            enabled: false,
            listen: cfg.listen,
            public_url: cfg.public_url,
            cert_hash: None,
            local_addr: None,
            error: Some("webtransport_not_enabled".to_string()),
            tickets: WebTransportRuntimeState::new_tickets(),
        });
        return Ok(());
    }

    let listen_addr = match cfg.listen.parse::<SocketAddr>() {
        Ok(address) => address,
        Err(error) => {
            let message = format!("invalid webtransport listen '{}': {error}", cfg.listen);
            publish_runtime_error(&state, &cfg, message.clone()).await;
            return Err(message);
        }
    };
    let remote_listener = !listen_addr.ip().is_loopback();
    match (&cfg.cert_path, &cfg.key_path) {
        (Some(_), Some(_)) | (None, None) => {}
        _ => {
            let message = "webtransport cert_path and key_path must be configured together";
            publish_runtime_error(&state, &cfg, message.to_string()).await;
            return Err(message.into());
        }
    }
    if remote_listener && (cfg.cert_path.is_none() || cfg.public_url.is_none()) {
        let message = "remote WebTransport requires explicit cert_path, key_path, and public_url";
        publish_runtime_error(&state, &cfg, message.to_string()).await;
        return Err(message.into());
    }
    let identity = match load_identity(&cfg).await {
        Ok(identity) => identity,
        Err(error) => {
            publish_runtime_error(&state, &cfg, error.clone()).await;
            return Err(error);
        }
    };
    let cert_hash = identity
        .certificate_chain()
        .as_slice()
        .first()
        .map(|cert| cert.hash().fmt(Sha256DigestFmt::BytesArray))
        .and_then(|s| serde_json::from_str::<Vec<u8>>(&s).ok());
    let server_config = ServerConfig::builder()
        .with_bind_address(listen_addr)
        .with_identity(identity)
        .keep_alive_interval(Some(Duration::from_secs(3)))
        .build();
    let endpoint = match Endpoint::server(server_config) {
        Ok(endpoint) => endpoint,
        Err(error) => {
            let message = format!("endpoint: {error}");
            publish_runtime_error(&state, &cfg, message.clone()).await;
            return Err(message);
        }
    };
    let local_addr = endpoint.local_addr().ok();
    *state.webtransport.write().await = Some(WebTransportRuntimeState {
        enabled: true,
        listen: cfg.listen.clone(),
        public_url: cfg.public_url.clone(),
        cert_hash,
        local_addr,
        error: None,
        tickets: WebTransportRuntimeState::new_tickets(),
    });

    info!("webtransport listening on {:?}", local_addr);
    loop {
        let incoming_session = endpoint.accept().await;
        let state = state.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_incoming_session(state, incoming_session).await {
                warn!("webtransport connection ended: {e}");
            }
        });
    }
}

async fn load_identity(cfg: &WebTransportConfig) -> Result<Identity, String> {
    match (&cfg.cert_path, &cfg.key_path) {
        (Some(cert), Some(key)) => Identity::load_pemfiles(cert, key)
            .await
            .map_err(|e| format!("load webtransport cert/key: {e}")),
        _ => Identity::self_signed(["localhost", "127.0.0.1", "::1"])
            .map_err(|e| format!("self-signed identity: {e}")),
    }
}

async fn handle_incoming_session(
    state: Arc<AppState>,
    incoming_session: IncomingSession,
) -> Result<(), String> {
    let session_request = tokio::time::timeout(Duration::from_secs(15), incoming_session)
        .await
        .map_err(|_| "incoming session timed out".to_string())?
        .map_err(|e| format!("incoming session: {e}"))?;
    let query = parse_query(session_request.path());
    let ticket_value = query
        .get("ticket")
        .ok_or_else(|| "missing webtransport ticket".to_string())?;
    let runtime = state
        .webtransport
        .read()
        .await
        .clone()
        .ok_or_else(|| "webtransport runtime is unavailable".to_string())?;
    let ticket = runtime
        .consume_ticket(ticket_value)
        .await
        .ok_or_else(|| "invalid or expired webtransport ticket".to_string())?;
    let fc = ticket
        .connection
        .upgrade()
        .filter(|connection| !connection.is_stopped())
        .ok_or_else(|| "webtransport ticket connection is no longer available".to_string())?;
    let bmc_id = ticket.bmc_id;
    let connection_id = ticket.connection_id;
    let codec_id = ticket.codec;
    let encoder_backend = normalize_stream_backend(&ticket.encoder).to_string();
    let codec =
        Codec::from_str(&codec_id).ok_or_else(|| format!("unsupported codec {codec_id}"))?;
    if !codec.supports_webrtc() {
        return Err(format!("codec {codec_id} is not a streaming codec"));
    }
    if !probe_webtransport_codec_for_backend(&encoder_backend, codec) {
        return Err(format!(
            "codec {} is not available for WebTransport with backend {} in this GStreamer runtime",
            codec.as_str(),
            encoder_backend
        ));
    }

    let connection = tokio::time::timeout(Duration::from_secs(15), session_request.accept())
        .await
        .map_err(|_| "accept webtransport timed out".to_string())?
        .map_err(|e| format!("accept webtransport: {e}"))?;
    let _connection_owner = WebTransportConnectionOwner(connection.clone());
    let mut stream = tokio::time::timeout(Duration::from_secs(15), async {
        connection
            .open_uni()
            .await
            .map_err(|e| format!("open uni: {e}"))?
            .await
            .map_err(|e| format!("finish open uni: {e}"))
    })
    .await
    .map_err(|_| "open uni timed out".to_string())??;

    info!(
        "[{bmc_id}:{connection_id}] webtransport accepted codec={} encoder={}",
        codec.as_str(),
        encoder_backend
    );

    let fault_connection = fc.clone();
    let setup_work = async {
        let guard =
            crate::bmc::feature::acquire_connected_feature(fc.clone(), Duration::from_secs(30))
                .await?;
        let processor = runtime::processor(&fc).await?;
        let rx = processor.frame_tx().subscribe();
        replay_initial_base_frame(
            &fc,
            &processor,
            &format!("[{bmc_id}:{connection_id}] webtransport"),
        )
        .await;
        let (width, height, pixel_format) = resolve_initial_frame_metadata(&processor).await;
        let backend = encoder_backend.clone();
        let encoder = crate::bmc::cpu::run(move || {
            create_stream_encoder(
                codec,
                &backend,
                width,
                height,
                DEFAULT_BITRATE_KBPS,
                DEFAULT_FRAMERATE,
            )
        })
        .await??;
        Ok((
            fc,
            guard,
            processor,
            rx,
            encoder,
            width,
            height,
            pixel_format,
        ))
    };
    let setup: Result<_, String> = tokio::select! {
        _ = connection.closed() => return Ok(()),
        result = tokio::time::timeout(Duration::from_secs(45), setup_work) =>
            result.unwrap_or_else(|_| Err("WebTransport setup timed out".into())),
    };

    let (fc, guard, processor, mut rx, encoder, width, height, pixel_format) = match setup {
        Ok(result) => result,
        Err(err) => {
            // QUIC close reasons are browser-visible. Keep the detailed setup
            // error in server logs only and close with a stable public code.
            fault_connection
                .faults
                .record(FeatureFault::child(
                    FeatureFaultDomain::BrowserTransport,
                    "webtransport_setup_failed",
                    "WebTransport video setup failed",
                    true,
                    "webtransport",
                    connection_id.clone(),
                ))
                .await;
            connection.close(VarInt::from_u32(1), b"webtransport_setup_failed");
            return Err(err);
        }
    };

    encoder.request_keyframe();
    let trace_session = Arc::new(ActiveWebTransportSession::new(
        codec.as_str().to_string(),
        encoder_backend.clone(),
        width,
        height,
        pixel_format,
    ));
    let _registration = processor
        .register_webtransport_connection(connection_id.clone(), trace_session.clone())
        .await?;
    info!(
        "[{bmc_id}:{connection_id}] webtransport connected codec={} encoder={} {}x{} {:?}",
        codec.as_str(),
        encoder_backend,
        width,
        height,
        pixel_format
    );

    let result = stream_frames(
        encoder,
        &connection,
        &mut rx,
        &mut stream,
        codec,
        connection_id.clone(),
        trace_session,
        guard,
        fc.stopped_rx(),
    )
    .await;
    if result.is_err() {
        processor
            .record_fault(FeatureFault::child(
                FeatureFaultDomain::BrowserTransport,
                "webtransport_stream_failed",
                "WebTransport video delivery failed",
                true,
                "webtransport",
                connection_id.clone(),
            ))
            .await;
    }
    result
}

async fn resolve_initial_frame_metadata(
    processor: &crate::bmc::feature::video::processor::VideoProcessor,
) -> (u32, u32, RawPixelFormat) {
    if let Some((w, h, fmt)) = processor.first_frame_metadata().await {
        if w > 0 && h > 0 {
            return (w, h, fmt);
        }
    }
    let mut rx = processor.frame_tx().subscribe();
    if let Ok(Ok(frame)) = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await {
        return (frame.width, frame.height, frame.pixel_format);
    }
    (1920, 1080, RawPixelFormat::Rgba)
}

#[allow(clippy::too_many_arguments)] // Keeps stream ownership and cancellation inputs explicit.
async fn stream_frames(
    mut encoder: Box<dyn VideoStreamEncoder>,
    connection: &wtransport::Connection,
    rx: &mut broadcast::Receiver<RawFrame>,
    stream: &mut wtransport::stream::SendStream,
    codec: Codec,
    connection_id: String,
    trace_session: Arc<ActiveWebTransportSession>,
    _guard: FeatureGuard,
    mut stopped_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<(), String> {
    loop {
        let Some(received) =
            while_connected(rx.recv(), connection.closed(), &mut stopped_rx).await?
        else {
            return Ok(());
        };
        let frame = match received {
            Ok(frame) => frame,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                debug!("webtransport lagged {n} raw frames");
                trace_session.record_lagged(n);
                encoder.request_keyframe();
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => {
                return Err("raw frame channel closed".into())
            }
        };
        trace_session.record_raw_frame(&frame);
        let encode_start = std::time::Instant::now();
        let encode_frame = frame.clone();
        let work = crate::bmc::cpu::run(move || {
            let result = encode_frame.to_rgba().and_then(|rgba| {
                encoder.encode_packets(&rgba, encode_frame.width, encode_frame.height)
            });
            (encoder, result)
        });
        let Some(encoded) = while_connected(work, connection.closed(), &mut stopped_rx).await?
        else {
            return Ok(());
        };
        let (returned_encoder, result) = encoded?;
        encoder = returned_encoder;
        let packets = result.inspect_err(|_| {
            trace_session.record_encode_error();
        })?;
        trace_session.record_encode_time(encode_start.elapsed());
        for packet in packets {
            let keyframe = packet.keyframe;
            let metadata = serde_json::json!({
                "connection_id": connection_id,
                "codec": codec.as_str(),
                "timestamp_us": packet.timestamp_us,
                "duration_us": packet.duration_us.unwrap_or(1_000_000 / DEFAULT_FRAMERATE as u64),
                "keyframe": keyframe,
                "width": frame.width,
                "height": frame.height,
                "trace_id": frame.trace_id,
                "pixel_format": frame.pixel_format.gst_caps_format(),
            });
            let payload = encode_message(&metadata, &packet.data)?;
            let Some(written) = while_connected(
                tokio::time::timeout(Duration::from_secs(10), stream.write_all(&payload)),
                connection.closed(),
                &mut stopped_rx,
            )
            .await?
            else {
                return Ok(());
            };
            written
                .map_err(|_| "write frame timed out".to_string())
                .and_then(|result| result.map_err(|error| format!("write frame: {error}")))
                .inspect_err(|_| trace_session.record_write_error())?;
            trace_session.record_packet(packet.data.len(), keyframe);
        }
    }
}

// Apply the same termination policy to idle receives, CPU work, and backpressured writes.
async fn while_connected<T, U>(
    operation: impl std::future::Future<Output = T>,
    closed: impl std::future::Future<Output = U>,
    stopped: &mut tokio::sync::watch::Receiver<bool>,
) -> Result<Option<T>, String> {
    if *stopped.borrow() {
        return Err("feature reconfigured".into());
    }
    tokio::select! {
        biased;
        _ = stopped.changed() => Err("feature reconfigured".into()),
        _ = closed => Ok(None),
        result = operation => Ok(Some(result)),
    }
}

fn encode_message(metadata: &serde_json::Value, payload: &[u8]) -> Result<Vec<u8>, String> {
    let metadata = serde_json::to_vec(metadata).map_err(|e| format!("metadata: {e}"))?;
    let len = u32::try_from(metadata.len()).map_err(|_| "metadata too large".to_string())?;
    let payload_len = u32::try_from(payload.len()).map_err(|_| "payload too large".to_string())?;
    let mut out = Vec::with_capacity(8 + metadata.len() + payload.len());
    out.extend_from_slice(&len.to_be_bytes());
    out.extend_from_slice(&payload_len.to_be_bytes());
    out.extend_from_slice(&metadata);
    out.extend_from_slice(payload);
    Ok(out)
}

fn parse_query(path: &str) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let Some((_, query)) = path.split_once('?') else {
        return out;
    };
    for pair in query.split('&') {
        let Some((key, value)) = pair.split_once('=') else {
            continue;
        };
        let key = urlencoding::decode(key).unwrap_or_else(|_| key.into());
        let value = urlencoding::decode(value).unwrap_or_else(|_| value.into());
        out.insert(key.into_owned(), value.into_owned());
    }
    out
}

fn valid_connection_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 96
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
}

// Explicitly close on setup failure, task cancellation, or normal completion.
struct WebTransportConnectionOwner(wtransport::Connection);

impl Drop for WebTransportConnectionOwner {
    fn drop(&mut self) {
        self.0.close(VarInt::from_u32(0), b"session_closed");
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn peer_close_interrupts_idle_or_blocked_operations() {
        let (_stop, mut stopped) = tokio::sync::watch::channel(false);
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            while_connected(
                std::future::pending::<()>(),
                std::future::ready(()),
                &mut stopped,
            ),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn feature_stop_interrupts_operations_including_already_stopped() {
        let (stop, mut stopped) = tokio::sync::watch::channel(false);
        let work = while_connected(
            std::future::pending::<()>(),
            std::future::pending::<()>(),
            &mut stopped,
        );
        let (result, ()) = tokio::join!(work, async {
            stop.send(true).unwrap();
        });
        assert!(result.is_err());
        assert!(while_connected(
            std::future::ready(()),
            std::future::pending::<()>(),
            &mut stopped
        )
        .await
        .is_err());
    }

    use super::*;

    fn ticket_runtime() -> WebTransportRuntimeState {
        WebTransportRuntimeState {
            enabled: true,
            listen: "127.0.0.1:0".into(),
            public_url: None,
            cert_hash: None,
            local_addr: Some("127.0.0.1:4443".parse().unwrap()),
            error: None,
            tickets: WebTransportRuntimeState::new_tickets(),
        }
    }

    fn ticket_device() -> Arc<crate::bmc::feature::managed::ManagedDevice> {
        use crate::bmc::feature::{
            config::{FeatureConfig, FeatureConfigs},
            connection::FeatureConnection,
        };
        let config = FeatureConfig {
            backend: "fixture".into(),
            backend_params: Default::default(),
            feature_params: Default::default(),
            providers: vec![],
        };
        let connection = FeatureConnection::new(
            "device".into(),
            "video".into(),
            "fixture".into(),
            config.clone(),
        );
        Arc::new(crate::bmc::feature::managed::ManagedDevice::new(
            "device".into(),
            FeatureConfigs {
                features: HashMap::from([("video".into(), config)]),
            },
            HashMap::from([("video".into(), connection)]),
        ))
    }

    fn ticket_connection(device_id: &str, instance_id: &str) -> Arc<FeatureConnection> {
        FeatureConnection::new_instance(
            device_id.into(),
            "video".into(),
            instance_id.into(),
            "fixture".into(),
            crate::bmc::feature::config::FeatureConfig {
                backend: "fixture".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        )
    }

    #[tokio::test]
    async fn webtransport_pending_tickets_cannot_grow_without_a_limit() {
        let runtime = ticket_runtime();
        let mut owners = Vec::new();
        for index in 0..MAX_PENDING_TICKETS {
            let connection = ticket_connection(
                &format!("device-{}", index / MAX_PENDING_TICKETS_PER_DEVICE),
                &format!("connection-{index}"),
            );
            runtime
                .issue_ticket(&connection, "h264", "software")
                .await
                .unwrap();
            owners.push(connection);
        }
        let extra = ticket_connection("another-device", "extra");
        assert_eq!(
            runtime.issue_ticket(&extra, "h264", "software").await,
            Err(TicketError::Capacity)
        );
        assert_eq!(runtime.tickets.lock().await.len(), MAX_PENDING_TICKETS);
        let ticket = runtime.tickets.lock().await.keys().next().unwrap().clone();
        assert!(runtime.consume_ticket(&ticket).await.is_some());
        assert!(runtime
            .issue_ticket(&extra, "h264", "software")
            .await
            .is_ok());
        drop(owners);
    }

    #[tokio::test]
    async fn webtransport_device_quota_reclaims_expired_stopped_and_dropped_connections() {
        let runtime = ticket_runtime();
        let connection = ticket_connection("device", "browser");
        for _ in 0..MAX_PENDING_TICKETS_PER_DEVICE {
            runtime
                .issue_ticket(&connection, "h264", "software")
                .await
                .unwrap();
        }
        assert_eq!(
            runtime.issue_ticket(&connection, "h264", "software").await,
            Err(TicketError::Capacity)
        );
        let other = ticket_connection("other-device", "browser");
        assert!(runtime
            .issue_ticket(&other, "h264", "software")
            .await
            .is_ok());
        for ticket in runtime
            .tickets
            .lock()
            .await
            .values_mut()
            .filter(|ticket| ticket.bmc_id == "device")
        {
            ticket.expires_at = Instant::now() - Duration::from_secs(1);
        }
        let expired_replacement = runtime
            .issue_ticket(&connection, "h264", "software")
            .await
            .unwrap();
        connection.shutdown();
        assert!(runtime.consume_ticket(&expired_replacement).await.is_none());
        assert_eq!(
            runtime.issue_ticket(&connection, "h264", "software").await,
            Err(TicketError::ConnectionStopped)
        );
        let new = ticket_connection("device", "browser");
        let dropped = runtime
            .issue_ticket(&new, "h264", "software")
            .await
            .unwrap();
        drop(new);
        assert!(runtime.consume_ticket(&dropped).await.is_none());
    }

    #[tokio::test]
    async fn webtransport_ticket_cannot_outlive_its_original_device_connection() {
        let runtime = ticket_runtime();
        let device = ticket_device();
        let registry = crate::bmc::feature::device_registry::DeviceRegistry::new();
        registry.insert("device".into(), device.clone()).await;
        let config: crate::config::Config = serde_json::from_value(serde_json::json!({
            "server":{"listen":"127.0.0.1:0","frontend_dir":"frontend/dist"}, "devices":[], "msd":{}
        }))
        .unwrap();
        let config = Arc::new(tokio::sync::RwLock::new(config));
        let app = Arc::new(AppState {
            registry: registry.clone(),
            config: config.clone(),
            config_path: String::new(),
            configuration: crate::bmc::configuration::ConfigurationCoordinator::new(),
            msd_storage: crate::bmc::msd_storage::FsMsdStorage::new(
                ".local/quality/ticket-fixture".into(),
                config,
            ),
            active_downloads: tokio::sync::RwLock::new(HashMap::new()),
            webtransport: tokio::sync::RwLock::new(Some(runtime.clone())),
        });
        let response = connect(
            Path("device".into()),
            State(app.clone()),
            Json(ConnectRequest {
                connection_id: Some("browser".into()),
                codec: Some("h264".into()),
                encoder: Some("software".into()),
            }),
        )
        .await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 8192)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let url = reqwest::Url::parse(body["url"].as_str().unwrap()).unwrap();
        let ticket = url
            .query_pairs()
            .find(|(name, _)| name == "ticket")
            .unwrap()
            .1
            .into_owned();
        let original = device.get_instance("video", "browser").await.unwrap();
        for _ in 1..MAX_PENDING_TICKETS_PER_DEVICE {
            runtime
                .issue_ticket(&original, "h264", "software")
                .await
                .unwrap();
        }
        let full = connect(
            Path("device".into()),
            State(app),
            Json(ConnectRequest {
                connection_id: Some("browser".into()),
                codec: Some("h264".into()),
                encoder: Some("software".into()),
            }),
        )
        .await;
        assert_eq!(full.status(), StatusCode::TOO_MANY_REQUESTS);
        let full = axum::body::to_bytes(full.into_body(), 8192).await.unwrap();
        let full: serde_json::Value = serde_json::from_slice(&full).unwrap();
        assert_eq!(full["fault"]["code"], "webtransport_ticket_limit_reached");
        device.shutdown().await;
        registry.insert("device".into(), ticket_device()).await;
        assert!(
            runtime.consume_ticket(&ticket).await.is_none(),
            "old ticket survived replacement by a different device connection"
        );
    }

    #[test]
    fn encodes_length_prefixed_message() {
        let msg = encode_message(&serde_json::json!({"a":1}), b"data").unwrap();
        let len = u32::from_be_bytes(msg[0..4].try_into().unwrap()) as usize;
        let payload_len = u32::from_be_bytes(msg[4..8].try_into().unwrap()) as usize;
        assert_eq!(payload_len, 4);
        assert_eq!(&msg[8 + len..], b"data");
    }

    #[tokio::test]
    async fn webtransport_tickets_are_single_use_and_device_bound() {
        let state = WebTransportRuntimeState {
            enabled: true,
            listen: "127.0.0.1:0".to_string(),
            public_url: None,
            cert_hash: None,
            local_addr: None,
            error: None,
            tickets: WebTransportRuntimeState::new_tickets(),
        };
        let connection = ticket_connection("device-a", "connection-a");
        let ticket = state
            .issue_ticket(&connection, "h264", "software")
            .await
            .unwrap();
        connection.clear_backend_runtime().await;
        let consumed = state.consume_ticket(&ticket).await.unwrap();
        assert_eq!(consumed.bmc_id, "device-a");
        assert_eq!(consumed.connection_id, "connection-a");
        assert!(Arc::ptr_eq(
            &connection,
            &consumed.connection.upgrade().unwrap()
        ));
        assert!(state.consume_ticket(&ticket).await.is_none());
    }
}
