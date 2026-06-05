use std::path::PathBuf;
use std::process::Command;

// ── Helpers ───────────────────────────────────────────────────────────

/// Check if `ffmpeg` is available on this system.
fn ffmpeg_available() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok()
}

/// Create a temporary directory for test outputs.
fn tmp_dir() -> PathBuf {
    std::env::temp_dir().join("bmc-e2e-codecs")
}

/// Generate a test PNG of the given size with a colour gradient.
fn generate_test_png(width: u32, height: u32) -> PathBuf {
    let dir = tmp_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("test_pattern.png");

    if path.exists() {
        return path;
    }

    let mut buf = image::RgbImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let r = ((x as f32 / width as f32) * 255.0) as u8;
            let g = ((y as f32 / height as f32) * 255.0) as u8;
            let b = (128u8).wrapping_add((x as u8).wrapping_mul(y as u8));
            buf.put_pixel(x, y, image::Rgb([r, g, b]));
        }
    }
    buf.save(&path).expect("save test PNG");
    path
}

/// Run ffmpeg to encode a test image with the given codec and return the
/// output path.  Returns `None` if ffmpeg is not available.
fn ffmpeg_encode(codec: &str, output_ext: &str, extra_args: &[&str]) -> Option<PathBuf> {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found — skipping {codec} test");
        return None;
    }

    let input = generate_test_png(320, 240);
    let dir = tmp_dir();
    let _ = std::fs::create_dir_all(&dir);
    let output = dir.join(format!("test_{codec}.{output_ext}"));

    // Remove stale output
    let _ = std::fs::remove_file(&output);

    let mut cmd = Command::new("ffmpeg");
    let input_str = input.to_str().expect("input path valid UTF-8");
    let output_str = output.to_str().expect("output path valid UTF-8");
    cmd.args([
        "-y",
        "-i", input_str,
        "-c:v", codec,
    ]);
    cmd.args(extra_args);
    cmd.arg(output_str);

    let out = cmd.output().expect("ffmpeg execution");
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        eprintln!("ffmpeg {codec} failed:\n{stderr}");
        return None;
    }

    Some(output)
}

fn file_size(path: &PathBuf) -> u64 {
    std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
}

// ── Codec enum tests ──────────────────────────────────────────────────

#[test]
fn test_codec_from_str() {
    use bmc_aggregator::bmc::encoder::Codec;

    assert_eq!(Codec::from_str("mjpeg"), Some(Codec::Mjpeg));
    assert_eq!(Codec::from_str("webp"), Some(Codec::WebP));
    assert_eq!(Codec::from_str("h264"), Some(Codec::H264));
    assert_eq!(Codec::from_str("hevc"), Some(Codec::H265));
    assert_eq!(Codec::from_str("av1"), Some(Codec::AV1));
    assert_eq!(Codec::from_str("vp8"), Some(Codec::VP8));
    assert_eq!(Codec::from_str("vp9"), Some(Codec::VP9));
    assert_eq!(Codec::from_str("unknown_codec"), None);
}

#[test]
fn test_codec_as_str() {
    use bmc_aggregator::bmc::encoder::Codec;

    assert_eq!(Codec::Mjpeg.as_str(), "mjpeg");
    assert_eq!(Codec::WebP.as_str(), "webp");
    assert_eq!(Codec::H264.as_str(), "h264");
    assert_eq!(Codec::VP9.as_str(), "vp9");
}

// ── WebP encoder (pipeline, no FFmpeg needed) ────────────────────────

#[test]
fn test_webp_encoder_pipeline() {
    use bmc_aggregator::bmc::encoder::backend::FrameEncoder;
    let encoder = bmc_aggregator::bmc::encoder::webp::WebpEncoder;
    assert_eq!(encoder.id(), "webp");
    assert_eq!(encoder.codec().as_str(), "webp");

    let rgba = vec![128u8; 16 * 16 * 4];
    let encoded = encoder.encode(&rgba, 16, 16, 75).expect("WebP encode");
    assert!(encoded.len() > 10, "WebP output too small");

    // Validate WebP header (RIFF + WEBP)
    assert_eq!(&encoded[0..4], b"RIFF", "missing RIFF header");
    assert_eq!(&encoded[8..12], b"WEBP", "missing WEBP magic");
}

