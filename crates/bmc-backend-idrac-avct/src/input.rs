//! AVCT control-packet encoding for keyboard and absolute pointer input.
//!
//! These packet layouts are taken from the Dell Avocent client classes
//! (`com.avocent.c.d.f` and `com.avocent.c.d.rc`).  Keeping the encoders here
//! makes the otherwise proprietary control plane explicit and testable.

use bytes::Bytes;

use super::avct::AvctPacket;

const KEYBOARD_TYPE: u16 = 0x0200;
const POINTER_TYPE: u16 = 0x0201;
#[cfg(test)]
const MOUSE_ORIGIN_TYPE: u16 = 0x0202;
const MOUSE_RESET_TYPE: u16 = 0x0203;
const KEYBOARD_LED_REQUEST_TYPE: u16 = 0x0204;
const FOCUS_CONTROL_TYPE: u16 = 0x0208;
#[cfg(test)]
const MOUSE_ABS_SYNC_TYPE: u16 = 0x020b;

/// Encode an Avocent keyboard state change.
///
/// The BMC uses `0` for a key press and `1` for a release.  The key code is
/// the USB HID usage supplied by the browser-facing HID layer.
pub fn keyboard_packet(usage: u16, pressed: bool) -> AvctPacket {
    let mut payload = [0u8; 8];
    payload[1] = u8::from(!pressed);
    payload[2..4].copy_from_slice(&usage.to_be_bytes());
    AvctPacket::beef(KEYBOARD_TYPE, Bytes::copy_from_slice(&payload))
}

/// Map the common frontend button mask to Avocent's button bit order.
///
/// The feature API uses bit 0=left, bit 1=middle, bit 2=right.  AVCT uses
/// left=1, right=2, middle=4.  Extra browser buttons have no AVCT equivalent.
pub fn avct_button_mask(frontend_buttons: u8) -> u16 {
    let mut avct = 0u16;
    if frontend_buttons & 0b001 != 0 {
        avct |= 0b001;
    }
    if frontend_buttons & 0b010 != 0 {
        avct |= 0b100;
    }
    if frontend_buttons & 0b100 != 0 {
        avct |= 0b010;
    }
    avct
}

/// Encode an absolute pointer update, including an optional wheel delta.
pub fn pointer_packet(x: u16, y: u16, frontend_buttons: u8, scroll: i8) -> AvctPacket {
    pointer_packet_type(POINTER_TYPE, x, y, frontend_buttons, scroll)
}

pub fn delta_pointer_packet(x: i16, y: i16, frontend_buttons: u8, scroll: i8) -> AvctPacket {
    pointer_packet_type(0x0209, x as u16, y as u16, frontend_buttons, scroll)
}

fn pointer_packet_type(
    message_type: u16,
    x: u16,
    y: u16,
    frontend_buttons: u8,
    scroll: i8,
) -> AvctPacket {
    let mut payload = [0u8; 8];
    payload[1] = avct_button_mask(frontend_buttons) as u8;
    payload[2..4].copy_from_slice(&x.to_be_bytes());
    payload[4..6].copy_from_slice(&y.to_be_bytes());
    payload[6..8].copy_from_slice(&(i16::from(scroll)).to_be_bytes());
    AvctPacket::beef(message_type, Bytes::copy_from_slice(&payload))
}

fn zero_control_packet(message_type: u16) -> AvctPacket {
    AvctPacket::beef(message_type, Bytes::from_static(&[0; 8]))
}

#[cfg(test)]
pub fn mouse_origin_packet() -> AvctPacket {
    zero_control_packet(MOUSE_ORIGIN_TYPE)
}
pub fn mouse_reset_packet() -> AvctPacket {
    zero_control_packet(MOUSE_RESET_TYPE)
}
pub fn keyboard_led_request_packet() -> AvctPacket {
    zero_control_packet(KEYBOARD_LED_REQUEST_TYPE)
}

/// The source-confirmed boolean façade operation, not a pointer move.
pub fn focus_control_packet(enabled: bool) -> AvctPacket {
    let mut payload = [0; 8];
    payload[0] = u8::from(enabled);
    AvctPacket::beef(FOCUS_CONTROL_TYPE, Bytes::copy_from_slice(&payload))
}

/// `MouseAbsSyncRequest`: the first byte defaults to enabled in the official
/// constructor; the second is its separately-settable raw boolean.
#[cfg(test)]
pub fn mouse_abs_sync_packet(first: bool, second: bool) -> AvctPacket {
    let mut payload = [0; 8];
    payload[0] = u8::from(first);
    payload[1] = u8::from(second);
    AvctPacket::beef(MOUSE_ABS_SYNC_TYPE, Bytes::copy_from_slice(&payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyboard_matches_the_reference_avct_layout() {
        let pressed = keyboard_packet(0x04, true);
        assert_eq!(pressed.message_type, KEYBOARD_TYPE);
        assert_eq!(&pressed.payload[..], &[0, 0, 0, 4, 0, 0, 0, 0]);

        let released = keyboard_packet(0x04, false);
        assert_eq!(&released.payload[..], &[0, 1, 0, 4, 0, 0, 0, 0]);
    }

    #[test]
    fn pointer_maps_frontend_buttons_and_preserves_signed_wheel() {
        let packet = pointer_packet(0x0123, 0x0456, 0b111, -2);
        assert_eq!(packet.message_type, POINTER_TYPE);
        assert_eq!(
            &packet.payload[..],
            &[0, 0x07, 0x01, 0x23, 0x04, 0x56, 0xff, 0xfe]
        );
    }

    #[test]
    fn wheel_and_middle_button_are_distinct_official_fields() {
        let wheel = pointer_packet(0x0123, 0x0456, 0, -1);
        assert_eq!(
            &wheel.payload[..],
            &[0, 0, 0x01, 0x23, 0x04, 0x56, 0xff, 0xff]
        );

        let middle_down = pointer_packet(0x0123, 0x0456, 0b010, 0);
        assert_eq!(
            &middle_down.payload[..],
            &[0, 0x04, 0x01, 0x23, 0x04, 0x56, 0, 0]
        );
    }

    #[test]
    fn relative_pointer_uses_the_official_delta_message_type() {
        let packet = delta_pointer_packet(-2, 3, 0b101, 0);
        assert_eq!(packet.message_type, 0x0209);
        // Frontend bits are left=1, middle=2, right=4; AVCT orders them
        // left=1, right=2, middle=4.  0b101 is therefore AVCT 0b011.
        assert_eq!(&packet.payload[..], &[0, 3, 0xff, 0xfe, 0, 3, 0, 0]);
    }

    #[test]
    fn dedicated_hid_controls_do_not_alias_to_pointer_packets() {
        assert_eq!(mouse_origin_packet().message_type, MOUSE_ORIGIN_TYPE);
        assert_eq!(mouse_reset_packet().message_type, MOUSE_RESET_TYPE);
        assert_eq!(
            keyboard_led_request_packet().message_type,
            KEYBOARD_LED_REQUEST_TYPE
        );
        assert_eq!(
            &focus_control_packet(true).payload[..],
            &[1, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            &mouse_abs_sync_packet(true, false).payload[..],
            &[1, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            &mouse_abs_sync_packet(true, true).payload[..],
            &[1, 1, 0, 0, 0, 0, 0, 0]
        );
    }
}
