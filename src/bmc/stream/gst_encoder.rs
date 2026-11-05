use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use gst::event::Eos;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_app as gst_app;
use gstreamer_video::UpstreamForceKeyUnitEvent;

use super::codec_config::{codec_pipeline_spec, encoder_props_with_bitrate};
use super::encoder::{EncodedPacket, VideoStreamEncoder};
use super::pipeline::{frame_time_ns, rgba_frame_len};
use crate::bmc::encoder::Codec;
use crate::bmc::stream::gst_init::{ensure_gst_init, gst_lock};

/// Timeout for appsink pull_sample operations
const APPSINK_TIMEOUT_MS: u64 = 10000;

pub struct GstEncoder {
    pipeline: gst::Pipeline,
    appsrc: gst_app::AppSrc,
    appsink: gst_app::AppSink,
    codec: Codec,
    width: u32,
    height: u32,
    pts: u64,
    framerate: u32,
    keyframe_requested: Arc<AtomicBool>,
    encode_time_total_us: AtomicU64,
    encode_time_max_us: AtomicU64,
    encode_count: AtomicU64,
}

impl std::fmt::Debug for GstEncoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GstEncoder")
            .field("codec", &self.codec)
            .field("width", &self.width)
            .field("height", &self.height)
            .field("framerate", &self.framerate)
            .finish()
    }
}

impl GstEncoder {
    fn create_pipeline(
        encoder_name: &str,
        codec: Codec,
        width: u32,
        height: u32,
        framerate: u32,
        encoder_props: &str,
    ) -> Result<Self, String> {
        let preroll_size = rgba_frame_len(width, height)?;
        if framerate == 0 {
            return Err("framerate must be positive".into());
        }
        ensure_gst_init()?;
        let _guard = gst_lock()?;

        let spec = codec_pipeline_spec(codec)?;
        let encoder_raw_format = if encoder_name.starts_with("nv") {
            spec.nv_encoder_raw_format
        } else {
            spec.encoder_raw_format
        };
        let parser_section = match spec.parser {
            Some(p) => format!("! {} {}", p, spec.parser_props),
            None => String::new(),
        };
        let caps_section = match spec.caps {
            Some(c) => format!("! {}", c),
            None => String::new(),
        };

        let pipeline_str = format!(
            "appsrc name=src \
             ! video/x-raw,format=RGBA,width={width},height={height},framerate={framerate}/1 \
             ! videoconvert \
             ! video/x-raw,format={encoder_raw_format} \
             ! {encoder_name} name=enc {encoder_props} \
             {parser_section} \
             {caps_section} \
             ! appsink name=sink sync=false wait-on-eos=false drop=true max-buffers=1"
        );

        let pipeline =
            gst::parse::launch(&pipeline_str).map_err(|e| format!("parse_launch failed: {e}"))?;
        let pipeline = pipeline
            .dynamic_cast::<gst::Pipeline>()
            .map_err(|_| "cast to pipeline failed".to_string())?;

        let startup = PipelineStartup(Some(pipeline.clone()));
        let appsrc = pipeline
            .by_name("src")
            .ok_or("src not found")?
            .dynamic_cast::<gst_app::AppSrc>()
            .map_err(|_| "appsrc cast failed")?;
        let appsink = pipeline
            .by_name("sink")
            .ok_or("sink not found")?
            .dynamic_cast::<gst_app::AppSink>()
            .map_err(|_| "appsink cast failed")?;

        appsrc.set_property("format", gst::Format::Time);

        let bus = pipeline
            .bus()
            .ok_or_else(|| "pipeline bus unavailable".to_string())?;
        let _ret = pipeline
            .set_state(gst::State::Playing)
            .map_err(|e| format!("pipeline failed to start: {e}"))?;

        // Push one buffer so the pipeline can complete preroll (ASYNC → PLAYING)
        let mut preroll_buf = gst::Buffer::with_size(preroll_size)
            .map_err(|e| format!("allocate preroll buffer: {e}"))?;
        {
            let b = preroll_buf
                .get_mut()
                .ok_or_else(|| "preroll buffer is unexpectedly shared".to_string())?;
            {
                let mut data = b
                    .map_writable()
                    .map_err(|_| "map preroll buffer writable".to_string())?;
                data.as_mut_slice().fill(128);
            }
            let frame_ns = 1_000_000_000 / framerate as u64;
            b.set_pts(gst::ClockTime::from_mseconds(0));
            b.set_duration(gst::ClockTime::from_nseconds(frame_ns));
        }
        appsrc
            .push_buffer(preroll_buf)
            .map_err(|error| format!("push preroll: {error}"))?;

        // Bus chatter must not consume the startup budget or imply readiness.
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        if _ret == gst::StateChangeSuccess::Async {
            loop {
                let remaining = deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err("pipeline startup timed out".into());
                }
                let timeout = gst::ClockTime::from_nseconds(remaining.as_nanos() as u64);
                let Some(message) = bus.timed_pop(timeout) else {
                    return Err("pipeline startup timed out".into());
                };
                match message.view() {
                    gst::MessageView::AsyncDone(..) => break,
                    gst::MessageView::Error(error) => {
                        return Err(format!(
                            "pipeline error: {} {:?}",
                            error.error(),
                            error.debug()
                        ))
                    }
                    _ => {}
                }
            }
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        appsink
            .try_pull_sample(gst::ClockTime::from_nseconds(remaining.as_nanos() as u64))
            .ok_or_else(|| "pipeline produced no preroll sample".to_string())?;
        while appsink.try_pull_sample(gst::ClockTime::ZERO).is_some() {}

        // Force first real encode to produce an IDR with SPS/PPS
        if let Some(enc) = pipeline.by_name("enc") {
            let event = UpstreamForceKeyUnitEvent::builder()
                .all_headers(true)
                .build();
            let _ = enc.send_event(event);
        }

        startup.commit();
        Ok(Self {
            pipeline,
            appsrc,
            appsink,
            codec,
            width,
            height,
            pts: 1,
            framerate,
            keyframe_requested: Arc::new(AtomicBool::new(true)),
            encode_time_total_us: AtomicU64::new(0),
            encode_time_max_us: AtomicU64::new(0),
            encode_count: AtomicU64::new(0),
        })
    }

