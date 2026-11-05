use tokio::io::{AsyncRead, AsyncReadExt};

pub const REMOVABLE_BLOCK_SIZE: usize = 512;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RemovableGeometry {
    pub capacity_blocks: u32,
    pub cylinders: u16,
    pub heads: u8,
    pub sectors: u8,
}

pub async fn validate_removable_image(path: &str, size: u64) -> Result<RemovableGeometry, String> {
    if size == 0 || !size.is_multiple_of(REMOVABLE_BLOCK_SIZE as u64) {
        return Err("removable image size must be a non-zero multiple of 512 bytes".to_string());
    }
    let capacity_blocks = u32::try_from(size / REMOVABLE_BLOCK_SIZE as u64)
        .map_err(|_| "removable image is too large".to_string())?;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|error| format!("open removable image: {error}"))?;
    detect_removable_geometry(&mut file, capacity_blocks).await
}

/// Detect geometry for a removable-media image. FAT superfloppies retain
/// their BPB geometry, partitioned images are bounds checked, and other raw
/// filesystem/blank images use translated CHS.
pub async fn detect_removable_geometry<R>(
    image: &mut R,
    capacity_blocks: u32,
) -> Result<RemovableGeometry, String>
where
    R: AsyncRead + Unpin,
{
    let mut sector0 = [0u8; REMOVABLE_BLOCK_SIZE];
    image
        .read_exact(&mut sector0)
        .await
        .map_err(|error| format!("read removable boot sector: {error}"))?;

    if matches!(sector0[0], 0xeb | 0xe9) {
        let standard = (
            u16::from_le_bytes([sector0[26], sector0[27]]),
            u16::from_le_bytes([sector0[24], sector0[25]]),
        );
        // Older Dell clients consumed these bytes in a shifted order.
        let dell_legacy = (
            u16::from_be_bytes([sector0[25], sector0[26]]),
            u16::from_be_bytes([sector0[23], sector0[24]]),
        );
        for (heads, sectors) in [standard, dell_legacy] {
            if heads > 0
                && heads <= u16::from(u8::MAX)
                && sectors > 0
                && sectors <= u16::from(u8::MAX)
            {
                return geometry(capacity_blocks, heads as u8, sectors as u8, true);
            }
        }
        return Err("removable image has an invalid FAT BPB geometry".to_string());
    }

    if sector0[510..512] == [0x55, 0xaa] {
        let partitions: Vec<_> = (0..4)
            .map(|index| 446 + index * 16)
            .filter_map(|offset| {
                let partition_type = sector0[offset + 4];
                let start = u32::from_le_bytes(sector0[offset + 8..offset + 12].try_into().ok()?);
                let blocks = u32::from_le_bytes(sector0[offset + 12..offset + 16].try_into().ok()?);
                (start > 0 && blocks > 0).then_some((partition_type, start, blocks))
            })
            .collect();
        if partitions.is_empty() {
            return Err("removable MBR has no non-empty partition".to_string());
        }
        for (_, start, blocks) in &partitions {
            let end = start
                .checked_add(*blocks)
                .ok_or_else(|| "removable MBR partition capacity overflows".to_string())?;
            if end > capacity_blocks {
                return Err("removable MBR partition extends beyond the image".to_string());
            }
        }
        if partitions.iter().any(|(kind, _, _)| *kind == 0xee) {
            let mut header = [0u8; REMOVABLE_BLOCK_SIZE];
            image
                .read_exact(&mut header)
                .await
                .map_err(|error| format!("read removable GPT header: {error}"))?;
            validate_gpt_header(&header, capacity_blocks)?;
        }
        return translated_geometry(capacity_blocks, true);
    }

    capacity_geometry(capacity_blocks)
}

fn validate_gpt_header(
    header: &[u8; REMOVABLE_BLOCK_SIZE],
    capacity_blocks: u32,
) -> Result<(), String> {
    if &header[..8] != b"EFI PART" {
        return Err("removable protective MBR has no valid GPT header".to_string());
    }
    let header_size = u32::from_le_bytes(header[12..16].try_into().expect("bounded"));
    if !(92..=REMOVABLE_BLOCK_SIZE as u32).contains(&header_size) {
        return Err("removable GPT header has an invalid size".to_string());
    }
    let first_usable = u64::from_le_bytes(header[40..48].try_into().expect("bounded"));
    let last_usable = u64::from_le_bytes(header[48..56].try_into().expect("bounded"));
    if first_usable == 0 || first_usable > last_usable || last_usable >= u64::from(capacity_blocks)
    {
        return Err("removable GPT usable range extends beyond the image".to_string());
    }
    Ok(())
}

fn capacity_geometry(capacity_blocks: u32) -> Result<RemovableGeometry, String> {
    match capacity_blocks {
        320 => geometry(capacity_blocks, 1, 8, true),
        360 => geometry(capacity_blocks, 1, 9, true),
        640 => geometry(capacity_blocks, 2, 8, true),
        720 => geometry(capacity_blocks, 2, 9, true),
        1_440 => geometry(capacity_blocks, 2, 9, true),
        2_400 => geometry(capacity_blocks, 2, 15, true),
        2_880 => geometry(capacity_blocks, 2, 18, true),
        5_760 => geometry(capacity_blocks, 2, 36, true),
        _ => translated_geometry(capacity_blocks, true),
    }
}

fn translated_geometry(capacity_blocks: u32, round_up: bool) -> Result<RemovableGeometry, String> {
    let blocks_per_cylinder = 16 * 63;
    let cylinders = (if round_up {
        capacity_blocks.div_ceil(blocks_per_cylinder)
    } else {
        capacity_blocks / blocks_per_cylinder
    })
    .clamp(1, 0x3fff) as u16;
    Ok(RemovableGeometry {
        capacity_blocks,
        cylinders,
        heads: 16,
        sectors: 63,
    })
}

fn geometry(
    capacity_blocks: u32,
    heads: u8,
    sectors: u8,
    round_up: bool,
) -> Result<RemovableGeometry, String> {
    if capacity_blocks == 0 || heads == 0 || sectors == 0 {
        return Err("removable image has invalid geometry".to_string());
    }
    let divisor = u32::from(heads) * u32::from(sectors);
    let cylinders = if round_up {
        capacity_blocks.div_ceil(divisor)
    } else {
        capacity_blocks / divisor
    };
    let cylinders = u16::try_from(cylinders)
        .map_err(|_| "removable image has too many cylinders".to_string())?;
    Ok(RemovableGeometry {
        capacity_blocks,
        cylinders,
        heads,
        sectors,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[tokio::test]
    async fn file_validation_rejects_non_block_aligned_images() {
        let mut image = tempfile::NamedTempFile::new().unwrap();
        image.write_all(&[0; 513]).unwrap();
        let error = validate_removable_image(image.path().to_str().unwrap(), 513)
            .await
            .unwrap_err();
        assert!(error.contains("multiple of 512"));
    }

    #[tokio::test]
    async fn file_validation_accepts_conventional_raw_floppy_images() {
        let mut image = tempfile::NamedTempFile::new().unwrap();
        image.write_all(&vec![0; 1_474_560]).unwrap();
        let geometry = validate_removable_image(image.path().to_str().unwrap(), 1_474_560)
            .await
            .unwrap();
        assert_eq!(geometry.capacity_blocks, 2_880);
        assert_eq!(
            (geometry.cylinders, geometry.heads, geometry.sectors),
            (80, 2, 18)
        );
    }
}
