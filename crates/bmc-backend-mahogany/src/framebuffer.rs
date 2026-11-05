use gstreamer as gst;

use bmc_feature_api::video::RawPixelFormat;

pub struct Framebuffer {
    accum_buffer: Option<gst::Buffer>,
    pub width: u16,
    pub height: u16,
    pub dirty: bool,
    pub frame_count: u64,
    pub frame_start: Option<std::time::Instant>,
}

impl Framebuffer {
    pub const PIXEL_FORMAT: RawPixelFormat = RawPixelFormat::Rgba;

    pub fn new(width: u16, height: u16, _pool_size: usize) -> Self {
        Self {
            accum_buffer: None,
            width,
            height,
            dirty: false,
            frame_count: 0,
            frame_start: None,
        }
    }

    fn ensure_accum(&mut self) -> Result<&mut gst::Buffer, String> {
        gst::init().map_err(|error| format!("GStreamer initialization failed: {error}"))?;
        if self.accum_buffer.is_none() {
            self.accum_buffer = Some(blank_frame_buffer(self.width, self.height)?);
        }
        self.accum_buffer
            .as_mut()
            .ok_or_else(|| "framebuffer allocation produced no buffer".to_string())
    }

    #[cfg(test)]
    pub fn blit_tile(&mut self, x: u16, y: u16, tile_data: &[u8], tile_w: u16, tile_h: u16) {
        self.try_blit_tile(x, y, tile_data, tile_w, tile_h).unwrap();
    }

    pub fn try_blit_tile(
        &mut self,
        x: u16,
        y: u16,
        tile_data: &[u8],
        tile_w: u16,
        tile_h: u16,
    ) -> Result<(), String> {
        let required_bytes = usize::from(tile_w)
            .checked_mul(usize::from(tile_h))
            .and_then(|pixels| pixels.checked_mul(4))
            .ok_or_else(|| "tile byte length overflow".to_string())?;
        if tile_data.len() < required_bytes {
            return Err(format!(
                "tile needs {required_bytes} pixel bytes but has {}",
                tile_data.len()
            ));
        }
        if !self.dirty {
            self.frame_start = Some(std::time::Instant::now());
        }
        self.dirty = true;

        let fb_w = self.width as usize;
        let copy_w = usize::from(tile_w.min(self.width.saturating_sub(x)));
        let copy_h = usize::from(tile_h.min(self.height.saturating_sub(y)));
        let buf = self.ensure_accum()?;
        let buf_mut = buf
            .get_mut()
            .ok_or_else(|| "framebuffer is unexpectedly shared".to_string())?;
        let mut map = buf_mut
            .map_writable()
            .map_err(|error| format!("map framebuffer writable: {error}"))?;
        let pixels = map.as_mut_slice();

        // Clip in two dimensions while retaining the original source row stride.
        // A linear buffer bound alone lets right-edge pixels wrap into the next row.
        for ty in 0..copy_h {
            for tx in 0..copy_w {
                let src = (ty * tile_w as usize + tx) * 4;
                let dst = ((y as usize + ty) * fb_w + (x as usize + tx)) * 4;
                pixels[dst] = tile_data[src + 2];
                pixels[dst + 1] = tile_data[src + 1];
                pixels[dst + 2] = tile_data[src];
                pixels[dst + 3] = 255;
            }
        }
        Ok(())
    }

    /// Deep-copy the accumulation buffer into a new GstBuffer as a consistent
    /// snapshot for the WebRTC pipeline. Every call allocates and copies the
    /// full frame (8 MB at 1920×1080 RGBA) — acceptable because the encoder
    /// task runs at most at the configured FPS (typically 30, i.e. 240 MB/s).
    #[cfg(test)]
    pub fn finish_frame(&mut self) -> gst::Buffer {
        self.try_finish_frame().unwrap()
    }

    pub fn try_finish_frame(&mut self) -> Result<gst::Buffer, String> {
        let size = frame_byte_len(self.width, self.height)?;
        self.dirty = false;
        let mut dst = gst::Buffer::with_size(size)
            .map_err(|error| format!("allocate snapshot buffer: {error}"))?;
        {
            let src_map = self
                .accum_buffer
                .as_ref()
                .ok_or_else(|| "framebuffer has no accumulated pixels".to_string())?
                .map_readable()
                .map_err(|error| format!("map framebuffer readable: {error}"))?;
            let dst_mut = dst
                .get_mut()
                .ok_or_else(|| "new snapshot buffer is shared".to_string())?;
            let mut dst_map = dst_mut
                .map_writable()
                .map_err(|error| format!("map snapshot writable: {error}"))?;
            dst_map.as_mut_slice().copy_from_slice(&src_map);
        }
        Ok(dst)
    }

