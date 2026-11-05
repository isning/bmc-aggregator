use super::pool::VncLease;
use async_trait::async_trait;
use bmc_backend_model::input::{Event, Input, Protocol};
use bmc_feature_api::hid::HidBackend;
use tokio::sync::Mutex;

pub struct VncHidBackend {
    lease: VncLease,
    input: Mutex<Input>,
}
impl VncHidBackend {
    pub fn new(lease: VncLease) -> Self {
        Self {
            lease,
            input: Mutex::new(Input::new(Protocol::Vnc)),
        }
    }
    async fn apply(&self, event: Event) -> Result<(), String> {
        let mut input = self.input.lock().await;
        let (width, height) = self.lease.session().resolution();
        let (next, actions) = input
            .clone()
            .step(event, width, height)
            .map_err(|error| format!("VNC input rejected: {error:?}"))?;
        let lossy = matches!(event, Event::Absolute(..) | Event::Relative(..));
        if self.lease.session().send_input(actions, lossy).await? {
            *input = next;
        }
        Ok(())
    }
}
#[async_trait]
impl HidBackend for VncHidBackend {
    async fn keyboard(&self, scancode: u16, pressed: bool) -> Result<(), String> {
        let key = hid_usage_to_keysym(scancode)
            .ok_or_else(|| format!("USB HID usage {scancode:#x} is not supported by VNC"))?;
        self.apply(Event::Key(key, pressed)).await
    }
    async fn consumer(&self, usage: u16) -> Result<(), String> {
        self.apply(Event::Consumer(u32::from(usage))).await
    }
    async fn mouse_move(&self, x: i16, y: i16) -> Result<(), String> {
        self.apply(Event::Absolute(x, y)).await
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
}

fn hid_usage_to_keysym(usage: u16) -> Option<u32> {
    match usage {
        0x04..=0x1d => Some(u32::from(b'a' + (usage as u8 - 0x04))),
        0x1e..=0x26 => Some(u32::from(b'1' + (usage as u8 - 0x1e))),
        0x27 => Some(u32::from(b'0')),
        0x28 => Some(0xff0d), // Return
        0x29 => Some(0xff1b), // Escape
        0x2a => Some(0xff08), // Backspace
        0x2b => Some(0xff09), // Tab
        0x2c => Some(u32::from(b' ')),
        0x2d => Some(u32::from(b'-')),
        0x2e => Some(u32::from(b'=')),
        0x2f => Some(u32::from(b'[')),
        0x30 => Some(u32::from(b']')),
        0x31 => Some(u32::from(b'\\')),
        0x33 => Some(u32::from(b';')),
        0x34 => Some(u32::from(b'\'')),
        0x35 => Some(u32::from(b'`')),
        0x36 => Some(u32::from(b',')),
        0x37 => Some(u32::from(b'.')),
        0x38 => Some(u32::from(b'/')),
        0x39 => Some(0xffe5), // Caps Lock
        0x3a..=0x45 => Some(0xffbe + u32::from(usage - 0x3a)),
        0x46 => Some(0xff61), // Print Screen
        0x47 => Some(0xff14), // Scroll Lock
        0x48 => Some(0xff13), // Pause
        0x49 => Some(0xff63), // Insert
        0x4a => Some(0xff50), // Home
        0x4b => Some(0xff55), // Page Up
        0x4c => Some(0xffff), // Delete
        0x4d => Some(0xff57), // End
        0x4e => Some(0xff56), // Page Down
        0x4f => Some(0xff53), // Right
        0x50 => Some(0xff51), // Left
        0x51 => Some(0xff54), // Down
        0x52 => Some(0xff52), // Up
        0x53 => Some(0xff7f), // Num Lock
        0x54 => Some(0xffaf), // Keypad Divide
        0x55 => Some(0xffaa), // Keypad Multiply
        0x56 => Some(0xffad), // Keypad Subtract
        0x57 => Some(0xffab), // Keypad Add
        0x58 => Some(0xff8d), // Keypad Enter
        0x59..=0x61 => Some(0xffb1 + u32::from(usage - 0x59)),
        0x62 => Some(0xffb0),                                  // Keypad 0
        0x63 => Some(0xffae),                                  // Keypad Decimal
        0x64 => Some(u32::from(b'\\')),                        // Non-US backslash
        0x65 => Some(0xff67),                                  // Context Menu
        0x67 => Some(0xffbd),                                  // Keypad Equal
        0x68..=0x73 => Some(0xffca + u32::from(usage - 0x68)), // F13-F24
        0xe0 => Some(0xffe3),                                  // Left Control
        0xe1 => Some(0xffe1),                                  // Left Shift
        0xe2 => Some(0xffe9),                                  // Left Alt
        0xe3 => Some(0xffeb),                                  // Left Super
        0xe4 => Some(0xffe4),                                  // Right Control
        0xe5 => Some(0xffe2),                                  // Right Shift
        0xe6 => Some(0xffea),                                  // Right Alt
        0xe7 => Some(0xffec),                                  // Right Super
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::hid_usage_to_keysym;

    #[test]
    fn maps_common_usb_hid_usages_to_x11_keysyms() {
        assert_eq!(hid_usage_to_keysym(0x04), Some(u32::from(b'a')));
        assert_eq!(hid_usage_to_keysym(0x28), Some(0xff0d));
        assert_eq!(hid_usage_to_keysym(0x4a), Some(0xff50));
        assert_eq!(hid_usage_to_keysym(0xe1), Some(0xffe1));
        assert_eq!(hid_usage_to_keysym(0x00), None);
    }

    #[test]
    fn special_keys_and_keypad_are_distinct_and_match_browser_usb_usages() {
        for (usage, expected) in [
            (0x46, 0xff61),
            (0x47, 0xff14),
            (0x48, 0xff13),
            (0x54, 0xffaf),
            (0x55, 0xffaa),
            (0x56, 0xffad),
            (0x57, 0xffab),
            (0x58, 0xff8d),
            (0x59, 0xffb1),
            (0x61, 0xffb9),
            (0x62, 0xffb0),
            (0x63, 0xffae),
            (0x65, 0xff67),
            (0x67, 0xffbd),
            (0x68, 0xffca),
            (0x73, 0xffd5),
        ] {
            assert_eq!(
                hid_usage_to_keysym(usage),
                Some(expected),
                "usage {usage:#x}"
            );
        }
        assert_ne!(hid_usage_to_keysym(0x28), hid_usage_to_keysym(0x58));
    }
}
