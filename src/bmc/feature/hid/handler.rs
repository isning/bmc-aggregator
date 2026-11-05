use super::input::Input;
use std::collections::HashSet;
use std::sync::{Arc, Weak};
use std::time::Duration;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;

use futures::future::BoxFuture;

use crate::bmc::feature::fault::{
    acquisition_fault_response, fault_response, record_fault_response, unavailable_fault_response,
    FeatureFault, FeatureFaultDomain,
};
use crate::bmc::feature::hid::{HidBackend, HidEventCodec};
use crate::bmc::feature::run_feature_event_loop;
use crate::bmc::feature::{acquire_connected_feature, feature_connect_status, retry_feature};
use crate::bmc::feature::{build_feature_instances_status, build_feature_status};
use crate::web::state::AppState;

#[derive(Default)]
struct SocketInputs {
    backend: Option<Weak<dyn HidBackend>>,
    keys: HashSet<u16>,
    buttons: HashSet<u8>,
    position: (i16, i16),
}

impl SocketInputs {
    fn record(&mut self, backend: &Arc<dyn HidBackend>, input: &Input) {
        let owner = Arc::downgrade(backend);
        if !self
            .backend
            .as_ref()
            .is_some_and(|previous| previous.ptr_eq(&owner))
        {
            *self = Self::default();
            self.backend = Some(owner);
        }
        match input {
            Input::Keyboard(key) => {
                if key.pressed {
                    self.keys.insert(key.scancode);
                } else {
                    self.keys.remove(&key.scancode);
                }
            }
            Input::Transition(x, y, button, pressed) => {
                self.position = (*x, *y);
                if *pressed {
                    self.buttons.insert(*button);
                } else {
                    self.buttons.remove(button);
                }
            }
            Input::Buttons(x, y, mask) => {
                self.position = (*x, *y);
                self.buttons = (0..3).filter(|button| mask & (1 << button) != 0).collect();
            }
            Input::Move(point, absolute) => {
                self.position = if *absolute {
                    (point.x, point.y)
                } else {
                    (0, 0)
                };
            }
            Input::Scroll(x, y, _) => self.position = (*x, *y),
            Input::Consumer(_) => {}
        }
    }
}

/// Own cleanup across cancellation without replaying any input operation.
struct InputCleanup {
    inputs: Arc<tokio::sync::Mutex<SocketInputs>>,
    runtime: tokio::runtime::Handle,
    finished: bool,
}

impl InputCleanup {
    fn new() -> Self {
        Self {
            inputs: Arc::new(tokio::sync::Mutex::new(SocketInputs::default())),
            runtime: tokio::runtime::Handle::current(),
            finished: false,
        }
    }

    fn start_release(&mut self) -> tokio::task::JoinHandle<()> {
        self.finished = true;
        let inputs = self.inputs.clone();
        self.runtime
            .spawn(async move { release_hid_socket_inputs(&inputs).await })
    }

    async fn release(&mut self) {
        let _ = self.start_release().await;
    }
}

impl Drop for InputCleanup {
    fn drop(&mut self) {
        if !self.finished {
            // Dropping the join handle detaches cleanup; it must outlive its caller.
            drop(self.start_release());
        }
    }
}

async fn get_backend(
    fc: &Arc<crate::bmc::feature::connection::FeatureConnection>,
) -> Option<Arc<dyn HidBackend>> {
    fc.backend_runtime::<Arc<dyn HidBackend>>().await
}

/// Keep input dispatch bound to one live backend while replacement waits.
async fn backend_for_effect(
    fc: &Arc<crate::bmc::feature::connection::FeatureConnection>,
) -> Option<(Arc<dyn HidBackend>, tokio::sync::OwnedMutexGuard<()>)> {
    let generation = fc.backend_generation();
    let effect = fc.backend_owned_effect_guard(generation).await.ok()?;
    let backend = get_backend(fc).await?;
    Some((backend, effect))
}

fn device_not_found(operation: &'static str) -> Response {
    fault_response(
        StatusCode::NOT_FOUND,
        FeatureFault::operation(
            FeatureFaultDomain::Lifecycle,
            "device_not_found",
            "The requested BMC device was not found",
            false,
            operation,
        ),
    )
}

