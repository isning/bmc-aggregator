//! Native decoder for the iDRAC Avocent VQ/IDCT display stream.
//!
//! AVCT frames are a small stateful YCbCr codec: JPEG-style DCT tiles and
//! indexed YCbCr vector-quantised tiles share one bitstream. The tables and
//! control codes below are transcribed from Dell's client decoder, but the
//! implementation is independent Rust and owns no Java/JNLP runtime.

use bytes::{Bytes, BytesMut};
use gstreamer as gst;

use bmc_feature_api::video::RawPixelFormat;

use super::block_video::BlockVideoPacket;
use super::display::{
    DisplayPacket, GRAY7_RLE_DISPLAY_PACKET_TYPE, RGB555_RLE_DISPLAY_PACKET_TYPE,
    RGB7_RLE_DISPLAY_PACKET_TYPE, RGB888_RLE_DISPLAY_PACKET_TYPE,
};

#[derive(Clone, Copy)]
enum RleLiteralFormat {
    Rgb555,
    Rgb7Palette,
    Gray7,
    Rgb787,
}

const ZIGZAG: [usize; 64] = [
    0, 1, 5, 6, 14, 15, 27, 28, 2, 4, 7, 13, 16, 26, 29, 42, 3, 8, 12, 17, 25, 30, 41, 43, 9, 11,
    18, 24, 31, 40, 44, 53, 10, 19, 23, 32, 39, 45, 52, 54, 20, 22, 33, 38, 46, 51, 55, 60, 21, 34,
    37, 47, 50, 56, 59, 61, 35, 36, 48, 49, 57, 58, 62, 63,
];
const AAN: [f32; 8] = [
    1.0,
    1.387_039_9,
    1.306_563,
    1.175_875_5,
    1.0,
    0.785_694_96,
    0.541_196_1,
    0.275_899_38,
];
const EXTEND_NEGATIVE: [i16; 16] = [
    0, -1, -3, -7, -15, -31, -63, -127, -255, -511, -1023, -2047, -4095, -8191, -16383, -32767,
];
// iDRAC 8 advertises up to 3840x2160. Keep room for that mode without letting
// an untrusted display header allocate an arbitrarily large framebuffer.
const MAX_DISPLAY_PIXELS: u32 = 16 * 1024 * 1024;

const LUMA_QUANT: [[u8; 64]; 8] = [
    [
        20, 13, 12, 20, 30, 50, 63, 76, 15, 15, 17, 23, 32, 72, 75, 68, 17, 16, 20, 30, 50, 71, 86,
        70, 17, 21, 27, 36, 63, 108, 100, 77, 22, 27, 46, 70, 85, 136, 128, 96, 30, 43, 68, 80,
        101, 130, 141, 115, 61, 80, 97, 108, 128, 151, 150, 126, 90, 115, 118, 122, 140, 125, 128,
        123,
    ],
    [
        17, 12, 10, 17, 26, 43, 55, 66, 13, 13, 15, 20, 28, 63, 65, 60, 15, 14, 17, 26, 43, 62, 75,
        61, 15, 18, 24, 31, 55, 95, 87, 67, 19, 24, 40, 61, 74, 119, 112, 84, 26, 38, 60, 70, 88,
        113, 123, 100, 53, 70, 85, 95, 112, 132, 131, 110, 78, 100, 103, 107, 122, 109, 112, 108,
    ],
    [
        14, 9, 9, 14, 21, 36, 46, 55, 10, 10, 12, 17, 23, 52, 54, 49, 12, 11, 14, 21, 36, 51, 62,
        50, 12, 15, 19, 26, 46, 78, 72, 56, 16, 19, 33, 50, 61, 98, 93, 69, 21, 31, 49, 58, 73, 94,
        102, 83, 44, 58, 70, 78, 93, 109, 108, 91, 65, 83, 86, 88, 101, 90, 93, 89,
    ],
    [
        11, 7, 7, 11, 17, 28, 36, 43, 8, 8, 10, 13, 18, 41, 43, 39, 10, 9, 11, 17, 28, 40, 49, 40,
        10, 12, 15, 20, 36, 62, 57, 44, 12, 15, 26, 40, 48, 78, 74, 55, 17, 25, 39, 46, 58, 74, 81,
        66, 35, 46, 56, 62, 74, 86, 86, 72, 51, 66, 68, 70, 80, 71, 74, 71,
    ],
    [
        9, 6, 5, 9, 13, 22, 28, 34, 6, 6, 7, 10, 14, 32, 33, 30, 7, 7, 9, 13, 22, 32, 38, 31, 7, 9,
        12, 16, 28, 48, 45, 34, 10, 12, 20, 31, 38, 61, 57, 43, 13, 19, 30, 36, 45, 58, 63, 51, 27,
        36, 43, 48, 57, 68, 67, 56, 40, 51, 53, 55, 63, 56, 57, 55,
    ],
    [
        6, 4, 3, 6, 9, 15, 19, 22, 4, 4, 5, 7, 9, 21, 22, 20, 5, 4, 6, 9, 15, 21, 25, 21, 5, 6, 8,
        10, 19, 32, 30, 23, 6, 8, 13, 21, 25, 40, 38, 28, 9, 13, 20, 24, 30, 39, 42, 34, 18, 24,
        29, 32, 38, 45, 45, 37, 27, 34, 35, 36, 42, 37, 38, 37,
    ],
    [
        3, 2, 1, 3, 4, 7, 9, 11, 2, 2, 2, 3, 4, 10, 11, 10, 2, 2, 3, 4, 7, 10, 12, 10, 2, 3, 4, 5,
        9, 16, 15, 11, 3, 4, 6, 10, 12, 20, 19, 14, 4, 6, 10, 12, 15, 19, 21, 17, 9, 12, 14, 16,
        19, 22, 22, 18, 13, 17, 17, 18, 21, 18, 19, 18,
    ],
    [
        2, 1, 1, 2, 3, 5, 6, 7, 1, 1, 1, 2, 3, 7, 7, 6, 1, 1, 2, 3, 5, 7, 8, 7, 1, 2, 2, 3, 6, 10,
        10, 7, 2, 2, 4, 7, 8, 13, 12, 9, 3, 4, 6, 8, 10, 13, 14, 11, 6, 8, 9, 10, 12, 15, 15, 12,
        9, 11, 11, 12, 14, 12, 12, 12,
    ],
];
const CHROMA_QUANT: [[u8; 64]; 8] = [
    [
        31, 33, 45, 88, 185, 185, 185, 185, 33, 39, 48, 123, 185, 185, 185, 185, 45, 48, 105, 185,
        185, 185, 185, 185, 88, 123, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185,
        185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185, 185,
        185, 185, 185, 185, 185, 185, 185, 185,
    ],
    [
        27, 29, 39, 76, 160, 160, 160, 160, 29, 34, 42, 107, 160, 160, 160, 160, 39, 42, 91, 160,
        160, 160, 160, 160, 76, 107, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160,
        160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160, 160,
        160, 160, 160, 160, 160, 160, 160, 160,
    ],
    [
        22, 24, 32, 63, 133, 133, 133, 133, 24, 28, 34, 88, 133, 133, 133, 133, 32, 34, 75, 133,
        133, 133, 133, 133, 63, 88, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133,
        133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133, 133,
        133, 133, 133, 133, 133, 133, 133, 133,
    ],
    [
        18, 19, 26, 51, 108, 108, 108, 108, 19, 22, 28, 72, 108, 108, 108, 108, 26, 28, 61, 108,
        108, 108, 108, 108, 51, 72, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108,
        108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108, 108,
        108, 108, 108, 108, 108, 108, 108, 108,
    ],
    [
        13, 14, 19, 38, 80, 80, 80, 80, 14, 17, 21, 53, 80, 80, 80, 80, 19, 21, 45, 80, 80, 80, 80,
        80, 38, 53, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80,
        80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80, 80,
    ],
    [
        9, 10, 13, 26, 55, 55, 55, 55, 10, 11, 14, 37, 55, 55, 55, 55, 13, 14, 31, 55, 55, 55, 55,
        55, 26, 37, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
        55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55, 55,
    ],
    [
        4, 5, 6, 13, 27, 27, 27, 27, 5, 5, 7, 18, 27, 27, 27, 27, 6, 7, 15, 27, 27, 27, 27, 27, 13,
        18, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
        27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27, 27,
    ],
    [
        3, 3, 4, 8, 18, 18, 18, 18, 3, 3, 4, 12, 18, 18, 18, 18, 4, 4, 10, 18, 18, 18, 18, 18, 8,
        12, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18,
        18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18, 18,
    ],
];

