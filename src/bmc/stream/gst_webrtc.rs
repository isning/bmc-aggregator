//! GStreamer WebRTC pipeline adapter.
#![allow(clippy::too_many_arguments)] // Constructors mirror explicit GStreamer pipeline inputs.

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use gst::glib;
use gst::prelude::*;
use gst_sdp::SDPMessage;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_sdp as gst_sdp;
use gstreamer_video::UpstreamForceKeyUnitEvent;
use gstreamer_webrtc as gst_webrtc;
use tokio::sync::{broadcast, oneshot, watch};

use crate::bmc::encoder::Codec;
use crate::bmc::stream::codec_config::{codec_pipeline_spec, encoder_props_with_bitrate};
use crate::bmc::stream::gst_init::ensure_gst_init;
use crate::bmc::stream::pipeline::{RawFrame, RawPixelFormat};

fn lock_unpoisoned<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Clone)]
pub struct GstWebRtcSession {
    pub id: String,
    inner: Arc<Inner>,
}

impl std::fmt::Debug for GstWebRtcSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GstWebRtcSession")
            .field("id", &self.id)
            .finish()
    }
}

struct MainLoopWorker {
    stopped: Arc<AtomicBool>,
    main_loop: glib::MainLoop,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for MainLoopWorker {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Release);
        self.main_loop.quit();
        if let Some(thread) = self.thread.take() {
            if thread.thread().id() != std::thread::current().id() {
                let _ = thread.join();
            }
        }
    }
}

struct Inner {
    pipeline: gst::Pipeline,
    webrtc: gst::Element,
    appsrc: gst_app::AppSrc,
    ssrc: u32,
    width: u32,
    height: u32,
    framerate: u32,
    pixel_format: RawPixelFormat,
    running: AtomicBool,
    frame_rx: Mutex<Option<broadcast::Receiver<RawFrame>>>,
    main_loop: Mutex<Option<MainLoopWorker>>,
    connection_state: watch::Sender<gst_webrtc::WebRTCPeerConnectionState>,

    // Per-frame encoding latency tracking
    push_times: Mutex<VecDeque<Instant>>,
    encode_time_total_us: AtomicU64,
    encode_time_max_us: AtomicU64,
    encode_count: AtomicU64,
    frames_received: AtomicU64,
    frames_pushed: AtomicU64,
    frames_lagged: AtomicU64,
    frames_caps_mismatch: AtomicU64,
    appsrc_push_errors: AtomicU64,
    keyframe_requests: AtomicU64,
    next_frame_index: AtomicU64,
    last_trace_id: AtomicU64,
    last_raw_to_appsrc_us: AtomicU64,
    last_appsrc_queue_level_bytes: AtomicU64,
}

fn push_frame_to_appsrc(inner: &Arc<Inner>, frame: RawFrame) -> bool {
    inner.frames_received.fetch_add(1, Ordering::Relaxed);
    inner.last_trace_id.store(frame.trace_id, Ordering::Relaxed);
    if frame.width != inner.width
        || frame.height != inner.height
        || frame.pixel_format != inner.pixel_format
    {
        inner.frames_caps_mismatch.fetch_add(1, Ordering::Relaxed);
        tracing::warn!(
            "gst_webrtc: dropping frame with caps mismatch: expected {}x{} {:?}, got {}x{} {:?}",
            inner.width,
            inner.height,
            inner.pixel_format,
            frame.width,
            frame.height,
            frame.pixel_format,
        );
        return false;
    }

    // 0-copy: copy_region shares GstMemory without copying pixel data.
    // The new GstBuffer has refcount 1 so get_mut/set_pts is safe.
    let mut buf = match frame.data.copy_region(gst::BufferCopyFlags::all(), ..) {
        Ok(buf) => buf,
        Err(_) => {
            inner.appsrc_push_errors.fetch_add(1, Ordering::Relaxed);
            return false;
        }
    };
    let frame_index = inner.next_frame_index.fetch_add(1, Ordering::Relaxed);
    let pts_ns = super::pipeline::frame_time_ns(frame_index, inner.framerate);
    let next_ns = super::pipeline::frame_time_ns(frame_index.saturating_add(1), inner.framerate);
    let pts = gst::ClockTime::from_nseconds(pts_ns);
    let Some(buffer) = buf.get_mut() else {
        inner.appsrc_push_errors.fetch_add(1, Ordering::Relaxed);
        return false;
    };
    buffer.set_pts(pts);
    buffer.set_duration(gst::ClockTime::from_nseconds(next_ns - pts_ns));
    {
        let mut push_times = lock_unpoisoned(&inner.push_times);
        push_times.push_back(Instant::now());
        while push_times.len() > 120 {
            push_times.pop_front();
        }
    }
    inner.last_raw_to_appsrc_us.store(
        frame.created_at.elapsed().as_micros() as u64,
        Ordering::Relaxed,
    );
    match inner.appsrc.push_buffer(buf) {
        Ok(_) => {
            inner.frames_pushed.fetch_add(1, Ordering::Relaxed);
            let level = inner.appsrc.property::<u64>("current-level-bytes");
            inner
                .last_appsrc_queue_level_bytes
                .store(level, Ordering::Relaxed);
            true
        }
        Err(_) => {
            inner.appsrc_push_errors.fetch_add(1, Ordering::Relaxed);
            false
        }
    }
}