    #[cfg(test)]
    pub fn snapshot(&self) -> Option<gst::Buffer> {
        self.try_snapshot().unwrap()
    }

    pub fn try_snapshot(&self) -> Result<Option<gst::Buffer>, String> {
        let Some(src) = self.accum_buffer.as_ref() else {
            return Ok(None);
        };
        let size = frame_byte_len(self.width, self.height)?;
        let mut dst = gst::Buffer::with_size(size)
            .map_err(|error| format!("allocate snapshot buffer: {error}"))?;
        {
            let src_map = src
                .map_readable()
                .map_err(|error| format!("map framebuffer readable: {error}"))?;
            let dst_mut = dst
                .get_mut()
                .ok_or_else(|| "new snapshot buffer is shared".to_string())?;
            let mut dst_map = dst_mut
                .map_writable()
                .map_err(|error| format!("map snapshot writable: {error}"))?;
            dst_map.as_mut_slice().copy_from_slice(&src_map);
        }
        Ok(Some(dst))
    }

    pub fn snapshot_or_blank(&mut self) -> Result<gst::Buffer, String> {
        {
            let _ = self.ensure_accum()?;
        }
        self.try_snapshot()?
            .ok_or_else(|| "snapshot missing after framebuffer initialization".to_string())
    }

    #[cfg(test)]
    pub fn resize(&mut self, width: u16, height: u16) -> bool {
        self.try_resize(width, height).unwrap()
    }

    pub fn try_resize(&mut self, width: u16, height: u16) -> Result<bool, String> {
        frame_byte_len(width, height)?;
        if width == self.width && height == self.height {
            return Ok(false);
        }
        gst::init().map_err(|error| format!("GStreamer initialization failed: {error}"))?;
        self.accum_buffer = Some(blank_frame_buffer(width, height)?);
        self.width = width;
        self.height = height;
        self.dirty = false;
        self.frame_start = None;
        Ok(true)
    }
}

// Match the browser-facing raw-frame budget before allocating native buffers.
fn frame_byte_len(width: u16, height: u16) -> Result<usize, String> {
    const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;
    usize::from(width)
        .checked_mul(usize::from(height))
        .and_then(|pixels| pixels.checked_mul(4))
        .filter(|bytes| *bytes > 0 && *bytes <= MAX_FRAME_BYTES)
        .ok_or_else(|| format!("invalid or oversized Mahogany framebuffer: {width}x{height}"))
}