const DC_LUMA_COUNTS: [u8; 17] = [0, 0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_LUMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const DC_CHROMA_COUNTS: [u8; 17] = [0, 0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0];
const DC_CHROMA_VALUES: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
const AC_LUMA_COUNTS: [u8; 17] = [0, 0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 125];
const AC_LUMA_VALUES: [u8; 162] = [
    1, 2, 3, 0, 4, 17, 5, 18, 33, 49, 65, 6, 19, 81, 97, 7, 34, 113, 20, 50, 129, 145, 161, 8, 35,
    66, 177, 193, 21, 82, 209, 240, 36, 51, 98, 114, 130, 9, 10, 22, 23, 24, 25, 26, 37, 38, 39,
    40, 41, 42, 52, 53, 54, 55, 56, 57, 58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87, 88,
    89, 90, 99, 100, 101, 102, 103, 104, 105, 106, 115, 116, 117, 118, 119, 120, 121, 122, 131,
    132, 133, 134, 135, 136, 137, 138, 146, 147, 148, 149, 150, 151, 152, 153, 154, 162, 163, 164,
    165, 166, 167, 168, 169, 170, 178, 179, 180, 181, 182, 183, 184, 185, 186, 194, 195, 196, 197,
    198, 199, 200, 201, 202, 210, 211, 212, 213, 214, 215, 216, 217, 218, 225, 226, 227, 228, 229,
    230, 231, 232, 233, 234, 241, 242, 243, 244, 245, 246, 247, 248, 249, 250,
];
const AC_CHROMA_COUNTS: [u8; 17] = [0, 0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 119];
const AC_CHROMA_VALUES: [u8; 162] = [
    0, 1, 2, 3, 17, 4, 5, 33, 49, 6, 18, 65, 81, 7, 97, 113, 19, 34, 50, 129, 8, 20, 66, 145, 161,
    177, 193, 9, 35, 51, 82, 240, 21, 98, 114, 209, 10, 22, 36, 52, 225, 37, 241, 23, 24, 25, 26,
    38, 39, 40, 41, 42, 53, 54, 55, 56, 57, 58, 67, 68, 69, 70, 71, 72, 73, 74, 83, 84, 85, 86, 87,
    88, 89, 90, 99, 100, 101, 102, 103, 104, 105, 106, 115, 116, 117, 118, 119, 120, 121, 122, 130,
    131, 132, 133, 134, 135, 136, 137, 138, 146, 147, 148, 149, 150, 151, 152, 153, 154, 162, 163,
    164, 165, 166, 167, 168, 169, 170, 178, 179, 180, 181, 182, 183, 184, 185, 186, 194, 195, 196,
    197, 198, 199, 200, 201, 202, 210, 211, 212, 213, 214, 215, 216, 217, 218, 226, 227, 228, 229,
    230, 231, 232, 233, 234, 242, 243, 244, 245, 246, 247, 248, 249, 250,
];

#[derive(Clone)]
pub struct DecodedFrame {
    pub buffer: gst::Buffer,
    pub width: u32,
    pub height: u32,
    pub pixel_format: RawPixelFormat,
}

pub struct AvctDisplayDecoder {
    width: u32,
    height: u32,
    buffer: gst::Buffer,
    rgb_rle_cursor: usize,
    pending_rle: Option<PendingRle>,
    pending_block_video: Option<PendingBlockVideo>,
    rle_dialect: RleDialect,
    assembly: bmc_idrac_model::assembly::Assembly,
}

#[derive(Clone, Copy, Default)]
enum RleDialect {
    #[default]
    Standard,
    Idrac6Legacy,
}

/// The official iDRAC DVC reader can fetch the next display payload while it
/// is extending a run, reading a literal, or reading a pattern group.  Keep
/// only the unfinished command here; completed commands have already updated
/// the retained framebuffer and must not be replayed.
struct PendingRle {
    message_type: u16,
    width: u16,
    height: u16,
    bytes: BytesMut,
}

struct PendingBlockVideo {
    selector: u8,
    width: u16,
    height: u16,
    luma_quality: u8,
    chroma_quality: u8,
    compressed: BytesMut,
}

impl Default for AvctDisplayDecoder {
    fn default() -> Self {
        // AVCT's receiver task can be the first video path touched by a
        // process. Creating a GStreamer buffer before global initialization
        // panics inside gstreamer-rs and silently kills that task.
        // Initialization is performed by the application runtime. Keep the
        // decoder constructible when an optional GStreamer installation is
        // unavailable; buffer allocation and frame publication report the
        // resulting error at their fallible boundaries.
        let _ = gst::init();
        Self {
            width: 0,
            height: 0,
            buffer: gst::Buffer::new(),
            rgb_rle_cursor: 0,
            pending_rle: None,
            pending_block_video: None,
            rle_dialect: RleDialect::Standard,
            assembly: Default::default(),
        }
    }
}

impl AvctDisplayDecoder {
    /// iDRAC6 keeps the `0x0082/0x8602` and `0x0083/0x8603` envelopes used by
    /// the DVC7 and GRAY7 families but emits the RGB555 command stream used by
    /// `0x0081/0x8601`. Type `0x0083` is byte-identical to a captured DVC15
    /// chain; type `0x0082` is established by live official-DVC7/RGB555
    /// differential rendering. Select the appliance dialect explicitly
    /// instead of probing compressed image content.
    pub fn idrac6_legacy() -> Self {
        Self {
            rle_dialect: RleDialect::Idrac6Legacy,
            ..Self::default()
        }
    }

    /// Drop only an unfinished wire command while retaining the decoded
    /// framebuffer. Dell's colour-depth control can change the DVC literal
    /// family between command chains; the next reset-marked chain starts at
    /// cursor zero but still applies to the same official framebuffer.
    pub fn discard_pending_commands(&mut self) {
        self.pending_rle = None;
        self.pending_block_video = None;
        self.assembly = self
            .assembly
            .step(bmc_idrac_model::assembly::AssemblyEvent::Reset)
            .state;
    }

    /// Return the retained framebuffer without copying its pixels.  GStreamer
    /// buffer cloning is ref-counted; a later decoder write performs COW.
    /// This is used when a full refresh legitimately produces an all-black or
    /// otherwise byte-identical frame that `decode()` reports as unchanged.
    pub fn snapshot_frame(&self) -> Option<DecodedFrame> {
        (self.width != 0 && self.height != 0).then(|| DecodedFrame {
            buffer: self.buffer.clone(),
            width: self.width,
            height: self.height,
            pixel_format: RawPixelFormat::Bgra,
        })
    }

    pub fn assembly_deadline(&self) -> Option<core::time::Duration> {
        self.assembly.deadline()
    }
    pub fn expire_assembly(&mut self, now: core::time::Duration) -> bool {
        let transition = self
            .assembly
            .step(bmc_idrac_model::assembly::AssemblyEvent::Tick { now });
        self.assembly = transition.state;
        transition.effect == bmc_idrac_model::assembly::AssemblyEffect::Reject
    }
    fn begin_decode(&mut self, incoming: usize, now: core::time::Duration) -> Result<(), String> {
        let retained = self
            .pending_rle
            .as_ref()
            .map_or(0, |pending| pending.bytes.len())
            .saturating_add(
                self.pending_block_video
                    .as_ref()
                    .map_or(0, |pending| pending.compressed.len()),
            );
        let transition = self
            .assembly
            .step(bmc_idrac_model::assembly::AssemblyEvent::Input {
                now,
                retained,
                incoming,
            });
        self.assembly = transition.state;
        if transition.effect == bmc_idrac_model::assembly::AssemblyEffect::Decode {
            Ok(())
        } else {
            Err(
                "AVCT command assembly exceeded its time, byte, fragment, or phase boundary"
                    .to_string(),
            )
        }
    }
    fn finish_decode(&mut self, success: bool) {
        use bmc_idrac_model::assembly::AssemblyEvent;
        self.assembly = self
            .assembly
            .step(if success {
                AssemblyEvent::Decoded {
                    pending: self.pending_rle.is_some() || self.pending_block_video.is_some(),
                }
            } else {
                AssemblyEvent::Fault
            })
            .state;
    }
    pub fn decode_at(
        &mut self,
        packet: &DisplayPacket,
        now: core::time::Duration,
    ) -> Result<Option<DecodedFrame>, String> {
        self.begin_decode(packet.compressed.len(), now)?;
        let result = self.decode_inner(packet);
        self.finish_decode(result.is_ok());
        result
    }
    pub fn decode_block_video_at(
        &mut self,
        packet: &BlockVideoPacket,
        now: core::time::Duration,
    ) -> Result<Option<DecodedFrame>, String> {
        self.begin_decode(packet.compressed.len(), now)?;
        let result = self.decode_block_video_inner(packet);
        self.finish_decode(result.is_ok());
        result
    }
    #[cfg(test)]
    pub fn decode(&mut self, packet: &DisplayPacket) -> Result<Option<DecodedFrame>, String> {
        self.decode_at(packet, core::time::Duration::ZERO)
    }
    #[cfg(test)]
    pub fn decode_block_video(
        &mut self,
        packet: &BlockVideoPacket,
    ) -> Result<Option<DecodedFrame>, String> {
        self.decode_block_video_at(packet, core::time::Duration::ZERO)
    }

    /// Decode Dell's `0x0086/0x8606` block-video family through the same
    /// source-confirmed VQ/IDCT worker used by the official client. Flag bit
    /// zero retains compressed words until the terminal fragment arrives.
    fn decode_block_video_inner(
        &mut self,
        packet: &BlockVideoPacket,
    ) -> Result<Option<DecodedFrame>, String> {
        if packet.first_dimension == 0 || packet.second_dimension == 0 {
            return Err("AVCT block-video packet has an invalid zero resolution".to_string());
        }
        if packet.selector > 3 {
            return Err(format!(
                "AVCT block-video packet has unsupported official selector {}",
                packet.selector
            ));
        }

        // Selectors 2/3 are source-confirmed auxiliary ARGB4444/RGB444
        // objects keyed by packed_4_low. They do not enter the display block
        // worker and therefore do not replace the retained console canvas.
        if matches!(packet.selector, 2 | 3) {
            let expected = usize::from(packet.first_dimension)
                .checked_mul(usize::from(packet.second_dimension))
                .and_then(|pixels| pixels.checked_mul(2))
                .ok_or_else(|| "AVCT auxiliary block-video dimensions overflow".to_string())?;
            if packet.compressed.len() < expected {
                return Err("AVCT auxiliary block-video image is truncated".to_string());
            }
            return Ok(None);
        }

        let width = packet.first_dimension;
        let height = packet.second_dimension;
        let selector = packet.selector;
        let luma_quality = packet.byte_11_high;
        let chroma_quality = packet.byte_11_low;
        if luma_quality > 7 || chroma_quality > 7 {
            return Err("AVCT block-video packet has an unsupported quantiser index".to_string());
        }

        if packet.continuation() {
            match self.pending_block_video.as_mut() {
                Some(pending)
                    if pending.selector == selector
                        && pending.width == width
                        && pending.height == height
                        && pending.luma_quality == luma_quality
                        && pending.chroma_quality == chroma_quality =>
                {
                    pending.compressed.extend_from_slice(&packet.compressed);
                }
                Some(_) => {
                    return Err(
                        "AVCT block-video continuation changed decoder metadata".to_string()
                    );
                }
                None => {
                    self.pending_block_video = Some(PendingBlockVideo {
                        selector,
                        width,
                        height,
                        luma_quality,
                        chroma_quality,
                        compressed: BytesMut::from(packet.compressed.as_ref()),
                    });
                }
            }
            return Ok(None);
        }

        let compressed = if let Some(mut pending) = self.pending_block_video.take() {
            if pending.selector != selector
                || pending.width != width
                || pending.height != height
                || pending.luma_quality != luma_quality
                || pending.chroma_quality != chroma_quality
            {
                return Err(
                    "AVCT block-video terminal fragment changed decoder metadata".to_string(),
                );
            }
            pending.compressed.extend_from_slice(&packet.compressed);
            pending.compressed.freeze()
        } else {
            packet.compressed.clone()
        };

        // The official worker masks selector to bit zero before configuring
        // its 8x8/16x16 codec. DecodeState implements that exact command,
        // Huffman, quantisation, IDCT and palette pipeline.
        self.decode_inner(&DisplayPacket {
            message_type: 0,
            codec: selector & 1,
            width,
            height,
            flags: packet.flags,
            luma_quality,
            chroma_quality,
            compressed,
        })
    }

    fn decode_inner(&mut self, packet: &DisplayPacket) -> Result<Option<DecodedFrame>, String> {
        let rle_literal_format = match (self.rle_dialect, packet.message_type) {
            (RleDialect::Idrac6Legacy, RGB7_RLE_DISPLAY_PACKET_TYPE | 0x8602) => {
                Some(RleLiteralFormat::Rgb555)
            }
            (RleDialect::Idrac6Legacy, GRAY7_RLE_DISPLAY_PACKET_TYPE | 0x8603) => {
                Some(RleLiteralFormat::Rgb555)
            }
            (_, RGB555_RLE_DISPLAY_PACKET_TYPE | 0x8601) => Some(RleLiteralFormat::Rgb555),
            (_, RGB7_RLE_DISPLAY_PACKET_TYPE | 0x8602) => Some(RleLiteralFormat::Rgb7Palette),
            (_, GRAY7_RLE_DISPLAY_PACKET_TYPE | 0x8603) => Some(RleLiteralFormat::Gray7),
            (_, RGB888_RLE_DISPLAY_PACKET_TYPE | 0x860a) => Some(RleLiteralFormat::Rgb787),
            _ => None,
        };
        if !matches!(packet.codec, 0..=3) && rle_literal_format.is_none() {
            return Err(format!(
                "unsupported AVCT display codec mode {}",
                packet.codec
            ));
        }
        if rle_literal_format.is_none()
            && packet.codec <= 1
            && (packet.luma_quality > 7 || packet.chroma_quality > 7)
        {
            return Err("AVCT display packet has an unsupported quantiser index".to_string());
        }
        let width = packet.width as u32;
        let height = packet.height as u32;
        if width == 0 || height == 0 {
            return Err("AVCT display packet has a zero resolution".to_string());
        }
        let pixel_count = width
            .checked_mul(height)
            .ok_or_else(|| "AVCT display resolution overflows the frame buffer".to_string())?;
        if pixel_count > MAX_DISPLAY_PIXELS {
            return Err(format!(
                "AVCT display resolution {width}x{height} exceeds the safe limit"
            ));
        }
        if self.width != width || self.height != height {
            let bytes = pixel_count
                .checked_mul(4)
                .ok_or_else(|| "AVCT display resolution overflows the frame buffer".to_string())?;
            let mut initial = vec![0u8; bytes as usize];
            for pixel in initial.chunks_exact_mut(4) {
                // The official DVC framebuffer is initialized with opaque
                // black. A transparent zero value is reserved for the
                // guarded previous-pixel lookup at cursor zero.
                pixel[3] = u8::MAX;
            }
            self.buffer = gst::Buffer::from_mut_slice(initial);
            self.width = width;
            self.height = height;
            self.rgb_rle_cursor = 0;
            self.pending_rle = None;
        }

        let buffer = self.buffer.make_mut();
        let mut mapped = buffer
            .map_writable()
            .map_err(|_| "AVCT display frame buffer is not writable".to_string())?;
        let mut frame = FrameWriter {
            bytes: mapped.as_mut_slice(),
            width,
            height,
        };
        let changed = if let Some(literal_format) = rle_literal_format {
            if let Some(pending) = self.pending_rle.as_ref() {
                if pending.message_type != packet.message_type
                    || pending.width != packet.width
                    || pending.height != packet.height
                {
                    return Err(
                        "iDRAC DVC continuation changed its display type or resolution".into(),
                    );
                }
                if packet.flags & 1 != 0 {
                    return Err(
                        "iDRAC DVC continuation unexpectedly resets the display cursor".into(),
                    );
                }
            }
            if packet.flags & 1 != 0 {
                self.rgb_rle_cursor = 0;
            }
            // In the reference reader `i#l()` (envelope bit 1) prevents a
            // command from fetching the following display payload.  The
            // inverse therefore tells us whether an incomplete command may
            // be retained for the next packet.
            let may_fetch_next_payload = packet.flags & 2 == 0;
            let combined = self.pending_rle.take().map(|mut pending| {
                pending.bytes.extend_from_slice(&packet.compressed);
                pending.bytes
            });
            let data: &[u8] = combined.as_deref().unwrap_or(packet.compressed.as_ref());
            let cursor_before_packet = self.rgb_rle_cursor;
            let outcome = match decode_idrac_rle(
                data,
                packet.width,
                packet.height,
                &mut frame,
                &mut self.rgb_rle_cursor,
                literal_format,
                may_fetch_next_payload,
            ) {
                Ok(outcome) => outcome,
                Err(error) => {
                    // A rejected packet must not poison the state used by the
                    // following display packet. Completed pixel writes may be
                    // overwritten by the next update, but the cursor and any
                    // unfinished command are transactional per packet.
                    self.rgb_rle_cursor = cursor_before_packet;
                    self.pending_rle = None;
                    return Err(error);
                }
            };
            match outcome {
                RleDecodeOutcome::Complete { changed } => changed,
                RleDecodeOutcome::NeedMore {
                    changed,
                    command_start,
                } => {
                    self.pending_rle = Some(PendingRle {
                        message_type: packet.message_type,
                        width: packet.width,
                        height: packet.height,
                        bytes: BytesMut::from(&data[command_start..]),
                    });
                    changed
                }
            }
        } else {
            match packet.codec {
                0 | 1 => DecodeState::new(packet)?.decode(&mut frame)?,
                2 | 3 => decode_raw_16bpp(packet, &mut frame)?,
                _ => unreachable!("codec range was validated above"),
            }
        };
        drop(mapped);
        Ok(changed.then(|| DecodedFrame {
            buffer: self.buffer.clone(),
            width,
            height,
            pixel_format: RawPixelFormat::Bgra,
        }))
    }
}

/// Decoder for the iDRAC stateful RGB RLE packet families. The common control
/// flow comes from `com.avocent.kvm.a.a.c`; `d` and `e` differ only in the
/// literal-pixel width and RGB layout selected by the packet type.
enum RleDecodeOutcome {
    Complete { changed: bool },
    NeedMore { changed: bool, command_start: usize },
}

fn decode_idrac_rle(
    data: &[u8],
    width: u16,
    height: u16,
    frame: &mut FrameWriter<'_>,
    cursor: &mut usize,
    literal_format: RleLiteralFormat,
    may_fetch_next_payload: bool,
) -> Result<RleDecodeOutcome, String> {
    let pixels = usize::from(width)
        .checked_mul(usize::from(height))
        .ok_or_else(|| "iDRAC8 RGB RLE display resolution overflows canvas".to_string())?;
    let frame_pixels = frame.bytes.len() / 4;
    if pixels > frame_pixels || *cursor > pixels {
        return Err("iDRAC8 RGB RLE cursor is outside the display canvas".into());
    }
    let mut offset = 0usize;
    let mut changed = false;
    let mut pattern_history = None;
    while offset < data.len() {
        let command_start = offset;
        let token = data[offset];
        offset += 1;
        match token & 0xe0 {
            0x00 => {
                let Some(count) = rle_count(
                    token,
                    data,
                    &mut offset,
                    literal_format,
                    may_fetch_next_payload,
                )?
                else {
                    return Ok(RleDecodeOutcome::NeedMore {
                        changed,
                        command_start,
                    });
                };
                *cursor = cursor
                    .checked_add(count)
                    .ok_or_else(|| "iDRAC8 RGB RLE cursor overflow".to_string())?
                    .min(pixels);
            }
            0x20 => {
                let Some(count) = rle_count(
                    token,
                    data,
                    &mut offset,
                    literal_format,
                    may_fetch_next_payload,
                )?
                else {
                    return Ok(RleDecodeOutcome::NeedMore {
                        changed,
                        command_start,
                    });
                };
                let color = previous_pixel(frame, *cursor);
                write_repeated(frame, cursor, pixels, color, count)?;
                changed = true;
            }
            0x40 => {
                // `com.avocent.kvm.a.a.c.d()` copies from the same offset on
                // the preceding scanline (`current_position - canvas_width`).
                let Some(count) = rle_count(
                    token,
                    data,
                    &mut offset,
                    literal_format,
                    may_fetch_next_payload,
                )?
                else {
                    return Ok(RleDecodeOutcome::NeedMore {
                        changed,
                        command_start,
                    });
                };
                copy_previous_row(frame, cursor, pixels, count)?;
                changed = true;
            }
            0x60 => {
                let (latest, previous) = last_two_pixels(frame, *cursor, &mut pattern_history);
                let bits = [
                    if token & 8 != 0 { previous } else { latest },
                    if token & 4 != 0 { previous } else { latest },
                    if token & 2 != 0 { previous } else { latest },
                    if token & 1 != 0 { previous } else { latest },
                ];
                // Read all variable-length groups before writing the fixed
                // four pixels.  If a group crosses a payload boundary, this
                // command is replayed from its first byte on the next call.
                // Writing first would duplicate those four pixels.
                let mut groups = Vec::new();
                if token & 0x10 != 0 {
                    loop {
                        if offset >= data.len() {
                            if may_fetch_next_payload {
                                return Ok(RleDecodeOutcome::NeedMore {
                                    changed,
                                    command_start,
                                });
                            }
                            return Err(
                                "iDRAC RGB RLE pattern is truncated at a terminal payload".into()
                            );
                        }
                        let pattern = data[offset];
                        offset += 1;
                        groups.push(pattern);
                        if pattern & 0x80 == 0 {
                            break;
                        }
                    }
                }
                for color in bits {
                    write_repeated(frame, cursor, pixels, color, 1)?;
                }
                for pattern in groups {
                    for bit in (0..7).rev() {
                        let color = if pattern & (1 << bit) != 0 {
                            previous
                        } else {
                            latest
                        };
                        write_repeated(frame, cursor, pixels, color, 1)?;
                    }
                }
                changed = true;
            }
            _ => {
                let Some((red, green, blue)) = decode_rle_literal(
                    token,
                    data,
                    &mut offset,
                    literal_format,
                    may_fetch_next_payload,
                )?
                else {
                    return Ok(RleDecodeOutcome::NeedMore {
                        changed,
                        command_start,
                    });
                };
                write_repeated(frame, cursor, pixels, [blue, green, red, 0xff], 1)?;
                changed = true;
            }
        }
        if *cursor > pixels {
            return Err("iDRAC8 RGB RLE operation exceeds the display canvas".into());
        }
    }
    Ok(RleDecodeOutcome::Complete { changed })
}

fn decode_rle_literal(
    first: u8,
    data: &[u8],
    offset: &mut usize,
    literal_format: RleLiteralFormat,
    may_fetch_next_payload: bool,
) -> Result<Option<(u8, u8, u8)>, String> {
    match literal_format {
        // `com.avocent.kvm.a.a.d#b`: the literal is a 15-bit RGB555 word.
        // The reference renderer expands each five-bit component by shifting
        // it left three places, rather than replicating the high bits.
        RleLiteralFormat::Rgb555 => {
            let Some(low) = data.get(*offset).copied() else {
                return if may_fetch_next_payload {
                    Ok(None)
                } else {
                    Err("iDRAC RGB555 RLE literal is truncated at a terminal payload".into())
                };
            };
            *offset += 1;
            let packed = u16::from_be_bytes([first & 0x7f, low]);
            Ok(Some((
                ((packed & 0x7c00) >> 7) as u8,
                ((packed & 0x03e0) >> 2) as u8,
                ((packed & 0x001f) << 3) as u8,
            )))
        }
        RleLiteralFormat::Rgb7Palette => {
            let mapped = DVC7_PALETTE_ORDER[usize::from(first & 0x7f)] as usize;
            if mapped >= 125 {
                // The official 128-entry palette reserves its final three
                // slots for neutral greys: 0xff5f5f5f, 0xff9f9f9f and
                // 0xffdfdfdf. They are not part of the 5×5×5 colour cube.
                let value = [95, 159, 223][mapped - 125];
                return Ok(Some((value, value, value)));
            }
            Ok(Some((
                DVC7_LEVELS[mapped % 5],
                DVC7_LEVELS[(mapped / 5) % 5],
                DVC7_LEVELS[mapped / 25],
            )))
        }
        RleLiteralFormat::Gray7 => {
            let value = (first & 0x7f) << 1;
            Ok(Some((value, value, value)))
        }
        // `com.avocent.kvm.a.a.e#b`: 7 red bits, 8 green bits, and 7 blue
        // bits occupy three bytes including the high-bit literal marker.
        RleLiteralFormat::Rgb787 => {
            let (Some(second), Some(third)) =
                (data.get(*offset).copied(), data.get(*offset + 1).copied())
            else {
                return if may_fetch_next_payload {
                    Ok(None)
                } else {
                    Err("iDRAC RGB787 RLE literal is truncated at a terminal payload".into())
                };
            };
            *offset += 2;
            let packed =
                (u32::from(first & 0x7f) << 16) | (u32::from(second) << 8) | u32::from(third);
            Ok(Some((
                ((packed & 0x7f8000) >> 15) as u8,
                ((packed & 0x7f80) >> 7) as u8,
                ((packed & 0x7f) << 1) as u8,
            )))
        }
    }
}

fn copy_previous_row(
    frame: &mut FrameWriter<'_>,
    cursor: &mut usize,
    pixels: usize,
    count: usize,
) -> Result<(), String> {
    let width = frame.width as usize;
    // `com.avocent.kvm.a.a.c#d` does not move the framebuffer cursor when a
    // preceding scanline does not exist. Its `com.avocent.b.d.c.b#b` writer
    // also clips both source and destination ranges at the canvas boundary.
    if *cursor < width || *cursor >= pixels {
        return Ok(());
    }
    let source = *cursor - width;
    let clipped_count = count.min(pixels - source).min(pixels - *cursor);
    for index in 0..clipped_count {
        let source_offset = (source + index) * 4;
        let destination_offset = (*cursor + index) * 4;
        let color: [u8; 4] = frame.bytes[source_offset..source_offset + 4]
            .try_into()
            .unwrap();
        frame.bytes[destination_offset..destination_offset + 4].copy_from_slice(&color);
    }
    *cursor += clipped_count;
    Ok(())
}

fn rle_count(
    first: u8,
    data: &[u8],
    offset: &mut usize,
    literal_format: RleLiteralFormat,
    may_fetch_next_payload: bool,
) -> Result<Option<usize>, String> {
    let class = first & 0xe0;
    let mut count = usize::from(first & 0x1f);
    let mut groups = 1usize;
    while groups < 5 && *offset < data.len() && data[*offset] & 0xe0 == class {
        count |= usize::from(data[*offset] & 0x1f) << (groups * 5);
        *offset += 1;
        groups += 1;
    }
    if groups < 5 && *offset == data.len() && may_fetch_next_payload {
        return Ok(None);
    }
    Ok(Some(
        count
            + if matches!(
                literal_format,
                RleLiteralFormat::Rgb7Palette | RleLiteralFormat::Gray7
            ) {
                2
            } else {
                0
            },
    ))
}

const DVC7_LEVELS: [u8; 5] = [0, 70, 127, 191, 255];
const DVC7_PALETTE_ORDER: [u8; 128] = [
    0, 25, 50, 75, 5, 30, 55, 80, 10, 35, 60, 85, 15, 40, 65, 90, 1, 26, 51, 76, 6, 31, 56, 81, 11,
    36, 61, 86, 16, 41, 66, 91, 2, 27, 52, 77, 7, 32, 57, 82, 12, 37, 62, 87, 17, 42, 67, 92, 3,
    28, 53, 78, 8, 33, 58, 83, 13, 38, 63, 88, 18, 43, 68, 93, 100, 101, 102, 103, 105, 106, 107,
    108, 110, 111, 112, 113, 115, 116, 117, 118, 20, 45, 70, 95, 21, 46, 71, 96, 22, 47, 72, 97,
    23, 48, 73, 98, 4, 29, 54, 79, 9, 34, 59, 84, 14, 39, 64, 89, 19, 44, 69, 94, 120, 121, 122,
    123, 104, 109, 114, 119, 24, 49, 74, 99, 124, 125, 126, 127,
];

fn previous_pixel(frame: &FrameWriter<'_>, cursor: usize) -> [u8; 4] {
    if cursor == 0 {
        // `com.avocent.kvm.c.j#a(-1)` returns the integer zero rather than
        // the framebuffer's initialized opaque-black pixel.
        return [0, 0, 0, 0];
    }
    let offset = (cursor - 1) * 4;
    frame.bytes[offset..offset + 4].try_into().unwrap()
}

struct PatternHistory {
    cursor: usize,
    latest: [u8; 4],
    previous: [u8; 4],
}

fn last_two_pixels(
    frame: &FrameWriter<'_>,
    cursor: usize,
    history: &mut Option<PatternHistory>,
) -> ([u8; 4], [u8; 4]) {
    let latest = previous_pixel(frame, cursor);
    // Commands only write at/after the advancing cursor within this decode call.
    // Reuse the nearest distinct color in an unchanged prefix instead of scanning
    // that prefix again for every pattern in a single-color frame.
    let cached = history
        .as_ref()
        .filter(|cached| cached.cursor <= cursor && cached.latest == latest);
    let first = cached.map_or(0, |cached| cached.cursor);
    let previous = (first..cursor.saturating_sub(1))
        .rev()
        .map(|index| {
            #[cfg(test)]
            tests::PATTERN_PIXEL_VISITS.with(|visits| visits.set(visits.get() + 1));
            frame.bytes[index * 4..index * 4 + 4].try_into().unwrap()
        })
        .find(|candidate: &[u8; 4]| *candidate != latest)
        .unwrap_or_else(|| cached.map_or(latest, |cached| cached.previous));
    *history = Some(PatternHistory {
        cursor,
        latest,
        previous,
    });
    (latest, previous)
}

fn write_repeated(
    frame: &mut FrameWriter<'_>,
    cursor: &mut usize,
    pixels: usize,
    color: [u8; 4],
    count: usize,
) -> Result<(), String> {
    if *cursor >= pixels {
        return Ok(());
    }
    let end = cursor
        .checked_add(count)
        .ok_or_else(|| "iDRAC8 RGB RLE cursor overflow".to_string())?
        .min(pixels);
    for index in *cursor..end {
        let offset = index * 4;
        frame.bytes[offset..offset + 4].copy_from_slice(&color);
    }
    *cursor = end;
    Ok(())
}

struct FrameWriter<'a> {
    bytes: &'a mut [u8],
    width: u32,
    height: u32,
}

