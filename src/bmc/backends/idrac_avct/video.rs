use super::config;
use crate::bmc::feature::connection::FeatureConnection;
use crate::bmc::feature::fault::{lifecycle_connect_fault, FeatureFault, FeatureFaultDomain};
use crate::bmc::feature::video::runtime::{self, VideoSignalState};
use crate::bmc::feature::video::VideoSource;
use bmc_backend_idrac_avct::{
    get_or_connect, AvctTerminalReason, AvctVideoState, DecodedFrame, IdracAvctVideoBackend,
};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use tokio::sync::{broadcast, watch};
use tracing::info;

fn avct_session_disconnect_fault(terminal_reason: AvctTerminalReason) -> FeatureFault {
    match terminal_reason {
        // Block-video and other unported video families must surface as a
        // deterministic protocol limitation, not as a retryable network
        // interruption that leaves a browser waiting for a frame.
        AvctTerminalReason::UnsupportedVideoPacket => FeatureFault::feature(
            FeatureFaultDomain::Protocol,
            "idrac_avct_unsupported_video_packet",
            "The iDRAC console selected an unsupported video packet format",
            false,
        ),
        AvctTerminalReason::DisplayPacketRejected => FeatureFault::feature(
            FeatureFaultDomain::Decode,
            "idrac_avct_display_packet_rejected",
            "The iDRAC console sent an invalid display packet",
            false,
        ),
        AvctTerminalReason::Idrac6Dvc15NegotiationFailed => FeatureFault::feature(
            FeatureFaultDomain::Protocol,
            "idrac6_dvc15_negotiation_failed",
            "The iDRAC 6 console could not establish a complete refreshed DVC15 stream",
            false,
        ),
        AvctTerminalReason::Idrac6DvcProfileRecoveryFailed => FeatureFault::feature(
            FeatureFaultDomain::Protocol,
            "idrac6_dvc_profile_recovery_failed",
            "The iDRAC 6 console could not recover its DVC15 baseline after a color-depth transition",
            false,
        ),
        _ => FeatureFault::feature(
            FeatureFaultDomain::Transport,
            "idrac_avct_session_ended",
            "The iDRAC console video session ended",
            true,
        ),
    }
}

