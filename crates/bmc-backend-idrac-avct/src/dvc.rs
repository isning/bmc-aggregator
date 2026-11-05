//! Dell Virtual Console color-depth control, recovered from the official
//! Avocent client rather than inferred from packet captures.

use bytes::Bytes;

use super::avct::AvctPacket;

pub const SET_DVC_COLOR_DEPTH_TYPE: u16 = 0x0403;
pub const DVC_COLOR_MODE_RESPONSE_TYPE: u16 = 0x8204;

/// A selectable DVC colour-depth/preference combination.
///
/// `variant` is intentionally a protocol-neutral identifier. The vendor
/// client carries this as an unnamed boolean, so calling it e.g. "high
/// colour" would be speculation.
pub use bmc_idrac_model::avct::ColorDepthProfile;

pub const COLOR_DEPTH_PROFILES: &[ColorDepthProfile] = &[
    ColorDepthProfile {
        id: "base-4",
        depth_bpp: 4,
        variant: false,
    },
    ColorDepthProfile {
        id: "base-5",
        depth_bpp: 5,
        variant: false,
    },
    ColorDepthProfile {
        id: "base-6",
        depth_bpp: 6,
        variant: false,
    },
    ColorDepthProfile {
        id: "base-7",
        depth_bpp: 7,
        variant: false,
    },
    ColorDepthProfile {
        id: "variant-1-4",
        depth_bpp: 4,
        variant: true,
    },
    ColorDepthProfile {
        id: "variant-1-5",
        depth_bpp: 5,
        variant: true,
    },
    ColorDepthProfile {
        id: "variant-1-6",
        depth_bpp: 6,
        variant: true,
    },
    ColorDepthProfile {
        id: "variant-1-7",
        depth_bpp: 7,
        variant: true,
    },
    ColorDepthProfile {
        id: "variant-1-9",
        depth_bpp: 9,
        variant: true,
    },
    ColorDepthProfile {
        id: "variant-1-12",
        depth_bpp: 12,
        variant: true,
    },
    ColorDepthProfile {
        id: "variant-1-15",
        depth_bpp: 15,
        variant: true,
    },
    ColorDepthProfile {
        id: "variant-1-18",
        depth_bpp: 18,
        variant: true,
    },
    ColorDepthProfile {
        id: "variant-1-21",
        depth_bpp: 21,
        variant: true,
    },
    ColorDepthProfile {
        id: "variant-1-23",
        depth_bpp: 23,
        variant: true,
    },
];

/// Hardware-confirmed iDRAC6 combinations. Max Video 4/5/6 and Performance
/// 9+ are ignored by the appliance when requested from the opposite family.
pub const IDRAC6_COLOR_DEPTH_PROFILES: &[ColorDepthProfile] = &[
    COLOR_DEPTH_PROFILES[0],
    COLOR_DEPTH_PROFILES[1],
    COLOR_DEPTH_PROFILES[2],
    COLOR_DEPTH_PROFILES[3],
    COLOR_DEPTH_PROFILES[7],
    COLOR_DEPTH_PROFILES[8],
    COLOR_DEPTH_PROFILES[9],
    COLOR_DEPTH_PROFILES[10],
    COLOR_DEPTH_PROFILES[11],
    COLOR_DEPTH_PROFILES[12],
    COLOR_DEPTH_PROFILES[13],
];

/// Hardware-confirmed iDRAC8 combinations. Its raw `0x8204` status confirms
/// the three low-depth Max Video entries omitted by the official slider.
pub const IDRAC8_COLOR_DEPTH_PROFILES: &[ColorDepthProfile] = COLOR_DEPTH_PROFILES;

pub fn color_depth_profile(id: &str) -> Option<ColorDepthProfile> {
    COLOR_DEPTH_PROFILES
        .iter()
        .copied()
        .find(|profile| profile.id == id)
}

/// Encode `com.avocent.c.d.p` / `SetDVCColorDepthMessage`.
pub fn color_depth_request(profile: ColorDepthProfile) -> AvctPacket {
    let mut payload = [0u8; 8];
    // The official slider calls p.c(variant) and p.a(depth).  p.l remains its
    // constructor default (`true`), which serializes as zero.
    payload[1] = profile.depth_bpp;
    payload[2] = u8::from(profile.variant);
    AvctPacket::beef(SET_DVC_COLOR_DEPTH_TYPE, Bytes::copy_from_slice(&payload))
}

/// Encode the reference client's automatic display-family transition.
pub fn default_color_depth_request() -> AvctPacket {
    let mut packet = color_depth_request(COLOR_DEPTH_PROFILES[0]);
    // The automatic path additionally calls p.b(false), inverting byte zero.
    packet.payload = Bytes::from_static(&[1, 0, 0, 0, 0, 0, 0, 0]);
    packet
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ColorModeResponse {
    pub mode: u8,
    pub flag_1: bool,
    pub flag_2: bool,
}

pub fn parse_color_mode_response(packet: &AvctPacket) -> Result<ColorModeResponse, String> {
    if packet.message_type != DVC_COLOR_MODE_RESPONSE_TYPE {
        return Err("not a DVC color-mode response".to_string());
    }
    if packet.payload.len() < 3 {
        return Err("DVC color-mode response is shorter than three fields".to_string());
    }
    Ok(ColorModeResponse {
        mode: packet.payload[0],
        flag_1: packet.payload[1] != 0,
        flag_2: packet.payload[2] != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_cover_the_hardware_confirmed_union() {
        assert_eq!(COLOR_DEPTH_PROFILES.len(), 14);
        assert_eq!(color_depth_profile("base-4").unwrap().depth_bpp, 4);
        assert!(color_depth_profile("base-9").is_none());
        assert_eq!(color_depth_profile("variant-1-4").unwrap().depth_bpp, 4);
        assert_eq!(color_depth_profile("variant-1-23").unwrap().depth_bpp, 23);
        assert_eq!(IDRAC6_COLOR_DEPTH_PROFILES.len(), 11);
        assert_eq!(IDRAC8_COLOR_DEPTH_PROFILES.len(), 14);
    }

    #[test]
    fn encodes_the_reference_slider_and_automatic_packets() {
        assert_eq!(
            &color_depth_request(color_depth_profile("variant-1-15").unwrap()).payload[..],
            &[0, 15, 1, 0, 0, 0, 0, 0]
        );
        assert_eq!(
            &default_color_depth_request().payload[..],
            &[1, 0, 0, 0, 0, 0, 0, 0]
        );
    }
}