impl FrameWriter<'_> {
    fn put(&mut self, x: usize, y: usize, yv: u8, cb: u8, cr: u8) {
        if x >= self.width as usize || y >= self.height as usize {
            return;
        }
        let (b, g, r) = ycbcr_to_bgr(yv, cb, cr);
        self.put_bgra(x, y, b, g, r, 0xff);
    }

    fn put_bgra(&mut self, x: usize, y: usize, b: u8, g: u8, r: u8, a: u8) {
        if x >= self.width as usize || y >= self.height as usize {
            return;
        }
        let offset = (y * self.width as usize + x) * 4;
        self.bytes[offset..offset + 4].copy_from_slice(&[b, g, r, a]);
    }

    fn composite_argb(&mut self, x: usize, y: usize, a: u8, r: u8, g: u8, b: u8) {
        if x >= self.width as usize || y >= self.height as usize || a == 0 {
            return;
        }
        if a == u8::MAX {
            self.put_bgra(x, y, b, g, r, u8::MAX);
            return;
        }
        let offset = (y * self.width as usize + x) * 4;
        let inverse_alpha = u16::from(u8::MAX - a);
        let alpha = u16::from(a);
        for (destination, source) in [(offset, b), (offset + 1, g), (offset + 2, r)] {
            self.bytes[destination] = ((u16::from(source) * alpha
                + u16::from(self.bytes[destination]) * inverse_alpha
                + 127)
                / 255) as u8;
        }
        self.bytes[offset + 3] = u8::MAX;
    }
}

