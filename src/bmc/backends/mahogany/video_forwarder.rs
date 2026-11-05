use std::sync::Arc;

use tokio::sync::{broadcast, watch};

use crate::bmc::feature::video::runtime::VideoRuntime;
use crate::bmc::stream::pipeline::RawFrame;
use bmc_backend_mahogany::MahoganyFrame;

/// Forward pooled raw frames into the feature-owned processor until the
/// lease-backed video adapter is dropped or the pool closes the channel.
pub(crate) async fn forward_raw_frames(
    mut frame_rx: broadcast::Receiver<MahoganyFrame>,
    mut shutdown_rx: watch::Receiver<bool>,
    runtime: Arc<VideoRuntime>,
) {
    loop {
        if *shutdown_rx.borrow() || shutdown_rx.has_changed().is_err() {
            break;
        }
        let frame = tokio::select! {
            biased;
            _ = shutdown_rx.changed() => break,
            frame = frame_rx.recv() => frame,
        };
        match frame {
            Ok(frame) => {
                tokio::select! {
                    biased;
                    _ = shutdown_rx.changed() => break,
                    _ = runtime.publish_raw_frame_with_trace(RawFrame {
                        data: frame.data,
                        width: frame.width,
                        height: frame.height,
                        pixel_format: frame.pixel_format,
                        trace_id: frame.trace_id,
                        created_at: frame.created_at,
                    }) => {}
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

    fn frame() -> MahoganyFrame {
        crate::bmc::stream::gst_init::ensure_gst_init().unwrap();
        MahoganyFrame {
            data: gstreamer::Buffer::from_mut_slice(vec![3u8; 4]),
            width: 1,
            height: 1,
            pixel_format: crate::bmc::stream::pipeline::RawPixelFormat::Rgba,
            trace_id: 42,
            created_at: std::time::Instant::now(),
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
        let forward = forward_raw_frames(frame_rx, shutdown_rx, runtime.clone());
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
        forward_raw_frames(frame_rx, shutdown_rx, runtime.clone()).await;
        assert!(runtime.processor().last_frame().await.is_none());
    }

    #[tokio::test]
    async fn active_lease_publishes_frame_before_source_closes() {
        let (frames, frame_rx) = broadcast::channel(1);
        let (_shutdown, shutdown_rx) = tokio::sync::watch::channel(false);
        let runtime = Arc::new(crate::bmc::feature::video::runtime::VideoRuntime::new());
        frames.send(frame()).unwrap();
        drop(frames);
        forward_raw_frames(frame_rx, shutdown_rx, runtime.clone()).await;
        assert!(runtime.processor().last_frame().await.is_some());
        assert_eq!(
            runtime.signal_status().state,
            crate::bmc::feature::video::runtime::VideoSignalState::Streaming
        );
    }

    #[tokio::test]
    async fn forwarder_exits_when_its_lease_shutdowns() {
        let (_frames, frame_rx) = broadcast::channel(1);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let task = tokio::spawn(forward_raw_frames(
            frame_rx,
            shutdown_rx,
            Arc::new(VideoRuntime::new()),
        ));

        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .expect("frame forwarder should stop after its lease is dropped")
            .unwrap();
    }
}