fn invalid_request(message: &'static str, operation: &'static str) -> Response {
    fault_response(
        StatusCode::BAD_REQUEST,
        FeatureFault::operation(
            FeatureFaultDomain::Operation,
            "invalid_request",
            message,
            false,
            operation,
        ),
    )
}

async fn backend_unavailable(
    fc: &crate::bmc::feature::connection::FeatureConnection,
    operation: &'static str,
) -> Response {
    record_fault_response(
        fc,
        StatusCode::SERVICE_UNAVAILABLE,
        FeatureFault::operation(
            FeatureFaultDomain::Lifecycle,
            "backend_runtime_unavailable",
            "The HID backend runtime is not available",
            true,
            operation,
        ),
    )
    .await
}

async fn input_failed(
    fc: &crate::bmc::feature::connection::FeatureConnection,
    operation: &'static str,
) -> Response {
    record_fault_response(
        fc,
        StatusCode::BAD_GATEWAY,
        FeatureFault::operation(
            FeatureFaultDomain::Operation,
            "input_failed",
            "The HID backend could not deliver the input event",
            true,
            operation,
        ),
    )
    .await
}

async fn batch_failed(
    fc: &crate::bmc::feature::connection::FeatureConnection,
    status: StatusCode,
) -> Response {
    record_fault_response(fc, status, FeatureFault::operation(
        FeatureFaultDomain::Operation,
        "batch_incomplete",
        "The HID batch did not finish; earlier inputs may have been delivered. Inspect remote state before sending another batch",
        false,
        "batch",
    )).await
}

pub async fn hid_status(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Json<serde_json::Value> {
    if let Some(dev) = state.registry.get_dev(&bmc_id).await {
        if let Some(fc) = dev.features.get("hid") {
            let mut status = build_feature_status(fc).await;
            if let serde_json::Value::Object(ref mut map) = status {
                let (instances, active_instances) =
                    build_feature_instances_status(&dev, "hid").await;
                map.insert("instances".into(), serde_json::json!(instances));
                map.insert(
                    "active_instances".into(),
                    serde_json::json!(active_instances),
                );
                if let Some(backend) = get_backend(fc).await {
                    map.insert(
                        "keyboard_leds".into(),
                        serde_json::json!({
                            "capabilities": backend.keyboard_led_capabilities().await,
                            "state": backend.keyboard_led_state().await.unwrap_or_default(),
                        }),
                    );
                }
            }
            return Json(status);
        }
    }
    Json(serde_json::json!({
        "available": false,
        "online": false,
    }))
}

async fn execute_http(
    bmc_id: String,
    state: Arc<AppState>,
    input: Result<Input, String>,
    operation: &'static str,
) -> Response {
    let input = match input {
        Ok(input) => input,
        Err(_) => return invalid_request("Invalid or out-of-range HID parameters", operation),
    };
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found(operation);
    };
    let Some(fc) = dev.features.get("hid").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "hid");
    };
    let _guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => guard,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };
    let Some((backend, _effect)) = backend_for_effect(&fc).await else {
        return backend_unavailable(&fc, operation).await;
    };
    match input.execute(backend.as_ref()).await {
        Ok(()) => Json(serde_json::json!({"success": true})).into_response(),
        Err(_) => input_failed(&fc, operation).await,
    }
}

pub async fn hid_keyboard(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<serde_json::Value>,
) -> Response {
    let input = (|| {
        let key = params
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or("missing key")?
            .parse::<u16>()
            .map_err(|_| "invalid key")?;
        let pressed = match params.get("type").and_then(|v| v.as_str()) {
            Some("down") => true,
            Some("up") => false,
            _ => return Err("invalid key transition"),
        };
        Ok(Input::Keyboard(super::input::Keyboard {
            scancode: key,
            pressed,
        }))
    })()
    .map_err(str::to_owned);
    execute_http(bmc_id, state, input, "keyboard").await
}
pub async fn hid_consumer(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<serde_json::Value>,
) -> Response {
    execute_http(bmc_id, state, Input::parse("consumer", params), "consumer").await
}
pub async fn hid_mouse(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(mut params): Json<serde_json::Value>,
) -> Response {
    let mode = params
        .get("type")
        .and_then(|v| v.as_str())
        .unwrap_or("move")
        .to_owned();
    let action = match mode.as_str() {
        "down" | "up" => {
            let button = match params.get("button").and_then(|v| v.as_str()) {
                Some("left") => 0,
                Some("middle") => 1,
                Some("right") => 2,
                _ => return invalid_request("Invalid mouse button", "mouse"),
            };
            params["button"] = serde_json::json!(button);
            params["pressed"] = serde_json::json!(mode == "down");
            "button_transition"
        }
        "scroll" => "scroll",
        "moveabs" | "absolute" => "mouse_absolute",
        "move" | "relative" => "mouse_relative",
        _ => return invalid_request("Invalid mouse mode", "mouse"),
    };
    execute_http(bmc_id, state, Input::parse(action, params), "mouse").await
}