/// Decode the two uncompressed 16-bit display formats handled by the official
/// AVCT client. They are emitted as complete canvases, while transparent
/// source pixels preserve the retained framebuffer just as Java's ARGB image
/// compositing does.
fn decode_raw_16bpp(packet: &DisplayPacket, frame: &mut FrameWriter<'_>) -> Result<bool, String> {
    let expected = usize::from(packet.width)
        .checked_mul(usize::from(packet.height))
        .and_then(|pixels| pixels.checked_mul(2))
        .ok_or_else(|| "AVCT raw display resolution overflows the packet size".to_string())?;
    if packet.compressed.len() < expected {
        return Err("AVCT raw display packet is shorter than its declared canvas".to_string());
    }
    for (index, encoded) in packet.compressed[..expected].chunks_exact(2).enumerate() {
        let pixel = u16::from_le_bytes(encoded.try_into().unwrap());
        let x = index % usize::from(packet.width);
        let y = index / usize::from(packet.width);
        match packet.codec {
            // ARGB4444: composite source alpha over the retained canvas.
            3 => {
                let a = expand_nibble((pixel >> 12) as u8);
                let r = expand_nibble((pixel >> 8) as u8);
                let g = expand_nibble((pixel >> 4) as u8);
                let b = expand_nibble(pixel as u8);
                frame.composite_argb(x, y, a, r, g, b);
            }
            // The companion format has an opaque RGB444 path plus an
            // explicitly-inverted RGB444 path; its 0b10xx control pixels are
            // transparent and therefore leave the previous canvas untouched.
            2 if pixel & 0x8000 == 0 => frame.put_bgra(
                x,
                y,
                expand_nibble(pixel as u8),
                expand_nibble((pixel >> 4) as u8),
                expand_nibble((pixel >> 8) as u8),
                u8::MAX,
            ),
            2 if pixel & 0x4000 != 0 => frame.put_bgra(
                x,
                y,
                u8::MAX - expand_nibble(pixel as u8),
                u8::MAX - expand_nibble((pixel >> 4) as u8),
                u8::MAX - expand_nibble((pixel >> 8) as u8),
                u8::MAX,
            ),
            2 => {}
            _ => unreachable!("codec range was validated above"),
        }
    }
    Ok(true)
}

