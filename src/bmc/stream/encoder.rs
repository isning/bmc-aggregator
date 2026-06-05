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

        // Patch SPS constraint_set2_flag to match SDP profile-level-id=42e01f
        // NVENC outputs 42c01f (constraint_set2=0) but Chrome expects 42e01f (constraint_set2=1).
        // SPS layout: [start_code] 67 {profile_idc} {constraint_flags} {level_idc} ...
        // Setting bit 5 (0x20) in constraint_flags byte = constraint_set2_flag = 1.
        for pkt in &mut packets {
            let mut i = 0;
            while i + 4 < pkt.len() {
                if pkt[i..i+4] == [0, 0, 0, 1] || (i + 3 <= pkt.len() && pkt[i..i+3] == [0, 0, 1]) {
                    let nal_start = if pkt[i+2] == 1 { i + 3 } else { i + 4 };
                    if nal_start < pkt.len() && (pkt[nal_start] & 0x1F) == 7 {
                        // SPS NALU: profile_idc at nal_start+1, constraint_flags at nal_start+2
                        if nal_start + 2 < pkt.len() {
                            pkt[nal_start + 2] |= 0x20;
                        }
                        break;
                    }
                    i = nal_start;
                } else {
                    i += 1;
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
}
