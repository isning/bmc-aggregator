use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ffmpeg_next::codec;
use ffmpeg_next::encoder;
use ffmpeg_next::format::Pixel;
use ffmpeg_next::frame::Video;
use ffmpeg_next::software::scaling::{self, Context};
use ffmpeg_next::{Dictionary, Error, Packet, Rational};

use crate::bmc::encoder::Codec;

pub trait VideoStreamEncoder: Send {
    fn encode(&mut self, rgba: &[u8], width: u32, height: u32) -> Result<Vec<Vec<u8>>, String>;
    fn codec(&self) -> Codec;
    fn request_keyframe(&self);
    fn perf_snapshot(&self) -> serde_json::Value { serde_json::json!({}) }
}

type FfmpegResult<T> = Result<T, String>;

// ── Shared encode core ────────────────────────────────────────────

struct FfmpegCore {
    encoder: encoder::Video,
    scaler: Option<Context>,
    scaler_yuv: Option<Context>,
    width: u32,
    height: u32,
    pts: i64,
    keyframe_requested: Arc<AtomicBool>,
    encode_count: AtomicU64,
    encode_time_total_us: AtomicU64,
    encode_time_max_us: AtomicU64,
    // Cached SPS/PPS from the first keyframe. Used to re-inject them into
    // subsequent keyframes, because some encoders (notably NVENC) only output
    // SPS/PPS on the very first frame. WebRTC's H264Payloader emits SPS/PPS
    // via a STAP-A only once; if Chrome misses that packet it can never
    // decode any frame.  By re-inserting SPS/PPS into every keyframe we
    // guarantee each keyframe stands alone.
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
}

impl FfmpegCore {
    fn new(
        encoder_name: &str,
        pixel_format: Pixel,
        width: u32,
        height: u32,
        bitrate_kbps: u32,
        framerate: u32,
        opts: Dictionary,
    ) -> FfmpegResult<Self> {
        ffmpeg_next::init().map_err(|e| format!("ffmpeg init: {e}"))?;

        let enc = encoder::find_by_name(encoder_name)
            .ok_or_else(|| format!("encoder '{encoder_name}' not found"))?;

        let ctx = codec::Context::new_with_codec(enc);
        let mut video = ctx.encoder().video()
            .map_err(|e| format!("encoder video: {e}"))?;
        video.set_bit_rate((bitrate_kbps * 1000) as usize);
        video.set_width(width);
        video.set_height(height);
        video.set_format(pixel_format);
        video.set_frame_rate(Some(Rational::new(framerate as i32, 1)));
        video.set_time_base(Rational::new(1, framerate as i32));
        video.set_max_b_frames(0);

        let enc = video.open_with(opts)
            .map_err(|e| format!("encoder open: {e}"))?;

        Ok(Self {
            encoder: enc,
            scaler: None,
            scaler_yuv: None,
            width,
            height,
            pts: 0,
            keyframe_requested: Arc::new(AtomicBool::new(false)),
            encode_count: AtomicU64::new(0),
            encode_time_total_us: AtomicU64::new(0),
            encode_time_max_us: AtomicU64::new(0),
            sps: None,
            pps: None,
        })
    }