fn expand_nibble(value: u8) -> u8 {
    let value = value & 0x0f;
    value | value << 4
}

struct DecodeState {
    reader: BitReader,
    mode_420: bool,
    luma_quant: [f32; 64],
    chroma_quant: [f32; 64],
    dc_luma: Huffman,
    ac_luma: Huffman,
    dc_chroma: Huffman,
    ac_chroma: Huffman,
    predictors: [i16; 3],
    palette: [u32; 4],
    block_x: usize,
    block_y: usize,
}

impl DecodeState {
    fn new(packet: &DisplayPacket) -> Result<Self, String> {
        Ok(Self {
            reader: BitReader::new(packet.compressed.clone())?,
            mode_420: packet.codec & 1 != 0,
            luma_quant: quant_table(&LUMA_QUANT[packet.luma_quality as usize]),
            chroma_quant: quant_table(&CHROMA_QUANT[packet.chroma_quality as usize]),
            dc_luma: Huffman::new(&DC_LUMA_COUNTS, &DC_LUMA_VALUES),
            ac_luma: Huffman::new(&AC_LUMA_COUNTS, &AC_LUMA_VALUES),
            dc_chroma: Huffman::new(&DC_CHROMA_COUNTS, &DC_CHROMA_VALUES),
            ac_chroma: Huffman::new(&AC_CHROMA_COUNTS, &AC_CHROMA_VALUES),
            predictors: [0; 3],
            palette: [0x00_80_80, 0xff_80_80, 0x80_80_80, 0xc0_80_80],
            block_x: 0,
            block_y: 0,
        })
    }

    fn decode(&mut self, frame: &mut FrameWriter<'_>) -> Result<bool, String> {
        let mut changed = false;
        while self.reader.remaining() >= 4 {
            let kind = self.reader.peek(4)? as u8;
            match kind {
                0 => {
                    self.reader.skip(4)?;
                    self.paint_dct(frame, 0)?;
                    self.advance(frame);
                    changed = true;
                }
                4 => {
                    self.reader.skip(4)?;
                    self.paint_dct(frame, 2)?;
                    self.advance(frame);
                    changed = true;
                }
                5..=7 => {
                    self.reader.skip(4)?;
                    self.load_palette(kind - 5)?;
                    self.paint_palette(frame, kind - 5)?;
                    self.advance(frame);
                    changed = true;
                }
                8 | 9 => return Ok(changed),
                12..=15 => {
                    self.set_position()?;
                    match kind {
                        12 => self.paint_dct(frame, 2)?,
                        13 => {
                            self.load_palette(0)?;
                            self.paint_palette(frame, 0)?;
                        }
                        14 => {
                            self.load_palette(1)?;
                            self.paint_palette(frame, 1)?;
                        }
                        15 => {
                            self.load_palette(2)?;
                            self.paint_palette(frame, 2)?;
                        }
                        _ => unreachable!(),
                    }
                    self.advance(frame);
                    changed = true;
                }
                other => return Err(format!("unsupported AVCT VQ command {other}")),
            }
        }
        Ok(changed)
    }

    fn set_position(&mut self) -> Result<(), String> {
        self.block_x = ((self.reader.peek(28)? >> 20) & 0xff) as usize;
        self.block_y = ((self.reader.peek(28)? >> 12) & 0xff) as usize;
        self.reader.skip(20)
    }

    fn advance(&mut self, frame: &FrameWriter<'_>) {
        let size = if self.mode_420 { 16 } else { 8 };
        self.block_x += 1;
        if self.block_x >= (frame.width as usize).div_ceil(size) {
            self.block_x = 0;
            self.block_y = (self.block_y + 1) % (frame.height as usize).div_ceil(size).max(1);
        }
    }

    fn load_palette(&mut self, bits: u8) -> Result<(), String> {
        let entries = match bits {
            0 => 1,
            1 => 2,
            2 => 4,
            _ => unreachable!(),
        };
        for index in 0..entries {
            let palette_index = self.reader.peek(3)? as usize & 3;
            if self.reader.peek(1)? == 0 {
                self.reader.skip(3)?;
            }
            let value = self.reader.peek(27)? & 0x00ff_ffff;
            self.reader.skip(27)?;
            self.palette[palette_index] = value;
            if index == 0 && entries == 1 {
                break;
            }
        }
        Ok(())
    }

    fn palette_pixel(&mut self, bits: u8) -> Result<(u8, u8, u8), String> {
        let index = if bits == 0 {
            0
        } else {
            self.reader.read(bits)? as usize & 3
        };
        let value = self.palette[index];
        Ok(((value >> 16) as u8, (value >> 8) as u8, value as u8))
    }

    fn paint_palette(&mut self, frame: &mut FrameWriter<'_>, bits: u8) -> Result<(), String> {
        let size = if self.mode_420 { 16 } else { 8 };
        let origin_x = self.block_x * size;
        let origin_y = self.block_y * size;
        for y in 0..size {
            for x in 0..size {
                let (yv, cb, cr) = self.palette_pixel(bits)?;
                frame.put(origin_x + x, origin_y + y, yv, cb, cr);
            }
        }
        Ok(())
    }

    fn paint_dct(&mut self, frame: &mut FrameWriter<'_>, quant_variant: u8) -> Result<(), String> {
        let luma_blocks = if self.mode_420 { 4 } else { 1 };
        let mut luma = [[0u8; 64]; 4];
        for block in luma.iter_mut().take(luma_blocks) {
            *block = self.decode_component(0, 0, 0, quant_variant)?;
        }
        let cb = self.decode_component(1, 1, 1, quant_variant.saturating_add(1))?;
        let cr = self.decode_component(1, 1, 2, quant_variant.saturating_add(1))?;
        let size = if self.mode_420 { 16 } else { 8 };
        let origin_x = self.block_x * size;
        let origin_y = self.block_y * size;
        for y in 0..size {
            for x in 0..size {
                let yv = if self.mode_420 {
                    luma[(y / 8) * 2 + (x / 8)][(y % 8) * 8 + x % 8]
                } else {
                    luma[0][y * 8 + x]
                };
                let chroma = if self.mode_420 {
                    (y / 2) * 8 + x / 2
                } else {
                    y * 8 + x
                };
                frame.put(origin_x + x, origin_y + y, yv, cb[chroma], cr[chroma]);
            }
        }
        Ok(())
    }

