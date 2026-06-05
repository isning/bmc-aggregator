use std::sync::Arc;

use tracing::{info, warn};

use crate::bmc::session::registry::DeviceRegistry;

pub struct EncoderManager {
    registry: Arc<DeviceRegistry>,
}

impl EncoderManager {
    pub fn new(registry: Arc<DeviceRegistry>) -> Self {
        Self { registry }
    }

    pub async fn set_encoder_backend(&self, id: &str, backend: String) -> Result<(), anyhow::Error> {
        let dev = self.registry.get_dev(id).await
            .ok_or_else(|| anyhow::anyhow!("device not found: {}", id))?;
        *dev.current_encoder_backend.write().await = backend.clone();
        Ok(())
    }

    pub async fn set_encoder_runtime(&self, id: &str, backend_id: &str) -> String {
        let dev = match self.registry.get_dev(id).await {
            Some(d) => d,
            None => {
                warn!("[{}] cannot switch encoder: device not found", id);
                return String::new();
            }
        };

        // Check if the backend is a valid frame encoder.
        let is_frame_enc = crate::bmc::encoder::backend::probe_encoder(backend_id).available;
        if is_frame_enc {
            // Frame encoder available — switch the HTTP runtime encoder.
            *dev.encoder_error.write().await = None;
            let new_enc = crate::bmc::encoder::backend::create_encoder(backend_id);
            *dev.encoder.write().await = new_enc;
            *dev.current_encoder_backend.write().await = backend_id.to_string();
            info!("[{}] encoder switched to '{}' at runtime", id, backend_id);
            return backend_id.to_string();
        }

        // Not a frame encoder — check if it's a known stream encoder (e.g. nvenc).
        let is_stream_enc = crate::bmc::registry::find_backend(
            crate::bmc::stream::encoder::STREAM_ENCODER_BACKENDS,
            backend_id,
        ).is_some();
        if is_stream_enc {
            // Stream encoder — update current_encoder_backend only, FrameEncoder stays as-is.
            if crate::bmc::encoder::registry::probe_stream_encoder(backend_id) {
                *dev.current_encoder_backend.write().await = backend_id.to_string();
                *dev.encoder_error.write().await = None;
                info!("[{}] encoder set to '{}'", id, backend_id);
                backend_id.to_string()
            } else {
                let msg = format!("Encoder '{}' not available on this system (FFmpeg build missing)", backend_id);
                warn!("[{}] {}", id, msg);
                *dev.encoder_error.write().await = Some(msg);
                String::new()
            }
        } else {
            // Unknown backend — fallback to software.
            let msg = format!("Encoder '{}' not available, using software", backend_id);
            warn!("[{}] {}", id, msg);
            *dev.encoder_error.write().await = Some(msg);
            let new_enc = crate::bmc::encoder::backend::create_encoder("software");
            *dev.encoder.write().await = new_enc;
            *dev.current_encoder_backend.write().await = "software".to_string();
            "software".to_string()
        }
    }

    pub async fn backend_info(&self, id: &str) -> Option<(String, String, Option<String>)> {
        let dev = self.registry.get_dev(id).await?;
        let enc = dev.current_encoder_backend.read().await.clone();
        Some((
            dev.cfg.video_backend.clone(),
            enc,
            dev.cfg.audio_backend.clone(),
        ))
    }

    pub async fn encoder_error(&self, id: &str) -> Option<String> {
        let dev = self.registry.get_dev(id).await;
        let dev = match dev {
            Some(d) => d,
            None => return None,
        };
        let err = dev.encoder_error.read().await.clone();
        err
    }

    pub async fn pipeline_perf_snapshot(&self, id: &str) -> Option<serde_json::Value> {
        let dev = self.registry.get_dev(id).await?;
        let pipeline = dev.pipeline.read().await;
        let perf = pipeline.as_ref().map(|p| p.perf_snapshot());
        if perf.is_none() {
            tracing::debug!(target: "perf", "pipeline_perf_snapshot: no pipeline for {id}");
        }
        perf
    }
}
