//! iDRAC AVCT `0x0086`/`0x8606` block-video envelope.
//!
//! This is deliberately separate from the DVC/RLE decoder.  Dell's
//! `com.avocent.kvm.a.c.g` retains the twelve-byte header and passes its
//! suffix to a stateful block codec.  Parsing here is allocation-free: the
//! compressed suffix remains a `Bytes` slice of the received AVCT packet.
//!
//! Command-level rendering remains an isolated decoder oracle until
//! interoperability is confirmed against hardware captures.
#![allow(dead_code)]

use bytes::Bytes;

use super::avct::AvctPacket;

pub const BLOCK_VIDEO_PACKET_TYPE: u16 = 0x0086;
pub const BLOCK_VIDEO_EXTENDED_PACKET_TYPE: u16 = 0x8606;
const BLOCK_VIDEO_HEADER_BYTES: usize = 12;

pub const fn is_block_video_packet_type(message_type: u16) -> bool {
    matches!(
        message_type,
        BLOCK_VIDEO_PACKET_TYPE | BLOCK_VIDEO_EXTENDED_PACKET_TYPE
    )
}

/// Source-level fields of `kvm.a.c.g`.  Names intentionally describe layout,
/// not an unproven display meaning.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BlockVideoPacket {
    pub message_type: u16,
    pub selector: u8,
    pub packed_12_high: u16,
    pub packed_12_low: u16,
    /// `g.p`: the official worker receives this as its first dimension.
    pub first_dimension: u16,
    /// `g.o`: the official worker receives this as its second dimension.
    pub second_dimension: u16,
    pub flags: u8,
    pub packed_6_high: u8,
    pub packed_6_low: u8,
    pub packed_4_low: u8,
    pub byte_11_low: u8,
    pub byte_11_high: u8,
    pub compressed: Bytes,
}

impl BlockVideoPacket {
    pub fn parse(packet: &AvctPacket) -> Result<Self, String> {
        if !is_block_video_packet_type(packet.message_type) {
            return Err(format!(
                "expected AVCT block-video packet 0x{BLOCK_VIDEO_PACKET_TYPE:04x} or 0x{BLOCK_VIDEO_EXTENDED_PACKET_TYPE:04x}, got 0x{:04x}",
                packet.message_type
            ));
        }
        if packet.payload.len() < BLOCK_VIDEO_HEADER_BYTES {
            return Err("AVCT block-video packet is shorter than its 12-byte header".to_string());
        }
        let packed_24 = (u32::from(packet.payload[1]) << 16)
            | (u32::from(packet.payload[2]) << 8)
            | u32::from(packet.payload[3]);
        let packed_16 = u16::from_be_bytes([packet.payload[9], packet.payload[10]]);
        Ok(Self {
            message_type: packet.message_type,
            selector: packet.payload[0],
            packed_12_high: ((packed_24 >> 12) & 0x0fff) as u16,
            packed_12_low: (packed_24 & 0x0fff) as u16,
            first_dimension: u16::from_be_bytes([packet.payload[6], packet.payload[7]]),
            second_dimension: u16::from_be_bytes([packet.payload[4], packet.payload[5]]),
            flags: packet.payload[8],
            packed_6_high: ((packed_16 >> 10) & 0x3f) as u8,
            packed_6_low: ((packed_16 >> 4) & 0x3f) as u8,
            packed_4_low: (packed_16 & 0x0f) as u8,
            byte_11_low: packet.payload[11] & 0x0f,
            byte_11_high: packet.payload[11] >> 4,
            compressed: packet.payload.slice(BLOCK_VIDEO_HEADER_BYTES..),
        })
    }

    pub const fn continuation(&self) -> bool {
        self.flags & 1 != 0
    }

    pub const fn block_edge(&self) -> u8 {
        if self.selector & 1 == 0 {
            8
        } else {
            16
        }
    }
}

/// The block codec's exact command-word reader (`kvm.a.c.b#a` / `#a(int)`).
/// Words are assembled little-endian from the packet suffix, but bits are
/// consumed most-significant-bit first from each assembled word.  It is not
/// a normal network-order bit reader.
#[derive(Clone, Debug)]
pub struct BlockBitReader<'a> {
    bytes: &'a [u8],
    word_index: usize,
    bits_consumed: u8,
}