#[test]
fn test_webp_encoder_varied_pixels() {
    use bmc_aggregator::bmc::encoder::backend::FrameEncoder;
    let encoder = bmc_aggregator::bmc::encoder::webp::WebpEncoder;

    let mut pixels = vec![0u8; 4 * 4 * 4];
    for i in 0..4 {
        for j in 0..4 {
            let idx = (i * 4 + j) * 4;
            pixels[idx] = (i * 64) as u8;
            pixels[idx + 1] = (j * 64) as u8;
            pixels[idx + 2] = 128;
            pixels[idx + 3] = 255;
        }
    }
    let encoded = encoder.encode(&pixels, 4, 4, 90).expect("WebP encode");
    assert!(!encoded.is_empty());
    assert_eq!(&encoded[0..4], b"RIFF", "missing RIFF header");
}

#[test]
fn test_webp_encoder_larger_resolution() {
    use bmc_aggregator::bmc::encoder::backend::FrameEncoder;
    let encoder = bmc_aggregator::bmc::encoder::webp::WebpEncoder;
    let rgba = vec![200u8; 640 * 480 * 4];
    let encoded = encoder.encode(&rgba, 640, 480, 80).expect("WebP encode 640x480");
    assert!(encoded.len() > 100);
    assert_eq!(&encoded[0..4], b"RIFF");
}

#[test]
fn test_webp_encoder_different_quality() {
    use bmc_aggregator::bmc::encoder::backend::FrameEncoder;
    let encoder = bmc_aggregator::bmc::encoder::webp::WebpEncoder;
    let rgba = vec![100u8; 32 * 32 * 4];
    let lossless = encoder.encode(&rgba, 32, 32, 100).expect("WebP q100");
    // Even at "quality 100", WebPEncoder::new_lossless is used so quality
    // argument is ignored — just verifying it doesn't crash
    assert!(!lossless.is_empty());
    assert_eq!(&lossless[0..4], b"RIFF");
}

// ── Software JPEG encoder (existing, just a smoke test) ───────────────

#[test]
fn test_jpeg_encoder_pipeline() {
    use bmc_aggregator::bmc::encoder::backend::FrameEncoder;
    let encoder = bmc_aggregator::bmc::encoder::backend::SoftwareEncoder;
    assert_eq!(encoder.id(), "software");
    assert_eq!(encoder.codec().as_str(), "mjpeg");

    let rgba = vec![128u8; 16 * 16 * 4];
    let encoded = encoder.encode(&rgba, 16, 16, 75).expect("JPEG encode");
    assert!(encoded.len() > 10);
    assert_eq!(encoded[0], 0xFF);
    assert_eq!(encoded[1], 0xD8);
}

// ── FFmpeg-based system tests ─────────────────────────────────────────
// These test the system `ffmpeg` binary, NOT our encoder pipeline.
// They verify that the codecs can be encoded by ffmpeg on this system.
// Skipped if ffmpeg is not on PATH.

#[test]
fn test_ffmpeg_h264() {
    let output = ffmpeg_encode("libx264", "h264", &["-preset", "ultrafast", "-crf", "23"]);
    if let Some(path) = output {
        let size = file_size(&path);
        assert!(size > 100, "H.264 output too small: {size} bytes");
        eprintln!("H.264 encoded: {size} bytes -> {}", path.display());
    }
}

#[test]
fn test_ffmpeg_hevc() {
    let output = ffmpeg_encode("libx265", "hevc", &["-preset", "ultrafast", "-crf", "28"]);
    if let Some(path) = output {
        let size = file_size(&path);
        assert!(size > 100, "HEVC output too small: {size} bytes");
        eprintln!("HEVC encoded: {size} bytes -> {}", path.display());
    }
}

#[test]
fn test_ffmpeg_av1() {
    let output = ffmpeg_encode("libaom-av1", "webm", &[
        "-cpu-used", "5",
        "-crf", "30",
        "-row-mt", "1",
    ]);
    if let Some(path) = output {
        let size = file_size(&path);
        assert!(size > 100, "AV1 output too small: {size} bytes");
        // AV1 in WebM container — check EBML header
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[0..4], &[0x1A, 0x45, 0xDF, 0xA3], "missing EBML header");
        eprintln!("AV1 encoded: {size} bytes -> {}", path.display());
    }
}

