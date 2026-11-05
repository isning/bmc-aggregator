// Protocol message types (matching connection.rs msg module)
const MSG_KEY_STATE_CHANGE: u8 = 0xD1; // 209
const MSG_MOUSE_MOVE: u8 = 0xB5; // 181
const MSG_BUTTON_STATE_ABSOLUTE: u8 = 0xB3; // 179
const MSG_BUTTON_STATE_RELATIVE: u8 = 0xB4; // 180

/// Encode keyboard event: KeyStateChange (0xD1)
/// Format: [0xD1, scancode: u16 LE, pressed: u8, preTranslated: u8]
/// The frontend sends USB HID usage IDs. Mahogany expects those as
/// pre-translated key codes; the remote BMC/guest still owns key hold state
/// and typematic repeat because we only send key state edges.
pub fn encode_keyboard_event(scancode: u16, pressed: bool) -> Vec<u8> {
    vec![
        MSG_KEY_STATE_CHANGE,
        (scancode & 0xFF) as u8,
        (scancode >> 8) as u8,
        if pressed { 1 } else { 0 },
        1, // preTranslated = true for frontend USB HID usages.
    ]
}

/// Encode a Consumer Control usage as a one-shot Mahogany key event.
///
/// Mahogany carries the 16-bit key code through the same KeyStateChange
/// message as keyboard keys. Consumer usages are sent as a press/release pair
/// because the frontend media-key UI represents commands, not held keys.
#[cfg(test)]
pub fn encode_consumer_event(usage: u16) -> [Vec<u8>; 2] {
    [
        encode_keyboard_event(usage, true),
        encode_keyboard_event(usage, false),
    ]
}

/// Encode mouse move event: MouseMove (0xB5)
/// Format: [0xB5, x: i32 LE, y: i32 LE]
pub fn encode_mouse_move(x: i16, y: i16) -> Vec<u8> {
    let mut buf = vec![MSG_MOUSE_MOVE];
    buf.extend_from_slice(&(x as i32).to_le_bytes());
    buf.extend_from_slice(&(y as i32).to_le_bytes());
    buf
}

/// Encode button state at absolute position: ButtonStateAtAbsolute (0xB3)
/// Format: [0xB3, x: i32 LE, y: i32 LE, numButtons: u8, buttonState...]
/// button_mask bits: bit0=left, bit1=middle, bit2=right. The official
/// Mahogany order is left, right, middle, so the latter two are remapped.
#[cfg(test)]
pub fn encode_button_state(x: i16, y: i16, button_mask: u8) -> Vec<u8> {
    encode_button_state_for_mode(x, y, button_mask, true)
}

/// Encode a button-state report for Mahogany relative mode (0xB4).
#[cfg(test)]
pub fn encode_button_state_relative(x: i16, y: i16, button_mask: u8) -> Vec<u8> {
    encode_button_state_with_wheel(x, y, button_mask, false, 0)
}

#[cfg(test)]
fn encode_button_state_for_mode(x: i16, y: i16, button_mask: u8, absolute: bool) -> Vec<u8> {
    encode_button_state_with_wheel(x, y, button_mask, absolute, 0)
}

/// Encode a button report with the official track-wheel field. Mahogany's
/// Java client centers the wheel at 64 and adds the signed wheel delta.
pub fn encode_button_state_with_wheel(
    x: i16,
    y: i16,
    button_mask: u8,
    absolute: bool,
    wheel: i8,
) -> Vec<u8> {
    // The official client always serializes exactly three ButtonState
    // elements (left, middle, right). Extra browser buttons have no
    // Mahogany wire representation.
    let num_buttons = 3u8;
    let mut buf = vec![if absolute {
        MSG_BUTTON_STATE_ABSOLUTE
    } else {
        MSG_BUTTON_STATE_RELATIVE
    }];
    buf.extend_from_slice(&(x as i32).to_le_bytes());
    buf.extend_from_slice(&(y as i32).to_le_bytes());
    buf.push(num_buttons);
    for (index, source_bit) in [0u8, 2, 1].into_iter().enumerate() {
        let pressed = if (button_mask >> source_bit) & 1 != 0 {
            1
        } else {
            0
        };
        buf.push(if index == 2 {
            // Keep the pressed bit while carrying the centered wheel value
            // in bits 1..7, exactly as ButtonState#setTrackWheelPosition.
            pressed | (((64i16 + i16::from(wheel)).clamp(0, 127) as u8) << 1)
        } else {
            pressed
        });
    }
    buf
}

/// NOP keepalive
pub fn encode_nop() -> Vec<u8> {
    vec![0xD2, 0, 0, 0, 0, 0, 0, 0]
}