    fn encode_frame(&mut self, rgba: &[u8], pixel_format: Pixel) -> FfmpegResult<Vec<Vec<u8>>> {
        let start = Instant::now();
        let w = self.width;
        let h = self.height;

        if rgba.len() < (w * h * 4) as usize {
            return Err(format!(
                "buffer too small: expected {}, got {}",
                w * h * 4,
                rgba.len()
            ));
        }

        let mut src = Video::new(Pixel::RGBA, w, h);
        src.data_mut(0).copy_from_slice(&rgba[..(w * h * 4) as usize]);

        let scaler = self.get_scaler(pixel_format, w, h)?;

        let mut dst = Video::new(pixel_format, w, h);
        scaler.run(&src, &mut dst)
            .map_err(|e| format!("swscale: {e}"))?;

        dst.set_pts(Some(self.pts));
        self.pts += 1;

        if self.keyframe_requested.swap(false, Ordering::SeqCst) {
            // Force this frame to be a keyframe by setting picture type
            // to I-frame directly on the AVFrame. gop_size cannot be
            // changed after the encoder is open(), so we must tell the
            // encoder via the input frame's type field.
            unsafe {
                let ptr = dst.as_mut_ptr() as *mut ffmpeg_next::sys::AVFrame;
                (*ptr).pict_type = ffmpeg_next::sys::AVPictureType::AV_PICTURE_TYPE_I;
            }
        }

        self.encoder.send_frame(&dst)
            .map_err(|e| format!("send_frame: {e}"))?;

        let mut packets = Vec::new();
        let mut packet = Packet::empty();
        loop {
            match self.encoder.receive_packet(&mut packet) {
                Ok(()) => {
                    if let Some(data) = packet.data() {
                        if !data.is_empty() {
                            packets.push(data.to_vec());
                        }
                    }
                    packet = Packet::empty();
                }
                Err(Error::Eof) | Err(Error::Other { errno: ffmpeg_next::util::error::EAGAIN }) => break,
                Err(e) => return Err(format!("receive_packet: {e}")),
            }
        }

        // ── SPS/PPS management ──────────────────────────────────────
        // 1. Scan packets and cache the first SPS and PPS we see.
        // 2. If this is a keyframe and we have cached SPS/PPS, prepend them
        //    to the first packet so every keyframe carries its own headers.
        let mut has_idr = false;
        let mut found_sps = false;
        let mut found_pps = false;
        for pkt in &mut packets {
            let mut i = 0;
            while i + 4 < pkt.len() {
                if pkt[i..i+4] == [0, 0, 0, 1] || (i + 3 <= pkt.len() && pkt[i..i+3] == [0, 0, 1]) {
                    let nal_start = if pkt[i+2] == 1 { i + 3 } else { i + 4 };
                    if nal_start < pkt.len() {
                        let nal_type = pkt[nal_start] & 0x1F;
                        if nal_type == 7 && !found_sps {
                            let start_pos = if pkt[i+2] == 1 { i } else { i };
                            self.sps = Some(pkt[start_pos..].to_vec());
                            found_sps = true;
                            if nal_start + 2 < pkt.len() {
                                pkt[nal_start + 2] |= 0x20;
                            }
                        } else if nal_type == 8 && !found_pps {
                            let start_pos = if pkt[i+2] == 1 { i } else { i };
                            self.pps = Some(pkt[start_pos..].to_vec());
                            found_pps = true;
                        } else if nal_type == 5 {
                            has_idr = true;
                        }
                    }
                    i = nal_start;
                } else {
                    i += 1;
                }
            }
        }

        // Re-inject cached SPS/PPS into keyframes that lack them.
        if has_idr && (!found_sps || !found_pps) {
            if let (Some(sps), Some(pps)) = (self.sps.as_ref(), self.pps.as_ref()) {
                if let Some(first) = packets.first_mut() {
                    let mut new_pkt = sps.clone();
                    new_pkt.extend_from_slice(pps);
                    new_pkt.extend_from_slice(first);
                    *first = new_pkt;
                    tracing::trace!("prepended SPS/PPS to keyframe (found_sps={found_sps} found_pps={found_pps})");
                }
            }
        }

        let elapsed_us = start.elapsed().as_micros() as u64;
        self.encode_count.fetch_add(1, Ordering::Relaxed);
        self.encode_time_total_us.fetch_add(elapsed_us, Ordering::Relaxed);
        let prev_max = self.encode_time_max_us.load(Ordering::Relaxed);
        if elapsed_us > prev_max {
            self.encode_time_max_us.store(elapsed_us, Ordering::Relaxed);
        }

        Ok(packets)
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

    fn get_scaler(&mut self, dst_fmt: Pixel, width_: u32, height_: u32) -> FfmpegResult<&mut Context> {
        let scaler = match dst_fmt {
            Pixel::NV12 => &mut self.scaler,
            Pixel::YUV420P | Pixel::YUVJ420P => &mut self.scaler_yuv,
            _ => &mut self.scaler,
        };

        if scaler.is_none() {
            let ctx = Context::get(
                Pixel::RGBA, width_, height_,
                dst_fmt, width_, height_,
                scaling::Flags::BILINEAR,
            ).map_err(|e| format!("scaler init: {e}"))?;
            *scaler = Some(ctx);
        }

        Ok(scaler.as_mut().unwrap())
    }
}

impl Drop for FfmpegCore {
    fn drop(&mut self) {
        let _ = self.encoder.send_eof();
    }
}

unsafe impl Send for FfmpegCore {}
unsafe impl Sync for FfmpegCore {}

// ── Software ffmpeg encoder (libx264 / libx265 / libvpx / libsvtav1) ──

pub struct SoftwareFfmpegEncoder {
    core: FfmpegCore,
    codec: Codec,
    pixel_format: Pixel,
}

impl SoftwareFfmpegEncoder {
    pub fn new(codec: Codec, width: u32, height: u32, bitrate_kbps: u32, framerate: u32) -> FfmpegResult<Self> {
        let (name, fmt, opts) = software_encoder_config(codec)?;
        let core = FfmpegCore::new(name, fmt, width, height, bitrate_kbps, framerate, opts)?;
        Ok(Self { core, codec, pixel_format: fmt })
    }
}

impl VideoStreamEncoder for SoftwareFfmpegEncoder {
    fn encode(&mut self, rgba: &[u8], width: u32, height: u32) -> FfmpegResult<Vec<Vec<u8>>> {
        if width != self.core.width || height != self.core.height {
            return Err(format!(
                "resolution mismatch: expected {}x{}, got {}x{}",
                self.core.width, self.core.height, width, height
            ));
        }
        self.core.encode_frame(rgba, self.pixel_format)
    }