impl<'a> BlockBitReader<'a> {
    pub fn new(bytes: &'a [u8]) -> Result<Self, String> {
        if !bytes.len().is_multiple_of(4) {
            return Err(
                "AVCT block-video command data is not a whole number of 32-bit words".to_string(),
            );
        }
        Ok(Self {
            bytes,
            word_index: 0,
            bits_consumed: 0,
        })
    }

    fn current_word(&self) -> Result<u32, String> {
        let offset = self
            .word_index
            .checked_mul(4)
            .ok_or_else(|| "AVCT block-video command offset overflowed".to_string())?;
        let word: [u8; 4] = self
            .bytes
            .get(offset..offset + 4)
            .ok_or_else(|| "AVCT block-video command stream is truncated".to_string())?
            .try_into()
            .expect("bounded word");
        Ok(u32::from_le_bytes(word))
    }

    /// Read one through 32 bits in the exact order used by the Dell client.
    pub fn read_bits(&mut self, count: u8) -> Result<u32, String> {
        if count == 0 || count > 32 {
            return Err("AVCT block-video bit count must be between 1 and 32".to_string());
        }
        let mut value = 0u32;
        let mut remaining = count;
        while remaining != 0 {
            let available = 32 - self.bits_consumed;
            let take = remaining.min(available);
            let word = self.current_word()?;
            let shift = available - take;
            let mask = if take == 32 {
                u32::MAX
            } else {
                (1u32 << take) - 1
            };
            value = (value << take) | ((word >> shift) & mask);
            self.bits_consumed += take;
            remaining -= take;
            if self.bits_consumed == 32 {
                self.word_index += 1;
                self.bits_consumed = 0;
            }
        }
        Ok(value)
    }

    pub fn remaining_bits(&self) -> usize {
        self.bytes.len() * 8 - self.word_index * 32 - usize::from(self.bits_consumed)
    }
}

/// One source-visible component record used by opcode families 5, 6, 7, 13,
/// 14 and 15.  `present` controls whether the three reserved bits exist.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ComponentUpdate {
    pub index: u8,
    pub present: bool,
    pub value: u32,
}

/// Block-command grammar from `kvm.a.c.e#b`.  This layer intentionally does
/// not attach display semantics to the component table; it only establishes
/// the exact source grammar that the renderer consumes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum BlockCommand {
    Transform {
        variant: u8,
    },
    Component {
        updates: Vec<ComponentUpdate>,
        /// One selector per 8x8 output pixel.  Opcode 5 has no selector
        /// bits and therefore uses an empty vector.
        pixel_selectors: Vec<u8>,
    },
    ExplicitTransform {
        x: u8,
        y: u8,
        variant: u8,
    },
    ExplicitComponent {
        x: u8,
        y: u8,
        updates: Vec<ComponentUpdate>,
        pixel_selectors: Vec<u8>,
    },
    Stop,
}

fn read_component(
    reader: &mut BlockBitReader<'_>,
    mask_value: bool,
) -> Result<ComponentUpdate, String> {
    let index = reader.read_bits(2)? as u8;
    let present = reader.read_bits(1)? != 0;
    if !present {
        // The Java reader skips these bits only for an absent component.
        reader.read_bits(3)?;
    }
    let mut value = reader.read_bits(27)?;
    if mask_value {
        value &= 0x00ff_ffff;
    }
    Ok(ComponentUpdate {
        index,
        present,
        value,
    })
}

fn read_pixel_selectors(
    reader: &mut BlockBitReader<'_>,
    component_count: usize,
) -> Result<Vec<u8>, String> {
    let bits_per_pixel = match component_count {
        1 => return Ok(Vec::new()),
        2 => 1,
        4 => 2,
        _ => return Err("AVCT block-video has an invalid component count".to_string()),
    };
    (0..64)
        .map(|_| Ok(reader.read_bits(bits_per_pixel)? as u8))
        .collect()
}

