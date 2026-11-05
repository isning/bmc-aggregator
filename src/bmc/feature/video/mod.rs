pub mod encoding;
pub mod handler;
pub mod http_stream;
pub mod ice_config;
pub mod mode;
pub mod processor;
pub mod runtime;
pub mod stream_config;
pub mod webrtc;
pub mod webtransport;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;
use tracing::warn;

use crate::bmc::feature::connection::FeatureConnection;
use crate::web::state::AppState;

pub use bmc_feature_api::video::{RawPixelFormat, VideoColorDepthProfile, VideoSource};

pub(super) const MAX_INPUT_DIMENSION: u32 = 16_384;
pub(super) const MAX_INPUT_FPS: u32 = 240;

pub fn configuration_descriptor() -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    use bmc_feature_api::configuration::{
        ConfigFieldGroup, ConfigFieldKind, ConfigOptionSource, ConfigValue, FeatureBindingKind,
    };

    let integer = |id: &str, min: i64, max: i64, default: Option<i64>| {
        crate::bmc::configuration::configuration_field(
            id,
            ConfigFieldGroup::Advanced,
            false,
            default.map(ConfigValue::Integer),
            ConfigFieldKind::Integer {
                min: Some(min),
                max: Some(max),
                step: Some(1),
            },
        )
    };
    crate::bmc::configuration::feature_descriptor(
        "video",
        FeatureBindingKind::Single,
        true,
        vec![
            integer("max_fps", 1, i64::from(MAX_INPUT_FPS), Some(60)),
            crate::bmc::configuration::configuration_field(
                "video_device",
                ConfigFieldGroup::Primary,
                false,
                None,
                ConfigFieldKind::Selection {
                    options: ConfigOptionSource::RegisteredResource {
                        resource_id: "local.video_inputs".to_string(),
                    },
                    allow_custom: true,
                },
            ),
            crate::bmc::configuration::configuration_field(
                "video_format",
                ConfigFieldGroup::Advanced,
                false,
                None,
                ConfigFieldKind::Text { trim: true },
            ),
            integer("video_width", 1, i64::from(MAX_INPUT_DIMENSION), None),
            integer("video_height", 1, i64::from(MAX_INPUT_DIMENSION), None),
            integer("video_fps", 1, i64::from(MAX_INPUT_FPS), None),
            integer("video_quality", 1, 100, Some(80)),
        ],
    )
}

pub async fn replay_initial_base_frame(
    fc: &Arc<FeatureConnection>,
    processor: &Arc<processor::VideoProcessor>,
    label: &str,
) -> bool {
    let previous_trace_id = processor.last_trace_id();
    match runtime::source(fc).await {
        Ok(source) => {
            if let Err(e) = source.request_full_frame().await {
                warn!("{label}: source base-frame replay failed: {e}");
            }
        }
        Err(e) => warn!("{label}: source unavailable for base-frame replay: {e}"),
    }

    for _ in 0..20 {
        if processor.last_frame().await.is_some() && processor.last_trace_id() != previous_trace_id
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    processor.publish_last_frame().await
}

pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/devices"), get(handler::video_devices))
        .route(
            &format!("{base}/stream/http"),
            get(http_stream::http_stream),
        )
        .route(
            &format!("{base}/encoder_self_check"),
            get(handler::encoder_self_check),
        )
        .route(
            &format!("{base}/input-config"),
            get(stream_config::video_input_config_get)
                .patch(stream_config::video_input_config_patch),
        )
        .route(
            &format!("{base}/config"),
            get(stream_config::stream_config_get).patch(stream_config::stream_config_patch),
        )
        .route(&format!("{base}/start"), post(handler::stream_start))
        .route(&format!("{base}/stop"), post(handler::stream_stop))
        .route(&format!("{base}/reset"), post(handler::stream_reset))
        .route(&format!("{base}/retry"), post(handler::stream_retry))
        .route(&format!("{base}/status"), get(handler::stream_status))
        .route(
            &format!("{base}/mode"),
            get(mode::get_mode).post(mode::post_mode),
        )
        .route(&format!("{base}/codecs"), get(handler::stream_codecs))
        .route(
            &format!("{base}/constraints"),
            get(handler::stream_constraints),
        )
        .route(
            &format!("{base}/bitrate"),
            get(stream_config::stream_bitrate_get).post(stream_config::stream_bitrate),
        )
        .route(&format!("{base}/trace"), get(handler::trace_snapshot))
        .route(
            &format!("{base}/source-controls/color-depth"),
            get(handler::color_depth_get).post(handler::color_depth_set),
        )
        .route(
            &format!("{base}/webtransport/capabilities"),
            get(webtransport::capabilities),
        )
        .route(
            &format!("{base}/webtransport/connect"),
            post(webtransport::connect),
        )
        // WebRTC
        .route(&format!("{base}/webrtc/offer"), post(webrtc::webrtc_offer))
        .route(&format!("{base}/webrtc/ice"), post(webrtc::webrtc_ice))
        .route(&format!("{base}/webrtc/close"), post(webrtc::webrtc_close))
        .route(
            &format!("{base}/webrtc/ice-servers"),
            get(webrtc::webrtc_ice_servers),
        )
        .route(
            &format!("{base}/pipeline/encoder"),
            post(webrtc::pipeline_set_encoder),
        )
}