    fn codec(&self) -> Codec {
        self.codec
    }

    fn request_keyframe(&self) {
        self.core.request_keyframe()
    }

    fn perf_snapshot(&self) -> serde_json::Value {
        self.core.perf_snapshot()
    }
}

fn software_encoder_config(codec: Codec) -> FfmpegResult<(&'static str, Pixel, Dictionary<'static>)> {
    let (name, fmt): (&str, Pixel) = match codec {
        Codec::H264 => ("libx264", Pixel::NV12),
        Codec::H265 => ("libx265", Pixel::YUV420P),
        Codec::VP8 => ("libvpx", Pixel::YUV420P),
        Codec::VP9 => ("libvpx-vp9", Pixel::YUV420P),
        Codec::AV1 => ("libsvtav1", Pixel::YUV420P),
        _ => return Err(format!("codec {codec:?} is not a streaming codec")),
    };

    let mut opts = Dictionary::new();
    match codec {
        Codec::H264 => {
            opts.set("preset", "ultrafast");
            opts.set("tune", "zerolatency");
            opts.set("profile", "baseline");
        }
        Codec::H265 => {
            opts.set("preset", "ultrafast");
            opts.set("tune", "zerolatency");
        }
        Codec::VP8 | Codec::VP9 => {
            opts.set("deadline", "realtime");
            opts.set("cpu-used", "8");
            opts.set("lag-in-frames", "0");
            opts.set("error-resilient", "1");
        }
        Codec::AV1 => {
            opts.set("preset", "8");
        }
        _ => {}
    }

    Ok((name, fmt, opts))
}

// ── NVENC ffmpeg encoder (h264_nvenc / h265_nvenc) ──

pub struct NvencFfmpegEncoder {
    core: FfmpegCore,
    codec: Codec,
    pixel_format: Pixel,
}

impl NvencFfmpegEncoder {
    pub fn new(codec: Codec, width: u32, height: u32, bitrate_kbps: u32, framerate: u32) -> FfmpegResult<Self> {
        let (name, fmt, opts) = nvenc_encoder_config(codec)?;
        let core = FfmpegCore::new(name, fmt, width, height, bitrate_kbps, framerate, opts)?;
        Ok(Self { core, codec, pixel_format: fmt })
    }
}

impl VideoStreamEncoder for NvencFfmpegEncoder {
    fn encode(&mut self, rgba: &[u8], width: u32, height: u32) -> FfmpegResult<Vec<Vec<u8>>> {
        if width != self.core.width || height != self.core.height {
            return Err(format!(
                "resolution mismatch: expected {}x{}, got {}x{}",
                self.core.width, self.core.height, width, height
            ));
        }
        self.core.encode_frame(rgba, self.pixel_format)
    }