pub async fn hid_batch(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<serde_json::Value>,
) -> Response {
    let (events, delay_ms) = match super::input::batch(params) {
        Ok(batch) => batch,
        Err(_) => return invalid_request("Invalid HID batch", "batch"),
    };
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found("batch");
    };
    let Some(fc) = dev.features.get("hid").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "hid");
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => guard,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };
    let generation = fc.backend_generation();
    let mut stopped = fc.stopped_rx();
    let mut generations = fc.backend_generation_rx();
    let Some(backend) = get_backend(&fc).await else {
        return batch_failed(&fc, StatusCode::SERVICE_UNAVAILABLE).await;
    };
    let mut inputs = InputCleanup::new();
    let count = events.len();
    for (index, input) in events.into_iter().enumerate() {
        let Ok(effect) = fc.backend_effect_guard(generation).await else {
            inputs.release().await;
            return batch_failed(&fc, StatusCode::SERVICE_UNAVAILABLE).await;
        };
        if input.execute(backend.as_ref()).await.is_err() {
            drop(effect);
            inputs.release().await;
            return batch_failed(&fc, StatusCode::BAD_GATEWAY).await;
        }
        inputs.inputs.lock().await.record(&backend, &input);
        drop(effect);
        if index + 1 < count && delay_ms > 0 {
            let interrupted = tokio::select! {
                biased;
                _ = stopped.changed() => true,
                _ = generations.changed() => true,
                _ = tokio::time::sleep(Duration::from_millis(delay_ms)) => false,
            };
            if interrupted {
                inputs.release().await;
                return batch_failed(&fc, StatusCode::SERVICE_UNAVAILABLE).await;
            }
        }
    }
    inputs.finished = true;
    drop(guard);

    Json(serde_json::json!({"success": true, "count": count})).into_response()
}

pub async fn hid_reset(Path(bmc_id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    let Some(dev) = state.registry.get_dev(&bmc_id).await else {
        return device_not_found("reset");
    };
    let Some(fc) = dev.features.get("hid").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "hid");
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(guard) => guard,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };
    let Some((backend, _effect)) = backend_for_effect(&fc).await else {
        return backend_unavailable(&fc, "reset").await;
    };
    let result = backend.reset().await;
    drop(guard);
    match result {
        Ok(()) => Json(serde_json::json!({"success": true})).into_response(),
        Err(_) => {
            record_fault_response(
                &fc,
                StatusCode::NOT_IMPLEMENTED,
                FeatureFault::operation(
                    FeatureFaultDomain::Operation,
                    "reset_unsupported",
                    "The HID backend does not support reset",
                    false,
                    "reset",
                ),
            )
            .await
        }
    }
}

pub async fn hid_retry(Path(bmc_id): Path<String>, State(state): State<Arc<AppState>>) -> Response {
    retry_feature(&state, &bmc_id, "hid").await
}

pub async fn ws_hid_upgrade(
    ws: axum::extract::ws::WebSocketUpgrade,
    headers: axum::http::HeaderMap,
    State(state): State<Arc<AppState>>,
    Path(bmc_id): Path<String>,
) -> axum::response::Response {
    if let Err(status) = crate::web::validate_websocket_origin(&headers) {
        return status.into_response();
    }
    ws.on_upgrade(move |socket| handle_hid_socket(socket, state, bmc_id))
}

