use std::sync::atomic::Ordering;
use std::sync::Arc;

use bmc_backend_vnc::{get_or_connect, VncFrame, VncVideoBackend};
use tokio::sync::broadcast;
use tracing::info;

use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::video::runtime;
use crate::bmc::feature::video::VideoSource;

use super::config;

pub async fn connect(fc: Arc<FeatureConnection>) -> Result<(), String> {
    let config = config(&fc.config.backend_params)?;
    let lease = get_or_connect(&config).await?;
    let session = lease.session().clone();
    let frames = session.subscribe_frames();
    let (frame_shutdown_tx, frame_shutdown_rx) = tokio::sync::watch::channel(false);
    let video_runtime = runtime::ensure_runtime(&fc).await;
    tokio::spawn(forward_frames(frames, frame_shutdown_rx, video_runtime));

    let source = Arc::new(VncVideoBackend::new(lease, frame_shutdown_tx)) as Arc<dyn VideoSource>;
    fc.set_backend_runtime(source).await;

    let fc_for_disconnect = fc.clone();
    let disconnect = session.disconnected_rx();
    let backend_generation = fc.backend_generation();
    tokio::spawn(async move {
        if let Some(_current) = fc_for_disconnect
            .wait_for_backend_disconnect(backend_generation, disconnect)
            .await
        {
            fc_for_disconnect.disconnected.store(true, Ordering::SeqCst);
            fc_for_disconnect.wake.notify_one();
            info!(
                "[{}] VNC video backend disconnect detected",
                fc_for_disconnect.device_id
            );
        }
    });
    info!("[{}] VNC video connected", fc.device_id);
    Ok(())
}

async fn forward_frames(
    mut frames: broadcast::Receiver<VncFrame>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
    runtime: Arc<runtime::VideoRuntime>,
) {
    loop {
        if *shutdown.borrow() || shutdown.has_changed().is_err() {
            break;
        }
        let frame = tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            frame = frames.recv() => frame,
        };
        match frame {
            Ok(frame) => {
                tokio::select! {
                    biased;
                    _ = shutdown.changed() => break,
                    _ = runtime.publish_raw_frame(frame.buffer, frame.width, frame.height, frame.pixel_format) => {}
                }
            }
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame() -> VncFrame {
        crate::bmc::stream::gst_init::ensure_gst_init().unwrap();
        VncFrame {
            buffer: gstreamer::Buffer::from_mut_slice(vec![3u8; 4]),
            width: 1,
            height: 1,
            pixel_format: crate::bmc::stream::pipeline::RawPixelFormat::Rgba,
        }
    }

    #[tokio::test]
    async fn shutdown_interrupts_frame_waiting_for_processor() {
        let (frames, frame_rx) = broadcast::channel(1);
        let (shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        let runtime = Arc::new(crate::bmc::feature::video::runtime::VideoRuntime::new());
        let processor = runtime.processor();
        let blocked = processor.block_frame_publication().await;
        frames.send(frame()).unwrap();
        let forward = forward_frames(frame_rx, shutdown_rx, runtime.clone());
        tokio::pin!(forward);
        assert!(futures::poll!(forward.as_mut()).is_pending());
        shutdown.send_replace(true);
        let result =
            tokio::time::timeout(std::time::Duration::from_millis(250), forward.as_mut()).await;
        drop(blocked);
        assert!(
            result.is_ok(),
            "lease shutdown must interrupt a frame blocked on the processor"
        );
        assert!(processor.last_frame().await.is_none());
    }

    #[tokio::test]
    async fn stopped_lease_does_not_publish_queued_frame() {
        let (frames, frame_rx) = broadcast::channel(1);
        let (_shutdown, shutdown_rx) = tokio::sync::watch::channel(true);
        let runtime = Arc::new(crate::bmc::feature::video::runtime::VideoRuntime::new());
        frames.send(frame()).unwrap();
        drop(frames);
        forward_frames(frame_rx, shutdown_rx, runtime.clone()).await;
        assert!(runtime.processor().last_frame().await.is_none());
    }

    #[tokio::test]
    async fn active_lease_publishes_frame_before_source_closes() {
        let (frames, frame_rx) = broadcast::channel(1);
        let (_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        let runtime = Arc::new(crate::bmc::feature::video::runtime::VideoRuntime::new());
        frames.send(frame()).unwrap();
        drop(frames);
        forward_frames(frame_rx, shutdown_rx, runtime.clone()).await;
        assert!(runtime.processor().last_frame().await.is_some());
        assert_eq!(
            runtime.signal_status().state,
            crate::bmc::feature::video::runtime::VideoSignalState::Streaming
        );
    }
}
