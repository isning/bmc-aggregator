//! iDRAC AVCT display-packet envelope.
//!
//! The proprietary VQ/IDCT payload starts after a small packet-local header.
//! This module deliberately parses only that stable envelope and retains the
//! compressed bytes as a `Bytes` slice.  A decoder can therefore consume the
//! same allocation without a second application-level copy.

use bytes::Bytes;

use super::avct::AvctPacket;

// Dell's official iDRAC client routes these two packet types through the same
// envelope (`com.avocent.kvm.a.a.j`) but to different renderers.  `0x0081`
// uses the RGB555 RLE renderer (`d`); `0x008a` uses the RGB 7/8/7 RLE
// renderer (`e`).  Keep the type through to the decoder instead of treating
// the two payload formats as interchangeable.
pub const RGB555_RLE_DISPLAY_PACKET_TYPE: u16 = 0x0081;
pub const RGB7_RLE_DISPLAY_PACKET_TYPE: u16 = 0x0082;
pub const GRAY7_RLE_DISPLAY_PACKET_TYPE: u16 = 0x0083;
pub const RGB888_RLE_DISPLAY_PACKET_TYPE: u16 = 0x008a;
const DISPLAY_HEADER_BYTES: usize = 12;

pub const fn is_display_packet_type(message_type: u16) -> bool {
    matches!(
        message_type,
        RGB555_RLE_DISPLAY_PACKET_TYPE
            | RGB7_RLE_DISPLAY_PACKET_TYPE
            | GRAY7_RLE_DISPLAY_PACKET_TYPE
            | RGB888_RLE_DISPLAY_PACKET_TYPE
            | 0x8601
            | 0x8602
            | 0x8603
            | 0x860a
    )
}

#[derive(Clone, Debug)]
pub struct DisplayPacket {
    /// AVCT display packet family.  It selects the pixel-literal encoding for
    /// the stateful RLE decoder; the legacy fields below are only meaningful
    /// for the older VQ/IDCT family.
    pub message_type: u16,
    /// AVCT VQ/IDCT codec mode from the packet header.
    pub codec: u8,
    pub width: u16,
    pub height: u16,
    pub flags: u8,
    pub luma_quality: u8,
    pub chroma_quality: u8,
    pub compressed: Bytes,
}

impl DisplayPacket {
    pub fn parse(packet: &AvctPacket) -> Result<Self, String> {
        if !is_display_packet_type(packet.message_type) {
            return Err(format!(
                "expected AVCT RGB555 (0x{RGB555_RLE_DISPLAY_PACKET_TYPE:04x}) or RGB888 (0x{RGB888_RLE_DISPLAY_PACKET_TYPE:04x}) display packet, got 0x{:04x}",
                packet.message_type
            ));
        }
        if packet.payload.len() < DISPLAY_HEADER_BYTES {
            return Err("AVCT display packet is shorter than its 12-byte header".to_string());
        }
        // The Avocent display object names payload[4..6] as its first
        // coordinate and payload[6..8] as its second coordinate, but the
        // official renderer calls the second value first when allocating its
        // canvas. This is therefore height followed by width on the wire.
        let height = u16::from_be_bytes([packet.payload[4], packet.payload[5]]);
        let width = u16::from_be_bytes([packet.payload[6], packet.payload[7]]);
        if width == 0 || height == 0 {
            return Err("AVCT display packet has an invalid zero resolution".to_string());
        }
        Ok(Self {
            message_type: packet.message_type,
            codec: packet.payload[0],
            width,
            height,
            flags: packet.payload[8],
            luma_quality: packet.payload[11] >> 4,
            chroma_quality: packet.payload[11] & 0x0f,
            compressed: packet.payload.slice(DISPLAY_HEADER_BYTES..),
        })
    }
}

#[cfg(test)]
mod tests {
    use bytes::Bytes;

    use super::*;

    #[test]
    fn parses_display_metadata_without_copying_the_compressed_slice() {
        let payload = Bytes::from_static(&[
            0, 0, 0, 0, 0x04, 0x00, 0x03, 0x00, 0x07, 0, 0, 0, 0xaa, 0xbb,
        ]);
        let packet = AvctPacket {
            magic: [0; 4],
            message_type: RGB888_RLE_DISPLAY_PACKET_TYPE,
            payload,
        };
        let display = DisplayPacket::parse(&packet).unwrap();
        assert_eq!(display.message_type, RGB888_RLE_DISPLAY_PACKET_TYPE);
        assert_eq!(
            (display.width, display.height, display.flags),
            (768, 1024, 7)
        );
        assert_eq!(
            (display.codec, display.luma_quality, display.chroma_quality),
            (0, 0, 0)
        );
        assert_eq!(&display.compressed[..], &[0xaa, 0xbb]);
        assert_eq!(
            display.compressed.as_ptr(),
            packet.payload[DISPLAY_HEADER_BYTES..].as_ptr()
        );
    }

    #[test]
    fn rejects_non_display_or_malformed_display_packets() {
        let wrong_type = AvctPacket::beef(0, Bytes::new());
        assert!(DisplayPacket::parse(&wrong_type).is_err());
        let historical_0086 = AvctPacket {
            magic: [0; 4],
            message_type: 0x0086,
            payload: Bytes::from_static(&[0; DISPLAY_HEADER_BYTES]),
        };
        let error = DisplayPacket::parse(&historical_0086).unwrap_err();
        assert!(error.contains("0x0081"));
        assert!(error.contains("0x008a"));
        assert!(error.contains("0x0086"));
        let truncated = AvctPacket {
            magic: [0; 4],
            message_type: RGB888_RLE_DISPLAY_PACKET_TYPE,
            payload: Bytes::from_static(&[0; DISPLAY_HEADER_BYTES - 1]),
        };
        assert!(DisplayPacket::parse(&truncated).is_err());
    }

    #[test]
    fn accepts_the_official_rgb555_display_packet_type() {
        let packet = AvctPacket {
            magic: *b"VIDS",
            message_type: RGB555_RLE_DISPLAY_PACKET_TYPE,
            payload: Bytes::from_static(&[0, 0, 0, 0, 0, 1, 0, 1, 1, 0, 0, 0, 0xfc, 0x00]),
        };

        let display = DisplayPacket::parse(&packet).unwrap();
        assert_eq!(display.message_type, RGB555_RLE_DISPLAY_PACKET_TYPE);
        assert_eq!((display.width, display.height), (1, 1));
        assert_eq!(&display.compressed[..], &[0xfc, 0x00]);
    }
}