impl<'a> BlockBitReader<'a> {
    /// Parse exactly one command.  `Ok(None)` is the official stop marker;
    /// unsupported high nibbles are terminal decoder errors, never no-ops.
    pub fn next_command(&mut self) -> Result<Option<BlockCommand>, String> {
        let opcode = self.read_bits(4)?;
        let components = |reader: &mut Self, count: usize, mask_value: bool| {
            let updates = (0..count)
                .map(|_| read_component(reader, mask_value))
                .collect::<Result<Vec<_>, _>>()?;
            let pixel_selectors = read_pixel_selectors(reader, count)?;
            Ok::<_, String>((updates, pixel_selectors))
        };
        match opcode {
            0 => Ok(Some(BlockCommand::Transform { variant: 0 })),
            4 => Ok(Some(BlockCommand::Transform { variant: 2 })),
            5..=7 => {
                let count = match opcode {
                    5 => 1,
                    6 => 2,
                    7 => 4,
                    _ => unreachable!(),
                };
                let (updates, pixel_selectors) = components(self, count, false)?;
                Ok(Some(BlockCommand::Component {
                    updates,
                    pixel_selectors,
                }))
            }
            8 | 12 | 13 | 14 | 15 => {
                let x = self.read_bits(8)? as u8;
                let y = self.read_bits(8)? as u8;
                self.read_bits(4)?;
                match opcode {
                    8 => Ok(Some(BlockCommand::ExplicitTransform { x, y, variant: 0 })),
                    12 => Ok(Some(BlockCommand::ExplicitTransform { x, y, variant: 2 })),
                    13..=15 => {
                        let count = match opcode {
                            13 => 1,
                            14 => 2,
                            15 => 4,
                            _ => unreachable!(),
                        };
                        let (updates, pixel_selectors) = components(self, count, opcode == 14)?;
                        Ok(Some(BlockCommand::ExplicitComponent {
                            x,
                            y,
                            updates,
                            pixel_selectors,
                        }))
                    }
                    _ => unreachable!("opcode was matched above"),
                }
            }
            9 => Ok(None),
            1..=3 | 10 | 11 => Err(format!(
                "AVCT block-video command has unsupported official opcode {opcode}"
            )),
            _ => unreachable!("opcode is four bits"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source_words(bits: &[u8]) -> Vec<u8> {
        let mut padded = bits.to_vec();
        while !padded.len().is_multiple_of(32) {
            padded.push(0);
        }
        padded
            .chunks_exact(32)
            .flat_map(|chunk| {
                let word = chunk
                    .iter()
                    .fold(0u32, |word, bit| (word << 1) | u32::from(*bit));
                word.to_le_bytes()
            })
            .collect()
    }

    fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
        for shift in (0..width).rev() {
            bits.push(((value >> shift) & 1) as u8);
        }
    }

    #[test]
    fn parses_the_official_block_video_envelope_without_copying_suffix() {
        let packet = AvctPacket {
            magic: *b"BEEF",
            message_type: BLOCK_VIDEO_PACKET_TYPE,
            payload: Bytes::from_static(&[
                3, 0xab, 0xcd, 0xef, 0x01, 0x02, 0x03, 0x04, 0x11, 0xaa, 0xbc, 0x5e, 9, 8,
            ]),
        };
        let block = BlockVideoPacket::parse(&packet).unwrap();
        assert_eq!(block.selector, 3);
        assert_eq!((block.packed_12_high, block.packed_12_low), (0xabc, 0xdef));
        assert_eq!(
            (block.first_dimension, block.second_dimension),
            (0x0304, 0x0102)
        );
        assert_eq!(
            (block.packed_6_high, block.packed_6_low, block.packed_4_low),
            (42, 43, 12)
        );
        assert_eq!((block.byte_11_high, block.byte_11_low), (5, 14));
        assert!(block.continuation());
        assert_eq!(block.block_edge(), 16);
        assert_eq!(&block.compressed[..], &[9, 8]);
        assert_eq!(block.compressed.as_ptr(), packet.payload[12..].as_ptr());
    }

    #[test]
    fn rejects_wrong_type_and_short_header() {
        assert!(BlockVideoPacket::parse(&AvctPacket::beef(0x0081, Bytes::new())).is_err());
        assert!(BlockVideoPacket::parse(&AvctPacket::beef(
            BLOCK_VIDEO_EXTENDED_PACKET_TYPE,
            Bytes::from_static(&[0; BLOCK_VIDEO_HEADER_BYTES - 1]),
        ))
        .is_err());
    }

    #[test]
    fn command_reader_uses_little_endian_words_but_msb_first_bits() {
        let mut reader = BlockBitReader::new(&[
            0x78, 0x56, 0x34, 0x12, // assembled word: 0x12345678
            0xef, 0xcd, 0xab, 0x90, // assembled word: 0x90abcdef
        ])
        .unwrap();
        assert_eq!(reader.read_bits(4).unwrap(), 0x1);
        assert_eq!(reader.read_bits(12).unwrap(), 0x234);
        assert_eq!(reader.read_bits(16).unwrap(), 0x5678);
        assert_eq!(reader.read_bits(8).unwrap(), 0x90);
        assert_eq!(reader.read_bits(24).unwrap(), 0xabcdef);
        assert_eq!(reader.remaining_bits(), 0);
        assert!(reader.read_bits(1).is_err());
    }

    #[test]
    fn command_reader_rejects_partial_words_and_invalid_widths() {
        assert!(BlockBitReader::new(&[0; 3]).is_err());
        let mut reader = BlockBitReader::new(&[0; 4]).unwrap();
        assert!(reader.read_bits(0).is_err());
    }

    #[test]
    fn command_parser_matches_component_and_explicit_cursor_layouts() {
        // opcode=13, x=0x12, y=0x34, discard=0; component index=2,
        // present=0, skip=0b101, value=0x05_4321.  The remainder pads the
        // command word exactly as the official reader does.
        let word = (13u64 << 60)
            | (0x12u64 << 52)
            | (0x34u64 << 44)
            | (2u64 << 38)
            | (5u64 << 34)
            | (0x05_4321u64 << 7);
        // Each source word is independently little-endian; the first command
        // bits live in the MSB of the *first* assembled u32.
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&((word >> 32) as u32).to_le_bytes());
        bytes[4..].copy_from_slice(&(word as u32).to_le_bytes());
        let mut reader = BlockBitReader::new(&bytes).unwrap();
        assert_eq!(
            reader.next_command().unwrap(),
            Some(BlockCommand::ExplicitComponent {
                x: 0x12,
                y: 0x34,
                updates: vec![ComponentUpdate {
                    index: 2,
                    present: false,
                    value: 0x05_4321,
                }],
                pixel_selectors: vec![],
            })
        );
    }

    #[test]
    fn command_parser_keeps_stop_and_invalid_opcode_terminal() {
        let stop_bytes = 0x9000_0000u32.to_le_bytes();
        let mut stop = BlockBitReader::new(&stop_bytes).unwrap();
        assert_eq!(stop.next_command().unwrap(), None);
        let invalid_bytes = 0x1000_0000u32.to_le_bytes();
        let mut invalid = BlockBitReader::new(&invalid_bytes).unwrap();
        assert!(invalid.next_command().is_err());
    }

    #[test]
    fn component_selector_stream_does_not_shift_the_next_command() {
        let mut bits = Vec::new();
        // opcode 6, then two present component records with values 1 and 2.
        push_bits(&mut bits, 6, 4);
        for (index, value) in [(0, 1), (1, 2)] {
            push_bits(&mut bits, index, 2);
            push_bits(&mut bits, 1, 1);
            push_bits(&mut bits, value, 27);
        }
        for selector in 0..64 {
            bits.push((selector & 1) as u8);
        }
        push_bits(&mut bits, 9, 4);
        let bytes = source_words(&bits);
        let mut reader = BlockBitReader::new(&bytes).unwrap();
        let Some(BlockCommand::Component {
            pixel_selectors, ..
        }) = reader.next_command().unwrap()
        else {
            panic!("expected component command");
        };
        assert_eq!(pixel_selectors.len(), 64);
        assert_eq!(&pixel_selectors[..4], &[0, 1, 0, 1]);
        assert_eq!(reader.next_command().unwrap(), None);
    }
}
