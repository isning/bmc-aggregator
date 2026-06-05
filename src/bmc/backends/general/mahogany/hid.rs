// Protocol message types (matching connection.rs msg module)
const MSG_KEY_STATE_CHANGE: u8 = 0xD1; // 209
const MSG_MOUSE_MOVE: u8 = 0xB5;       // 181
const MSG_BUTTON_STATE: u8 = 0xB3;     // 179

/// Encode keyboard event: KeyStateChange (0xD1)
/// Format: [0xD1, scancode: u16 LE, pressed: u8, preTranslated: u8]
/// `preTranslated = 1` because the scancode is already a USB HID usage code
/// and should not be translated by the BMC.
pub fn encode_keyboard_event(scancode: u16, pressed: bool) -> Vec<u8> {
    vec![
        MSG_KEY_STATE_CHANGE,
        (scancode & 0xFF) as u8,
        (scancode >> 8) as u8,
        if pressed { 1 } else { 0 },
        1, // preTranslated = true — scancode is USB HID usage code
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
/// button_mask bits: bit0=left, bit1=middle, bit2=right, bit3=back, bit4=forward
pub fn encode_button_state(x: i16, y: i16, button_mask: u8) -> Vec<u8> {
    let num_buttons = 5u8;
    let mut buf = vec![MSG_BUTTON_STATE];
    buf.extend_from_slice(&(x as i32).to_le_bytes());
    buf.extend_from_slice(&(y as i32).to_le_bytes());
    buf.push(num_buttons);
    for i in 0..num_buttons {
        buf.push(if (button_mask >> i) & 1 != 0 { 1 } else { 0 });
    }
    buf
}

/// NOP keepalive
pub fn encode_nop() -> Vec<u8> {
    let mut buf = vec![0xD2];
    buf.resize(8, 0);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_keyboard_pressed() {
        let result = encode_keyboard_event(0x04, true);
        assert_eq!(result.len(), 5);
        assert_eq!(result[0], 0xD1);
        assert_eq!(result[1], 0x04); // scancode low
        assert_eq!(result[2], 0x00); // scancode high
        assert_eq!(result[3], 0x01); // pressed = true
        assert_eq!(result[4], 0x01); // preTranslated = true
    }

    #[test]
    fn test_encode_keyboard_released() {
        let result = encode_keyboard_event(0x04, false);
        assert_eq!(result[3], 0x00);
        assert_eq!(result[4], 0x01); // preTranslated = true
    }

    #[test]
    fn test_encode_keyboard_extended_scancode() {
        let result = encode_keyboard_event(0xE048, true);
        assert_eq!(result[1], 0x48); // low byte
        assert_eq!(result[2], 0xE0); // high byte
    }

    #[test]
    fn test_encode_mouse_move() {
        let result = encode_mouse_move(100, 200);
        assert_eq!(result.len(), 9);
        assert_eq!(result[0], 0xB5);
        assert_eq!(i32::from_le_bytes([result[1], result[2], result[3], result[4]]), 100);
        assert_eq!(i32::from_le_bytes([result[5], result[6], result[7], result[8]]), 200);
    }

    #[test]
    fn test_encode_mouse_move_negative() {
        let result = encode_mouse_move(-50, -100);
        assert_eq!(i32::from_le_bytes([result[1], result[2], result[3], result[4]]), -50);
        assert_eq!(i32::from_le_bytes([result[5], result[6], result[7], result[8]]), -100);
    }

    #[test]
    fn test_encode_button_state_no_buttons() {
        let result = encode_button_state(100, 200, 0);
        assert_eq!(result[0], 0xB3);
        assert_eq!(i32::from_le_bytes([result[1], result[2], result[3], result[4]]), 100);
        assert_eq!(i32::from_le_bytes([result[5], result[6], result[7], result[8]]), 200);
        assert_eq!(result[9], 5); // numButtons
        assert_eq!(&result[10..15], &[0, 0, 0, 0, 0]); // all buttons released
        assert_eq!(result.len(), 15);
    }

    #[test]
    fn test_encode_button_state_left_click() {
        let result = encode_button_state(100, 200, 0x01);
        assert_eq!(result[0], 0xB3);
        assert_eq!(&result[10..15], &[1, 0, 0, 0, 0]); // only left button pressed
    }

    #[test]
    fn test_encode_button_state_all_buttons() {
        let result = encode_button_state(0, 0, 0x1F);
        assert_eq!(&result[10..15], &[1, 1, 1, 1, 1]);
    }

    #[test]
    fn test_encode_nop() {
        let result = encode_nop();
        assert_eq!(result.len(), 8);
        assert_eq!(result[0], 0xD2);
        assert!(result[1..].iter().all(|&b| b == 0));
    }
}