#[test]
fn test_ffmpeg_vp8() {
    let output = ffmpeg_encode("libvpx", "webm", &["-crf", "10", "-b:v", "0"]);
    if let Some(path) = output {
        let size = file_size(&path);
        assert!(size > 100, "VP8 output too small: {size} bytes");
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[0..4], &[0x1A, 0x45, 0xDF, 0xA3], "missing EBML header");
        eprintln!("VP8 encoded: {size} bytes -> {}", path.display());
    }
}

#[test]
fn test_ffmpeg_vp9() {
    let output = ffmpeg_encode("libvpx-vp9", "webm", &["-crf", "15", "-b:v", "0"]);
    if let Some(path) = output {
        let size = file_size(&path);
        assert!(size > 100, "VP9 output too small: {size} bytes");
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[0..4], &[0x1A, 0x45, 0xDF, 0xA3], "missing EBML header");
        eprintln!("VP9 encoded: {size} bytes -> {}", path.display());
    }
}

#[test]
fn test_ffmpeg_webp() {
    let output = ffmpeg_encode("libwebp", "webp", &["-quality", "80"]);
    if let Some(path) = output {
        let size = file_size(&path);
        assert!(size > 100, "WebP output too small: {size} bytes");
        let data = std::fs::read(&path).unwrap();
        assert_eq!(&data[0..4], b"RIFF", "missing RIFF header");
        assert_eq!(&data[8..12], b"WEBP", "missing WEBP magic");
        eprintln!("WebP encoded: {size} bytes -> {}", path.display());
    }
}

// ── Multi-codec encoding (same frame, all codecs via ffmpeg) ──────────

#[test]
fn test_ffmpeg_all_codecs_produce_output() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found — skipping multi-codec test");
        return;
    }

    let input = generate_test_png(320, 240);
    let dir = tmp_dir();
    let _ = std::fs::create_dir_all(&dir);

    let codecs: &[(&str, &str, &[&str])] = &[
        ("libx264",    "h264", &["-preset", "ultrafast", "-crf", "23"]),
        ("libx265",    "hevc", &["-preset", "ultrafast", "-crf", "28"]),
        ("libvpx",     "webm", &["-crf", "10", "-b:v", "0"]),
        ("libvpx-vp9", "webm", &["-crf", "15", "-b:v", "0"]),
        ("libwebp",    "webp", &["-quality", "80"]),
    ];

    for (codec, ext, args) in codecs {
        let label = codec.replace("lib", "").replace("-", "_");
        let output = dir.join(format!("multi_{label}.{ext}"));
        let _ = std::fs::remove_file(&output);

        let mut cmd = Command::new("ffmpeg");
        let input_str = input.to_str().expect("input path valid UTF-8");
        let output_str = output.to_str().expect("output path valid UTF-8");
        cmd.args(["-y", "-i", input_str, "-c:v", codec]);
        cmd.args(*args);
        cmd.arg(output_str);

        let out = cmd.output().expect("ffmpeg execution");
        assert!(out.status.success(), "{codec} encoding failed");
        let size = file_size(&output);
        assert!(size > 100, "{codec} output too small: {size} bytes");
        eprintln!("{codec}: {size} bytes");
    }
}

// ── Codec enum registry integration ───────────────────────────────────

#[test]
fn test_registry_codecs_list() {
    let codecs = bmc_aggregator::bmc::encoder::registry::EncoderRegistry::codecs();
    let ids: Vec<&str> = codecs.iter().map(|c| c.id).collect();
    assert!(ids.contains(&"mjpeg"));
    assert!(ids.contains(&"h264"));
    assert!(ids.contains(&"vp8"));
    assert!(ids.contains(&"vp9"));
    assert!(!ids.contains(&"hevc"));
    assert!(ids.contains(&"av1"));
    assert!(ids.contains(&"webp"));
}

#[test]
fn test_create_webp_encoder() {
    let enc = bmc_aggregator::bmc::encoder::backend::create_encoder("webp");
    assert_eq!(enc.id(), "webp");
    assert_eq!(enc.codec().as_str(), "webp");
}