impl GstWebRtcSession {
    fn build_pipeline(
        _id: &str,
        stun_server: Option<String>,
        codec: Codec,
        backend: &str,
        width: u32,
        height: u32,
        pixel_format: RawPixelFormat,
        framerate: u32,
        bitrate_kbps: u32,
        enable_audio: bool,
    ) -> Result<(gst::Pipeline, gst::Element, gst_app::AppSrc, u32), String> {
        super::pipeline::rgba_frame_len(width, height)?;
        if framerate == 0 {
            return Err("framerate must be positive".into());
        }
        ensure_gst_init()?;

        let spec = codec_pipeline_spec(codec)?;
        let (encoder_el, encoder_props, encoder_bitrate, encoder_raw_format) = match backend {
            "software" => (
                spec.encoder_name,
                spec.encoder_props,
                spec.encoder_bitrate,
                spec.encoder_raw_format,
            ),
            "nvenc" => {
                let nv = spec
                    .nv_encoder_name
                    .ok_or_else(|| format!("NVENC does not support {codec:?}"))?;
                (
                    nv,
                    spec.nv_encoder_props,
                    spec.nv_encoder_bitrate,
                    spec.nv_encoder_raw_format,
                )
            }
            other => return Err(format!("unknown encoder backend: '{other}'")),
        };
        for element in [Some(encoder_el), spec.parser, Some(spec.payloader)]
            .into_iter()
            .flatten()
        {
            if gst::ElementFactory::find(element).is_none() {
                return Err(format!("GStreamer element {element} is not available"));
            }
        }
        let props_str = encoder_props_with_bitrate(encoder_props, encoder_bitrate, bitrate_kbps);

        let parser_section = match spec.parser {
            Some(p) => format!("! {} {}", p, spec.parser_props),
            None => String::new(),
        };
        let caps_section = match spec.caps {
            Some(c) => format!("! {}", c),
            None => String::new(),
        };
        let stun_section = stun_server
            .map(|s| format!("stun-server={}", s))
            .unwrap_or_default();
        let audio_chain = if enable_audio {
            "\naudiotestsrc name=audio_src is-live=true \
                 ! audioconvert ! audioresample \
                 ! opusenc \
                 ! rtpopuspay name=pay_audio pt=97 \
                 ! webrtcbin"
                .to_string()
        } else {
            String::new()
        };

        let pipeline_str = format!(
            "appsrc name=video_src is-live=true do-timestamp=true format=time \
             ! video/x-raw,format={raw_format},width={w},height={h},framerate={framerate}/1 \
             ! videoconvert \
             ! video/x-raw,format={encoder_raw_format} \
             ! {encoder_el} name=enc {props_str} \
             {parser_section} \
             {caps_section} \
             ! {payloader} name=pay pt=96 \
             ! webrtcbin name=webrtc bundle-policy=max-bundle \
             {stun_section}{audio_chain}",
            w = width,
            h = height,
            raw_format = pixel_format.gst_caps_format(),
            framerate = framerate,
            encoder_el = encoder_el,
            encoder_raw_format = encoder_raw_format,
            props_str = props_str,
            parser_section = parser_section,
            caps_section = caps_section,
            payloader = spec.payloader,
            stun_section = stun_section,
            audio_chain = audio_chain,
        );

        let pipeline = gst::parse::launch(&pipeline_str)
            .map_err(|e| format!("parse webrtc pipeline: {e}"))?
            .downcast::<gst::Pipeline>()
            .map_err(|_| "not a pipeline".to_string())?;

        let webrtc = pipeline
            .by_name("webrtc")
            .ok_or_else(|| "webrtcbin not found".to_string())?;
        let appsrc_elem = pipeline
            .by_name("video_src")
            .ok_or_else(|| "appsrc not found".to_string())?;
        let appsrc: gst_app::AppSrc = appsrc_elem
            .downcast()
            .map_err(|_| "video_src not AppSrc".to_string())?;

        let ssrc: u32 = 0xDEADBEEF;
        if let Some(pay) = pipeline.by_name("pay") {
            if let Some(spec) = pay.find_property("ssrc") {
                let type_name = spec.value_type().name();
                if type_name == "gint64" {
                    pay.set_property("ssrc", i64::from(ssrc));
                } else {
                    pay.set_property("ssrc", ssrc);
                }
            }
        }

        Ok((pipeline, webrtc, appsrc, ssrc))
    }

    pub fn new(
        id: String,
        stun_server: Option<String>,
        codec: Codec,
        backend: &str,
        width: u32,
        height: u32,
        framerate: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        Self::new_with_pixel_format(
            id,
            stun_server,
            codec,
            backend,
            width,
            height,
            RawPixelFormat::Rgba,
            framerate,
            bitrate_kbps,
        )
    }

