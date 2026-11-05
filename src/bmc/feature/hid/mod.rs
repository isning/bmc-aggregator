pub mod config;
pub mod handler;
mod input;

use std::sync::Arc;

use axum::routing::{get, post};
use axum::Router;

use crate::web::state::AppState;

pub use bmc_feature_api::hid::HidBackend;

pub fn configuration_descriptor() -> bmc_feature_api::configuration::FeatureConfigDescriptor {
    use bmc_feature_api::configuration::{
        ConfigFieldGroup, ConfigFieldKind, ConfigValue, FeatureBindingKind,
    };
    crate::bmc::configuration::feature_descriptor(
        "hid",
        FeatureBindingKind::Single,
        true,
        vec![crate::bmc::configuration::configuration_field(
            "mouse_absolute",
            ConfigFieldGroup::Primary,
            false,
            Some(ConfigValue::Boolean(true)),
            ConfigFieldKind::Boolean,
        )],
    )
}

use crate::bmc::feature::FeatureEventCodec;
use crate::bmc::hid_protocol::{parse_hid_message, HidEvent};

/// HID protocol event codec (used by the generic event loop).
pub struct HidEventCodec;

impl FeatureEventCodec for HidEventCodec {
    fn parse_request(&self, data: &[u8]) -> Result<(String, serde_json::Value), String> {
        let event = parse_hid_message(data).ok_or_else(|| "invalid HID message".to_string())?;
        Ok(match event {
            HidEvent::Keyboard { usage, pressed } => (
                "keyboard".into(),
                serde_json::json!({"scancode": usage, "pressed": pressed}),
            ),
            HidEvent::MouseMoveRelative { x, y } => {
                ("mouse_relative".into(), serde_json::json!({"x": x, "y": y}))
            }
            HidEvent::MouseMoveAbsolute { x, y } => {
                ("mouse_absolute".into(), serde_json::json!({"x": x, "y": y}))
            }
            HidEvent::MouseDown { x, y, button } => (
                "button_transition".into(),
                serde_json::json!({"x": x, "y": y, "button": button, "pressed": true}),
            ),
            HidEvent::MouseUp { x, y, button } => (
                "button_transition".into(),
                serde_json::json!({"x": x, "y": y, "button": button, "pressed": false}),
            ),
            HidEvent::MouseScroll { x, y, delta } => (
                "scroll".into(),
                serde_json::json!({"x": x, "y": y, "scroll": delta}),
            ),
            HidEvent::Consumer { usage } => {
                ("consumer".into(), serde_json::json!({"usage": usage}))
            }
        })
    }

    fn encode_connected(&self) -> Option<Vec<u8>> {
        Some(vec![0x00])
    }

    fn encode_error(&self, err: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + err.len());
        out.push(0x01);
        out.extend_from_slice(err.as_bytes());
        out
    }

    fn encode_invalid_message(&self, err: &str) -> Vec<u8> {
        let mut out = Vec::with_capacity(1 + err.len());
        out.push(0x02);
        out.extend_from_slice(err.as_bytes());
        out
    }
}

pub fn mount(router: Router<Arc<AppState>>, base: &str) -> Router<Arc<AppState>> {
    router
        .route(&format!("{base}/status"), get(handler::hid_status))
        .route(&format!("{base}/keyboard"), post(handler::hid_keyboard))
        .route(&format!("{base}/consumer"), post(handler::hid_consumer))
        .route(&format!("{base}/mouse"), post(handler::hid_mouse))
        .route(&format!("{base}/batch"), post(handler::hid_batch))
        .route(&format!("{base}/reset"), post(handler::hid_reset))
        .route(
            &format!("{base}/keyboard-screen"),
            post(handler::hid_keyboard_screen),
        )
        .route(
            &format!("{base}/request-control"),
            post(handler::hid_request_control),
        )
        .route(
            &format!("{base}/relinquish-control"),
            post(handler::hid_relinquish_control),
        )
        .route(&format!("{base}/retry"), post(handler::hid_retry))
        .route(&format!("{base}/ws"), get(handler::ws_hid_upgrade))
        .route(
            &format!("{base}/config"),
            get(config::config_hid).patch(config::config_hid_update),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn invalid_binary_frames_never_dispatch_and_leave_the_connection_usable() {
        use crate::bmc::feature::config::FeatureConfig;
        use crate::bmc::feature::connection::ConnectionState;
        use crate::bmc::feature::{connection::FeatureConnection, run_feature_event_loop};
        use std::sync::Mutex;

        let fc = FeatureConnection::new(
            "device".into(),
            "hid".into(),
            "test".into(),
            FeatureConfig {
                backend: "test".into(),
                backend_params: Default::default(),
                feature_params: Default::default(),
                providers: vec![],
            },
        );
        fc.transition(ConnectionState::Connected).await;
        let mut frames = std::collections::VecDeque::from([
            vec![1, 2, 4, 0],
            vec![2, 255, 0, 0, 0, 0, 0],
            vec![2, 2, 0, 0, 0, 0, 3],
            vec![1, 0, 4, 0, 0],
            vec![1, 0, 4, 0],
        ]);
        let responses = Arc::new(Mutex::new(Vec::new()));
        let actions = Arc::new(Mutex::new(Vec::new()));
        let sent = responses.clone();
        let dispatched = actions.clone();
        run_feature_event_loop(
            fc,
            Arc::new(HidEventCodec),
            move || {
                let frame = frames.pop_front();
                Box::pin(async move { frame })
            },
            move |response| {
                sent.lock().unwrap().push(response);
                Box::pin(async { Ok(()) })
            },
            move |action, params| {
                dispatched.lock().unwrap().push((action, params));
                Box::pin(async { Ok(()) })
            },
        )
        .await;
        let responses = responses.lock().unwrap();
        assert_eq!(responses[0], vec![0]);
        assert_eq!(responses.len(), 5);
        assert!(responses[1..].iter().all(|response| response[0] == 2));
        assert_eq!(
            *actions.lock().unwrap(),
            vec![(
                "keyboard".into(),
                serde_json::json!({"scancode": 4, "pressed": true})
            )]
        );
    }

    #[test]
    fn binary_mouse_release_retains_the_physical_button_identity() {
        let codec = HidEventCodec;
        let (action, params) = codec.parse_request(&[0x02, 0x03, 1, 0, 2, 0, 2]).unwrap();
        assert_eq!(action, "button_transition");
        assert_eq!(params["button"], 2);
        assert_eq!(params["pressed"], false);
    }
}