    fn decode_component(
        &mut self,
        dc: usize,
        ac: usize,
        predictor: usize,
        variant: u8,
    ) -> Result<[u8; 64], String> {
        let dc_table = if dc == 0 {
            &self.dc_luma
        } else {
            &self.dc_chroma
        };
        let ac_table = if ac == 0 {
            &self.ac_luma
        } else {
            &self.ac_chroma
        };
        let size = dc_table.decode(&mut self.reader)? as usize;
        let delta = if size == 0 {
            0
        } else {
            self.reader.signed(size)?
        };
        self.predictors[predictor] = self.predictors[predictor].saturating_add(delta);
        let mut zigzag = [0i16; 64];
        zigzag[0] = self.predictors[predictor];
        let mut position = 1usize;
        while position < 64 {
            let value = ac_table.decode(&mut self.reader)?;
            let run = (value >> 4) as usize;
            let bits = (value & 0x0f) as usize;
            if bits == 0 {
                if run == 0 {
                    break;
                }
                if run == 15 {
                    position = (position + 16).min(64);
                    continue;
                }
                return Err("invalid AVCT DCT run-length code".to_string());
            }
            position = position.saturating_add(run);
            if position >= 64 {
                return Err("AVCT DCT coefficient exceeds block".to_string());
            }
            zigzag[position] = self.reader.signed(bits)?;
            position += 1;
        }
        let mut coefficients = [0i16; 64];
        for index in 0..64 {
            coefficients[index] = zigzag[ZIGZAG[index]];
        }
        let quant = if variant & 1 == 0 {
            &self.luma_quant
        } else {
            &self.chroma_quant
        };
        Ok(idct(&coefficients, quant))
    }
}

struct BitReader {
    words: Vec<u32>,
    position: usize,
}
impl BitReader {
    fn new(bytes: Bytes) -> Result<Self, String> {
        let mut words = Vec::with_capacity(bytes.len() / 4);
        for chunk in bytes.chunks_exact(4) {
            words.push(u32::from_le_bytes(chunk.try_into().unwrap()));
        }
        if words.len() < 2 {
            return Err("AVCT VQ payload is shorter than two words".to_string());
        }
        Ok(Self { words, position: 0 })
    }
    fn remaining(&self) -> usize {
        self.words.len() * 32 - self.position
    }
    fn peek(&self, bits: usize) -> Result<u32, String> {
        if bits == 0 || bits > 32 || self.remaining() < bits {
            return Err("truncated AVCT VQ bitstream".to_string());
        }
        let word = self.position / 32;
        let offset = self.position % 32;
        let first = self.words[word] << offset;
        if offset + bits <= 32 {
            return Ok(first >> (32 - bits));
        }
        let second = self
            .words
            .get(word + 1)
            .copied()
            .ok_or_else(|| "truncated AVCT VQ word".to_string())?;
        Ok((first | (second >> (32 - offset))) >> (32 - bits))
    }
    fn skip(&mut self, bits: usize) -> Result<(), String> {
        self.peek(bits)?;
        self.position += bits;
        Ok(())
    }
    fn read(&mut self, bits: u8) -> Result<u32, String> {
        let result = self.peek(bits as usize)?;
        self.position += bits as usize;
        Ok(result)
    }
    fn signed(&mut self, bits: usize) -> Result<i16, String> {
        if bits == 0 || bits >= EXTEND_NEGATIVE.len() {
            return Err("invalid AVCT DCT value width".to_string());
        }
        let value = self.read(bits as u8)? as i16;
        Ok(if value & (1 << (bits - 1)) == 0 {
            value + EXTEND_NEGATIVE[bits]
        } else {
            value
        })
    }
}

struct Huffman {
    entries: Vec<(u16, u8, u8)>,
}
impl Huffman {
    fn new(counts: &[u8; 17], values: &[u8]) -> Self {
        let mut entries = Vec::with_capacity(values.len());
        let mut code = 0u16;
        let mut value = 0usize;
        for (bits, count) in counts.iter().enumerate().skip(1) {
            for _ in 0..*count {
                entries.push((code, bits as u8, values[value]));
                code += 1;
                value += 1;
            }
            code <<= 1;
        }
        Self { entries }
    }
    fn decode(&self, reader: &mut BitReader) -> Result<u8, String> {
        let mut code = 0u16;
        for bits in 1..=16u8 {
            code = (code << 1) | reader.read(1)? as u16;
            if let Some((_, _, value)) = self
                .entries
                .iter()
                .find(|(candidate, length, _)| *candidate == code && *length == bits)
            {
                return Ok(*value);
            }
        }
        Err("invalid AVCT Huffman code".to_string())
    }
}

fn quant_table(base: &[u8; 64]) -> [f32; 64] {
    let mut scaled = [0u8; 64];
    for index in 0..64 {
        scaled[ZIGZAG[index]] = ((base[index] as u16).clamp(1, 255)) as u8;
    }
    let mut output = [0f32; 64];
    for (y, y_scale) in AAN.iter().enumerate() {
        for (x, x_scale) in AAN.iter().enumerate() {
            let index = y * 8 + x;
            output[index] = scaled[ZIGZAG[index]] as f32 * y_scale * x_scale;
        }
    }
    output
}

fn idct(coefficients: &[i16; 64], quant: &[f32; 64]) -> [u8; 64] {
    let mut work = [0i32; 64];
    for x in 0..8 {
        let at = |y: usize| (coefficients[x + y * 8] as f32 * quant[x + y * 8]) as i32;
        if (1..8).all(|y| coefficients[x + y * 8] == 0) {
            let value = at(0);
            for y in 0..8 {
                work[x + y * 8] = value;
            }
            continue;
        }
        let (a0, a2, a4, a6) = (at(0), at(2), at(4), at(6));
        let (t0, t1) = (a0 + a4, a0 - a4);
        let t2 = a2 + a6;
        let t3 = mul(a2 - a6, 362) - t2;
        let e0 = t0 + t2;
        let e3 = t0 - t2;
        let e1 = t1 + t3;
        let e2 = t1 - t3;
        let (a1, a3, a5, a7) = (at(1), at(3), at(5), at(7));
        let p0 = a5 + a3;
        let p1 = a5 - a3;
        let p2 = a1 + a7;
        let p3 = a1 - a7;
        let q0 = p2 + p0;
        let q1 = mul(p2 - p0, 362);
        let q2 = mul(p1 + p3, 473);
        let q3 = mul(p3, 277) - q2;
        let q4 = mul(p1, -669) + q2;
        let r0 = q4 - q0;
        let r1 = q1 - r0;
        let r2 = q3 + r1;
        work[x] = e0 + q0;
        work[x + 7 * 8] = e0 - q0;
        work[x + 8] = e1 + r0;
        work[x + 6 * 8] = e1 - r0;
        work[x + 2 * 8] = e2 + r1;
        work[x + 5 * 8] = e2 - r1;
        work[x + 4 * 8] = e3 + r2;
        work[x + 3 * 8] = e3 - r2;
    }
    let mut output = [0u8; 64];
    for y in 0..8 {
        let offset = y * 8;
        let (a0, a2, a4, a6) = (
            work[offset],
            work[offset + 2],
            work[offset + 4],
            work[offset + 6],
        );
        let (t0, t1) = (a0 + a4, a0 - a4);
        let t2 = a2 + a6;
        let t3 = mul(a2 - a6, 362) - t2;
        let e0 = t0 + t2;
        let e3 = t0 - t2;
        let e1 = t1 + t3;
        let e2 = t1 - t3;
        let p0 = work[offset + 5] + work[offset + 3];
        let p1 = work[offset + 5] - work[offset + 3];
        let p2 = work[offset + 1] + work[offset + 7];
        let p3 = work[offset + 1] - work[offset + 7];
        let q0 = p2 + p0;
        let q1 = mul(p2 - p0, 362);
        let q2 = mul(p1 + p3, 473);
        let q3 = mul(p3, 277) - q2;
        let q4 = mul(p1, -669) + q2;
        let r0 = q4 - q0;
        let r1 = q1 - r0;
        let r2 = q3 + r1;
        for (index, value) in [
            (0, e0 + q0),
            (7, e0 - q0),
            (1, e1 + r0),
            (6, e1 - r0),
            (2, e2 + r1),
            (5, e2 - r1),
            (4, e3 + r2),
            (3, e3 - r2),
        ] {
            output[offset + index] = ((value >> 3) + 128).clamp(0, 255) as u8;
        }
    }
    output
}
fn mul(value: i32, factor: i32) -> i32 {
    (value * factor) >> 8
}