    fn codec(&self) -> Codec {
        self.codec
    }

    fn request_keyframe(&self) {
        self.core.request_keyframe()
    }

    fn perf_snapshot(&self) -> serde_json::Value {
        self.core.perf_snapshot()
    }
}

/// A registered stream encoder backend.
pub struct StreamEncoderBackend {
    pub id: &'static str,
    pub name: &'static str,
    pub create: fn(Codec, u32, u32, u32, u32) -> FfmpegResult<Box<dyn VideoStreamEncoder>>,
}

impl crate::bmc::registry::HasId for StreamEncoderBackend {
    fn id(&self) -> &str {
        self.id
    }
}

/// Inventory of all known stream encoder backends.
/// Adding a new backend means adding one entry here — no match arms to edit.
pub static STREAM_ENCODER_BACKENDS: &[StreamEncoderBackend] = &[
    StreamEncoderBackend {
        id: "software",
        name: "Software",
        create: |codec, w, h, bitrate, fps| {
            SoftwareFfmpegEncoder::new(codec, w, h, bitrate, fps)
                .map(|e| Box::new(e) as Box<dyn VideoStreamEncoder>)
        },
    },
    StreamEncoderBackend {
        id: "nvenc",
        name: "NVENC",
        create: |codec, w, h, bitrate, fps| {
            NvencFfmpegEncoder::new(codec, w, h, bitrate, fps)
                .map(|e| Box::new(e) as Box<dyn VideoStreamEncoder>)
        },
    },
];

/// Look up a stream encoder backend by ID and create an encoder.
/// No silent fallback: returns an error if the backend is unknown or fails to init.
pub fn create_stream_encoder(
    codec: Codec,
    backend: &str,
    width: u32,
    height: u32,
    bitrate_kbps: u32,
    framerate: u32,
) -> FfmpegResult<Box<dyn VideoStreamEncoder>> {
    crate::bmc::registry::find_backend(STREAM_ENCODER_BACKENDS, backend)
        .map(|b| (b.create)(codec, width, height, bitrate_kbps, framerate))
        .unwrap_or_else(|| Err(format!("unknown stream encoder backend: '{backend}'")))
}

fn nvenc_encoder_config(codec: Codec) -> FfmpegResult<(&'static str, Pixel, Dictionary<'static>)> {
    let (name, fmt, extra_opts): (&str, Pixel, Dictionary) = match codec {
        Codec::H264 => {
            let mut o = Dictionary::new();
            o.set("preset", "p2");
            o.set("tune", "ull");
            o.set("zerolatency", "1");
            o.set("profile", "baseline");
            // Force CAVLC (not CABAC) for Baseline profile compatibility
            o.set("coder", "cavlc");
            ("h264_nvenc", Pixel::NV12, o)
        }
        Codec::H265 => {
            let mut o = Dictionary::new();
            o.set("preset", "p2");
            o.set("tune", "ull");
            o.set("zerolatency", "1");
            ("hevc_nvenc", Pixel::YUV420P, o)
        }
        Codec::AV1 => {
            let mut o = Dictionary::new();
            o.set("preset", "p1");
            o.set("tune", "ull");
            o.set("rc", "cbr");
            ("av1_nvenc", Pixel::YUV420P, o)
        }
        _ => return Err(format!("NVENC does not support codec {codec:?}")),
    };

    Ok((name, fmt, extra_opts))
}

#[cfg(test)]
mod tests {
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
        for start in 0..data.len().saturating_sub(4) {
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
        packets.iter().any(|p| has_nal_type(p, 5) || has_nal_type(p, 7))
    }

    fn has_pframe(packets: &[Vec<u8>]) -> bool {
        packets.iter().any(|p| has_nal_type(p, 1) && !has_keyframe(packets))
    }