async fn handle_hid_socket(
    socket: axum::extract::ws::WebSocket,
    state: Arc<AppState>,
    bmc_id: String,
) {
    use axum::extract::ws::Message;
    use tracing::{info, warn};

    let fc = match state.registry.get_dev(&bmc_id).await {
        Some(dev) => match dev.features.get("hid") {
            Some(fc) => fc.clone(),
            None => {
                warn!("[{}] hid ws: no hid feature", bmc_id);
                return;
            }
        },
        None => {
            warn!("[{}] hid ws: device not found", bmc_id);
            return;
        }
    };

    let fc_dispatch = fc.clone();
    let mut inputs = InputCleanup::new();
    let inputs_dispatch = inputs.inputs.clone();
    let ws = Arc::new(tokio::sync::Mutex::new(socket));
    let ws_recv = ws.clone();
    let ws_send = ws;

    let recv = move || {
        let ws = ws_recv.clone();
        Box::pin(async move {
            loop {
                match ws.lock().await.recv().await {
                    Some(Ok(Message::Binary(data))) => return Some(data),
                    Some(Ok(Message::Close(_))) | None => return None,
                    _ => continue,
                }
            }
        }) as BoxFuture<'static, Option<Vec<u8>>>
    };
    let send = move |data: Vec<u8>| {
        let ws = ws_send.clone();
        Box::pin(async move {
            crate::web::send_websocket(&mut *ws.lock().await, Message::Binary(data)).await
        }) as BoxFuture<'static, Result<(), String>>
    };
    let dispatch = move |action: String, params: serde_json::Value| {
        let fc = fc_dispatch.clone();
        let inputs = inputs_dispatch.clone();
        Box::pin(async move {
            let Some((backend, _effect)) = backend_for_effect(&fc).await else {
                fc.faults
                    .record(FeatureFault::operation(
                        FeatureFaultDomain::Lifecycle,
                        "backend_runtime_unavailable",
                        "The HID backend runtime is not available",
                        true,
                        "websocket",
                    ))
                    .await;
                return Err("hid_backend_unavailable".to_string());
            };
            let input = Input::parse(&action, params)?;
            let result = input.execute(backend.as_ref()).await;
            if result.is_ok() {
                inputs.lock().await.record(&backend, &input);
            }
            match result {
                Ok(()) => Ok(()),
                Err(_) => {
                    fc.faults
                        .record(FeatureFault::operation(
                            FeatureFaultDomain::Operation,
                            "input_failed",
                            "The HID backend could not deliver the input event",
                            true,
                            "websocket",
                        ))
                        .await;
                    Err("hid_input_failed".to_string())
                }
            }
        }) as BoxFuture<'static, Result<(), String>>
    };

    info!("[{}] hid ws connected", bmc_id);
    run_feature_event_loop(fc.clone(), Arc::new(HidEventCodec), recv, send, dispatch).await;
    inputs.release().await;
    info!("[{}] hid ws disconnected", bmc_id);
}

async fn release_hid_socket_inputs(inputs: &tokio::sync::Mutex<SocketInputs>) {
    let inputs = std::mem::take(&mut *inputs.lock().await);
    // Never release a replacement backend's inputs or retain an old session
    // merely because its browser socket has not closed yet.
    let Some(backend) = inputs.backend.and_then(|owner| owner.upgrade()) else {
        return;
    };
    for scancode in inputs.keys {
        let _ = backend.keyboard(scancode, false).await;
    }
    for button in inputs.buttons {
        let _ = backend
            .mouse_button_transition(inputs.position.0, inputs.position.1, button, false)
            .await;
    }
}

pub async fn hid_keyboard_screen(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
    Json(params): Json<serde_json::Value>,
) -> Response {
    let enabled = match super::input::screen_enabled(params) {
        Ok(enabled) => enabled,
        Err(_) => return invalid_request("A boolean enabled flag is required", "keyboard_screen"),
    };
    hid_control_action(&bmc_id, &state, "keyboard_screen", |backend| {
        Box::pin(async move { backend.keyboard_screen(enabled).await })
    })
    .await
}

pub async fn hid_request_control(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    hid_control_action(&bmc_id, &state, "request_control", |backend| {
        Box::pin(async move { backend.request_control().await })
    })
    .await
}