fn ycbcr_to_bgr(y: u8, cb: u8, cr: u8) -> (u8, u8, u8) {
    // This exactly follows the lookup-table arithmetic in the Dell client:
    // luma and the blue/red chroma adjustments are rounded independently;
    // green combines both chroma products before its final shift.
    let y = (76_284 * (i32::from(y) - 16) + 32_768) >> 16;
    let b = y + ((132_096 * (i32::from(cb) - 128) + 32_768) >> 16);
    let g = y + ((-25_600 * (i32::from(cb) - 128) + 32_768 - 53_248 * (i32::from(cr) - 128)) >> 16);
    let r = y + ((104_704 * (i32::from(cr) - 128) + 32_768) >> 16);
    (
        b.clamp(0, 255) as u8,
        g.clamp(0, 255) as u8,
        r.clamp(0, 255) as u8,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    std::thread_local! {
        pub(super) static PATTERN_PIXEL_VISITS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn repeated_rle_patterns_bound_pixel_history_work() {
        let width = 128u16;
        let height = 128u16;
        let pixels = usize::from(width) * usize::from(height);
        let mut bytes = vec![0; pixels * 4];
        let mut frame = FrameWriter {
            bytes: &mut bytes,
            width: u32::from(width),
            height: u32::from(height),
        };
        let mut cursor = 0;
        PATTERN_PIXEL_VISITS.with(|visits| visits.set(0));
        let outcome = decode_idrac_rle(
            &vec![0x60; pixels / 4],
            width,
            height,
            &mut frame,
            &mut cursor,
            RleLiteralFormat::Rgb555,
            false,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            RleDecodeOutcome::Complete { changed: true }
        ));
        assert_eq!(cursor, pixels);
        assert!(frame.bytes.iter().all(|byte| *byte == 0));
        let visits = PATTERN_PIXEL_VISITS.with(std::cell::Cell::get);
        assert!(
            visits <= pixels,
            "pattern decoding rescanned {visits} historical pixels for a {pixels}-pixel frame"
        );
    }

    #[test]
    fn cached_pattern_history_matches_full_prefix_search() {
        for period in [1, 2, 3, 7, 31, 128] {
            let mut bytes: Vec<u8> = (0..128)
                .flat_map(|index| [((index / period) % 3) as u8, 0, 0, 255])
                .collect();
            let frame = FrameWriter {
                bytes: &mut bytes,
                width: 128,
                height: 1,
            };
            let mut history = None;
            for cursor in 0..=128 {
                let latest = previous_pixel(&frame, cursor);
                let previous = frame.bytes[..cursor.saturating_sub(1) * 4]
                    .chunks_exact(4)
                    .rev()
                    .map(|pixel| <[u8; 4]>::try_from(pixel).unwrap())
                    .find(|pixel| *pixel != latest)
                    .unwrap_or(latest);
                assert_eq!(
                    last_two_pixels(&frame, cursor, &mut history),
                    (latest, previous)
                );
                // Repeated queries must preserve the inherited distinct color.
                assert_eq!(
                    last_two_pixels(&frame, cursor, &mut history),
                    (latest, previous)
                );
            }
        }
    }

    fn block_packet(flags: u8, compressed: Bytes) -> BlockVideoPacket {
        BlockVideoPacket {
            message_type: super::super::block_video::BLOCK_VIDEO_PACKET_TYPE,
            selector: 0,
            packed_12_high: 0,
            packed_12_low: 0,
            first_dimension: 8,
            second_dimension: 8,
            flags,
            packed_6_high: 0,
            packed_6_low: 0,
            packed_4_low: 0,
            byte_11_low: 0,
            byte_11_high: 0,
            compressed,
        }
    }

    fn raw_packet(codec: u8, width: u16, height: u16, compressed: &[u8]) -> DisplayPacket {
        DisplayPacket {
            message_type: 0,
            codec,
            width,
            height,
            flags: 0,
            luma_quality: 0,
            chroma_quality: 0,
            compressed: Bytes::copy_from_slice(compressed),
        }
    }

    fn pixels(frame: &DecodedFrame) -> Vec<u8> {
        frame.buffer.map_readable().unwrap().as_slice().to_vec()
    }

    #[test]
    fn huffman_tables_decode_their_first_symbols() {
        let huffman = Huffman::new(&DC_LUMA_COUNTS, &DC_LUMA_VALUES);
        assert_eq!(huffman.entries.first(), Some(&(0, 2, 0)));
    }

    #[test]
    fn block_video_continuation_matches_a_complete_official_stop_record() {
        // Opcode nine is the source-confirmed stop record. Keep a second word
        // because the official bit reader primes two words at startup.
        let bytes = Bytes::from_static(&[0, 0, 0, 0x90, 0, 0, 0, 0]);
        let mut direct = AvctDisplayDecoder::default();
        let expected = direct
            .decode_block_video(&block_packet(0, bytes.clone()))
            .unwrap();

        let mut fragmented = AvctDisplayDecoder::default();
        assert!(fragmented
            .decode_block_video(&block_packet(1, bytes.slice(..4)))
            .unwrap()
            .is_none());
        let actual = fragmented
            .decode_block_video(&block_packet(0, bytes.slice(4..)))
            .unwrap();
        assert_eq!(actual.is_some(), expected.is_some());
    }

    #[test]
    fn ycbcr_neutral_is_neutral_bgra() {
        let (b, g, r) = ycbcr_to_bgr(128, 128, 128);
        assert!((i16::from(b) - i16::from(g)).abs() <= 1);
        assert!((i16::from(g) - i16::from(r)).abs() <= 1);
    }

    #[test]
    fn ycbcr_video_range_endpoints_match_the_reference_lookup_tables() {
        assert_eq!(ycbcr_to_bgr(16, 128, 128), (0, 0, 0));
        assert_eq!(ycbcr_to_bgr(235, 128, 128), (255, 255, 255));
    }

    #[test]
    fn raw_16_bit_modes_match_the_reference_rgb_and_argb_rules() {
        let mut decoder = AvctDisplayDecoder::default();
        // Codec 2: opaque RGB444 red followed by a transparent control pixel.
        let initial = decoder
            .decode(&raw_packet(2, 2, 1, &[0x00, 0x0f, 0x00, 0x80]))
            .unwrap()
            .unwrap();
        assert_eq!(pixels(&initial), vec![0, 0, 255, 255, 0, 0, 0, 255]);

        // Codec 3: ARGB4444 blends source blue at alpha 8/15 over the retained
        // opaque red pixel and leaves a transparent source pixel unchanged.
        let blended = decoder
            .decode(&raw_packet(3, 2, 1, &[0x0f, 0x80, 0x00, 0x00]))
            .unwrap()
            .unwrap();
        assert_eq!(pixels(&blended), vec![136, 0, 119, 255, 0, 0, 0, 255]);
    }

    #[test]
    fn dvc7_pattern_uses_the_reference_zero_guard_at_cursor_zero() {
        let mut decoder = AvctDisplayDecoder::default();
        let frame = decoder
            .decode(&DisplayPacket {
                message_type: RGB7_RLE_DISPLAY_PACKET_TYPE,
                codec: 0,
                width: 8,
                height: 1,
                flags: 2,
                luma_quality: 0,
                chroma_quality: 0,
                // The official int[] framebuffer refuses an exact-end
                // pattern write and starts from the guarded integer zero.
                compressed: Bytes::from_static(&[0x60]),
            })
            .unwrap()
            .unwrap();
        assert_eq!(
            pixels(&frame),
            vec![
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 0, 0, 0, 255, 0, 0,
                0, 255, 0, 0, 0, 255,
            ]
        );
    }

    #[test]
    fn retained_frames_are_copy_on_write_when_the_next_canvas_arrives() {
        let mut decoder = AvctDisplayDecoder::default();
        let old = decoder
            .decode(&raw_packet(2, 1, 1, &[0x00, 0x0f]))
            .unwrap()
            .unwrap();
        let new = decoder
            .decode(&raw_packet(2, 1, 1, &[0x0f, 0x00]))
            .unwrap()
            .unwrap();

        assert_eq!(pixels(&old), vec![0, 0, 255, 255]);
        assert_eq!(pixels(&new), vec![255, 0, 0, 255]);
    }

    #[test]
    fn discarding_an_incomplete_command_preserves_the_shared_dvc_framebuffer() {
        let mut decoder = AvctDisplayDecoder::default();
        decoder
            .decode(&DisplayPacket {
                message_type: RGB555_RLE_DISPLAY_PACKET_TYPE,
                codec: 0,
                width: 3,
                height: 1,
                flags: 3,
                luma_quality: 0,
                chroma_quality: 0,
                compressed: Bytes::from_static(&[0xfc, 0x00, 0x02]),
            })
            .unwrap();

        assert!(decoder
            .decode(&DisplayPacket {
                message_type: RGB555_RLE_DISPLAY_PACKET_TYPE,
                codec: 0,
                width: 3,
                height: 1,
                flags: 0,
                luma_quality: 0,
                chroma_quality: 0,
                compressed: Bytes::from_static(&[0xfc]),
            })
            .unwrap()
            .is_none());

        decoder.discard_pending_commands();
        decoder
            .decode(&DisplayPacket {
                message_type: RGB7_RLE_DISPLAY_PACKET_TYPE,
                codec: 1,
                width: 3,
                height: 1,
                flags: 3,
                luma_quality: 0,
                chroma_quality: 0,
                compressed: Bytes::from_static(&[0x00, 0x80]),
            })
            .unwrap();

        let frame = decoder.snapshot_frame().unwrap();
        assert_eq!(&pixels(&frame)[..4], &[0, 0, 248, 255]);
    }

    #[test]
    fn idrac8_rgb_rle_decodes_literals_skips_and_repeats() {
        let mut decoder = AvctDisplayDecoder::default();
        // Red literal, skip one retained pixel, then repeat the immediately
        // preceding (retained black) pixel.
        // Literal packing is 7/8/7 RGB, as used by the official type-138
        // decoder; this fixture deliberately contains no captured video data.
        let packet = DisplayPacket {
            message_type: RGB888_RLE_DISPLAY_PACKET_TYPE,
            codec: 0xde,
            width: 3,
            height: 1,
            flags: 1,
            luma_quality: 0,
            chroma_quality: 0,
            compressed: Bytes::from_static(&[0xff, 0, 0, 0x01, 0x21]),
        };
        let frame = decoder.decode(&packet).unwrap().unwrap();
        assert_eq!(
            pixels(&frame),
            vec![0, 0, 254, 255, 0, 0, 0, 255, 0, 0, 0, 255]
        );
    }

    #[test]
    fn rle_run_past_canvas_is_clipped_like_the_official_renderer() {
        let mut decoder = AvctDisplayDecoder::default();
        let packet = DisplayPacket {
            message_type: RGB888_RLE_DISPLAY_PACKET_TYPE,
            codec: 0,
            width: 2,
            height: 1,
            // The official framebuffer clips the repeat to the final pixel
            // instead of rejecting the display packet.
            flags: 3,
            luma_quality: 0,
            chroma_quality: 0,
            compressed: Bytes::from_static(&[0xff, 0, 0, 0x22]),
        };
        let frame = decoder.decode(&packet).unwrap().unwrap();
        assert_eq!(pixels(&frame), vec![0, 0, 254, 255, 0, 0, 254, 255]);
        assert_eq!(decoder.rgb_rle_cursor, 2);
    }

    #[test]
    fn previous_row_copy_on_the_first_scanline_does_not_advance_the_cursor() {
        let mut decoder = AvctDisplayDecoder::default();
        let frame = decoder
            .decode(&DisplayPacket {
                message_type: RGB888_RLE_DISPLAY_PACKET_TYPE,
                codec: 0,
                width: 2,
                height: 2,
                flags: 3,
                luma_quality: 0,
                chroma_quality: 0,
                // The official renderer ignores this copy while the cursor
                // is on row zero. The red literal therefore remains pixel 0.
                compressed: Bytes::from_static(&[0x42, 0xff, 0, 0]),
            })
            .unwrap()
            .unwrap();
        assert_eq!(&pixels(&frame)[..4], &[0, 0, 254, 255]);
        assert_eq!(decoder.rgb_rle_cursor, 1);
    }

    #[test]
    fn idrac8_rgb_rle_copies_from_the_preceding_scanline() {
        let mut decoder = AvctDisplayDecoder::default();
        let packet = DisplayPacket {
            message_type: RGB888_RLE_DISPLAY_PACKET_TYPE,
            codec: 0,
            width: 2,
            height: 2,
            // Bit 1 prevents the official reader from fetching a following
            // payload while looking ahead for another 0x40 run group.
            flags: 3,
            luma_quality: 0,
            chroma_quality: 0,
            // Red, green, then a 0x40-group count-two copy of row zero.
            compressed: Bytes::from_static(&[0xff, 0, 0, 0x80, 0xff, 0, 0x42]),
        };
        let frame = decoder.decode(&packet).unwrap().unwrap();
        let pixels = pixels(&frame);
        assert_eq!(&pixels[..8], &pixels[8..]);
    }

    #[test]
    fn idrac8_rgb555_rle_uses_two_byte_rgb555_literals() {
        let mut decoder = AvctDisplayDecoder::default();
        // `com.avocent.kvm.a.a.d`: the high-bit literal's lower seven bits
        // and one following byte form a big-endian RGB555 word.  0x7c00 is
        // opaque red with the official client's x8 expansion.
        let packet = DisplayPacket {
            message_type: 0x0081,
            codec: 0xde,
            width: 1,
            height: 1,
            flags: 1,
            luma_quality: 0,
            chroma_quality: 0,
            compressed: Bytes::from_static(&[0xfc, 0x00]),
        };

        let frame = decoder.decode(&packet).unwrap().unwrap();
        assert_eq!(pixels(&frame), vec![0, 0, 248, 255]);
    }

    #[test]
    fn retained_framebuffer_snapshot_publishes_an_unchanged_black_refresh_zero_copy() {
        let mut decoder = AvctDisplayDecoder::default();
        assert!(decoder.snapshot_frame().is_none());
        let decoded = decoder
            .decode(&DisplayPacket {
                message_type: RGB555_RLE_DISPLAY_PACKET_TYPE,
                codec: 0,
                width: 1,
                height: 1,
                flags: 3,
                luma_quality: 0,
                chroma_quality: 0,
                // A terminal zero-length DVC15 skip constructs the retained
                // canvas but intentionally writes no pixels.
                compressed: Bytes::from_static(&[0x00]),
            })
            .unwrap();
        assert!(decoded.is_none());

        let snapshot = decoder.snapshot_frame().expect("retained framebuffer");
        assert_eq!((snapshot.width, snapshot.height), (1, 1));
        assert_eq!(pixels(&snapshot), vec![0, 0, 0, 255]);
        assert_eq!(
            snapshot.buffer.as_ptr(),
            decoder.buffer.as_ptr(),
            "snapshot must retain the GStreamer buffer instead of copying pixels"
        );
    }

    #[test]
    fn dvc15_literal_continues_into_the_next_payload() {
        let mut decoder = AvctDisplayDecoder::default();
        let first = DisplayPacket {
            message_type: RGB555_RLE_DISPLAY_PACKET_TYPE,
            codec: 0,
            width: 2,
            height: 1,
            // Bit 1 clear is the source-confirmed condition under which the
            // official reader may fetch the next DVC payload.
            flags: 0,
            luma_quality: 0,
            chroma_quality: 0,
            compressed: Bytes::from_static(&[0xfc]),
        };
        assert!(decoder.decode(&first).unwrap().is_none());

        let second = DisplayPacket {
            flags: 2,
            compressed: Bytes::from_static(&[0x00, 0xfc, 0x00]),
            ..first
        };
        let frame = decoder.decode(&second).unwrap().unwrap();
        assert_eq!(pixels(&frame), vec![0, 0, 248, 255, 0, 0, 248, 255]);
    }

    #[test]
    fn dvc_run_extension_and_pattern_group_continue_into_next_payload() {
        let mut decoder = AvctDisplayDecoder::default();
        let run = DisplayPacket {
            message_type: RGB555_RLE_DISPLAY_PACKET_TYPE,
            codec: 0,
            width: 32,
            height: 1,
            flags: 0,
            luma_quality: 0,
            chroma_quality: 0,
            compressed: Bytes::from_static(&[0x1f]),
        };
        assert!(decoder.decode(&run).unwrap().is_none());
        let run_end = DisplayPacket {
            flags: 2,
            compressed: Bytes::from_static(&[0x00, 0xfc, 0x00]),
            ..run
        };
        let frame = decoder.decode(&run_end).unwrap().unwrap();
        assert_eq!(&pixels(&frame)[31 * 4..], &[0, 0, 248, 255]);

        let mut decoder = AvctDisplayDecoder::default();
        let pattern = DisplayPacket {
            message_type: RGB555_RLE_DISPLAY_PACKET_TYPE,
            codec: 0,
            width: 11,
            height: 1,
            flags: 0,
            luma_quality: 0,
            chroma_quality: 0,
            compressed: Bytes::from_static(&[0x70]),
        };
        assert!(decoder.decode(&pattern).unwrap().is_none());
        assert!(decoder
            .decode(&DisplayPacket {
                flags: 2,
                compressed: Bytes::from_static(&[0x00]),
                ..pattern
            })
            .unwrap()
            .is_some());
    }

    #[test]
    fn dvc_terminal_payload_rejects_a_truncated_literal() {
        let mut decoder = AvctDisplayDecoder::default();
        let result = decoder.decode(&DisplayPacket {
            message_type: RGB555_RLE_DISPLAY_PACKET_TYPE,
            codec: 0,
            width: 1,
            height: 1,
            flags: 2,
            luma_quality: 0,
            chroma_quality: 0,
            compressed: Bytes::from_static(&[0xfc]),
        });
        let Err(error) = result else {
            panic!("terminal truncated literal unexpectedly decoded");
        };
        assert!(error.contains("terminal payload"));
    }

    #[test]
    fn idrac8_dvc7_uses_the_official_permuted_palette() {
        let mut decoder = AvctDisplayDecoder::default();
        let frame = decoder
            .decode(&DisplayPacket {
                message_type: RGB7_RLE_DISPLAY_PACKET_TYPE,
                codec: 0,
                width: 1,
                height: 1,
                flags: 1,
                luma_quality: 0,
                chroma_quality: 0,
                // Literal index 1 maps through the official permutation to
                // palette entry 25: blue 70, green/red zero.
                compressed: Bytes::from_static(&[0x81]),
            })
            .unwrap()
            .unwrap();
        assert_eq!(pixels(&frame), vec![70, 0, 0, 255]);
    }

    #[test]
    fn idrac8_dvc7_preserves_the_three_non_cube_palette_entries() {
        // Wire index 127 maps directly to official palette slot 127.
        let mut offset = 0;
        assert_eq!(
            decode_rle_literal(0xff, &[], &mut offset, RleLiteralFormat::Rgb7Palette, false,)
                .unwrap(),
            Some((223, 223, 223))
        );
    }

    #[test]
    fn idrac6_type83_uses_the_captured_rgb555_payload_dialect() {
        let mut decoder = AvctDisplayDecoder::idrac6_legacy();
        let frame = decoder
            .decode(&DisplayPacket {
                message_type: GRAY7_RLE_DISPLAY_PACKET_TYPE,
                codec: 0xde,
                width: 1,
                height: 1,
                flags: 3,
                luma_quality: 0,
                chroma_quality: 0,
                // The iDRAC6 type-0x83 capture is byte-identical to its
                // type-0x81 stream. This RGB555 literal is opaque red.
                compressed: Bytes::from_static(&[0xfc, 0x00]),
            })
            .unwrap()
            .unwrap();

        assert_eq!(pixels(&frame), vec![0, 0, 248, 255]);
        assert_eq!(decoder.rgb_rle_cursor, 1);
    }

    #[test]
    fn idrac6_type82_uses_the_rgb555_payload_dialect() {
        let mut decoder = AvctDisplayDecoder::idrac6_legacy();
        let frame = decoder
            .decode(&DisplayPacket {
                message_type: RGB7_RLE_DISPLAY_PACKET_TYPE,
                codec: 0xde,
                width: 1,
                height: 1,
                flags: 3,
                luma_quality: 0,
                chroma_quality: 0,
                compressed: Bytes::from_static(&[0xfc, 0x00]),
            })
            .unwrap()
            .unwrap();

        assert_eq!(pixels(&frame), vec![0, 0, 248, 255]);
        assert_eq!(decoder.rgb_rle_cursor, 1);
    }

    #[test]
    fn standard_type83_remains_the_official_gray7_dialect() {
        let mut decoder = AvctDisplayDecoder::default();
        let frame = decoder
            .decode(&DisplayPacket {
                message_type: GRAY7_RLE_DISPLAY_PACKET_TYPE,
                codec: 0xde,
                width: 1,
                height: 1,
                flags: 3,
                luma_quality: 0,
                chroma_quality: 0,
                compressed: Bytes::from_static(&[0xfc]),
            })
            .unwrap()
            .unwrap();

        assert_eq!(pixels(&frame), vec![248, 248, 248, 255]);
        assert_eq!(decoder.rgb_rle_cursor, 1);
    }
}