pub async fn connect(fc: Arc<FeatureConnection>) -> Result<(), String> {
    let video_runtime = runtime::ensure_runtime(&fc).await;
    let config = match config(&fc.config.backend_params) {
        Ok(config) => config,
        Err(error) => {
            video_runtime
                .processor()
                .record_fault(lifecycle_connect_fault(&error))
                .await;
            return Err(error);
        }
    };
    let lease = match get_or_connect(&config).await {
        Ok(lease) => lease,
        Err(error) => {
            video_runtime
                .processor()
                .record_fault(lifecycle_connect_fault(&error))
                .await;
            return Err(error);
        }
    };
    let session = lease.session().clone();
    let frames = session.subscribe_frames();
    // A HID lease can establish the shared AVCT session before the video
    // feature subscribes. Replay the retained zero-copy frame only when this
    // receiver has no newer broadcast queued; this preserves ordering across
    // the subscribe/snapshot race.
    let retained_frame = session.last_frame();
    let retained_frame = frames.is_empty().then_some(retained_frame).flatten();
    let video_state = session.video_state_rx();
    let (frame_shutdown_tx, frame_shutdown_rx) = watch::channel(false);
    tokio::spawn(forward_frames(
        frames,
        retained_frame,
        video_state,
        frame_shutdown_rx,
        video_runtime.clone(),
    ));
    fc.set_backend_runtime(
        Arc::new(IdracAvctVideoBackend::new(lease, frame_shutdown_tx.clone()))
            as Arc<dyn VideoSource>,
    )
    .await;

    let fc_for_disconnect = fc.clone();
    let disconnect = session.disconnected_rx();
    let backend_generation = fc.backend_generation();
    tokio::spawn(async move {
        if let Some(_current) = fc_for_disconnect
            .wait_for_backend_disconnect(backend_generation, disconnect)
            .await
        {
            frame_shutdown_tx.send_replace(true);
            video_runtime.set_signal(VideoSignalState::DeviceLost, Some("backend_disconnected"));
            fc_for_disconnect.healthy.store(false, Ordering::SeqCst);
            fc_for_disconnect.disconnected.store(true, Ordering::SeqCst);
            fc_for_disconnect.wake.notify_one();
            fc_for_disconnect
                .faults
                .record(avct_session_disconnect_fault(session.terminal_reason()))
                .await;
            info!(
                "[{}] iDRAC AVCT video backend disconnect detected",
                fc_for_disconnect.device_id
            );
        }
    });
    info!("[{}] iDRAC AVCT video connected", fc.device_id);
    Ok(())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;

    async fn wait_for_signal(runtime: &runtime::VideoRuntime, expected: VideoSignalState) {
        tokio::time::timeout(std::time::Duration::from_millis(250), async {
            while runtime.signal_status().state != expected {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the current source observation must reach the video runtime");
    }

    #[tokio::test]
    async fn late_video_subscriber_receives_existing_stopped_observation() {
        let (_states, states_rx) = watch::channel(AvctVideoState::Stopped);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let runtime = Arc::new(runtime::VideoRuntime::new());
        let (_frames, frames_rx) = broadcast::channel(1);
        let task = tokio::spawn(forward_frames(
            frames_rx,
            None,
            states_rx,
            shutdown_rx,
            runtime.clone(),
        ));
        let result = tokio::time::timeout(std::time::Duration::from_millis(250), async {
            while runtime.signal_status().state != VideoSignalState::NoSignal {
                tokio::task::yield_now().await;
            }
        })
        .await;
        shutdown.send_replace(true);
        task.await.unwrap();
        assert!(
            result.is_ok(),
            "a late subscriber lost the existing no-signal observation"
        );
    }

    #[tokio::test]
    async fn queued_frame_cannot_override_observed_no_signal() {
        crate::bmc::stream::gst_init::ensure_gst_init().unwrap();
        let (states, states_rx) = watch::channel(AvctVideoState::Negotiating);
        let (frames, frames_rx) = broadcast::channel(2);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let runtime = Arc::new(runtime::VideoRuntime::new());
        let frame_task = tokio::spawn(forward_frames(
            frames_rx,
            None,
            states_rx,
            shutdown_rx,
            runtime.clone(),
        ));
        states.send_replace(AvctVideoState::Stopped);
        wait_for_signal(&runtime, VideoSignalState::NoSignal).await;
        assert!(frames
            .send(DecodedFrame {
                buffer: gstreamer::Buffer::from_mut_slice(vec![0u8; 4]),
                width: 1,
                height: 1,
                pixel_format: crate::bmc::stream::pipeline::RawPixelFormat::Rgba,
            })
            .is_ok());
        drop(frames);
        frame_task.await.unwrap();
        shutdown.send_replace(true);
        assert_eq!(runtime.signal_status().state, VideoSignalState::NoSignal);
        assert!(runtime.processor().last_frame().await.is_none());
    }

    fn frame() -> DecodedFrame {
        crate::bmc::stream::gst_init::ensure_gst_init().unwrap();
        DecodedFrame {
            buffer: gstreamer::Buffer::from_mut_slice(vec![7u8; 4]),
            width: 1,
            height: 1,
            pixel_format: crate::bmc::stream::pipeline::RawPixelFormat::Rgba,
        }
    }

    #[tokio::test]
    async fn retained_frame_replay_requires_running_source_and_live_lease() {
        for (state, stopped, expected) in [
            (AvctVideoState::Running, false, VideoSignalState::Streaming),
            (AvctVideoState::Stopped, false, VideoSignalState::NoSignal),
            (
                AvctVideoState::Negotiating,
                false,
                VideoSignalState::Unknown,
            ),
            (AvctVideoState::Running, true, VideoSignalState::DeviceLost),
        ] {
            let (_states, states_rx) = watch::channel(state);
            let (shutdown, shutdown_rx) = watch::channel(stopped);
            let (frames, frames_rx) = broadcast::channel(1);
            drop(frames);
            let runtime = Arc::new(runtime::VideoRuntime::new());
            runtime.set_signal(VideoSignalState::DeviceLost, Some("previous_connection"));
            forward_frames(
                frames_rx,
                Some(frame()),
                states_rx,
                shutdown_rx,
                runtime.clone(),
            )
            .await;
            assert_eq!(runtime.signal_status().state, expected);
            assert_eq!(
                runtime.processor().last_frame().await.is_some(),
                state == AvctVideoState::Running && !stopped
            );
            drop(shutdown);
        }
    }

    #[tokio::test]
    async fn source_resume_publishes_new_frames_after_no_signal() {
        let (states, states_rx) = watch::channel(AvctVideoState::Stopped);
        let (_shutdown, shutdown_rx) = watch::channel(false);
        let (frames, frames_rx) = broadcast::channel(1);
        let runtime = Arc::new(runtime::VideoRuntime::new());
        let task = tokio::spawn(forward_frames(
            frames_rx,
            None,
            states_rx,
            shutdown_rx,
            runtime.clone(),
        ));
        wait_for_signal(&runtime, VideoSignalState::NoSignal).await;
        states.send_replace(AvctVideoState::Running);
        assert!(frames.send(frame()).is_ok());
        drop(frames);
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(runtime.signal_status().state, VideoSignalState::Streaming);
        assert!(runtime.processor().last_frame().await.is_some());
    }

    #[test]
    fn avct_transport_end_uses_the_shared_retryable_feature_fault() {
        let fault = avct_session_disconnect_fault(AvctTerminalReason::SocketOrControlQueueClosed);
        assert_eq!(fault.domain, FeatureFaultDomain::Transport);
        assert_eq!(fault.code, "idrac_avct_session_ended");
        assert!(fault.retryable);
    }

    #[test]
    fn unsupported_avct_video_is_a_non_retryable_protocol_fault() {
        let fault = avct_session_disconnect_fault(AvctTerminalReason::UnsupportedVideoPacket);
        assert_eq!(fault.domain, FeatureFaultDomain::Protocol);
        assert_eq!(fault.code, "idrac_avct_unsupported_video_packet");
        assert!(!fault.retryable);
    }

    #[test]
    fn idrac6_dvc15_negotiation_failure_is_terminal_and_structured() {
        let fault = avct_session_disconnect_fault(AvctTerminalReason::Idrac6Dvc15NegotiationFailed);
        assert_eq!(fault.domain, FeatureFaultDomain::Protocol);
        assert_eq!(fault.code, "idrac6_dvc15_negotiation_failed");
        assert!(!fault.retryable);
    }

    #[test]
    fn idrac6_dvc_profile_recovery_failure_is_terminal_and_structured() {
        let fault =
            avct_session_disconnect_fault(AvctTerminalReason::Idrac6DvcProfileRecoveryFailed);
        assert_eq!(fault.domain, FeatureFaultDomain::Protocol);
        assert_eq!(fault.code, "idrac6_dvc_profile_recovery_failed");
        assert!(!fault.retryable);
    }
}

fn forward_signal(state: AvctVideoState, runtime: &runtime::VideoRuntime) {
    match state {
        AvctVideoState::Negotiating => runtime.set_signal(VideoSignalState::Unknown, None),
        AvctVideoState::Running => runtime.set_signal(VideoSignalState::Streaming, None),
        AvctVideoState::Stopped => {
            runtime.set_signal(VideoSignalState::NoSignal, Some("bmc_video_stopped"));
        }
    }
}

/// One bridge orders source observations and frames. The backend remains the
/// authority for signal state; a queued frame cannot revive a stopped source.
async fn forward_frames(
    mut frames: broadcast::Receiver<DecodedFrame>,
    mut retained_frame: Option<DecodedFrame>,
    mut states: watch::Receiver<AvctVideoState>,
    mut shutdown: watch::Receiver<bool>,
    runtime: Arc<runtime::VideoRuntime>,
) {
    loop {
        if *shutdown.borrow() || shutdown.has_changed().is_err() {
            break;
        }
        // subscribe() marks the initial value as seen: forward that snapshot
        // explicitly, including no-signal observed before the video lease.
        forward_signal(*states.borrow_and_update(), &runtime);
        let frame = if let Some(frame) = retained_frame.take() {
            frame
        } else {
            tokio::select! {
                biased;
                changed = shutdown.changed() => {
                    if changed.is_err() || *shutdown.borrow() { break; }
                    continue;
                }
                changed = states.changed() => {
                    if changed.is_err() { break; }
                    continue;
                }
                frame = frames.recv() => match frame {
                    Ok(frame) => frame,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        };
        if *states.borrow() != AvctVideoState::Running {
            continue;
        }
        tokio::select! {
            biased;
            _ = shutdown.changed() => break,
            _ = runtime.publish_raw_frame(frame.buffer, frame.width, frame.height, frame.pixel_format) => {}
        }
        // Re-read source observations after publication's await before waiting
        // again. A stop during frame processing must remain the final signal.
    }
}