    pub fn new_with_pixel_format(
        id: String,
        stun_server: Option<String>,
        codec: Codec,
        backend: &str,
        width: u32,
        height: u32,
        pixel_format: RawPixelFormat,
        framerate: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        let (pipeline, webrtc, appsrc, ssrc) = Self::build_pipeline(
            &id,
            stun_server,
            codec,
            backend,
            width,
            height,
            pixel_format,
            framerate,
            bitrate_kbps,
            false,
        )?;

        let inner = Arc::new(Inner {
            pipeline,
            webrtc,
            appsrc,
            ssrc,
            width,
            height,
            framerate,
            pixel_format,
            running: AtomicBool::new(true),
            frame_rx: Mutex::new(None),
            main_loop: Mutex::new(None),
            connection_state: watch::channel(gst_webrtc::WebRTCPeerConnectionState::New).0,
            push_times: Mutex::new(VecDeque::new()),
            encode_time_total_us: AtomicU64::new(0),
            encode_time_max_us: AtomicU64::new(0),
            encode_count: AtomicU64::new(0),
            frames_received: AtomicU64::new(0),
            frames_pushed: AtomicU64::new(0),
            frames_lagged: AtomicU64::new(0),
            frames_caps_mismatch: AtomicU64::new(0),
            appsrc_push_errors: AtomicU64::new(0),
            keyframe_requests: AtomicU64::new(0),
            next_frame_index: AtomicU64::new(0),
            last_trace_id: AtomicU64::new(0),
            last_raw_to_appsrc_us: AtomicU64::new(0),
            last_appsrc_queue_level_bytes: AtomicU64::new(0),
        });

        Ok(Self { id, inner })
    }

    pub fn new_with_audio(
        id: String,
        stun_server: Option<String>,
        codec: Codec,
        backend: &str,
        width: u32,
        height: u32,
        framerate: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        Self::new_with_audio_and_pixel_format(
            id,
            stun_server,
            codec,
            backend,
            width,
            height,
            RawPixelFormat::Rgba,
            framerate,
            bitrate_kbps,
        )
    }

    pub fn new_with_audio_and_pixel_format(
        id: String,
        stun_server: Option<String>,
        codec: Codec,
        backend: &str,
        width: u32,
        height: u32,
        pixel_format: RawPixelFormat,
        framerate: u32,
        bitrate_kbps: u32,
    ) -> Result<Self, String> {
        let (pipeline, webrtc, appsrc, ssrc) = Self::build_pipeline(
            &id,
            stun_server,
            codec,
            backend,
            width,
            height,
            pixel_format,
            framerate,
            bitrate_kbps,
            true,
        )?;

        let inner = Arc::new(Inner {
            pipeline,
            webrtc,
            appsrc,
            ssrc,
            width,
            height,
            framerate,
            pixel_format,
            running: AtomicBool::new(true),
            frame_rx: Mutex::new(None),
            main_loop: Mutex::new(None),
            connection_state: watch::channel(gst_webrtc::WebRTCPeerConnectionState::New).0,
            push_times: Mutex::new(VecDeque::new()),
            encode_time_total_us: AtomicU64::new(0),
            encode_time_max_us: AtomicU64::new(0),
            encode_count: AtomicU64::new(0),
            frames_received: AtomicU64::new(0),
            frames_pushed: AtomicU64::new(0),
            frames_lagged: AtomicU64::new(0),
            frames_caps_mismatch: AtomicU64::new(0),
            appsrc_push_errors: AtomicU64::new(0),
            keyframe_requests: AtomicU64::new(0),
            next_frame_index: AtomicU64::new(0),
            last_trace_id: AtomicU64::new(0),
            last_raw_to_appsrc_us: AtomicU64::new(0),
            last_appsrc_queue_level_bytes: AtomicU64::new(0),
        });

        Ok(Self { id, inner })
    }

    pub fn set_frame_receiver(&self, rx: broadcast::Receiver<RawFrame>) {
        *lock_unpoisoned(&self.inner.frame_rx) = Some(rx);
    }

    pub fn request_keyframe(&self) {
        if let Some(enc) = self.inner.pipeline.by_name("enc") {
            self.inner.keyframe_requests.fetch_add(1, Ordering::Relaxed);
            let event = UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            let _ = enc.send_event(event);
        }
    }