fn blank_frame_buffer(width: u16, height: u16) -> Result<gst::Buffer, String> {
    let size = frame_byte_len(width, height)?;
    let mut buf =
        gst::Buffer::with_size(size).map_err(|error| format!("allocate framebuffer: {error}"))?;
    {
        let buf_mut = buf
            .get_mut()
            .ok_or_else(|| "new framebuffer is shared".to_string())?;
        let mut map = buf_mut
            .map_writable()
            .map_err(|error| format!("map blank framebuffer writable: {error}"))?;
        let pixels = map.as_mut_slice();
        for px in pixels.chunks_exact_mut(4) {
            px[0] = 0;
            px[1] = 0;
            px[2] = 0;
            px[3] = 255;
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_gst() {
        gst::init().unwrap_or(());
    }

    #[test]
    fn test_framebuffer_new() {
        init_gst();
        let fb = Framebuffer::new(640, 480, 3);
        assert_eq!(fb.width, 640);
        assert_eq!(fb.height, 480);
        assert!(!fb.dirty);
        assert_eq!(fb.frame_count, 0);
    }

    #[test]
    fn truncated_tile_is_rejected_without_changing_pixels_or_publication_state() {
        init_gst();
        let mut fb = Framebuffer::new(2, 2, 3);
        fb.blit_tile(0, 0, &[10, 20, 30, 255].repeat(4), 2, 2);
        let before = fb.finish_frame();
        let frame_start = fb.frame_start;
        for length in [0usize, 1, 4, 7, 15] {
            assert!(fb.try_blit_tile(0, 0, &vec![90; length], 2, 2).is_err());
            assert!(!fb.dirty);
            assert_eq!(fb.frame_start, frame_start);
            assert_eq!(
                fb.snapshot().unwrap().map_readable().unwrap().as_slice(),
                before.map_readable().unwrap().as_slice()
            );
        }
    }

    #[test]
    fn test_resize_larger() {
        init_gst();
        let mut fb = Framebuffer::new(10, 10, 3);
        let tile = vec![0x11, 0x22, 0x33, 0x44];
        fb.blit_tile(0, 0, &tile, 1, 1);
        fb.resize(20, 20);
        assert_eq!(fb.width, 20);
        assert_eq!(fb.height, 20);
        assert!(!fb.dirty);
    }

    #[test]
    fn test_resize_smaller() {
        init_gst();
        let mut fb = Framebuffer::new(100, 100, 3);
        fb.resize(50, 50);
        assert_eq!(fb.width, 50);
        assert_eq!(fb.height, 50);
    }

    #[test]
    fn test_resize_same_size_noop() {
        init_gst();
        let mut fb = Framebuffer::new(100, 100, 3);
        fb.blit_tile(0, 0, &[0; 4], 1, 1);
        assert!(!fb.resize(100, 100));
        assert_eq!(fb.width, 100);
        assert_eq!(fb.height, 100);
    }

    #[test]
    fn test_dirty_flag_set_on_blit() {
        init_gst();
        let mut fb = Framebuffer::new(10, 10, 3);
        assert!(!fb.dirty);
        fb.blit_tile(0, 0, &[0; 4], 1, 1);
        assert!(fb.dirty);
    }

    #[test]
    fn test_finish_frame_returns_snapshot() {
        init_gst();
        let mut fb = Framebuffer::new(16, 16, 3);
        fb.blit_tile(0, 0, &[0x11, 0x22, 0x33, 0x44], 1, 1);

        let snapshot = fb.finish_frame();
        let map = snapshot.map_readable().unwrap();
        assert_eq!(map[0], 0x33);
        assert_eq!(map[1], 0x22);
        assert_eq!(map[2], 0x11);
        assert_eq!(map[3], 255);
    }

    #[test]
    fn test_unwritten_pixels_are_black_opaque_on_first_snapshot() {
        init_gst();
        let mut fb = Framebuffer::new(2, 1, 3);
        fb.blit_tile(0, 0, &[0x11, 0x22, 0x33, 0x44], 1, 1);

        let snapshot = fb.finish_frame();
        let map = snapshot.map_readable().unwrap();
        assert_eq!(&map[0..4], &[0x33, 0x22, 0x11, 255]);
        assert_eq!(&map[4..8], &[0, 0, 0, 255]);
    }

    #[test]
    fn test_snapshot_or_blank_before_first_blit_returns_base_frame() {
        init_gst();
        let mut fb = Framebuffer::new(2, 1, 3);

        let snapshot = fb.snapshot_or_blank().unwrap();
        let map = snapshot.map_readable().unwrap();

        assert_eq!(map.len(), 8);
        assert_eq!(&map[0..4], &[0, 0, 0, 255]);
        assert_eq!(&map[4..8], &[0, 0, 0, 255]);
        assert!(fb.snapshot().is_some());
    }

    #[test]
    fn test_finish_frame_snapshot_independent() {
        init_gst();
        let mut fb = Framebuffer::new(16, 16, 3);
        fb.blit_tile(0, 0, &[0x11, 0x22, 0x33, 0x44], 1, 1);
        let snapshot1 = fb.finish_frame();
        assert_eq!(snapshot1.map_readable().unwrap()[0], 0x33);

        fb.blit_tile(0, 0, &[0xAA, 0xBB, 0xCC, 0xDD], 1, 1);
        let snapshot2 = fb.finish_frame();
        assert_eq!(snapshot2.map_readable().unwrap()[0], 0xCC);

        assert_eq!(snapshot1.map_readable().unwrap()[0], 0x33);
    }

    #[test]
    fn test_overlapping_tiles_last_wins() {
        init_gst();
        let mut fb = Framebuffer::new(10, 10, 3);
        let red = vec![0x00, 0x00, 0xFF, 0xFF];
        let green = vec![0x00, 0xFF, 0x00, 0xFF];
        fb.blit_tile(0, 0, &red, 1, 1);
        fb.blit_tile(0, 0, &green, 1, 1);
        let snap = fb.finish_frame();
        let map = snap.map_readable().unwrap();
        assert_eq!(map[0], 0x00);
        assert_eq!(map[1], 0xFF);
        assert_eq!(map[2], 0x00);
    }

    #[test]
    fn test_resize_realloc_clears_accum() {
        init_gst();
        let mut fb = Framebuffer::new(4, 4, 3);
        fb.blit_tile(0, 0, &[0x11, 0x22, 0x33, 0xFF], 1, 1);
        assert!(fb.resize(8, 8));
        assert_eq!(fb.width, 8);
        assert_eq!(fb.height, 8);
        let snap = fb.snapshot().expect("resized framebuffer should have base");
        let map = snap.map_readable().unwrap();
        assert_eq!(&map[0..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn test_resize_same_byte_size_reinitializes_base_frame() {
        init_gst();
        let mut fb = Framebuffer::new(2, 3, 3);
        fb.blit_tile(0, 0, &[0x11, 0x22, 0x33, 0xFF], 1, 1);
        let _ = fb.finish_frame();

        assert!(fb.resize(3, 2));

        assert_eq!(fb.width, 3);
        assert_eq!(fb.height, 2);
        assert!(!fb.dirty);
        let snap = fb.snapshot().expect("resized framebuffer should have base");
        let map = snap.map_readable().unwrap();
        assert_eq!(map.len(), 3 * 2 * 4);
        assert!(map.chunks_exact(4).all(|px| px == [0, 0, 0, 255]));
    }

    #[test]
    fn test_dirty_flag_cleared_on_finish_frame() {
        init_gst();
        let mut fb = Framebuffer::new(10, 10, 3);
        fb.blit_tile(0, 0, &[0; 4], 1, 1);
        assert!(fb.dirty);
        fb.finish_frame();
        assert!(!fb.dirty);
    }

    #[test]
    fn test_frame_start_set_on_first_blit() {
        init_gst();
        let mut fb = Framebuffer::new(10, 10, 3);
        assert!(fb.frame_start.is_none());
        fb.blit_tile(0, 0, &[0; 4], 1, 1);
        assert!(fb.frame_start.is_some());
    }

    #[test]
    fn test_tile_on_accum_survives_finish_frame() {
        init_gst();
        let mut fb = Framebuffer::new(8, 8, 3);
        fb.blit_tile(0, 0, &[0x11, 0x22, 0x33, 0xFF], 1, 1);
        let _snap = fb.finish_frame();
        fb.blit_tile(1, 0, &[0xAA, 0xBB, 0xCC, 0xFF], 1, 1);
        let snap = fb.finish_frame();
        let map = snap.map_readable().unwrap();
        assert_eq!(map[0], 0x33);
        assert_eq!(map[4], 0xCC);
    }

    #[test]
    fn test_consecutive_finish_frame_no_blit() {
        init_gst();
        let mut fb = Framebuffer::new(8, 8, 3);
        fb.blit_tile(0, 0, &[0x11, 0x22, 0x33, 0xFF], 1, 1);
        let snap1 = fb.finish_frame();
        let snap2 = fb.finish_frame();
        let m1 = snap1.map_readable().unwrap();
        let m2 = snap2.map_readable().unwrap();
        assert_eq!(m1.len(), m2.len());
        assert!(m1.iter().zip(m2.iter()).all(|(a, b)| a == b));
    }

    #[test]
    fn test_snapshot_without_dirty_returns_base_frame() {
        init_gst();
        let mut fb = Framebuffer::new(8, 8, 3);
        fb.blit_tile(0, 0, &[0x11, 0x22, 0x33, 0xFF], 1, 1);
        let _ = fb.finish_frame();
        assert!(!fb.dirty);
        let snap = fb.snapshot().expect("snapshot should exist");
        let map = snap.map_readable().unwrap();
        assert_eq!(map[0], 0x33);
        assert_eq!(map[1], 0x22);
        assert_eq!(map[2], 0x11);
        assert_eq!(map[3], 255);
    }

    #[test]
    fn test_bgra_to_rgba_byte_swap() {
        init_gst();
        let mut fb = Framebuffer::new(1, 1, 3);
        fb.blit_tile(0, 0, &[0xAA, 0xBB, 0xCC, 0xDD], 1, 1);
        let snap = fb.finish_frame();
        let map = snap.map_readable().unwrap();
        assert_eq!(map[0], 0xCC);
        assert_eq!(map[1], 0xBB);
        assert_eq!(map[2], 0xAA);
        assert_eq!(map[3], 255);
    }

    #[test]
    fn framebuffer_budget_rejects_oversized_dimensions_before_publication() {
        init_gst();
        assert_eq!(frame_byte_len(4096, 4096).unwrap(), 64 * 1024 * 1024);
        assert!(frame_byte_len(u16::MAX, u16::MAX).is_err());
        let mut fb = Framebuffer::new(1, 1, 3);
        fb.blit_tile(0, 0, &[0, 0, 80, 255], 1, 1);
        // A guard regression remains bounded to about 64 MiB in this test.
        assert!(fb.try_resize(4097, 4096).is_err());
        assert_eq!((fb.width, fb.height), (1, 1));
        assert_eq!(
            fb.snapshot().unwrap().map_readable().unwrap().as_slice(),
            &[80, 0, 0, 255]
        );
    }

    #[test]
    fn zero_size_resize_is_rejected_without_discarding_the_current_frame() {
        init_gst();
        for (width, height) in [(0, 1), (1, 0), (0, 0)] {
            let mut fb = Framebuffer::new(1, 1, 3);
            fb.blit_tile(0, 0, &[0, 0, 80, 255], 1, 1);
            assert!(
                fb.try_resize(width, height).is_err(),
                "accepted {width}x{height}"
            );
            assert_eq!((fb.width, fb.height), (1, 1));
            assert_eq!(
                fb.snapshot().unwrap().map_readable().unwrap().as_slice(),
                &[80, 0, 0, 255]
            );
        }
    }

    #[test]
    fn right_edge_tile_preserves_other_rows_and_original_source_stride() {
        init_gst();
        let mut fb = Framebuffer::new(4, 3, 3);
        let tile = [0, 0, 10, 255, 0, 0, 20, 255, 0, 0, 30, 255, 0, 0, 40, 255];
        fb.blit_tile(3, 0, &tile, 2, 2);
        let snapshot = fb.finish_frame();
        let pixels = snapshot.map_readable().unwrap();
        for y in 0..3 {
            for x in 0..4 {
                let red = match (x, y) {
                    (3, 0) => 10,
                    (3, 1) => 30,
                    _ => 0,
                };
                let offset = (y * 4 + x) * 4;
                assert_eq!(
                    &pixels[offset..offset + 4],
                    &[red, 0, 0, 255],
                    "pixel ({x},{y})"
                );
            }
        }
    }

    #[test]
    fn horizontally_offscreen_tiles_do_not_alias_visible_pixels() {
        init_gst();
        for x in [4, 5, u16::MAX] {
            let mut fb = Framebuffer::new(4, 3, 3);
            fb.blit_tile(0, 0, &[0, 0, 0, 255].repeat(12), 4, 3);
            let before = fb.snapshot().unwrap();
            fb.blit_tile(x, 0, &[0, 0, 200, 255], 1, 1);
            let after = fb.finish_frame();
            assert_eq!(
                before.map_readable().unwrap().as_slice(),
                after.map_readable().unwrap().as_slice(),
                "offscreen x={x}"
            );
        }
    }

    #[test]
    fn test_tile_clips_right_and_bottom_edge() {
        init_gst();
        let mut fb = Framebuffer::new(4, 4, 3);
        let mut tile = vec![0u8; 6 * 6 * 4];
        for i in 0..6u16 {
            for j in 0..6u16 {
                let idx = (i as usize * 6 + j as usize) * 4;
                let r = (i * 16 + j * 32) as u8;
                tile[idx] = 0;
                tile[idx + 1] = 0;
                tile[idx + 2] = r;
                tile[idx + 3] = 0xFF;
            }
        }
        fb.blit_tile(2, 2, &tile, 6, 6);
        let snap = fb.finish_frame();
        let map = snap.map_readable().unwrap();
        for y in 2..4 {
            for x in 2..4 {
                let pos = (y as usize * 4 + x as usize) * 4;
                let tx = x - 2;
                let ty = y - 2;
                let r = (ty * 16 + tx * 32) as u8;
                assert_eq!(map[pos], r, "R at ({},{})", x, y);
                assert_eq!(map[pos + 3], 255, "A at ({},{})", x, y);
            }
        }
    }
}
