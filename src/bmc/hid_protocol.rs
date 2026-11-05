/// Shared HID binary protocol parsing.
///
/// HID event-bus transports receive the same binary protocol from the browser
/// frontend. This module parses it once.
///
/// Protocol (matches SPA's `hid.ts`):
///   MSG_KEYBOARD (0x01) → 4 bytes: [type, event, usage_low, reserved]
///   MSG_MOUSE    (0x02) → 7 bytes: [type, event, x: i16 LE, y: i16 LE, button]
///   MSG_CONSUMER (0x03) → 3 bytes: [type, usage: u16 LE]

#[derive(Debug, Clone, PartialEq)]
pub enum HidEvent {
    Keyboard { usage: u16, pressed: bool },
    MouseMoveRelative { x: i16, y: i16 },
    MouseMoveAbsolute { x: i16, y: i16 },
    MouseDown { x: i16, y: i16, button: u8 },
    MouseUp { x: i16, y: i16, button: u8 },
    MouseScroll { x: i16, y: i16, delta: i8 },
    Consumer { usage: u16 },
}

/// Parse one binary HID message.
/// Returns `None` for unknown types/events, invalid buttons or incorrect frame lengths.
pub fn parse_hid_message(data: &[u8]) -> Option<HidEvent> {
    if data.is_empty() {
        return None;
    }
    match data[0] {
        0x01 if data.len() == 4 && matches!(data[1], 0x00 | 0x01) => Some(HidEvent::Keyboard {
            usage: data[2] as u16,
            pressed: data[1] == 0x00,
        }),
        0x02 if data.len() == 7 => {
            let event_type = data[1];
            let x = i16::from_le_bytes([data[2], data[3]]);
            let y = i16::from_le_bytes([data[4], data[5]]);
            let btn_data = data[6];
            match event_type {
                0x00 => Some(HidEvent::MouseMoveRelative { x, y }),
                0x01 => Some(HidEvent::MouseMoveAbsolute { x, y }),
                0x02 if btn_data <= 2 => Some(HidEvent::MouseDown {
                    x,
                    y,
                    button: btn_data,
                }),
                0x03 if btn_data <= 2 => Some(HidEvent::MouseUp {
                    x,
                    y,
                    button: btn_data,
                }),
                0x04 => Some(HidEvent::MouseScroll {
                    x,
                    y,
                    delta: btn_data as i8,
                }),
                _ => None,
            }
        }
        0x03 if data.len() == 3 => Some(HidEvent::Consumer {
            usage: u16::from_le_bytes([data[1], data[2]]),
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_event_byte_is_admitted_only_for_its_defined_operation() {
        for event in 0..=u8::MAX {
            assert_eq!(parse_hid_message(&[1, event, 4, 0]).is_some(), event <= 1);
            assert_eq!(
                parse_hid_message(&[2, event, 1, 0, 2, 0, 0]).is_some(),
                event <= 4
            );
        }
        for button in 0..=u8::MAX {
            for event in [2, 3] {
                assert_eq!(
                    parse_hid_message(&[2, event, 0, 0, 0, 0, button]).is_some(),
                    button <= 2
                );
            }
        }
    }

    #[test]
    fn messages_require_exact_lengths_without_partial_or_concatenated_actions() {
        for frame in [
            vec![1, 0, 4, 0],
            vec![2, 0, 0, 0, 0, 0, 0],
            vec![3, 0xe9, 0],
        ] {
            for length in 0..frame.len() {
                assert!(parse_hid_message(&frame[..length]).is_none());
            }
            let mut overlong = frame.clone();
            overlong.push(0);
            assert!(parse_hid_message(&overlong).is_none());
        }
    }

    #[test]
    fn test_empty_data_returns_none() {
        assert_eq!(parse_hid_message(&[]), None);
    }

    #[test]
    fn test_keyboard_down() {
        let msg = vec![0x01, 0x00, 0x04, 0x00];
        assert_eq!(
            parse_hid_message(&msg),
            Some(HidEvent::Keyboard {
                usage: 0x04,
                pressed: true
            })
        );
    }

    #[test]
    fn test_keyboard_up() {
        let msg = vec![0x01, 0x01, 0x04, 0x00];
        assert_eq!(
            parse_hid_message(&msg),
            Some(HidEvent::Keyboard {
                usage: 0x04,
                pressed: false
            })
        );
    }

    #[test]
    fn test_keyboard_truncated_returns_none() {
        let msg = vec![0x01, 0x00, 0x04];
        assert_eq!(parse_hid_message(&msg), None);
    }

    #[test]
    fn test_mouse_move() {
        let msg = vec![0x02, 0x00, 0x10, 0x00, 0x20, 0x00, 0x00];
        assert_eq!(
            parse_hid_message(&msg),
            Some(HidEvent::MouseMoveRelative { x: 16, y: 32 })
        );
    }

    #[test]
    fn test_mouse_move_negative() {
        let msg = vec![0x02, 0x01, 0xF0, 0xFF, 0xE0, 0xFF, 0x00];
        assert_eq!(
            parse_hid_message(&msg),
            Some(HidEvent::MouseMoveAbsolute { x: -16, y: -32 })
        );
    }

    #[test]
    fn mouse_move_event_type_preserves_relative_and_absolute_modes() {
        assert_eq!(
            parse_hid_message(&[0x02, 0x00, 1, 0, 2, 0, 0]),
            Some(HidEvent::MouseMoveRelative { x: 1, y: 2 })
        );
        assert_eq!(
            parse_hid_message(&[0x02, 0x01, 3, 0, 4, 0, 0]),
            Some(HidEvent::MouseMoveAbsolute { x: 3, y: 4 })
        );
    }

    #[test]
    fn test_mouse_down() {
        let msg = vec![0x02, 0x02, 0x10, 0x00, 0x20, 0x00, 0x00];
        assert_eq!(
            parse_hid_message(&msg),
            Some(HidEvent::MouseDown {
                x: 16,
                y: 32,
                button: 0
            })
        );
    }

    #[test]
    fn test_mouse_up() {
        let msg = vec![0x02, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];
        assert_eq!(
            parse_hid_message(&msg),
            Some(HidEvent::MouseUp {
                x: 0,
                y: 0,
                button: 1
            })
        );
    }

    #[test]
    fn test_mouse_scroll() {
        let msg = vec![0x02, 0x04, 0x00, 0x00, 0x00, 0x00, 0x01];
        assert_eq!(
            parse_hid_message(&msg),
            Some(HidEvent::MouseScroll {
                x: 0,
                y: 0,
                delta: 1
            })
        );
    }

    #[test]
    fn test_mouse_scroll_negative() {
        let msg = vec![0x02, 0x04, 0x00, 0x00, 0x00, 0x00, 0xFF];
        assert_eq!(
            parse_hid_message(&msg),
            Some(HidEvent::MouseScroll {
                x: 0,
                y: 0,
                delta: -1
            })
        );
    }

    #[test]
    fn test_mouse_truncated_returns_none() {
        let msg = vec![0x02, 0x00, 0x10, 0x00, 0x20];
        assert_eq!(parse_hid_message(&msg), None);
    }

    #[test]
    fn test_consumer_event() {
        let msg = vec![0x03, 0xE9, 0x00];
        assert_eq!(
            parse_hid_message(&msg),
            Some(HidEvent::Consumer { usage: 0x00E9 })
        );
    }

    #[test]
    fn test_consumer_truncated_returns_none() {
        let msg = vec![0x03, 0xE9];
        assert_eq!(parse_hid_message(&msg), None);
    }

    #[test]
    fn test_unknown_message_type_returns_none() {
        let msg = vec![0xFF, 0x00];
        assert_eq!(parse_hid_message(&msg), None);
    }
}