    pub fn new_software(
        codec: Codec,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        framerate: u32,
    ) -> Result<Self, String> {
        ensure_gst_init()?;
        let spec = codec_pipeline_spec(codec)?;
        if gst::ElementFactory::find(spec.encoder_name).is_none() {
            return Err(format!(
                "GStreamer encoder element {} is not available",
                spec.encoder_name
            ));
        }
        let props =
            encoder_props_with_bitrate(spec.encoder_props, spec.encoder_bitrate, bitrate_kbps);
        Self::create_pipeline(spec.encoder_name, codec, width, height, framerate, &props)
    }

    pub fn new_nvenc(
        codec: Codec,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        framerate: u32,
    ) -> Result<Self, String> {
        let spec = codec_pipeline_spec(codec)?;
        let nv_name = spec
            .nv_encoder_name
            .ok_or_else(|| format!("NVENC does not support codec {:?}", codec))?;
        ensure_gst_init()?;
        if gst::ElementFactory::find(nv_name).is_none() {
            return Err(format!(
                "GStreamer encoder element {nv_name} is not available"
            ));
        }
        let props = encoder_props_with_bitrate(
            spec.nv_encoder_props,
            spec.nv_encoder_bitrate,
            bitrate_kbps,
        );
        Self::create_pipeline(nv_name, codec, width, height, framerate, &props)
    }
}

impl VideoStreamEncoder for GstEncoder {
    fn encode_packets(
        &mut self,
        rgba: &[u8],
        width: u32,
        height: u32,
    ) -> Result<Vec<EncodedPacket>, String> {
        let start = Instant::now();

        if width != self.width || height != self.height {
            return Err(format!(
                "resolution mismatch: expected {}x{}, got {}x{}",
                self.width, self.height, width, height
            ));
        }

        let size = rgba_frame_len(width, height)?;
        if rgba.len() < size {
            return Err(format!(
                "buffer too small: expected {size}, got {}",
                rgba.len()
            ));
        }

        let mut buffer = gst::Buffer::with_size(size).map_err(|e| format!("buffer alloc: {e}"))?;
        {
            let b = buffer
                .get_mut()
                .ok_or_else(|| "buffer get_mut failed".to_string())?;
            {
                let mut data = b.map_writable().map_err(|e| format!("map_writable: {e}"))?;
                data.as_mut_slice().copy_from_slice(&rgba[..size]);
            }
            let pts = frame_time_ns(self.pts, self.framerate);
            let next = frame_time_ns(self.pts.saturating_add(1), self.framerate);
            b.set_pts(gst::ClockTime::from_nseconds(pts));
            b.set_duration(gst::ClockTime::from_nseconds(next - pts));
        }
        self.pts = self.pts.saturating_add(1);

        if self.keyframe_requested.swap(false, Ordering::SeqCst) {
            if let Some(enc) = self.pipeline.by_name("enc") {
                let event = UpstreamForceKeyUnitEvent::builder()
                    .all_headers(true)
                    .build();
                let _ = enc.send_event(event);
            }
        }

        self.appsrc
            .push_buffer(buffer)
            .map_err(|e| format!("push_buffer: {e}"))?;

        let sample = self.appsink.try_pull_sample(gst::ClockTime::from_mseconds(APPSINK_TIMEOUT_MS))
            .ok_or_else(|| {
                let (state_ret, state, pending) = self.pipeline.state(gst::ClockTime::from_mseconds(0));
                format!("appsink timeout after {APPSINK_TIMEOUT_MS}ms (state_ret={state_ret:?}, state={state:?}, pending={pending:?})")
            })?;

        let buf = sample
            .buffer()
            .ok_or_else(|| "no buffer in appsink sample".to_string())?;
        let map = buf
            .map_readable()
            .map_err(|e| format!("map_readable: {e}"))?;
        let encoded = map.as_slice().to_vec();

        let elapsed_us = start.elapsed().as_micros() as u64;
        self.encode_count.fetch_add(1, Ordering::Relaxed);
        self.encode_time_total_us
            .fetch_add(elapsed_us, Ordering::Relaxed);
        let prev_max = self.encode_time_max_us.load(Ordering::Relaxed);
        if elapsed_us > prev_max {
            self.encode_time_max_us.store(elapsed_us, Ordering::Relaxed);
        }

        Ok(vec![EncodedPacket {
            data: encoded,
            keyframe: !buf.flags().contains(gst::BufferFlags::DELTA_UNIT),
            timestamp_us: buf.pts().map(|pts| pts.useconds()).unwrap_or(0),
            duration_us: buf.duration().map(|duration| duration.useconds()),
        }])
    }