// ── Feature adapter ──────────────────────────────────────────────

use crate::pool::PoolLease;
use crate::protocol::{
    build_absolute_mode, build_keyboard_screen, build_relative_mode, build_relinquish_control,
    build_request_primary_control,
};
use async_trait::async_trait;
use bmc_backend_model::input::{Action, Event, Input, Mode, Protocol};
use bmc_feature_api::hid::HidBackend as HidBackendTrait;
use bmc_feature_api::hid::{KeyboardLedCapabilities, KeyboardLedState};

const KEYBOARD_CONTROL_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

pub struct MahoganyHidBackend {
    lease: PoolLease,
    input: tokio::sync::Mutex<Input>,
}
impl MahoganyHidBackend {
    pub fn new(lease: PoolLease) -> Self {
        Self {
            lease,
            input: tokio::sync::Mutex::new(Input::new(Protocol::Mahogany)),
        }
    }
    async fn apply(&self, event: Event) -> Result<(), String> {
        let mut input = self.input.lock().await;
        if !self.lease.entry().is_online() {
            return Err("Mahogany session is disconnected".into());
        }
        if matches!(event, Event::Key(_, true) | Event::Consumer(_)) {
            self.lease
                .entry()
                .wait_for_keyboard_control(KEYBOARD_CONTROL_WAIT)
                .await?;
        }
        let (next, actions) = input
            .clone()
            .step(event, i16::MAX as u16, i16::MAX as u16)
            .map_err(|error| format!("Mahogany input rejected: {error:?}"))?;
        let messages: Vec<u8> = actions
            .into_iter()
            .flat_map(|action| match action {
                Action::Key(key, pressed) => encode_keyboard_event(key as u16, pressed),
                Action::Move(x, y) => encode_mouse_move(x, y),
                Action::Report(x, y, buttons, mode, wheel) => {
                    encode_button_state_with_wheel(x, y, buttons, mode == Mode::Absolute, wheel)
                }
                Action::Mode(Mode::Absolute) => build_absolute_mode(true),
                Action::Mode(Mode::Relative) => build_relative_mode(true, false),
                Action::Pointer(..) => unreachable!("Mahogany model emits native input actions"),
            })
            .collect();
        if messages.is_empty() {
            return Ok(());
        }
        if matches!(event, Event::Absolute(..) | Event::Relative(..)) {
            match self.lease.entry().tcp_tx.try_send(messages) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(_)) => return Ok(()),
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return Err("Mahogany input channel is closed".into())
                }
            }
        } else {
            tokio::time::timeout(
                KEYBOARD_CONTROL_WAIT,
                self.lease.entry().tcp_tx.send(messages),
            )
            .await
            .map_err(|_| "Mahogany input queue is busy".to_string())?
            .map_err(|_| "Mahogany input channel is closed".to_string())?;
        }
        *input = next;
        Ok(())
    }
    async fn send_control(&self, message: Vec<u8>) -> Result<(), String> {
        tokio::time::timeout(
            KEYBOARD_CONTROL_WAIT,
            self.lease.entry().tcp_tx.send(message),
        )
        .await
        .map_err(|_| "Mahogany control queue is busy".to_string())?
        .map_err(|error| format!("Mahogany control channel: {error}"))
    }
}
#[async_trait]
impl HidBackendTrait for MahoganyHidBackend {
    async fn keyboard_led_capabilities(&self) -> KeyboardLedCapabilities {
        KeyboardLedCapabilities {
            reports_state: true,
        }
    }
    async fn keyboard_led_state(&self) -> Result<KeyboardLedState, String> {
        let mask = self.lease.entry().keyboard_led_mask();
        Ok(KeyboardLedState {
            num_lock: Some(mask & 1 != 0),
            caps_lock: Some(mask & 2 != 0),
            scroll_lock: Some(mask & 4 != 0),
        })
    }
    async fn keyboard(&self, scancode: u16, pressed: bool) -> Result<(), String> {
        self.apply(Event::Key(u32::from(scancode), pressed)).await
    }
    async fn consumer(&self, usage: u16) -> Result<(), String> {
        self.apply(Event::Consumer(u32::from(usage))).await
    }
    async fn mouse_move(&self, x: i16, y: i16) -> Result<(), String> {
        self.apply(Event::Relative(x, y)).await
    }
    async fn mouse_move_absolute(&self, x: i16, y: i16) -> Result<(), String> {
        self.apply(Event::Absolute(x, y)).await
    }
    async fn mouse_move_relative(&self, x: i16, y: i16) -> Result<(), String> {
        self.apply(Event::Relative(x, y)).await
    }
    async fn mouse_button(&self, x: i16, y: i16, buttons: u8) -> Result<(), String> {
        self.apply(Event::Button(x, y, buttons)).await
    }
    async fn mouse_button_transition(
        &self,
        x: i16,
        y: i16,
        button: u8,
        pressed: bool,
    ) -> Result<(), String> {
        self.apply(Event::ButtonEdge(x, y, button, pressed)).await
    }
    async fn mouse_scroll(&self, x: i16, y: i16, scroll: i8) -> Result<(), String> {
        self.apply(Event::Scroll(x, y, scroll)).await
    }
    async fn reset(&self) -> Result<(), String> {
        self.apply(Event::Reset).await
    }
    async fn keyboard_screen(&self, enabled: bool) -> Result<(), String> {
        self.send_control(build_keyboard_screen(enabled)).await
    }
    async fn request_control(&self) -> Result<(), String> {
        self.send_control(build_request_primary_control()).await
    }
    async fn relinquish_control(&self) -> Result<(), String> {
        self.send_control(build_relinquish_control()).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_keyboard_pressed() {
        let result = encode_keyboard_event(0x04, true);
        assert_eq!(result.len(), 5);
        assert_eq!(result[0], 0xD1);
        assert_eq!(result[1], 0x04);
        assert_eq!(result[2], 0x00);
        assert_eq!(result[3], 0x01);
        assert_eq!(result[4], 0x01);
    }

    #[test]
    fn test_encode_keyboard_released() {
        let result = encode_keyboard_event(0x04, false);
        assert_eq!(result[3], 0x00);
        assert_eq!(result[4], 0x01);
    }

    #[test]
    fn test_encode_keyboard_extended_scancode() {
        let result = encode_keyboard_event(0xE048, true);
        assert_eq!(result[1], 0x48);
        assert_eq!(result[2], 0xE0);
    }

    #[test]
    fn test_encode_consumer_event_is_press_release_pair() {
        let [down, up] = encode_consumer_event(0x00E9);
        assert_eq!(down, vec![0xD1, 0xE9, 0x00, 0x01, 0x01]);
        assert_eq!(up, vec![0xD1, 0xE9, 0x00, 0x00, 0x01]);
    }

    #[test]
    fn test_encode_mouse_move() {
        let result = encode_mouse_move(100, 200);
        assert_eq!(result.len(), 9);
        assert_eq!(result[0], 0xB5);
        assert_eq!(
            i32::from_le_bytes([result[1], result[2], result[3], result[4]]),
            100
        );
        assert_eq!(
            i32::from_le_bytes([result[5], result[6], result[7], result[8]]),
            200
        );
    }

    #[test]
    fn test_encode_mouse_move_negative() {
        let result = encode_mouse_move(-50, -100);
        assert_eq!(
            i32::from_le_bytes([result[1], result[2], result[3], result[4]]),
            -50
        );
        assert_eq!(
            i32::from_le_bytes([result[5], result[6], result[7], result[8]]),
            -100
        );
    }

    #[test]
    fn test_encode_button_state_no_buttons() {
        let result = encode_button_state(100, 200, 0);
        assert_eq!(result[0], 0xB3);
        assert_eq!(
            i32::from_le_bytes([result[1], result[2], result[3], result[4]]),
            100
        );
        assert_eq!(
            i32::from_le_bytes([result[5], result[6], result[7], result[8]]),
            200
        );
        assert_eq!(result[9], 3);
        assert_eq!(&result[10..13], &[0, 0, 0x80]);
        assert_eq!(result.len(), 13);
    }

    #[test]
    fn test_encode_button_state_left_click() {
        let result = encode_button_state(100, 200, 0x01);
        assert_eq!(result[0], 0xB3);
        assert_eq!(&result[10..13], &[1, 0, 0x80]);
    }

    #[test]
    fn test_encode_button_state_all_buttons() {
        let result = encode_button_state(0, 0, 0x07);
        assert_eq!(&result[10..13], &[1, 1, 0x81]);
    }

    #[test]
    fn test_encode_button_state_relative_uses_b4_and_three_buttons() {
        let result = encode_button_state_relative(-10, 20, 0b101);
        assert_eq!(result[0], 0xB4);
        assert_eq!(
            i32::from_le_bytes([result[1], result[2], result[3], result[4]]),
            -10
        );
        assert_eq!(&result[10..13], &[1, 1, 0x80]);
        assert_eq!(result.len(), 13);
    }

    #[test]
    fn test_encode_nop() {
        let result = encode_nop();
        assert_eq!(result.len(), 8);
        assert_eq!(result[0], 0xD2);
        assert!(result[1..].iter().all(|&b| b == 0));
    }
}
