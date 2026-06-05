/// Shared HID binary protocol parsing.
///
/// Both the WebSocket handler (`handlers/hid.rs`) and the WebRTC data channel
/// handler (`webrtc/session.rs`) receive the same binary protocol from the
/// browser frontend. This module parses it once.
///
/// Protocol (matches SPA's `hid.ts`):
///   MSG_KEYBOARD (0x01) → 4 bytes: [type, event, usage_low, modifier]
///   MSG_MOUSE    (0x02) → 7 bytes: [type, event, x: i16 LE, y: i16 LE, button]
///   MSG_CONSUMER (0x03) → 2 bytes: [type, usage_byte]

#[derive(Debug, Clone, PartialEq)]
pub enum HidEvent {
    Keyboard { usage: u16, pressed: bool },
    MouseMove { x: i16, y: i16 },
    MouseDown { x: i16, y: i16, button: u8 },
    MouseUp { x: i16, y: i16, button: u8 },
    MouseScroll { x: i16, y: i16, delta: i8 },
    Consumer { code: u8 },
}

/// Parse one binary HID message.
/// Returns `None` for empty data, unknown message types, or truncated messages.
pub fn parse_hid_message(data: &[u8]) -> Option<HidEvent> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        0x01 if data.len() >= 4 => {
            Some(HidEvent::Keyboard {
                usage: data[2] as u16,
                pressed: data[1] == 0x00,
            })
        }
        0x02 if data.len() >= 7 => {
            let event_type = data[1];
            let x = i16::from_le_bytes([data[2], data[3]]);
            let y = i16::from_le_bytes([data[4], data[5]]);
            let btn_data = data[6];
            match event_type {
                0x00 | 0x01 => Some(HidEvent::MouseMove { x, y }),
                0x02 => Some(HidEvent::MouseDown { x, y, button: btn_data }),
                0x03 => Some(HidEvent::MouseUp { x, y, button: btn_data }),
                0x04 => Some(HidEvent::MouseScroll { x, y, delta: btn_data as i8 }),
                _ => Some(HidEvent::MouseMove { x, y }),
            }
        }
        0x03 if data.len() >= 2 => Some(HidEvent::Consumer { code: data[1] }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_data_returns_none() {
        assert_eq!(parse_hid_message(&[]), None);
    }

    #[test]
    fn test_keyboard_down() {
        let msg = vec![0x01, 0x00, 0x04, 0x00];
        assert_eq!(parse_hid_message(&msg), Some(HidEvent::Keyboard { usage: 0x04, pressed: true }));
    }

    #[test]
    fn test_keyboard_up() {
        let msg = vec![0x01, 0x01, 0x04, 0x00];
        assert_eq!(parse_hid_message(&msg), Some(HidEvent::Keyboard { usage: 0x04, pressed: false }));
    }

    #[test]
    fn test_keyboard_truncated_returns_none() {
        let msg = vec![0x01, 0x00, 0x04];
        assert_eq!(parse_hid_message(&msg), None);
    }

    #[test]
    fn test_mouse_move() {
        let msg = vec![0x02, 0x00, 0x10, 0x00, 0x20, 0x00, 0x00];
        assert_eq!(parse_hid_message(&msg), Some(HidEvent::MouseMove { x: 16, y: 32 }));
    }

    #[test]
    fn test_mouse_move_negative() {
        let msg = vec![0x02, 0x01, 0xF0, 0xFF, 0xE0, 0xFF, 0x00];
        assert_eq!(parse_hid_message(&msg), Some(HidEvent::MouseMove { x: -16, y: -32 }));
    }

    #[test]
    fn test_mouse_down() {
        let msg = vec![0x02, 0x02, 0x10, 0x00, 0x20, 0x00, 0x00];
        assert_eq!(parse_hid_message(&msg), Some(HidEvent::MouseDown { x: 16, y: 32, button: 0 }));
    }

    #[test]
    fn test_mouse_up() {
        let msg = vec![0x02, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
        assert_eq!(parse_hid_message(&msg), Some(HidEvent::MouseUp { x: 0, y: 0, button: 1 }));
    }

    #[test]
    fn test_mouse_scroll() {
        let msg = vec![0x02, 0x04, 0x00, 0x00, 0x00, 0x00, 0x01];
        assert_eq!(parse_hid_message(&msg), Some(HidEvent::MouseScroll { x: 0, y: 0, delta: 1 }));
    }

    #[test]
    fn test_mouse_scroll_negative() {
        let msg = vec![0x02, 0x04, 0x00, 0x00, 0x00, 0x00, 0xFF];
        assert_eq!(parse_hid_message(&msg), Some(HidEvent::MouseScroll { x: 0, y: 0, delta: -1 }));
    }

    #[test]
    fn test_mouse_truncated_returns_none() {
        let msg = vec![0x02, 0x00, 0x10, 0x00, 0x20];
        assert_eq!(parse_hid_message(&msg), None);
    }

    #[test]
    fn test_consumer_event() {
        let msg = vec![0x03, 0xE8];
        assert_eq!(parse_hid_message(&msg), Some(HidEvent::Consumer { code: 0xE8 }));
    }

    #[test]
    fn test_consumer_truncated_returns_none() {
        let msg = vec![0x03];
        assert_eq!(parse_hid_message(&msg), None);
    }

    #[test]
    fn test_unknown_message_type_returns_none() {
        let msg = vec![0xFF, 0x00];
        assert_eq!(parse_hid_message(&msg), None);
    }
}