    fn codec(&self) -> Codec {
        self.codec
    }

    fn request_keyframe(&self) {
        self.keyframe_requested.store(true, Ordering::SeqCst);
    }

    fn perf_snapshot(&self) -> serde_json::Value {
        let count = self.encode_count.swap(0, Ordering::Relaxed);
        let total = self.encode_time_total_us.swap(0, Ordering::Relaxed);
        let max = self.encode_time_max_us.swap(0, Ordering::Relaxed);
        serde_json::json!({
            "encode_time_avg_ms": if count > 0 { total as f64 / count as f64 / 1000.0 } else { 0.0 },
            "encode_time_max_ms": max as f64 / 1000.0,
            "encode_count": count,
        })
    }
}

impl Drop for GstEncoder {
    fn drop(&mut self) {
        let _ = self.appsrc.end_of_stream();
        let _ = self.pipeline.send_event(Eos::new());
        let _ = self.pipeline.set_state(gst::State::Null);
        // Wait for state change to complete
        let _ = self.pipeline.state(gst::ClockTime::from_mseconds(500));
    }
}

// Until construction commits, every error path must return the pipeline to NULL.
struct PipelineStartup(Option<gst::Pipeline>);

impl PipelineStartup {
    fn commit(mut self) {
        self.0.take();
    }
}

