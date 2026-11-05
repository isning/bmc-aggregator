use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeyboardLedState {
    pub num_lock: Option<bool>,
    pub caps_lock: Option<bool>,
    pub scroll_lock: Option<bool>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeyboardLedCapabilities {
    pub reports_state: bool,
}

#[async_trait]
pub trait HidBackend: Send + Sync {
    async fn keyboard_led_capabilities(&self) -> KeyboardLedCapabilities {
        KeyboardLedCapabilities::default()
    }
    async fn keyboard_led_state(&self) -> Result<KeyboardLedState, String> {
        Ok(KeyboardLedState::default())
    }
    async fn keyboard(&self, scancode: u16, pressed: bool) -> Result<(), String>;
    async fn consumer(&self, usage: u16) -> Result<(), String>;
    async fn mouse_move(&self, x: i16, y: i16) -> Result<(), String>;
    async fn mouse_move_absolute(&self, x: i16, y: i16) -> Result<(), String> {
        self.mouse_move(x, y).await
    }
    async fn mouse_move_relative(&self, x: i16, y: i16) -> Result<(), String> {
        self.mouse_move(x, y).await
    }
    async fn mouse_button(&self, x: i16, y: i16, buttons: u8) -> Result<(), String>;
    async fn mouse_button_transition(
        &self,
        x: i16,
        y: i16,
        button: u8,
        pressed: bool,
    ) -> Result<(), String> {
        if button > 2 {
            return Err("unsupported mouse button".to_string());
        }
        self.mouse_button(x, y, if pressed { 1u8 << button } else { 0 })
            .await
    }
    async fn mouse_scroll(&self, x: i16, y: i16, scroll: i8) -> Result<(), String>;
    async fn reset(&self) -> Result<(), String> {
        Err("HID reset is not supported by this backend".to_string())
    }
    async fn keyboard_screen(&self, _enabled: bool) -> Result<(), String> {
        Err("keyboard screen control is not supported by this backend".to_string())
    }
    async fn request_control(&self) -> Result<(), String> {
        Err("control acquisition is not supported by this backend".to_string())
    }
    async fn relinquish_control(&self) -> Result<(), String> {
        Err("control relinquish is not supported by this backend".to_string())
    }
}
