use std::sync::Arc;

use crate::bmc::encoder::Codec;
use crate::bmc::session::registry::DeviceRegistry;
use crate::bmc::stream::encoder::create_stream_encoder;
use crate::bmc::stream::pipeline::SharedVideoPipeline;

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies that `std::thread::spawn` + `oneshot` + `timeout` fires when
    /// a CPU-hogging async task (simulating MJPEG encoding) blocks the ONLY worker thread.
    #[tokio::test(flavor = "current_thread")]
    async fn test_timeout_fires_when_async_runtime_blocked() {
        // Simulate MJPEG encoding loop hogging the single worker thread:
        // 50ms of CPU work + 1ms async yield, repeating.
        let hog = tokio::spawn(async {
            for _ in 0..200 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        });

        // The pattern we use in set_pipeline_encoder:
        let (tx, rx) = tokio::sync::oneshot::channel::<i32>();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(10));
            let _ = tx.send(99);
        });

        let start = std::time::Instant::now();
        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            rx,
        ).await {
            Ok(v) => panic!("expected timeout, got value={v:?} after {:?}", start.elapsed()),
            Err(_) => {
                let elapsed = start.elapsed();
                // Should fire within ~500ms + one CPU burst (50ms)
                assert!(elapsed.as_secs() < 2,
                    "timeout took too long: {:?} (runtime blocked by CPU-hogging task?)", elapsed);
                eprintln!("timeout correctly fired after {:?}", elapsed);
            }
        }

        drop(hog);
    }

    /// Same test but with multi-threaded runtime (2 workers) to compare.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn test_timeout_multithread_runtime() {
        let hog = tokio::spawn(async {
            for _ in 0..200 {
                std::thread::sleep(std::time::Duration::from_millis(50));
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        });

        let (tx, rx) = tokio::sync::oneshot::channel::<i32>();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(10));
            let _ = tx.send(99);
        });

        let start = std::time::Instant::now();
        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            rx,
        ).await {
            Ok(v) => panic!("expected timeout, got value={v:?} after {:?}", start.elapsed()),
            Err(_) => {
                let elapsed = start.elapsed();
                assert!(elapsed.as_secs() < 1,
                    "timeout took too long: {:?}", elapsed);
                eprintln!("multi-thread timeout fired after {:?}", elapsed);
            }
        }

        drop(hog);
    }

    /// Simulate real MJPEG loop: interval(33ms) + CPU work (60ms).
    /// With MissedTickBehavior::Skip, tick() always waits → runtime stays responsive.
    #[tokio::test(flavor = "current_thread")]
    async fn test_mjpeg_skip_behavior_unblocks_timeout() {
        let hog = tokio::spawn(async {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(33));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            for _ in 0..200 {
                interval.tick().await;
                std::thread::sleep(std::time::Duration::from_millis(60));
            }
        });

        let (tx, rx) = tokio::sync::oneshot::channel::<i32>();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(10));
            let _ = tx.send(99);
        });

        let start = std::time::Instant::now();
        match tokio::time::timeout(
            std::time::Duration::from_millis(500),
            rx,
        ).await {
            Ok(v) => panic!("expected timeout, got value={v:?} after {:?}", start.elapsed()),
            Err(_) => {
                let elapsed = start.elapsed();
                assert!(elapsed.as_millis() < 2000,
                    "Skip should unblock runtime, took {:?}", elapsed);
                eprintln!("Skip works: timeout after {:?}", elapsed);
            }
        }

        hog.abort();
    }
}

pub struct PipelineManager {
    registry: Arc<DeviceRegistry>,
}

impl PipelineManager {
    pub fn new(registry: Arc<DeviceRegistry>) -> Self {
        Self { registry }
    }