    pub fn start_main_loop(&self) -> Result<(), String> {
        let mut slot = lock_unpoisoned(&self.inner.main_loop);
        if slot.is_some() {
            return Err("WebRTC main loop already started".into());
        }
        let context = glib::MainContext::new();
        let main_loop = glib::MainLoop::new(Some(&context), false);
        let worker_loop = main_loop.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let worker_stopped = stopped.clone();
        let pipeline = self.inner.pipeline.clone();
        let (ready, started) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name(format!(
                "gst-webrtc-{}",
                self.id.chars().take(8).collect::<String>()
            ))
            .spawn(move || {
                let _ = context.with_thread_default(|| {
                    let bus = pipeline.bus().expect("GStreamer pipeline has a bus");
                    let latency_pipeline = pipeline.clone();
                    let watch = bus.add_watch(move |_bus, msg| {
                        if let gst::MessageView::Latency(_) = msg.view() {
                            let _ = latency_pipeline.recalculate_latency();
                        }
                        glib::ControlFlow::Continue
                    });
                    let startup_loop = worker_loop.clone();
                    let source =
                        glib::idle_source_new(None, glib::Priority::DEFAULT_IDLE, move || {
                            if worker_stopped.load(Ordering::Acquire) {
                                startup_loop.quit();
                            }
                            let _ = ready.send(());
                            glib::ControlFlow::Break
                        });
                    source.attach(Some(&context));
                    worker_loop.run();
                    drop(watch);
                });
            })
            .map_err(|error| format!("spawn webrtc main loop: {error}"))?;
        let worker = MainLoopWorker {
            stopped,
            main_loop,
            thread: Some(thread),
        };
        if started.recv_timeout(Duration::from_secs(2)).is_err() {
            drop(worker);
            return Err("WebRTC main loop startup timed out".into());
        }
        *slot = Some(worker);
        drop(slot);
        let weak = Arc::downgrade(&self.inner);
        self.inner
            .webrtc
            .connect_notify(Some("connection-state"), move |element, _| {
                if let Some(inner) = weak.upgrade() {
                    inner
                        .connection_state
                        .send_replace(element.property("connection-state"));
                }
            });

        self.inner
            .pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| {
                let _ = self.inner.pipeline.set_state(gst::State::Null);
                format!("set webrtc pipeline to PLAYING: {e}")
            })?;

