use std::sync::Arc;

use gstreamer as gst;

use crate::bmc::feature::connection::ConnectionEvent;
use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::fault::FeatureFaultStore;
use crate::bmc::feature::video::processor::VideoProcessor;
use crate::bmc::feature::video::VideoSource;
use crate::bmc::stream::pipeline::{RawFrame, RawPixelFormat};
use serde::Serialize;
use tokio::sync::watch;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoSignalState {
    Unknown,
    Streaming,
    NoSignal,
    DeviceLost,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct VideoSignalStatus {
    pub state: VideoSignalState,
    pub reason: Option<String>,
}

impl Default for VideoSignalStatus {
    fn default() -> Self {
        Self {
            state: VideoSignalState::Unknown,
            reason: None,
        }
    }
}

pub struct VideoRuntime {
    processor: Arc<VideoProcessor>,
    signal_tx: watch::Sender<VideoSignalStatus>,
    event_tx: std::sync::RwLock<Option<VideoEventBinding>>,
}

type VideoEventBinding = (watch::Sender<Option<ConnectionEvent>>, String, String);

impl VideoRuntime {
    pub fn new() -> Self {
        Self::with_encoder_backend(None)
    }

    pub fn with_encoder_backend(encoder_backend: Option<String>) -> Self {
        Self::with_encoder_backend_and_faults(encoder_backend, FeatureFaultStore::default())
    }

    pub fn with_encoder_backend_and_faults(
        encoder_backend: Option<String>,
        faults: FeatureFaultStore,
    ) -> Self {
        let (signal_tx, _) = watch::channel(VideoSignalStatus::default());
        Self {
            processor: Arc::new(VideoProcessor::with_encoder_backend_and_faults(
                encoder_backend,
                faults,
            )),
            signal_tx,
            event_tx: std::sync::RwLock::new(None),
        }
    }

    pub fn processor(&self) -> Arc<VideoProcessor> {
        self.processor.clone()
    }

    pub async fn publish_raw_frame(
        &self,
        buf: gst::Buffer,
        width: u32,
        height: u32,
        pixel_format: RawPixelFormat,
    ) {
        if self
            .processor
            .finish_frame(buf, width, height, pixel_format)
            .await
        {
            self.set_signal(VideoSignalState::Streaming, None);
        }
    }

    pub async fn publish_raw_frame_with_trace(&self, frame: RawFrame) {
        if self.processor.finish_raw_frame(frame).await {
            self.set_signal(VideoSignalState::Streaming, None);
        }
    }

    pub fn bind_events(&self, fc: &FeatureConnection) {
        *self
            .event_tx
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some((fc.event_tx.clone(), fc.id.clone(), fc.instance_id.clone()));
    }

    pub fn signal_status(&self) -> VideoSignalStatus {
        self.signal_tx.borrow().clone()
    }

    pub fn set_signal(&self, state: VideoSignalState, reason: Option<&str>) {
        let next = VideoSignalStatus {
            state,
            reason: reason.map(str::to_owned),
        };
        // Commit the snapshot and its notification in the same order. Otherwise
        // concurrent source observations can publish an older event last.
        let binding = self
            .event_tx
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *self.signal_tx.borrow() == next {
            return;
        }
        self.signal_tx.send_replace(next.clone());
        if let Some((event_tx, feature, instance)) = binding.as_ref() {
            let state = match next.state {
                VideoSignalState::Unknown => "uninitialized",
                VideoSignalState::Streaming => "streaming",
                VideoSignalState::NoSignal => "no_signal",
                VideoSignalState::DeviceLost => "device_lost",
            };
            let _ = event_tx.send(Some(ConnectionEvent::StreamStateChanged {
                feature: feature.clone(),
                instance: instance.clone(),
                state: state.to_string(),
                reason: next.reason,
            }));
        }
    }
}

impl Default for VideoRuntime {
    fn default() -> Self {
        Self::new()
    }
}

pub async fn ensure_runtime(fc: &Arc<FeatureConnection>) -> Arc<VideoRuntime> {
    let runtime = fc
        .get_or_init_feature_runtime(|| {
            Arc::new(VideoRuntime::with_encoder_backend_and_faults(
                fc.config.feature_params.encoder_backend.clone(),
                fc.faults.clone(),
            ))
        })
        .await;
    runtime.bind_events(fc);
    runtime
}

pub async fn runtime(fc: &Arc<FeatureConnection>) -> Option<Arc<VideoRuntime>> {
    fc.feature_runtime::<Arc<VideoRuntime>>().await
}

pub async fn source(fc: &Arc<FeatureConnection>) -> Result<Arc<dyn VideoSource>, String> {
    fc.backend_runtime::<Arc<dyn VideoSource>>()
        .await
        .ok_or_else(|| "Video source not connected".to_string())
}

pub async fn processor(fc: &Arc<FeatureConnection>) -> Result<Arc<VideoProcessor>, String> {
    Ok(ensure_runtime(fc).await.processor())
}

pub async fn publish_raw_frame(
    fc: &Arc<FeatureConnection>,
    buf: gst::Buffer,
    width: u32,
    height: u32,
    pixel_format: RawPixelFormat,
) {
    ensure_runtime(fc)
        .await
        .publish_raw_frame(buf, width, height, pixel_format)
        .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bmc::feature::config::FeatureConfig;

    fn connection() -> Arc<FeatureConnection> {
        FeatureConnection::new(
            "device".into(),
            "video".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        )
    }

    #[test]
    fn concurrent_signal_updates_keep_snapshot_and_notification_consistent() {
        let fc = connection();
        let runtime = Arc::new(VideoRuntime::new());
        runtime.bind_events(&fc);
        let barrier = std::sync::Barrier::new(9);
        let mut mismatch = None;
        std::thread::scope(|scope| {
            for writer in 0..8 {
                let runtime = &runtime;
                let barrier = &barrier;
                scope.spawn(move || {
                    for round in 0..512 {
                        barrier.wait();
                        let state = if writer % 2 == 0 {
                            VideoSignalState::Streaming
                        } else {
                            VideoSignalState::DeviceLost
                        };
                        runtime.set_signal(state, Some(&format!("{round}:{writer}")));
                        barrier.wait();
                    }
                });
            }
            for _ in 0..512 {
                barrier.wait();
                barrier.wait();
                let snapshot = runtime.signal_status();
                let event = fc.event_rx.borrow().clone();
                let expected_state = match snapshot.state {
                    VideoSignalState::Streaming => "streaming",
                    VideoSignalState::DeviceLost => "device_lost",
                    VideoSignalState::Unknown => "uninitialized",
                    VideoSignalState::NoSignal => "no_signal",
                };
                let consistent = matches!(&event,
                    Some(ConnectionEvent::StreamStateChanged { state, reason, .. })
                        if state == expected_state && reason == &snapshot.reason);
                if !consistent && mismatch.is_none() {
                    mismatch = Some(format!("snapshot={snapshot:?}, event={event:?}"));
                }
            }
        });
        assert!(mismatch.is_none(), "{mismatch:?}");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_initialization_preserves_one_processor_and_frame_cache() {
        crate::bmc::stream::gst_init::ensure_gst_init().unwrap();
        let fc = connection();
        let barrier = Arc::new(tokio::sync::Barrier::new(32));
        let mut tasks = Vec::new();
        for _ in 0..32 {
            let fc = fc.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                ensure_runtime(&fc).await
            }));
        }
        let expected = tasks.remove(0).await.unwrap();
        for task in tasks {
            assert!(Arc::ptr_eq(&expected, &task.await.unwrap()));
        }
        expected
            .publish_raw_frame(
                gst::Buffer::from_mut_slice(vec![1, 2, 3, 4]),
                1,
                1,
                RawPixelFormat::Rgba,
            )
            .await;
        let current = ensure_runtime(&fc).await;
        assert!(Arc::ptr_eq(&current.processor(), &expected.processor()));
        assert!(current.processor().last_frame().await.is_some());
    }

    #[tokio::test]
    async fn invalid_frames_never_publish_cache_or_report_streaming() {
        crate::bmc::stream::gst_init::ensure_gst_init().unwrap();
        let runtime = VideoRuntime::new();
        let processor = runtime.processor();
        let mut frames = processor.frame_tx().subscribe();
        for (width, height, size) in [(0, 1, 4), (1, 1, 3), (1, 1, 5), (u32::MAX, u32::MAX, 4)] {
            runtime
                .publish_raw_frame(
                    gst::Buffer::from_mut_slice(vec![0; size]),
                    width,
                    height,
                    RawPixelFormat::Rgba,
                )
                .await;
            assert!(processor.last_frame().await.is_none());
            assert_eq!(runtime.signal_status().state, VideoSignalState::Unknown);
            assert!(matches!(
                frames.try_recv(),
                Err(tokio::sync::broadcast::error::TryRecvError::Empty)
            ));
        }
        runtime
            .publish_raw_frame(
                gst::Buffer::from_mut_slice(vec![1, 2, 3, 4]),
                1,
                1,
                RawPixelFormat::Rgba,
            )
            .await;
        let valid = frames.recv().await.unwrap();
        assert_eq!(valid.width, 1);
        assert_eq!(runtime.signal_status().state, VideoSignalState::Streaming);
        runtime
            .publish_raw_frame(
                gst::Buffer::from_mut_slice(vec![0; 3]),
                1,
                1,
                RawPixelFormat::Rgba,
            )
            .await;
        assert_eq!(processor.last_frame().await.unwrap().data.size(), 4);
        assert!(frames.try_recv().is_err());
    }
}