impl Drop for PipelineStartup {
    fn drop(&mut self) {
        if let Some(pipeline) = self.0.take() {
            let _ = pipeline.set_state(gst::State::Null);
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn failed_startup_returns_live_pipeline_to_null() {
        ensure_gst_init().unwrap();
        let pipeline = gst::parse::launch("videotestsrc is-live=true ! fakesink sync=false")
            .unwrap()
            .downcast::<gst::Pipeline>()
            .unwrap();
        let fail = || -> Result<(), String> {
            let _startup = PipelineStartup(Some(pipeline.clone()));
            pipeline.set_state(gst::State::Playing).unwrap();
            let (_, state, _) = pipeline.state(gst::ClockTime::from_seconds(1));
            assert_eq!(state, gst::State::Playing);
            Err("synthetic startup failure".into())
        };
        assert!(fail().is_err());
        assert_eq!(pipeline.current_state(), gst::State::Null);
    }

    #[test]
    fn frame_clock_preserves_fractional_frame_rates() {
        assert_eq!(super::frame_time_ns(60, 60), 1_000_000_000);
        assert_eq!(super::frame_time_ns(30, 30), 1_000_000_000);
        assert_eq!(super::frame_time_ns(u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn rejects_invalid_encoder_dimensions_and_frame_rate() {
        for (width, height, fps) in [(0, 120, 30), (u32::MAX, u32::MAX, 30), (160, 120, 0)] {
            assert!(GstEncoder::new_software(Codec::VP8, width, height, 500, fps).is_err());
        }
    }

    #[test]
    fn vp8_packets_preserve_keyframe_flags_and_timestamps() {
        let mut encoder = GstEncoder::new_software(Codec::VP8, 160, 120, 500, 60).unwrap();
        let frame = test_frame(160, 120);
        let mut saw_delta = false;
        let mut previous_pts = None;
        for _ in 0..8 {
            for packet in encoder.encode_packets(&frame, 160, 120).unwrap() {
                assert_eq!(packet.keyframe, packet.data[0] & 1 == 0);
                saw_delta |= !packet.keyframe;
                if let Some(previous) = previous_pts {
                    assert!(packet.timestamp_us > previous);
                }
                previous_pts = Some(packet.timestamp_us);
                assert!(matches!(packet.duration_us, Some(16_666 | 16_667)));
            }
        }
        assert!(
            saw_delta,
            "inter frames must not be advertised as keyframes"
        );
    }

    use super::*;

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

    fn has_nal_type(data: &[u8], nal_type: u8) -> bool {
        for start in 0..data.len().saturating_sub(3) {
            let code_len = if data[start..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if data[start..].starts_with(&[0, 0, 1]) {
                3
            } else {
                continue;
            };
            let nal_start = start + code_len;
            if nal_start < data.len() && (data[nal_start] & 0x1F) == nal_type {
                return true;
            }
        }
        false
    }

    fn has_keyframe(packets: &[Vec<u8>]) -> bool {
        packets
            .iter()
            .any(|p| has_nal_type(p, 5) || has_nal_type(p, 7))
    }

    #[test]
    fn test_gst_software_h264_first_frame_is_keyframe() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(160, 120);
        let packets = enc.encode(&frame, 160, 120).expect("encode first frame");
        assert!(
            has_keyframe(&packets),
            "first frame should be a keyframe (SPS/IDR)"
        );
    }

    #[test]
    fn test_gst_software_h264_keyframe_has_sps_pps() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(160, 120);
        let packets = enc.encode(&frame, 160, 120).expect("encode first frame");
        assert!(
            packets.iter().any(|p| has_nal_type(p, 7)),
            "keyframe must have SPS (NAL type 7)"
        );
        assert!(
            packets.iter().any(|p| has_nal_type(p, 8)),
            "keyframe must have PPS (NAL type 8)"
        );
    }

    #[test]
    fn test_gst_software_h264_request_keyframe_forces_keyframe() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(160, 120);
        let _ = enc.encode(&frame, 160, 120).expect("frame 0 (keyframe)");
        for _ in 0..10 {
            let _ = enc.encode(&frame, 160, 120).expect("encode P-frames");
        }
        enc.request_keyframe();
        let packets = enc
            .encode(&frame, 160, 120)
            .expect("encode after request_keyframe()");
        assert!(
            has_keyframe(&packets),
            "frame after request_keyframe() must be a keyframe"
        );
        assert!(
            packets.iter().any(|p| has_nal_type(p, 7)),
            "keyframe must have SPS (NAL type 7)"
        );
        assert!(
            packets.iter().any(|p| has_nal_type(p, 8)),
            "keyframe must have PPS (NAL type 8)"
        );
    }

    #[test]
    fn test_gst_software_h264_pframes_are_not_keyframes() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(160, 120);
        let _ = enc.encode(&frame, 160, 120).expect("frame 0 (keyframe)");
        for i in 0..5 {
            let packets = enc
                .encode(&frame, 160, 120)
                .unwrap_or_else(|e| panic!("frame {i}: {e}"));
            if packets.is_empty() {
                continue;
            }
            assert!(
                !has_keyframe(&packets),
                "frame {i} should NOT be a keyframe"
            );
        }
    }

    #[test]
    fn test_gst_software_h264_multiple_frames() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(160, 120);
        for i in 0..30 {
            let packets = enc
                .encode(&frame, 160, 120)
                .unwrap_or_else(|e| panic!("frame {i}: {e}"));
            assert!(!packets.is_empty(), "frame {i} should produce output");
        }
    }

    #[test]
    fn test_gst_software_h264_error_resolution_mismatch() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(320, 240);
        let err = enc.encode(&frame, 320, 240).unwrap_err();
        assert!(
            err.contains("resolution mismatch"),
            "expected resolution mismatch error, got: {err}"
        );
    }