pub async fn hid_relinquish_control(
    Path(bmc_id): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Response {
    hid_control_action(&bmc_id, &state, "relinquish_control", |backend| {
        Box::pin(async move { backend.relinquish_control().await })
    })
    .await
}

async fn hid_control_action<F>(
    id: &str,
    state: &Arc<AppState>,
    operation: &'static str,
    action: F,
) -> Response
where
    F: FnOnce(Arc<dyn HidBackend>) -> futures::future::BoxFuture<'static, Result<(), String>>,
{
    let Some(dev) = state.registry.get_dev(id).await else {
        return device_not_found(operation);
    };
    let Some(fc) = dev.features.get("hid").cloned() else {
        return unavailable_fault_response(StatusCode::SERVICE_UNAVAILABLE, "hid");
    };
    let guard = match acquire_connected_feature(fc.clone(), Duration::from_secs(15)).await {
        Ok(g) => g,
        Err(error) => {
            return acquisition_fault_response(&fc, feature_connect_status(&error), &error).await
        }
    };
    let Some((backend, _effect)) = backend_for_effect(&fc).await else {
        drop(guard);
        return backend_unavailable(&fc, operation).await;
    };
    let result = action(backend).await;
    drop(guard);
    match result {
        Ok(()) => {
            Json(serde_json::json!({"success": true, "operation": operation})).into_response()
        }
        Err(_) => input_failed(&fc, operation).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    #[derive(Default)]
    struct RecordingHid {
        keys: tokio::sync::Mutex<Vec<(u16, bool)>>,
        buttons: tokio::sync::Mutex<Vec<(i16, i16, u8, bool)>>,
        stop: std::sync::Mutex<Option<Weak<crate::bmc::feature::connection::FeatureConnection>>>,
        paused: std::sync::atomic::AtomicBool,
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }
    impl RecordingHid {
        async fn gate(&self) {
            if self.paused.load(std::sync::atomic::Ordering::SeqCst) {
                self.started.notify_one();
                self.release.notified().await;
            }
        }
    }
    #[async_trait]
    impl HidBackend for RecordingHid {
        async fn keyboard(&self, key: u16, pressed: bool) -> Result<(), String> {
            self.keys.lock().await.push((key, pressed));
            if let Some(fc) = self.stop.lock().unwrap().take().and_then(|fc| fc.upgrade()) {
                fc.shutdown();
            }
            self.gate().await;
            Ok(())
        }
        async fn consumer(&self, _: u16) -> Result<(), String> {
            self.gate().await;
            Ok(())
        }
        async fn mouse_move(&self, _: i16, _: i16) -> Result<(), String> {
            self.gate().await;
            Ok(())
        }
        async fn mouse_button(&self, _: i16, _: i16, _: u8) -> Result<(), String> {
            Ok(())
        }
        async fn mouse_button_transition(
            &self,
            x: i16,
            y: i16,
            button: u8,
            pressed: bool,
        ) -> Result<(), String> {
            self.buttons.lock().await.push((x, y, button, pressed));
            Ok(())
        }
        async fn mouse_scroll(&self, _: i16, _: i16, _: i8) -> Result<(), String> {
            Ok(())
        }
        async fn reset(&self) -> Result<(), String> {
            self.gate().await;
            Ok(())
        }
        async fn keyboard_screen(&self, _: bool) -> Result<(), String> {
            self.gate().await;
            Ok(())
        }
        async fn request_control(&self) -> Result<(), String> {
            self.gate().await;
            Ok(())
        }
        async fn relinquish_control(&self) -> Result<(), String> {
            self.gate().await;
            Ok(())
        }
    }

    async fn fixture() -> (
        Arc<crate::bmc::feature::connection::FeatureConnection>,
        Arc<AppState>,
        Arc<RecordingHid>,
    ) {
        use crate::bmc::feature::{
            config::{FeatureConfig, FeatureConfigs},
            connection::{ConnectionState, FeatureConnection},
            device_registry::DeviceRegistry,
            managed::ManagedDevice,
        };
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
        fc.lifecycle_started
            .store(true, std::sync::atomic::Ordering::SeqCst);
        fc.transition(ConnectionState::Connected).await;
        let backend = Arc::new(RecordingHid::default());
        fc.set_backend_runtime(backend.clone() as Arc<dyn HidBackend>)
            .await;
        let registry = DeviceRegistry::new();
        registry
            .insert(
                "device".into(),
                Arc::new(ManagedDevice::new(
                    "device".into(),
                    FeatureConfigs {
                        features: Default::default(),
                    },
                    [("hid".into(), fc.clone())].into(),
                )),
            )
            .await;
        let config = Arc::new(tokio::sync::RwLock::new(serde_json::from_value::<crate::config::Config>(
            serde_json::json!({"server":{"listen":"127.0.0.1:0","frontend_dir":"frontend/dist"},"devices":[]})
        ).unwrap()));
        let state = Arc::new(AppState {
            registry,
            config: config.clone(),
            config_path: String::new(),
            configuration: Default::default(),
            msd_storage: crate::bmc::msd_storage::FsMsdStorage::new(
                ".local/quality/hid-test".into(),
                config,
            ),
            active_downloads: Default::default(),
            webtransport: Default::default(),
        });
        (fc, state, backend)
    }

    async fn assert_batch_stops_on_shutdown(delay_ms: u64) {
        let (fc, state, backend) = fixture().await;
        *backend.stop.lock().unwrap() = Some(Arc::downgrade(&fc));
        let response = tokio::time::timeout(
            Duration::from_millis(250),
            hid_batch(
                Path("device".into()),
                State(state),
                Json(serde_json::json!({"delay_ms":delay_ms,"events":[
                    {"action":"keyboard","params":{"scancode":4,"pressed":true}},
                    {"action":"keyboard","params":{"scancode":5,"pressed":true}}
                ]})),
            ),
        )
        .await
        .expect("shutdown must interrupt the batch delay");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(*backend.keys.lock().await, [(4, true), (4, false)]);
        let body = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["fault"]["code"], "batch_incomplete");
        assert_eq!(body["fault"]["retryable"], false);
    }

    #[tokio::test]
    async fn batch_shutdown_rejects_later_input_and_releases_owned_keys() {
        assert_batch_stops_on_shutdown(0).await;
    }

    #[tokio::test]
    async fn batch_shutdown_interrupts_its_delay() {
        assert_batch_stops_on_shutdown(1000).await;
    }

    #[tokio::test]
    async fn batch_replacement_interrupts_delay_without_retargeting_input() {
        let (fc, state, backend) = fixture().await;
        let batch = hid_batch(
            Path("device".into()),
            State(state),
            Json(serde_json::json!({"delay_ms":1000,"events":[
                {"action":"keyboard","params":{"scancode":4,"pressed":true}},
                {"action":"keyboard","params":{"scancode":5,"pressed":true}}
            ]})),
        );
        tokio::pin!(batch);
        assert!(futures::poll!(&mut batch).is_pending());
        let replacement = Arc::new(RecordingHid::default());
        fc.set_backend_runtime(replacement.clone() as Arc<dyn HidBackend>)
            .await;
        let response = tokio::time::timeout(Duration::from_millis(250), batch)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(*backend.keys.lock().await, [(4, true), (4, false)]);
        assert!(replacement.keys.lock().await.is_empty());
    }

    #[tokio::test]
    async fn cancelled_batch_releases_completed_pressed_input() {
        let (_fc, state, backend) = fixture().await;
        let batch = hid_batch(
            Path("device".into()),
            State(state),
            Json(serde_json::json!({"delay_ms":1000,"events":[
                {"action":"keyboard","params":{"scancode":4,"pressed":true}},
                {"action":"keyboard","params":{"scancode":5,"pressed":true}}
            ]})),
        );
        let mut batch = Box::pin(batch);
        assert!(futures::poll!(&mut batch).is_pending());
        drop(batch);
        tokio::time::timeout(Duration::from_millis(250), async {
            while backend.keys.lock().await.len() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled batch retained a pressed key");
        assert_eq!(*backend.keys.lock().await, [(4, true), (4, false)]);
    }

    #[tokio::test]
    async fn input_release_finishes_after_its_caller_is_cancelled() {
        let backend = Arc::new(RecordingHid::default());
        let adapter: Arc<dyn HidBackend> = backend.clone();
        let mut owner = InputCleanup::new();
        owner.inputs.lock().await.record(
            &adapter,
            &Input::Keyboard(super::super::input::Keyboard {
                scancode: 4,
                pressed: true,
            }),
        );
        owner
            .inputs
            .lock()
            .await
            .record(&adapter, &Input::Transition(1, 2, 0, true));
        backend
            .paused
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let mut cleanup = Box::pin(owner.release());
        assert!(futures::poll!(&mut cleanup).is_pending());
        tokio::time::timeout(Duration::from_secs(5), backend.started.notified())
            .await
            .unwrap();
        drop(cleanup);
        drop(owner);
        backend.release.notify_one();
        tokio::time::timeout(Duration::from_millis(250), async {
            while backend.buttons.lock().await.is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("input release was cancelled with its caller");
        assert_eq!(*backend.keys.lock().await, [(4, false)]);
        assert_eq!(*backend.buttons.lock().await, [(1, 2, 0, false)]);
    }

    #[tokio::test]
    async fn hid_mutations_hold_the_backend_replacement_lease() {
        for operation in [
            "keyboard",
            "consumer",
            "mouse",
            "reset",
            "screen",
            "request",
            "relinquish",
        ] {
            let (fc, state, backend) = fixture().await;
            backend
                .paused
                .store(true, std::sync::atomic::Ordering::SeqCst);
            let request = tokio::spawn(async move {
                let path = Path("device".into());
                let state = State(state);
                match operation {
                    "keyboard" => {
                        hid_keyboard(
                            path,
                            state,
                            Json(serde_json::json!({"key":"4","type":"down"})),
                        )
                        .await
                    }
                    "consumer" => {
                        hid_consumer(path, state, Json(serde_json::json!({"usage":1}))).await
                    }
                    "mouse" => {
                        hid_mouse(
                            path,
                            state,
                            Json(serde_json::json!({"type":"move","x":1,"y":1})),
                        )
                        .await
                    }
                    "reset" => hid_reset(path, state).await,
                    "screen" => {
                        hid_keyboard_screen(path, state, Json(serde_json::json!({"enabled":true})))
                            .await
                    }
                    "request" => hid_request_control(path, state).await,
                    "relinquish" => hid_relinquish_control(path, state).await,
                    _ => unreachable!(),
                }
            });
            tokio::time::timeout(Duration::from_secs(5), backend.started.notified())
                .await
                .unwrap();
            let replacement = fc.clear_backend_runtime();
            tokio::pin!(replacement);
            let waited = futures::poll!(&mut replacement).is_pending();
            backend.release.notify_one();
            let response = request.await.unwrap();
            if waited {
                replacement.await;
            }
            assert!(waited, "replacement overtook {operation}");
            assert_eq!(response.status(), StatusCode::OK);
        }
    }

    #[tokio::test]
    async fn socket_cleanup_releases_owned_keys_and_buttons_once() {
        let recording = Arc::new(RecordingHid::default());
        let backend: Arc<dyn HidBackend> = recording.clone();
        let mut inputs = SocketInputs::default();
        inputs.record(
            &backend,
            &Input::Keyboard(super::super::input::Keyboard {
                scancode: 4,
                pressed: true,
            }),
        );
        inputs.record(&backend, &Input::Transition(10, 20, 0, true));
        inputs.record(&backend, &Input::Transition(10, 20, 1, true));
        inputs.record(&backend, &Input::Transition(10, 20, 1, false));
        inputs.record(
            &backend,
            &Input::Move(super::super::input::Point { x: 30, y: 40 }, true),
        );
        let inputs = tokio::sync::Mutex::new(inputs);
        release_hid_socket_inputs(&inputs).await;
        release_hid_socket_inputs(&inputs).await;
        assert_eq!(*recording.keys.lock().await, [(4, false)]);
        assert_eq!(*recording.buttons.lock().await, [(30, 40, 0, false)]);
    }

    #[tokio::test]
    async fn socket_ownership_does_not_retain_or_release_a_replaced_backend() {
        let old = Arc::new(RecordingHid::default());
        let old_backend: Arc<dyn HidBackend> = old.clone();
        let current = Arc::new(RecordingHid::default());
        let current_backend: Arc<dyn HidBackend> = current.clone();
        let mut inputs = SocketInputs::default();
        inputs.record(&old_backend, &Input::Transition(1, 2, 0, true));
        inputs.record(&current_backend, &Input::Transition(0, 0, 2, true));
        let inputs = tokio::sync::Mutex::new(inputs);
        release_hid_socket_inputs(&inputs).await;
        assert!(old.buttons.lock().await.is_empty());
        assert_eq!(*current.buttons.lock().await, [(0, 0, 2, false)]);
        let mut expired = SocketInputs::default();
        expired.record(&old_backend, &Input::Transition(1, 2, 0, true));
        let weak = Arc::downgrade(&old);
        drop(old_backend);
        drop(old);
        assert!(weak.upgrade().is_none());
        release_hid_socket_inputs(&tokio::sync::Mutex::new(expired)).await;
        assert_eq!(current.buttons.lock().await.len(), 1);
    }
}
