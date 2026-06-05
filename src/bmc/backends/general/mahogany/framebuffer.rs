pub struct Framebuffer {
    pub width: u16,
    pub height: u16,
    pub pixels: Vec<u8>,
    pub dirty: bool,
    pub frame_count: u64,
    /// Timestamp when the current frame's first tile was received (pipeline latency start).
    pub frame_start: Option<std::time::Instant>,
}

impl Framebuffer {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
            dirty: false,
            frame_count: 0,
            frame_start: None,
        }
    }

    pub fn blit_tile(&mut self, x: u16, y: u16, tile_data: &[u8], tile_w: u16, tile_h: u16) {
        let fb_w = self.width as usize;
        for ty in 0..tile_h as usize {
            for tx in 0..tile_w as usize {
                let src = (ty * tile_w as usize + tx) * 4;
                let dst = ((y as usize + ty) * fb_w + (x as usize + tx)) * 4;
                if src + 3 < tile_data.len() && dst + 3 < self.pixels.len() {
                    // BGRA wire format → RGBA framebuffer
                    self.pixels[dst] = tile_data[src + 2];
                    self.pixels[dst + 1] = tile_data[src + 1];
                    self.pixels[dst + 2] = tile_data[src];
                    self.pixels[dst + 3] = 255;
                }
            }
        }
        if !self.dirty {
            self.frame_start = Some(std::time::Instant::now());
        }
        self.dirty = true;
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        let new_size = width as usize * height as usize * 4;
        if new_size != self.pixels.len() {
            self.pixels.resize(new_size, 0);
            self.width = width;
            self.height = height;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_framebuffer_new() {
        let fb = Framebuffer::new(640, 480);
        assert_eq!(fb.width, 640);
        assert_eq!(fb.height, 480);
        assert_eq!(fb.pixels.len(), 640 * 480 * 4);
        assert!(!fb.dirty);
        assert_eq!(fb.frame_count, 0);
        // All pixels should be zero (transparent black initially)
        assert!(fb.pixels.iter().all(|&b| b == 0));
    }

    #[test]
    fn test_blit_single_pixel() {
        let mut fb = Framebuffer::new(100, 100);
        // BGRA pixel: B=0x11, G=0x22, R=0x33, A=0x44
        let tile = vec![0x11, 0x22, 0x33, 0x44];
        fb.blit_tile(10, 20, &tile, 1, 1);

        // Read back: framebuffer stores RGBA
        let idx = (20 * 100 + 10) * 4;
        assert_eq!(fb.pixels[idx], 0x33); // R
        assert_eq!(fb.pixels[idx + 1], 0x22); // G
        assert_eq!(fb.pixels[idx + 2], 0x11); // B
        assert_eq!(fb.pixels[idx + 3], 255); // A forced to 255
        assert!(fb.dirty);
    }

    #[test]
    fn test_blit_tile_2x2() {
        let mut fb = Framebuffer::new(10, 10);
        // 2x2 tile: 4 pixels * 4 bytes = 16 bytes of BGRA
        // Pixel (0,0): B=1,G=2,R=3,A=4
        // Pixel (1,0): B=5,G=6,R=7,A=8
        // Pixel (0,1): B=9,G=10,R=11,A=12
        // Pixel (1,1): B=13,G=14,R=15,A=16
        let tile: Vec<u8> = (1..=16).collect();
        fb.blit_tile(0, 0, &tile, 2, 2);

        // Check pixel (0,0): should be R=3,G=2,B=1,A=255
        assert_eq!(fb.pixels[0], 3);
        assert_eq!(fb.pixels[1], 2);
        assert_eq!(fb.pixels[2], 1);
        assert_eq!(fb.pixels[3], 255);
        // Check pixel (1,0): should be R=7,G=6,B=5,A=255
        assert_eq!(fb.pixels[4], 7);
        assert_eq!(fb.pixels[5], 6);
        assert_eq!(fb.pixels[6], 5);
        assert_eq!(fb.pixels[7], 255);
        // Check pixel (0,1): R=11,G=10,B=9,A=255
        let idx = (1 * 10 + 0) * 4;
        assert_eq!(fb.pixels[idx], 11);
        assert_eq!(fb.pixels[idx + 1], 10);
        assert_eq!(fb.pixels[idx + 2], 9);
        assert_eq!(fb.pixels[idx + 3], 255);
    }

    #[test]
    fn test_blit_tile_clips_at_edge() {
        let mut fb = Framebuffer::new(5, 5);
        // Tile at (4,4) with size 2x2 — only one pixel should be in bounds
        let tile = vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88,
                        0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF, 0x00];
        fb.blit_tile(4, 4, &tile, 2, 2);
        // Only pixel at (4,4) should be written (indices 0-3 of tile)
        let idx = (4 * 5 + 4) * 4;
        assert_eq!(fb.pixels[idx], 0x33);
        assert_eq!(fb.pixels[idx + 1], 0x22);
        assert_eq!(fb.pixels[idx + 2], 0x11);
        assert!(fb.dirty);
    }

    #[test]
    fn test_resize_larger() {
        let mut fb = Framebuffer::new(10, 10);
        fb.blit_tile(0, 0, &[0x11, 0x22, 0x33, 0x44], 1, 1);
        fb.resize(20, 20);
        assert_eq!(fb.width, 20);
        assert_eq!(fb.height, 20);
        assert_eq!(fb.pixels.len(), 20 * 20 * 4);
        // Old pixel data should be preserved in the first pixel
        assert_eq!(fb.pixels[0], 0x33);
        assert_eq!(fb.pixels[1], 0x22);
    }

    #[test]
    fn test_resize_smaller() {
        let mut fb = Framebuffer::new(100, 100);
        fb.resize(50, 50);
        assert_eq!(fb.width, 50);
        assert_eq!(fb.height, 50);
        assert_eq!(fb.pixels.len(), 50 * 50 * 4);
    }

    #[test]
    fn test_resize_same_size_noop() {
        let mut fb = Framebuffer::new(100, 100);
        let ptr = fb.pixels.as_ptr();
        fb.resize(100, 100);
        assert_eq!(fb.pixels.as_ptr(), ptr); // no reallocation
    }

    #[test]
    fn test_dirty_flag_set_on_blit() {
        let mut fb = Framebuffer::new(10, 10);
        assert!(!fb.dirty);
        fb.blit_tile(0, 0, &[0; 4], 1, 1);
        assert!(fb.dirty);
    }
}