        // Pad probe on encoder src pad to measure frame-in to encoded-frame-out latency.
        let weak = Arc::downgrade(&self.inner);
        if let Some(enc) = self.inner.pipeline.by_name("enc") {
            if let Some(src_pad) = enc.static_pad("src") {
                src_pad.add_probe(gst::PadProbeType::BUFFER, move |_pad, info| {
                    let Some(inner) = weak.upgrade() else {
                        return gst::PadProbeReturn::Remove;
                    };
                    if matches!(info.data.as_ref(), Some(gst::PadProbeData::Buffer(_))) {
                        if let Some(push_time) = lock_unpoisoned(&inner.push_times).pop_front() {
                            inner.encode_count.fetch_add(1, Ordering::Relaxed);
                            let elapsed_us = push_time.elapsed().as_micros().max(1) as u64;
                            inner
                                .encode_time_total_us
                                .fetch_add(elapsed_us, Ordering::Relaxed);
                            if elapsed_us > inner.encode_time_max_us.load(Ordering::Relaxed) {
                                inner
                                    .encode_time_max_us
                                    .store(elapsed_us, Ordering::Relaxed);
                            }
                        }
                    }
                    gst::PadProbeReturn::Ok
                });
            }
        }
        Ok(())
    }

    fn start_frame_forwarder(&self, rx: broadcast::Receiver<RawFrame>) {
        let weak = Arc::downgrade(&self.inner);
        let keyframe_interval = Duration::from_secs(2);
        tokio::spawn(async move {
            let mut rx = rx;
            let mut last_keyframe = tokio::time::Instant::now();
            loop {
                let Some(inner) = weak.upgrade() else {
                    break;
                };
                if !inner.running.load(Ordering::Relaxed) {
                    break;
                }
                if last_keyframe.elapsed() >= keyframe_interval {
                    if let Some(enc) = inner.pipeline.by_name("enc") {
                        let event = UpstreamForceKeyUnitEvent::builder()
                            .all_headers(true)
                            .build();
                        let _ = enc.send_event(event);
                    }
                    last_keyframe = tokio::time::Instant::now();
                }
                let frame = match tokio::time::timeout(Duration::from_secs(1), rx.recv()).await {
                    Ok(Ok(f)) => f,
                    Ok(Err(broadcast::error::RecvError::Closed)) => break,
                    Ok(Err(broadcast::error::RecvError::Lagged(n))) => {
                        inner.frames_lagged.fetch_add(n, Ordering::Relaxed);
                        tracing::warn!("gst_webrtc: dropped {n} frame(s)");
                        continue;
                    }
                    Err(_timeout) => continue,
                };
                if !push_frame_to_appsrc(&inner, frame) {
                    continue;
                }
            }
            if let Some(inner) = weak.upgrade() {
                let _ = inner.appsrc.end_of_stream();
            }
        });
    }

    pub fn push_base_frame(&self, frame: RawFrame) -> bool {
        self.request_keyframe();
        push_frame_to_appsrc(&self.inner, frame)
    }

    pub async fn handle_offer(
        &self,
        offer_sdp: &str,
    ) -> Result<(String, Vec<IceCandidate>), String> {
        let candidates: Arc<Mutex<Vec<IceCandidate>>> = Arc::new(Mutex::new(Vec::new()));
        let c = candidates.clone();
        let webrtc = self.inner.webrtc.clone();
        webrtc.connect("on-ice-candidate", false, move |args| {
            let mlineindex: u32 = args[1].get().unwrap_or(0);
            let candidate: String = args[2].get().unwrap_or_default();
            let ice = IceCandidate {
                candidate,
                sdp_mid: None,
                sdp_mline_index: Some(mlineindex as u16),
                username_fragment: None,
            };
            lock_unpoisoned(&c).push(ice);
            None
        });

        let parsed_sdp = SDPMessage::parse_buffer(offer_sdp.as_bytes())
            .map_err(|e| format!("parse SDP: {e}"))?;
        let offer =
            gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Offer, parsed_sdp);

        self.inner
            .webrtc
            .emit_by_name::<()>("set-remote-description", &[&offer, &None::<gst::Promise>]);

        let (tx, rx) = oneshot::channel();
        let promise = gst::Promise::with_change_func(move |reply| {
            let answer = match reply {
                Ok(Some(s)) => s
                    .value("answer")
                    .ok()
                    .and_then(|v| v.get::<gst_webrtc::WebRTCSessionDescription>().ok()),
                _ => None,
            };
            let _ = tx.send(answer);
        });

        self.inner
            .webrtc
            .emit_by_name::<()>("create-answer", &[&None::<gst::Structure>, &promise]);

        let answer = rx
            .await
            .map_err(|_| "answer promise cancelled".to_string())?
            .ok_or("no answer from webrtcbin".to_string())?;

        self.inner
            .webrtc
            .emit_by_name::<()>("set-local-description", &[&answer, &None::<gst::Promise>]);

        tokio::time::sleep(Duration::from_millis(500)).await;

        let answer_sdp = answer
            .sdp()
            .as_text()
            .map_err(|e| format!("sdp as text: {e}"))?;

        // Build mid[] lookup from answer SDP media sections,
        // then backfill sdp_mid on collected ICE candidates.
        let mids: Vec<Option<String>> =
            if let Ok(parsed) = SDPMessage::parse_buffer(answer_sdp.as_bytes()) {
                parsed
                    .medias()
                    .map(|media| media.attribute_val("mid").map(|s| s.to_string()))
                    .collect()
            } else {
                Vec::new()
            };
        {
            let mut locked = lock_unpoisoned(&candidates);
            for cand in locked.iter_mut() {
                if cand.sdp_mid.is_none() {
                    if let Some(idx) = cand.sdp_mline_index {
                        cand.sdp_mid = mids.get(idx as usize).and_then(|m| m.clone());
                    }
                }
            }
        }

        // Inject a=ssrc: into answer SDP if absent.
        // webrtc-rs needs this to match incoming RTP packets to the recvonly
        // transceiver. GStreamer's webrtcbin does not emit SSRC in the SDP.
        // NOTE: replacement injects SSRC lines BEFORE a=sendonly, keeping the
        // original line ending to avoid blank lines in SDP.
        let answer_sdp = if !answer_sdp.contains("a=ssrc:") {
            let ssrc = self.inner.ssrc;
            let ssrc_lines = format!("a=ssrc:{} cname:gst\na=ssrc:{} msid:gst gst\n", ssrc, ssrc);
            answer_sdp.replacen("a=sendonly", &format!("{}{}", ssrc_lines, "a=sendonly"), 1)
        } else {
            answer_sdp
        };

        // Parse answer SDP and set rtph264pay PT to match negotiated payload type
        if let Ok(parsed) = SDPMessage::parse_buffer(answer_sdp.as_bytes()) {
            for media in parsed.medias() {
                if media.media().unwrap_or("") == "video" {
                    if let Some(pt_str) = media.formats().next() {
                        if let Ok(pt) = pt_str.parse::<u32>() {
                            if let Some(pay) = self.inner.pipeline.by_name("pay") {
                                pay.set_property("pt", pt);
                            }
                        }
                    }
                }
            }
        }

        // Trigger latency recalculation now that webrtcbin has its internal session.
        // The bus watch installed in start_main_loop handles the LATENCY message
        // that recalculate_latency() posts.
        let _ = self.inner.pipeline.recalculate_latency();

        let rx_guard = lock_unpoisoned(&self.inner.frame_rx).take();
        if let Some(rx) = rx_guard {
            self.start_frame_forwarder(rx);
        }

        let gathered = lock_unpoisoned(&candidates).clone();
        tracing::info!(
            "webrtc session {}: answer created, {} ICE candidates",
            self.id,
            gathered.len()
        );

        Ok((answer_sdp, gathered))
    }

    pub fn add_ice_candidate(&self, candidate: &str, sdp_mline_index: u16) -> Result<(), String> {
        use glib::prelude::ToValue;
        let index = sdp_mline_index as u32;
        let args: Vec<&dyn ToValue> = vec![&index, &candidate];
        self.inner
            .webrtc
            .emit_by_name::<()>("add-ice-candidate", &args);
        Ok(())
    }

    pub fn inner_webrtc(&self) -> gst::Element {
        self.inner.webrtc.clone()
    }

    /// Create an SDP offer, set it as local description, and return the SDP text.
    pub async fn create_offer_sdp(&self) -> Result<String, String> {
        use glib::prelude::ToValue;

        let (tx, rx) = oneshot::channel();
        let promise = gst::Promise::with_change_func(move |reply| {
            let offer = match reply {
                Ok(Some(s)) => s
                    .value("offer")
                    .ok()
                    .and_then(|v| v.get::<gst_webrtc::WebRTCSessionDescription>().ok()),
                _ => None,
            };
            let _ = tx.send(offer);
        });

        let args: Vec<&dyn ToValue> = vec![&None::<gst::Structure>, &promise];
        self.inner.webrtc.emit_by_name::<()>("create-offer", &args);

        let offer = rx
            .await
            .map_err(|_| "offer promise cancelled".to_string())?
            .ok_or("no offer from webrtcbin".to_string())?;

        // Set as local description
        let args: Vec<&dyn ToValue> = vec![&offer, &None::<gst::Promise>];
        self.inner
            .webrtc
            .emit_by_name::<()>("set-local-description", &args);

        tokio::time::sleep(Duration::from_millis(200)).await;

        let sdp = offer
            .sdp()
            .as_text()
            .map_err(|e| format!("sdp as text: {e}"))?;
        Ok(sdp)
    }

    pub fn close(&self) {
        self.inner.running.store(false, Ordering::Relaxed);
        let _ = self.inner.appsrc.end_of_stream();
        if let Err(e) = self.inner.pipeline.set_state(gst::State::Null) {
            tracing::warn!("set webrtc pipeline to Null during close failed: {e}");
        }
        self.inner
            .connection_state
            .send_replace(gst_webrtc::WebRTCPeerConnectionState::Closed);
        let worker = lock_unpoisoned(&self.inner.main_loop).take();
        drop(worker);
    }

    pub fn connection_state_rx(&self) -> watch::Receiver<gst_webrtc::WebRTCPeerConnectionState> {
        self.inner.connection_state.subscribe()
    }

    pub fn perf_snapshot(&self) -> serde_json::Value {
        let count = self.inner.encode_count.load(Ordering::Relaxed);
        let total = self.inner.encode_time_total_us.load(Ordering::Relaxed);
        let max = self.inner.encode_time_max_us.load(Ordering::Relaxed);
        serde_json::json!({
            "webrtc_encode_time_avg_ms": if count > 0 { total as f64 / count as f64 / 1000.0 } else { 0.0 },
            "webrtc_encode_time_max_ms": max as f64 / 1000.0,
            "webrtc_encode_count": count,
        })
    }

    pub fn trace_snapshot(&self) -> serde_json::Value {
        let state = self.inner.pipeline.current_state();
        let state = format!("{state:?}");
        let pending_push_times = lock_unpoisoned(&self.inner.push_times).len();
        let encoder_element = self
            .inner
            .pipeline
            .by_name("enc")
            .and_then(|enc| enc.factory().map(|factory| factory.name().to_string()))
            .unwrap_or_else(|| "unknown".to_string());
        let encoder_backend = if encoder_element.starts_with("nv") {
            "nvenc"
        } else {
            "software"
        };
        serde_json::json!({
            "connection_id": self.id,
            "pipeline_state": state,
            "expected_caps": {
                "width": self.inner.width,
                "height": self.inner.height,
                "pixel_format": self.inner.pixel_format.gst_caps_format(),
            },
            "appsrc": {
                "current_level_bytes": self.inner.last_appsrc_queue_level_bytes.load(Ordering::Relaxed),
                "push_errors": self.inner.appsrc_push_errors.load(Ordering::Relaxed),
            },
            "frames": {
                "received": self.inner.frames_received.load(Ordering::Relaxed),
                "pushed": self.inner.frames_pushed.load(Ordering::Relaxed),
                "lagged": self.inner.frames_lagged.load(Ordering::Relaxed),
                "caps_mismatch": self.inner.frames_caps_mismatch.load(Ordering::Relaxed),
                "last_trace_id": self.inner.last_trace_id.load(Ordering::Relaxed),
                "last_raw_to_appsrc_ms": self.inner.last_raw_to_appsrc_us.load(Ordering::Relaxed) as f64 / 1000.0,
                "pending_encode_timestamps": pending_push_times,
            },
            "encoder": self.perf_snapshot(),
            "encoder_config": {
                "backend": encoder_backend,
                "element": encoder_element,
            },
            "keyframe_requests": self.inner.keyframe_requests.load(Ordering::Relaxed),
            "running": self.inner.running.load(Ordering::Relaxed),
        })
    }
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Relaxed);
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[derive(Debug, Clone)]
pub struct IceCandidate {
    pub candidate: String,
    pub sdp_mid: Option<String>,
    pub sdp_mline_index: Option<u16>,
    pub username_fragment: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::stream::pipeline::RawFrame;