    #[test]
    fn test_vp8_encoder_creation() {
        let start = std::time::Instant::now();
        let result = std::panic::catch_unwind(|| {
            SoftwareFfmpegEncoder::new(Codec::VP8, 1024, 768, 2000, 30)
        });
        let elapsed = start.elapsed();
        match result {
            Ok(Ok(_)) => eprintln!("VP8 encoder OK in {:?}", elapsed),
            Ok(Err(e)) => eprintln!("VP8 encoder FAILED after {:?}: {}", elapsed, e),
            Err(p) => eprintln!("VP8 encoder PANICKED after {:?}: {:?}", elapsed, p),
        }
        assert!(elapsed.as_secs() < 30, "VP8 encoder creation took >30s -- possible hang");
    }

    #[test]
    fn test_h264_first_frame_is_keyframe() {
        let mut enc = SoftwareFfmpegEncoder::new(Codec::H264, 160, 120, 500, 30)
            .expect("create H264 encoder");
        let frame = test_frame(160, 120);
        let packets = enc.encode(&frame, 160, 120).expect("encode first frame");
        assert!(has_keyframe(&packets), "first frame should be a keyframe (SPS/IDR)");
    }

    #[test]
    fn test_h264_pframes_not_keyframes() {
        let mut enc = SoftwareFfmpegEncoder::new(Codec::H264, 160, 120, 500, 30)
            .expect("create H264 encoder");
        let frame = test_frame(160, 120);
        let _ = enc.encode(&frame, 160, 120).expect("frame 0 (keyframe)");
        for i in 1..5 {
            let packets = enc.encode(&frame, 160, 120)
                .unwrap_or_else(|e| panic!("frame {i}: {e}"));
            assert!(!has_keyframe(&packets), "frame {i} should NOT be a keyframe");
            assert!(has_pframe(&packets), "frame {i} should be a P-frame");
        }
    }

    #[test]
    fn test_h264_request_keyframe_forces_keyframe() {
        let mut enc = SoftwareFfmpegEncoder::new(Codec::H264, 160, 120, 500, 30)
            .expect("create H264 encoder");
        let frame = test_frame(160, 120);
        let _ = enc.encode(&frame, 160, 120).expect("frame 0 (keyframe)");
        for i in 1..5 {
            let _ = enc.encode(&frame, 160, 120)
                .unwrap_or_else(|e| panic!("frame {i}: {e}"));
        }
        enc.request_keyframe();
        let packets = enc.encode(&frame, 160, 120)
            .expect("encode after request_keyframe()");
        assert!(has_keyframe(&packets), "frame after request_keyframe() must be a keyframe");
    }

    #[test]
    fn test_h264_no_toctou_race_on_flag() {
        let mut enc = SoftwareFfmpegEncoder::new(Codec::H264, 160, 120, 500, 30)
            .expect("create H264 encoder");
        let frame = test_frame(160, 120);
        let _ = enc.encode(&frame, 160, 120).expect("frame 0 (keyframe)");
        for i in 1..5 {
            let _ = enc.encode(&frame, 160, 120)
                .unwrap_or_else(|e| panic!("frame {i}: {e}"));
        }

        enc.request_keyframe();
        let packets = enc.encode(&frame, 160, 120).expect("encode after request");
        assert!(has_keyframe(&packets), "request_keyframe must force keyframe");

        // Without another request, the next encode must NOT be a keyframe
        let packets2 = enc.encode(&frame, 160, 120).expect("encode without request");
        assert!(!has_keyframe(&packets2), "no request → should NOT be a keyframe");
    }