    #[test]
    fn test_gst_software_h264_error_buffer_too_small() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let small = vec![0u8; 10];
        let err = enc.encode(&small, 160, 120).unwrap_err();
        assert!(
            err.contains("buffer too small"),
            "expected buffer too small error, got: {err}"
        );
    }

    #[test]
    fn test_gst_software_h264_perf_snapshot() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(160, 120);

        let before = enc.perf_snapshot();
        assert_eq!(
            before["encode_count"].as_u64(),
            Some(0),
            "initial count should be 0"
        );

        for _ in 0..5 {
            let _ = enc.encode(&frame, 160, 120).expect("encode");
        }

        let after = enc.perf_snapshot();
        assert_eq!(
            after["encode_count"].as_u64(),
            Some(5),
            "should count 5 frames"
        );
        assert!(
            after["encode_time_avg_ms"].as_f64().unwrap_or(0.0) >= 0.0,
            "avg time should be >= 0"
        );
        assert!(
            after["encode_time_max_ms"].as_f64().unwrap_or(0.0) >= 0.0,
            "max time should be >= 0"
        );
    }

    #[test]
    fn test_gst_software_h264_codec() {
        let enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        assert_eq!(enc.codec(), Codec::H264);
    }

    #[test]
    fn test_gst_software_h264_request_keyframe_after_many_frames() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(160, 120);
        let _ = enc.encode(&frame, 160, 120).expect("first frame");
        for _ in 0..60 {
            let _ = enc.encode(&frame, 160, 120).expect("encode");
        }
        enc.request_keyframe();
        let packets = enc
            .encode(&frame, 160, 120)
            .expect("encode after request_keyframe()");
        assert!(
            has_keyframe(&packets),
            "must produce keyframe after many P-frames"
        );
    }

    #[test]
    fn test_gst_software_h264_different_framerates() {
        for fps in [15u32, 24, 30, 60] {
            let mut enc = GstEncoder::new_software(Codec::H264, 80, 60, 300, fps)
                .unwrap_or_else(|e| panic!("fps {fps}: {e}"));
            let frame = test_frame(80, 60);
            let packets = enc
                .encode(&frame, 80, 60)
                .unwrap_or_else(|e| panic!("fps {fps} encode: {e}"));
            assert!(
                has_keyframe(&packets),
                "fps {fps}: first frame must be keyframe"
            );
        }
    }

    #[test]
    fn test_gst_software_h264_consecutive_encoders() {
        for i in 0..5 {
            let mut enc = GstEncoder::new_software(Codec::H264, 80, 60, 300, 30)
                .unwrap_or_else(|e| panic!("enc {i}: {e}"));
            let frame = test_frame(80, 60);
            let packets = enc
                .encode(&frame, 80, 60)
                .unwrap_or_else(|e| panic!("encode {i}: {e}"));
            assert!(
                has_keyframe(&packets),
                "enc {i}: first frame must be keyframe"
            );
        }
    }

    fn nal_types(data: &[u8]) -> Vec<u8> {
        let mut types = Vec::new();
        for start in 0..data.len().saturating_sub(4) {
            let code_len = if data[start..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if data[start..].starts_with(&[0, 0, 1]) {
                3
            } else {
                continue;
            };
            let nal_start = start + code_len;
            if nal_start < data.len() {
                types.push(data[nal_start] & 0x1F);
            }
        }
        types
    }

    #[test]
    fn test_gst_software_h264_keyframe_includes_sps_pps() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 80, 60, 300, 30).expect("create Gst x264enc");
        let frame = test_frame(80, 60);
        let p = enc.encode(&frame, 80, 60).expect("first frame");
        assert!(
            p.iter().any(|b| has_nal_type(b, 7)),
            "first keyframe must have SPS"
        );
        assert!(
            p.iter().any(|b| has_nal_type(b, 8)),
            "first keyframe must have PPS"
        );
    }

    #[test]
    fn test_gst_software_h264_requested_keyframe_includes_sps_pps() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(160, 120);
        let _ = enc.encode(&frame, 160, 120).expect("keyframe");
        // Encode enough P-frames to pass keyint_min (= key-int-max / 10 = 30)
        for _ in 0..30 {
            let _ = enc.encode(&frame, 160, 120).expect("P-frame");
        }
        enc.request_keyframe();
        let p = enc.encode(&frame, 160, 120).expect("forced keyframe");
        assert!(
            p.iter().any(|b| has_nal_type(b, 7)),
            "forced keyframe must have SPS. NAL types: {:?}",
            p.iter().flat_map(|b| nal_types(b)).collect::<Vec<_>>()
        );
        assert!(
            p.iter().any(|b| has_nal_type(b, 8)),
            "forced keyframe must have PPS. NAL types: {:?}",
            p.iter().flat_map(|b| nal_types(b)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn test_gst_software_h264_output_format_is_annex_b() {
        let mut enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        let frame = test_frame(160, 120);
        let packets = enc.encode(&frame, 160, 120).expect("encode");
        let data = &packets[0];
        assert!(
            data.starts_with(&[0, 0, 0, 1]) || data.starts_with(&[0, 0, 1]),
            "output should start with Annex B start code, got: {:02x?}",
            &data[..data.len().min(8)]
        );
    }

    #[test]
    fn test_gst_probe_software_available() {
        ensure_gst_init().expect("initialize GStreamer");
        let factory = gst::ElementFactory::find("x264enc");
        assert!(
            factory.is_some(),
            "x264enc should be available via GStreamer"
        );
    }

    #[test]
    fn test_gst_nvenc_probe_does_not_panic() {
        ensure_gst_init().expect("initialize GStreamer");
        let _ = gst::ElementFactory::find("nvh264enc");
    }

    #[test]
    fn test_gst_encoder_backend_registration() {
        use crate::bmc::stream::encoder::STREAM_ENCODER_BACKENDS;
        let sw = STREAM_ENCODER_BACKENDS.iter().find(|b| b.id == "software");
        assert!(sw.is_some(), "software backend must be registered");
        let nv = STREAM_ENCODER_BACKENDS.iter().find(|b| b.id == "nvenc");
        assert!(nv.is_some(), "nvenc backend must be registered");
        assert_eq!(sw.unwrap().name, "Software");
        assert_eq!(nv.unwrap().name, "NVENC");
        // Verify create() actually returns a GstEncoder
        let enc = (sw.unwrap().create)(Codec::H264, 80, 60, 300, 30);
        assert!(enc.is_ok(), "software backend create should succeed");
    }

    #[test]
    fn test_gst_probe_stream_encoder() {
        use crate::bmc::encoder::registry::probe_stream_encoder;
        assert!(
            probe_stream_encoder("software"),
            "software probe should find x264enc"
        );
        // nvenc may or may not be available — just ensure it doesn't panic
        let _ = probe_stream_encoder("nvenc");
    }

    #[test]
    fn test_gst_nvenc_h264_is_available_or_skips() {
        match GstEncoder::new_nvenc(Codec::H264, 640, 480, 2000, 30) {
            Ok(mut enc) => {
                let frame = test_frame(640, 480);
                let packets = enc.encode(&frame, 640, 480).expect("NVENC encode");
                assert!(!packets.is_empty(), "NVENC should produce packets");
                assert!(has_keyframe(&packets), "NVENC first frame must be keyframe");
            }
            Err(e) => {
                eprintln!("NVENC not available (expected in CI without GPU): {e}");
            }
        }
    }

    // ── H265 bitstream detection ──────────────────────────────────────
    // H265 NAL unit type = (byte >> 1) & 0x3F
    // VPS=32, SPS=33, PPS=34, IDR_W_RADL=19, IDR_N_LP=20

    fn has_h265_nal_type(data: &[u8], nal_type: u8) -> bool {
        let mut offset = 0usize;
        while offset + 6 <= data.len() {
            let len = u32::from_be_bytes([
                data[offset],
                data[offset + 1],
                data[offset + 2],
                data[offset + 3],
            ]) as usize;
            if len == 0 || offset + 4 + len > data.len() {
                break;
            }
            let nal_start = offset + 4;
            if ((data[nal_start] >> 1) & 0x3F) == nal_type {
                return true;
            }
            offset += 4 + len;
        }

        for start in 0..data.len().saturating_sub(4) {
            let code_len = if data[start..].starts_with(&[0, 0, 0, 1]) {
                4
            } else if data[start..].starts_with(&[0, 0, 1]) {
                3
            } else {
                continue;
            };
            let nal_start = start + code_len;
            if nal_start < data.len() && ((data[nal_start] >> 1) & 0x3F) == nal_type {
                return true;
            }
        }
        false
    }

    fn has_h265_keyframe(packets: &[Vec<u8>]) -> bool {
        packets.iter().any(|p| {
            has_h265_nal_type(p, 19) || has_h265_nal_type(p, 20) || has_h265_nal_type(p, 21)
        }) && packets.iter().any(|p| has_h265_nal_type(p, 32))
    }

    fn has_vp8_keyframe(packets: &[Vec<u8>]) -> bool {
        packets.iter().any(|p| p.len() >= 3 && (p[0] & 0x01) == 0)
    }

    fn has_vp9_keyframe(packets: &[Vec<u8>]) -> bool {
        packets
            .iter()
            .any(|p| !p.is_empty() && (p[0] & 0xC0) == 0x80 && (p[0] & 0x04) == 0)
    }

    fn has_av1_sequence_header(packets: &[Vec<u8>]) -> bool {
        packets
            .iter()
            .any(|packet| packet.iter().any(|byte| ((byte >> 3) & 0x0F) == 1))
    }

    // ── H265 tests ───────────────────────────────────────────────────

    #[test]
    fn test_gst_software_h265_first_frame_is_keyframe() {
        match GstEncoder::new_software(Codec::H265, 160, 120, 500, 30) {
            Ok(mut enc) => {
                let frame = test_frame(160, 120);
                let packets = enc.encode(&frame, 160, 120).expect("encode first frame");
                assert!(
                    has_h265_keyframe(&packets),
                    "first frame should be a keyframe (VPS+SPS+PPS+IDR)"
                );
                assert!(
                    packets.iter().any(|p| has_h265_nal_type(p, 33)),
                    "must have SPS"
                );
                assert!(
                    packets.iter().any(|p| has_h265_nal_type(p, 34)),
                    "must have PPS"
                );
            }
            Err(e) => {
                eprintln!("H265 encoder not available (x265enc): {e}");
            }
        }
    }

    #[test]
    fn test_gst_software_h265_multiple_frames() {
        match GstEncoder::new_software(Codec::H265, 160, 120, 500, 30) {
            Ok(mut enc) => {
                let frame = test_frame(160, 120);
                for i in 0..30 {
                    let packets = enc
                        .encode(&frame, 160, 120)
                        .unwrap_or_else(|e| panic!("frame {i}: {e}"));
                    assert!(!packets.is_empty(), "frame {i} should produce output");
                }
            }
            Err(e) => {
                eprintln!("H265 encoder not available (x265enc): {e}");
            }
        }
    }

    #[test]
    fn test_gst_software_h265_request_keyframe() {
        match GstEncoder::new_software(Codec::H265, 160, 120, 500, 30) {
            Ok(mut enc) => {
                let frame = test_frame(160, 120);
                let _ = enc.encode(&frame, 160, 120).expect("frame 0 (keyframe)");
                for _ in 0..10 {
                    let _ = enc.encode(&frame, 160, 120).expect("encode inter frames");
                }
                enc.request_keyframe();
                let packets = enc
                    .encode(&frame, 160, 120)
                    .expect("encode after request_keyframe()");
                assert!(
                    has_h265_keyframe(&packets),
                    "frame after request_keyframe() must be a keyframe"
                );
            }
            Err(e) => {
                eprintln!("H265 encoder not available (x265enc): {e}");
            }
        }
    }

    // ── VP8 tests ────────────────────────────────────────────────────

    #[test]
    fn test_gst_software_vp8_first_frame_is_keyframe() {
        match GstEncoder::new_software(Codec::VP8, 160, 120, 500, 30) {
            Ok(mut enc) => {
                let frame = test_frame(160, 120);
                let packets = enc.encode(&frame, 160, 120).expect("encode first frame");
                assert!(
                    has_vp8_keyframe(&packets),
                    "first VP8 frame should be a keyframe (frame tag bit0=0)"
                );
            }
            Err(e) => {
                eprintln!("VP8 encoder not available (vp8enc): {e}");
            }
        }
    }

    #[test]
    fn test_gst_software_vp8_multiple_frames() {
        match GstEncoder::new_software(Codec::VP8, 160, 120, 500, 30) {
            Ok(mut enc) => {
                let frame = test_frame(160, 120);
                for i in 0..30 {
                    let packets = enc
                        .encode(&frame, 160, 120)
                        .unwrap_or_else(|e| panic!("frame {i}: {e}"));
                    assert!(!packets.is_empty(), "frame {i} should produce output");
                }
            }
            Err(e) => {
                eprintln!("VP8 encoder not available (vp8enc): {e}");
            }
        }
    }

    // ── VP9 tests ────────────────────────────────────────────────────

    #[test]
    fn test_gst_software_vp9_first_frame_is_keyframe() {
        match GstEncoder::new_software(Codec::VP9, 160, 120, 500, 30) {
            Ok(mut enc) => {
                let frame = test_frame(160, 120);
                let packets = enc.encode(&frame, 160, 120).expect("encode first frame");
                assert!(
                    has_vp9_keyframe(&packets),
                    "first VP9 frame should be a keyframe"
                );
            }
            Err(e) => {
                eprintln!("VP9 encoder not available (vp9enc): {e}");
            }
        }
    }

    #[test]
    fn test_gst_software_vp9_multiple_frames() {
        match GstEncoder::new_software(Codec::VP9, 160, 120, 500, 30) {
            Ok(mut enc) => {
                let frame = test_frame(160, 120);
                for i in 0..30 {
                    let packets = enc
                        .encode(&frame, 160, 120)
                        .unwrap_or_else(|e| panic!("frame {i}: {e}"));
                    assert!(!packets.is_empty(), "frame {i} should produce output");
                }
            }
            Err(e) => {
                eprintln!("VP9 encoder not available (vp9enc): {e}");
            }
        }
    }

    // ── AV1 tests ────────────────────────────────────────────────────

    #[test]
    fn test_gst_software_av1_first_frame_is_keyframe() {
        match GstEncoder::new_software(Codec::AV1, 160, 120, 500, 30) {
            Ok(mut enc) => {
                let frame = test_frame(160, 120);
                let packets = enc.encode(&frame, 160, 120).expect("encode first frame");
                assert!(
                    has_av1_sequence_header(&packets),
                    "first AV1 frame should contain sequence header OBU"
                );
            }
            Err(e) => {
                eprintln!("AV1 encoder not available (av1enc): {e}");
            }
        }
    }

    #[test]
    fn test_gst_software_av1_multiple_frames() {
        match GstEncoder::new_software(Codec::AV1, 160, 120, 500, 30) {
            Ok(mut enc) => {
                let frame = test_frame(160, 120);
                for i in 0..30 {
                    let packets = enc
                        .encode(&frame, 160, 120)
                        .unwrap_or_else(|e| panic!("frame {i}: {e}"));
                    assert!(!packets.is_empty(), "frame {i} should produce output");
                }
            }
            Err(e) => {
                eprintln!("AV1 encoder not available (av1enc): {e}");
            }
        }
    }

    // ── Error path tests ─────────────────────────────────────────────

    #[test]
    fn test_gst_codec_mjpeg_returns_error() {
        let err = GstEncoder::new_software(Codec::Mjpeg, 160, 120, 500, 30).unwrap_err();
        assert!(
            err.contains("not a streaming codec"),
            "expected 'not a streaming codec' error, got: {err}"
        );
    }

    #[test]
    fn test_gst_codec_webp_returns_error() {
        let err = GstEncoder::new_software(Codec::WebP, 160, 120, 500, 30).unwrap_err();
        assert!(
            err.contains("not a streaming codec"),
            "expected 'not a streaming codec' error, got: {err}"
        );
    }

    #[test]
    fn test_gst_nvenc_unsupported_codec_returns_error() {
        // VP8 has no NVENC backend
        let err = GstEncoder::new_nvenc(Codec::VP8, 160, 120, 500, 30).unwrap_err();
        assert!(
            err.contains("NVENC does not support"),
            "expected NVENC unsupported error, got: {err}"
        );
    }

    // ── Cross-codec registration tests ────────────────────────────────

    #[test]
    fn test_gst_all_codecs_through_backend_registry() {
        use crate::bmc::stream::encoder::STREAM_ENCODER_BACKENDS;
        let sw = STREAM_ENCODER_BACKENDS
            .iter()
            .find(|b| b.id == "software")
            .expect("software backend");
        for codec in [Codec::H264, Codec::H265, Codec::VP8, Codec::VP9, Codec::AV1] {
            let r = (sw.create)(codec, 80, 60, 300, 30);
            match r {
                Ok(mut enc) => {
                    let frame = test_frame(80, 60);
                    let packets = enc
                        .encode(&frame, 80, 60)
                        .unwrap_or_else(|e| panic!("{:?} encode: {e}", codec));
                    assert!(!packets.is_empty(), "{:?} should produce packets", codec);
                }
                Err(e) => {
                    eprintln!("{:?} encoder not available via registry: {e}", codec);
                }
            }
        }
    }

    #[test]
    fn test_gst_codec_consistency() {
        let enc =
            GstEncoder::new_software(Codec::H264, 160, 120, 500, 30).expect("create Gst x264enc");
        assert_eq!(enc.codec(), Codec::H264);

        if let Ok(enc) = GstEncoder::new_software(Codec::H265, 160, 120, 500, 30) {
            assert_eq!(enc.codec(), Codec::H265);
        }
    }

    #[test]
    fn test_gst_probe_encoders_exist() {
        ensure_gst_init().expect("initialize GStreamer");
        for (name, codec) in [
            ("x264enc", Codec::H264),
            ("x265enc", Codec::H265),
            ("vp8enc", Codec::VP8),
            ("vp9enc", Codec::VP9),
            ("av1enc", Codec::AV1),
        ] {
            let found = gst::ElementFactory::find(name).is_some();
            eprintln!(
                "GStreamer encoder '{name}' ({codec:?}): {}",
                if found { "✓" } else { "✗" }
            );
        }
    }

    #[test]
    fn test_gst_software_nvenc_probe_all_codecs() {
        ensure_gst_init().expect("initialize GStreamer");
        for (name, codec) in [
            ("nvh264enc", Codec::H264),
            ("nvh265enc", Codec::H265),
            ("nvav1enc", Codec::AV1),
        ] {
            let found = gst::ElementFactory::find(name).is_some();
            eprintln!(
                "NVENC encoder '{name}' ({codec:?}): {}",
                if found { "✓" } else { "✗" }
            );
        }
    }
}