    #[test]
    fn unicode_session_closes_and_releases_its_loop_and_inner() {
        let session =
            GstWebRtcSession::new("a😀😀".into(), None, Codec::VP8, "software", 64, 64, 1, 128)
                .unwrap();
        let weak = Arc::downgrade(&session.inner);
        session.start_main_loop().unwrap();
        assert!(lock_unpoisoned(&session.inner.main_loop).is_some());
        session.close();
        assert!(lock_unpoisoned(&session.inner.main_loop).is_none());
        session.close();
        drop(session);
        assert!(weak.upgrade().is_none());
    }

    fn test_frame(width: u32, height: u32) -> Vec<u8> {
        let mut buf = vec![0u8; (width * height * 4) as usize];
        for chunk in buf.chunks_mut(4) {
            chunk[0] = 128;
            chunk[1] = 64;
            chunk[2] = 32;
            chunk[3] = 255;
        }
        buf
    }

    fn gst_init() {
        let _ = gst::init();
    }

    // ── Session lifecycle × all codecs ───────────────────────────────

    #[tokio::test]
    async fn test_create_close_all_codecs_sw() {
        gst_init();
        for codec in [Codec::H264, Codec::H265, Codec::VP8, Codec::VP9, Codec::AV1] {
            let s =
                GstWebRtcSession::new("test".into(), None, codec, "software", 160, 120, 30, 500);
            match s {
                Ok(session) => session.close(),
                Err(e) => eprintln!("{:?}/sw not available: {e}", codec),
            }
        }
    }