    pub async fn set_pipeline_encoder(&self, device_id: &str, codec_id: &str) -> Result<(), String> {
        let dev = self.registry.get_dev(device_id).await
            .ok_or_else(|| "device not found".to_string())?;

        let base = dev.get_base().await
            .ok_or_else(|| "device not connected".to_string())?;
        let info = base.info().await;
        let width = info.width.max(1) as u32;
        let height = info.height.max(1) as u32;
        let fps = base.fps().max(1) as u32;

        let codec = Codec::from_str(codec_id)
            .filter(|c| c.supports_webrtc())
            .ok_or_else(|| format!("unknown or non-streaming codec: {codec_id}"))?;

        let backend = dev.current_encoder_backend.read().await.clone();
        tracing::info!("[{}] spawning encoder init thread with 5s timeout", device_id);
        let (tx, rx) = tokio::sync::oneshot::channel();
        let backend_owned = backend.clone();
        let before = std::time::Instant::now();
        std::thread::spawn(move || {
            let result = create_stream_encoder(codec, &backend_owned, width, height, 2000, fps);
            let _ = tx.send(result);
        });
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            rx,
        ).await;
        let elapsed = before.elapsed();
        tracing::warn!("[{}] timeout::elapsed = {:?}", device_id, elapsed);
        eprintln!("[{}] TIMEOUT_DIAG elapsed={:?} result_is_err={}", device_id, elapsed, result.is_err());

        let encoder = match result {
            Ok(Ok(Ok(enc))) => {
                *dev.encoder_error.write().await = None;
                enc
            }
            Ok(Ok(Err(e))) => {
                let msg = format!("encoder init for {codec_id} with backend '{backend}': {e}");
                tracing::error!("[{}] {msg}", device_id);
                *dev.encoder_error.write().await = Some(msg.clone());
                return Err(msg);
            }
            Ok(Err(_)) => {
                let msg = format!("encoder thread for {codec_id} died without sending result");
                tracing::error!("[{}] {msg}", device_id);
                *dev.encoder_error.write().await = Some(msg.clone());
                return Err(msg);
            }
            Err(_) => {
                let msg = format!("encoder init for {codec_id} with backend '{backend}': timed out after 5s");
                tracing::error!("[{}] {msg}", device_id);
                *dev.encoder_error.write().await = Some(msg.clone());
                return Err(msg);
            }
        };

        let pipeline_exists = dev.pipeline.read().await.is_some();
        if pipeline_exists {
            let pipeline = dev.pipeline.read().await.as_ref().cloned().unwrap();
            let sessions = dev.webrtc_sessions.read().await;
            for s in sessions.values() {
                s.close().await;
            }
            drop(sessions);
            dev.webrtc_sessions.write().await.clear();
            pipeline.set_encoder(encoder);
        } else {
            let pipeline = SharedVideoPipeline::new(encoder);
            base.set_pipeline(pipeline.clone());
            *dev.pipeline.write().await = Some(pipeline);
        }

        tracing::info!("[{}] pipeline encoder switched to {codec_id}", device_id);
        Ok(())
    }

    pub async fn stop_pipeline(&self, device_id: &str) -> Result<(), String> {
        let dev = self.registry.get_dev(device_id).await
            .ok_or_else(|| "device not found".to_string())?;

        if dev.pipeline.read().await.is_some() {
            let sessions = dev.webrtc_sessions.read().await;
            for s in sessions.values() {
                s.close().await;
            }
            drop(sessions);
            dev.webrtc_sessions.write().await.clear();
        }

        *dev.pipeline.write().await = None;
        tracing::info!("[{}] pipeline stopped, back to HTTP mode", device_id);
        Ok(())
    }

    pub async fn set_device_pipeline(&self, device_id: &str, pipeline: Option<Arc<SharedVideoPipeline>>) {
        if let Some(dev) = self.registry.get_dev(device_id).await {
            *dev.pipeline.write().await = pipeline;
        }
    }

    pub async fn set_device_mode(&self, device_id: &str, mode: &str) -> Result<(), String> {
        let dev = self.registry.get_dev(device_id).await
            .ok_or_else(|| "device not found".to_string())?;
        *dev.mode.write().await = mode.to_string();
        Ok(())
    }

    pub async fn device_mode(&self, device_id: &str) -> Option<String> {
        let dev = self.registry.get_dev(device_id).await;
        let dev = match dev {
            Some(d) => d,
            None => return None,
        };
        let mode = dev.mode.read().await.clone();
        Some(mode)
    }
}
