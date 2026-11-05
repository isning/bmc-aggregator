use async_trait::async_trait;
use serde::Serialize;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RawPixelFormat {
    Rgba,
    Bgra,
}

impl RawPixelFormat {
    pub fn gst_caps_format(self) -> &'static str {
        match self {
            Self::Rgba => "RGBA",
            Self::Bgra => "BGRA",
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct VideoColorDepthProfile {
    pub id: String,
    pub depth_bpp: u8,
    pub variant: bool,
}

#[async_trait]
pub trait VideoSource: Send + Sync {
    async fn request_full_frame(&self) -> Result<bool, String>;
    fn color_depth_profiles(&self) -> Vec<VideoColorDepthProfile> {
        Vec::new()
    }
    fn active_color_depth_profile_id(&self) -> Option<String> {
        None
    }
    async fn set_color_depth(&self, _profile_id: &str) -> Result<(), String> {
        Err("video source does not support color-depth control".to_string())
    }
    fn trace_snapshot(&self) -> serde_json::Value {
        serde_json::json!({})
    }
}