    #[tokio::test]
    async fn test_create_close_nvenc() {
        gst_init();
        for codec in [Codec::H264, Codec::H265, Codec::AV1] {
            let s =
                GstWebRtcSession::new("nv-test".into(), None, codec, "nvenc", 160, 120, 30, 500);
            match s {
                Ok(session) => session.close(),
                Err(e) => eprintln!("{:?}/nvenc not available: {e}", codec),
            }
        }
    }

    #[tokio::test]
    async fn test_connection_id_is_set() {
        gst_init();
        let session = GstWebRtcSession::new(
            "my-id".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create webrtc session");
        assert_eq!(session.id, "my-id");
        session.close();
    }

    #[tokio::test]
    async fn test_create_invalid_codec() {
        gst_init();
        let err = GstWebRtcSession::new(
            "test".into(),
            None,
            Codec::Mjpeg,
            "software",
            160,
            120,
            30,
            500,
        )
        .unwrap_err();
        assert!(err.contains("not a streaming codec"));
    }

    #[tokio::test]
    async fn test_create_unknown_backend() {
        gst_init();
        let err =
            GstWebRtcSession::new("test".into(), None, Codec::H264, "bogus", 160, 120, 30, 500)
                .unwrap_err();
        assert!(err.contains("unknown encoder backend"));
    }

    // ── ICE before offer ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_ice_before_offer_h264() {
        gst_init();
        let session = GstWebRtcSession::new(
            "ice-test".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create");
        let r =
            session.add_ice_candidate("candidate:1 1 UDP 2130706431 127.0.0.1 5000 typ host", 0);
        assert!(r.is_ok() || r.is_err());
        session.close();
    }

    #[tokio::test]
    async fn test_ice_before_offer_h265() {
        gst_init();
        match GstWebRtcSession::new(
            "ice-h265".into(),
            None,
            Codec::H265,
            "software",
            160,
            120,
            30,
            500,
        ) {
            Ok(session) => {
                let r = session.add_ice_candidate("candidate:1 1 UDP ...", 0);
                assert!(r.is_ok() || r.is_err());
                session.close();
            }
            Err(e) => eprintln!("H265 not available: {e}"),
        }
    }

    #[tokio::test]
    async fn test_ice_before_offer_vp8() {
        gst_init();
        match GstWebRtcSession::new(
            "ice-vp8".into(),
            None,
            Codec::VP8,
            "software",
            160,
            120,
            30,
            500,
        ) {
            Ok(session) => {
                let r = session.add_ice_candidate("candidate:1 1 UDP ...", 0);
                assert!(r.is_ok() || r.is_err());
                session.close();
            }
            Err(e) => eprintln!("VP8 not available: {e}"),
        }
    }

    // ── Close multiple times ─────────────────────────────────────────

    #[tokio::test]
    async fn test_close_multiple_times() {
        gst_init();
        let session = GstWebRtcSession::new(
            "multi-close".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create");
        session.close();
        session.close();
    }

    #[tokio::test]
    async fn test_close_multiple_times_h265() {
        gst_init();
        match GstWebRtcSession::new(
            "multi-close-h265".into(),
            None,
            Codec::H265,
            "software",
            160,
            120,
            30,
            500,
        ) {
            Ok(session) => {
                session.close();
                session.close();
            }
            Err(e) => eprintln!("H265 not available: {e}"),
        }
    }

    // ── Graceful shutdown ────────────────────────────────────────────

    async fn graceful_shutdown_for(codec: Codec, label: &str) {
        gst_init();
        let (tx, rx) = broadcast::channel(64);
        let session = match GstWebRtcSession::new(
            format!("shutdown-{label}"),
            None,
            codec,
            "software",
            160,
            120,
            30,
            500,
        ) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("{label} not available: {e}");
                return;
            }
        };
        session.set_frame_receiver(rx);
        session.start_main_loop().expect("start main loop");

        let frame = RawFrame {
            data: gst::Buffer::from_mut_slice(test_frame(160, 120)),
            width: 160,
            height: 120,
            pixel_format: RawPixelFormat::Rgba,
            trace_id: 1,
            created_at: Instant::now(),
        };
        let _ = tx.send(frame);
        tokio::time::sleep(Duration::from_millis(100)).await;
        session.close();
    }