    #[test]
    fn test_h264_multiple_keyframe_requests_all_served() {
        // Regression: multiple consecutive request_keyframe() calls
        // should each produce a keyframe, not lose flags.
        let mut enc = SoftwareFfmpegEncoder::new(Codec::H264, 160, 120, 500, 30)
            .expect("create H264 encoder");
        let frame = test_frame(160, 120);
        let _ = enc.encode(&frame, 160, 120).expect("frame 0 (keyframe)");
        for i in 1..5 {
            let _ = enc.encode(&frame, 160, 120)
                .unwrap_or_else(|e| panic!("frame {i}: {e}"));
        }

        for round in 0..3 {
            enc.request_keyframe();
            let packets = enc.encode(&frame, 160, 120)
                .unwrap_or_else(|e| panic!("round {round}: {e}"));
            assert!(has_keyframe(&packets), "round {round}: request must produce keyframe");
        }
    }

    #[test]
    fn test_nvenc_h264_is_annexb() {
        let _ = ffmpeg_next::init();
        let mut enc = match NvencFfmpegEncoder::new(Codec::H264, 640, 480, 2000, 30) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("NVENC not available (expected in CI without GPU): {e}");
                return;
            }
        };
        let frame = test_frame(640, 480);
        // NVENC may buffer frames internally — push several to flush output
        let mut output = Vec::new();
        for i in 0..10 {
            output = enc.encode(&frame, 640, 480).expect("NVENC encode");
            if !output.is_empty() {
                break;
            }
        }
        assert!(!output.is_empty(), "NVENC should produce packets after warmup");
        for (i, pkt) in output.iter().enumerate() {
            let preview: Vec<String> = pkt.iter().take(8).map(|b| format!("{b:02x}")).collect();
            eprintln!("  NVENC packet[{i}]: {} bytes, starts with {}", pkt.len(), preview.join(" "));
            if !pkt.is_empty() {
                let is_annexb = pkt[0] == 0 && pkt[1] == 0 && (pkt[2] == 1 || (pkt.len() > 3 && pkt[2] == 0 && pkt[3] == 1));
                assert!(is_annexb, "NVENC output must be Annex B format, got: {:02x} {:02x} {:02x}", pkt[0], pkt[1], pkt.get(2).copied().unwrap_or(0));
                assert!(has_nal_type(pkt, 7), "NVENC keyframe must contain SPS (NAL type 7)");
                eprintln!("  Has SPS: {}, Has PPS: {}, Has IDR: {}",
                    has_nal_type(pkt, 7), has_nal_type(pkt, 8), has_nal_type(pkt, 5));
            }
        }
    }

    /// Dump ALL NAL units from NVENC output to diagnose SPS/PPS presence.
    /// Runs only if NVENC is available; prints the NAL types and total byte count.
    #[test]
    fn test_nvenc_h264_dump_nalus() {
        let _ = ffmpeg_next::init();
        let mut enc = match NvencFfmpegEncoder::new(Codec::H264, 640, 480, 2000, 30) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("NVENC not available (expected in CI without GPU): {e}");
                return;
            }
        };
        let frame = test_frame(640, 480);
        // Warmup: push frames to flush encoder pipeline
        let mut output = Vec::new();
        for _ in 0..10 {
            output = enc.encode(&frame, 640, 480).expect("NVENC encode");
            if !output.is_empty() {
                break;
            }
        }
        assert!(!output.is_empty(), "NVENC output should not be empty");
        eprintln!("NVENC produced {} packets, total {} bytes",
            output.len(),
            output.iter().map(|p| p.len()).sum::<usize>());
        // Scan each packet for NAL units
        let mut packet_buf = Vec::new();
        for pkt in &output {
            packet_buf.extend_from_slice(pkt);
        }
        // Scan the concatenated buffer for NAL unit start codes
        let mut nal_count = 0;
        let mut i = 0;
        while i + 4 <= packet_buf.len() {
            let start_code_len = if packet_buf[i] == 0 && packet_buf[i+1] == 0 && packet_buf[i+2] == 0 && packet_buf[i+3] == 1 {
                4
            } else if packet_buf[i] == 0 && packet_buf[i+1] == 0 && packet_buf[i+2] == 1 {
                3
            } else {
                i += 1;
                continue;
            };
            let nal_start = i + start_code_len;
            if nal_start >= packet_buf.len() {
                break;
            }
            let nal_type = packet_buf[nal_start] & 0x1F;
            let nri = (packet_buf[nal_start] >> 5) & 3;
            let nal_size = packet_buf.len() - nal_start;
            eprintln!("  NAL[{nal_count}]: type={nal_type}, nri={nri}, offset={nal_start}, size={nal_size}");
            if nal_type == 7 {
                // Print the SPS bytes (first 8 bytes after NAL header)
                let sps_preview: Vec<String> = packet_buf[nal_start..(nal_start+8).min(packet_buf.len())]
                    .iter().map(|b| format!("{b:02x}")).collect();
                eprintln!("    SPS bytes: {}", sps_preview.join(" "));
            }
            nal_count += 1;
            // Skip past the NAL unit (find next start code)
            i = nal_start.saturating_sub(1);
        }
        eprintln!("Total NAL units found: {nal_count}");
        assert!(nal_count > 0, "Should find at least one NAL unit");
        assert!(packet_buf.iter().any(|&b| (b & 0x1F) == 7), "Should contain SPS (type 7)");
    }

    /// Dump NAL unit structure from libx264 vs NVENC to compare their output format.
    /// This helps diagnose why Chrome rejects NVENC's H264 stream.
    #[test]
    fn test_compare_h264_nal_structure() {
        let _ = ffmpeg_next::init();

        // ── libx264 ──
        let mut sw = SoftwareFfmpegEncoder::new(Codec::H264, 640, 480, 2000, 30)
            .expect("create software H264 encoder");
        let frame = test_frame(640, 480);
        let sw_packets = sw.encode(&frame, 640, 480).expect("software encode");
        eprintln!("=== libx264 ===");
        dump_nalus("sw", &sw_packets);

        // ── NVENC ──
        let mut nv = match NvencFfmpegEncoder::new(Codec::H264, 640, 480, 2000, 30) {
            Ok(e) => e,
            Err(e) => {
                eprintln!("NVENC not available: {e}");
                return;
            }
        };
        let mut nv_output = Vec::new();
        for _ in 0..10 {
            nv_output = nv.encode(&frame, 640, 480).expect("NVENC encode");
            if !nv_output.is_empty() {
                break;
            }
        }
        eprintln!("=== NVENC ===");
        dump_nalus("nv", &nv_output);

        // Verify both produce Annex B keyframes with SPS/PPS
        if !sw_packets.is_empty() && !nv_output.is_empty() {
            assert!(has_nal_type(&sw_packets[0], 7), "sw keyframe must have SPS");
            assert!(has_nal_type(&nv_output[0], 7), "NVENC keyframe must have SPS");
        }
    }

    /// Scan packets for NAL unit start codes and print NAL type / offset / size.
    fn dump_nalus(label: &str, packets: &[Vec<u8>]) {
        let total: usize = packets.iter().map(|p| p.len()).sum();
        eprintln!("{label}: {} packets, {} total bytes", packets.len(), total);
        let mut buf = Vec::new();
        for pkt in packets {
            buf.extend_from_slice(pkt);
        }
        let mut count = 0;
        let mut i = 0;
        while i + 4 <= buf.len() {
            let start_len = if buf[i] == 0 && buf[i+1] == 0 && buf[i+2] == 0 && buf[i+3] == 1 {
                4
            } else if i + 3 <= buf.len() && buf[i] == 0 && buf[i+1] == 0 && buf[i+2] == 1 {
                3
            } else {
                i += 1;
                continue;
            };
            let nal_start = i + start_len;
            if nal_start >= buf.len() { break; }
            let nal_type = buf[nal_start] & 0x1F;
            let nri = (buf[nal_start] >> 5) & 3;
            let size = buf.len() - nal_start;
            eprintln!("  {label} NAL[{count}]: type={nal_type} nri={nri} offset={nal_start} size={size}");
            if nal_type == 7 {
                let preview: Vec<String> = buf[nal_start..(nal_start + 10).min(buf.len())]
                    .iter().map(|b| format!("{b:02x}")).collect();
                eprintln!("    SPS: {}", preview.join(" "));
            }
            count += 1;
            i = nal_start.saturating_sub(1);
        }
        eprintln!("  {label} total NAL units: {count}");
    }
}