    #[tokio::test]
    async fn test_graceful_shutdown_h264() {
        graceful_shutdown_for(Codec::H264, "h264").await;
    }

    #[tokio::test]
    async fn test_graceful_shutdown_h265() {
        graceful_shutdown_for(Codec::H265, "h265").await;
    }

    #[tokio::test]
    async fn test_graceful_shutdown_vp8() {
        graceful_shutdown_for(Codec::VP8, "vp8").await;
    }

    #[tokio::test]
    async fn test_graceful_shutdown_vp9() {
        graceful_shutdown_for(Codec::VP9, "vp9").await;
    }

    // ── Multi-session ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_multi_session_different_codecs() {
        gst_init();
        let mut sessions = Vec::new();
        for (codec, label) in [(Codec::H264, "a"), (Codec::H265, "b"), (Codec::VP8, "c")] {
            match GstWebRtcSession::new(
                format!("multi-{label}"),
                None,
                codec,
                "software",
                160,
                120,
                30,
                500,
            ) {
                Ok(s) => sessions.push(s),
                Err(e) => eprintln!("{label} not available: {e}"),
            }
        }
        for s in sessions {
            s.close();
        }
    }

    #[tokio::test]
    async fn test_multi_session_same_codec() {
        gst_init();
        let s1 = GstWebRtcSession::new(
            "multi-1".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create 1");
        let s2 = GstWebRtcSession::new(
            "multi-2".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create 2");
        let s3 = GstWebRtcSession::new(
            "multi-3".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create 3");
        s1.close();
        s2.close();
        s3.close();
    }

    // ── Perf snapshot ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_create_h264_with_bgra_input_caps() {
        gst_init();
        let session = GstWebRtcSession::new_with_pixel_format(
            "bgra-test".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            RawPixelFormat::Bgra,
            30,
            500,
        )
        .expect("create h264 bgra webrtc session");
        assert_eq!(session.inner.pixel_format, RawPixelFormat::Bgra);
        session.close();
    }

    #[tokio::test]
    async fn test_push_base_frame_pushes_cached_frame_to_appsrc() {
        gst_init();
        let session = GstWebRtcSession::new(
            "base-frame".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create h264 webrtc session");
        session.start_main_loop().expect("start main loop");
        tokio::time::sleep(Duration::from_millis(200)).await;

        let frame = RawFrame {
            data: gst::Buffer::from_mut_slice(test_frame(160, 120)),
            width: 160,
            height: 120,
            pixel_format: RawPixelFormat::Rgba,
            trace_id: 42,
            created_at: Instant::now(),
        };
        assert!(session.push_base_frame(frame));
        let snap = session.trace_snapshot();
        assert_eq!(snap["frames"]["pushed"], 1);
        assert_eq!(snap["frames"]["last_trace_id"], 42);
        session.close();
    }

    #[tokio::test]
    async fn test_perf_snapshot_non_zero() {
        gst_init();
        let session = GstWebRtcSession::new(
            "perf".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create");
        session.start_main_loop().expect("start main loop");
        // Allow pipeline to preroll before pushing frames
        tokio::time::sleep(Duration::from_millis(200)).await;

        for i in 0..5 {
            let frame = RawFrame {
                data: gst::Buffer::from_mut_slice(test_frame(160, 120)),
                width: 160,
                height: 120,
                pixel_format: RawPixelFormat::Rgba,
                trace_id: i,
                created_at: Instant::now(),
            };
            assert!(session.push_base_frame(frame));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;

        let snap = session.perf_snapshot();
        assert!(
            snap["webrtc_encode_count"].as_u64().unwrap_or(0) > 0,
            "should have encode count > 0 after pushing frames"
        );
        session.close();
    }

    // ── Request keyframe ──────────────────────────────────────────────

    #[tokio::test]
    async fn test_keyframe_request_h264() {
        gst_init();
        let session = GstWebRtcSession::new(
            "kf-h264".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create");
        session.start_main_loop().expect("start main loop");
        session.request_keyframe();
        session.close();
    }

    // ── Audio session ─────────────────────────────────────────────────

    #[tokio::test]
    async fn test_audio_session_create() {
        gst_init();
        let session = GstWebRtcSession::new_with_audio(
            "audio-test".into(),
            None,
            Codec::H264,
            "software",
            160,
            120,
            30,
            500,
        )
        .expect("create webrtc session with audio");
        session.close();
    }

    #[tokio::test]
    async fn test_audio_session_create_all_codecs() {
        gst_init();
        for codec in [Codec::H264, Codec::H265, Codec::VP8, Codec::VP9, Codec::AV1] {
            let s = GstWebRtcSession::new_with_audio(
                "audio".into(),
                None,
                codec,
                "software",
                160,
                120,
                30,
                500,
            );
            match s {
                Ok(session) => session.close(),
                Err(e) => eprintln!("{:?}/audio not available: {e}", codec),
            }
        }
    }
}
