//! Version-selected AVCT Web-account login.
//!
//! The selected request encoder comes from the negotiated APCP protocol pair.
//! It must not be hard-coded to the iDRAC8 compatibility (`0x0102`) form.

use bytes::Bytes;
use rand::Rng;

use super::avct::AvctPacket;

const FIXED_CREDENTIAL_FIELD_BYTES: usize = 96;
const DIRECT_TICKET_BYTES: [u8; 8] = [0; 8];
const MAX_MODERN_CREDENTIAL_BYTES: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginProfile {
    Legacy,
    Compatibility,
    Modern,
}

impl LoginProfile {
    pub fn from_apcp_pair(major: u8, minor: u8) -> Self {
        match major {
            0 | 1 => Self::Legacy,
            2 if minor < 41 => Self::Compatibility,
            _ => Self::Modern,
        }
    }
    fn request_type(self) -> u16 {
        match self {
            Self::Legacy => 0x0100,
            Self::Compatibility => 0x0102,
            Self::Modern => 0x0104,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LoginResponse {
    pub status: u8,
    pub flags: u16,
    /// The source-visible response scalar subsequently placed in the second
    /// half of `Video Channel Auth`; it is not optional or zero-filled.
    pub appliance_random: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LoginResult {
    Accepted(LoginResponse),
    Rejected(LoginResponse),
}

pub struct LoginRequest {
    pub packet: AvctPacket,
    pub client_random: u32,
}

pub fn build_login_request(
    profile: LoginProfile,
    username: &str,
    password: &str,
    virtual_media_privilege: bool,
) -> Result<LoginRequest, String> {
    build_login_request_with_random(
        profile,
        username,
        password,
        rand::thread_rng().gen_range(0..10_000_000u32),
        virtual_media_privilege,
    )
}

fn build_login_request_with_random(
    profile: LoginProfile,
    username: &str,
    password: &str,
    client_random: u32,
    mode: bool,
) -> Result<LoginRequest, String> {
    let username = credential_bytes(username, "username")?;
    let password = credential_bytes(password, "password")?;
    let payload = match profile {
        LoginProfile::Legacy | LoginProfile::Compatibility => {
            if username.len() > FIXED_CREDENTIAL_FIELD_BYTES
                || password.len() > FIXED_CREDENTIAL_FIELD_BYTES
            {
                return Err(
                    "AVCT username or password exceeds the 96-byte legacy field".to_string()
                );
            }
            let mut payload = vec![
                0;
                if profile == LoginProfile::Legacy {
                    208
                } else {
                    209
                }
            ];
            payload[0] = username.len() as u8;
            payload[1..1 + username.len()].copy_from_slice(username);
            payload[97] = password.len() as u8;
            payload[98..98 + password.len()].copy_from_slice(password);
            payload[194..202].copy_from_slice(&DIRECT_TICKET_BYTES);
            payload[202] = 1;
            payload[203] = 0;
            payload[204..208].copy_from_slice(&client_random.to_be_bytes());
            if profile == LoginProfile::Compatibility {
                payload[208] = u8::from(mode);
            }
            payload
        }
        LoginProfile::Modern => {
            if username.len() > MAX_MODERN_CREDENTIAL_BYTES
                || password.len() > MAX_MODERN_CREDENTIAL_BYTES
            {
                return Err(
                    "AVCT username or password exceeds the bounded modern field".to_string()
                );
            }
            let mut payload = vec![0; 23 + username.len() + password.len()];
            payload[0..2].copy_from_slice(&(username.len() as u16).to_be_bytes());
            payload[2..4].copy_from_slice(&31u16.to_be_bytes());
            payload[4..6].copy_from_slice(&(password.len() as u16).to_be_bytes());
            payload[6..8].copy_from_slice(&(31 + username.len() as u16).to_be_bytes());
            payload[8..16].copy_from_slice(&DIRECT_TICKET_BYTES);
            payload[16] = 1;
            payload[17] = 0;
            payload[18..22].copy_from_slice(&client_random.to_be_bytes());
            payload[22] = u8::from(mode);
            payload[23..23 + username.len()].copy_from_slice(username);
            payload[23 + username.len()..].copy_from_slice(password);
            payload
        }
    };
    Ok(LoginRequest {
        packet: AvctPacket::beef(profile.request_type(), Bytes::from(payload)),
        client_random,
    })
}

pub fn parse_login_response(packet: &AvctPacket) -> Result<LoginResult, String> {
    if !packet.is_beef() {
        return Err("AVCT peer did not return a BEEF login response".to_string());
    }
    let p = &packet.payload;
    let response = match packet.message_type {
        0x8300 => {
            if p.len() < 9 {
                return Err("AVCT 0x8300 login response is truncated".to_string());
            }
            let name_len = usize::from(p[8]);
            if p.len() < 9 + name_len {
                return Err("AVCT 0x8300 login response has a truncated name".to_string());
            }
            LoginResponse {
                status: p[0],
                flags: u16::from(p[3]),
                appliance_random: u32::from_be_bytes(p[4..8].try_into().expect("bounded")),
            }
        }
        0x8305 => {
            if p.len() < 11 {
                return Err("AVCT 0x8305 login response is truncated".to_string());
            }
            let name_len = usize::from(p[10]);
            if p.len() < 11 + name_len {
                return Err("AVCT 0x8305 login response has a truncated name".to_string());
            }
            LoginResponse {
                status: p[0],
                flags: u16::from_be_bytes(p[8..10].try_into().expect("bounded")),
                appliance_random: u32::from_be_bytes(p[4..8].try_into().expect("bounded")),
            }
        }
        other => {
            return Err(format!(
                "AVCT peer returned unexpected login response 0x{other:04x}"
            ))
        }
    };
    Ok(if response.status == 0 {
        LoginResult::Accepted(response)
    } else {
        LoginResult::Rejected(response)
    })
}

fn credential_bytes<'a>(value: &'a str, label: &str) -> Result<&'a [u8], String> {
    if value.is_empty() {
        return Err(format!("AVCT {label} is empty"));
    }
    if !value.is_ascii() {
        return Err(format!(
            "AVCT {label} must be ASCII for this iDRAC protocol version"
        ));
    }
    Ok(value.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn each_official_login_profile_has_a_byte_oracle() {
        let cases = [
            (LoginProfile::Legacy, 0x0100, 216),
            (LoginProfile::Compatibility, 0x0102, 217),
            (LoginProfile::Modern, 0x0104, 33),
        ];
        for (profile, typ, size) in cases {
            let request =
                build_login_request_with_random(profile, "u", "p", 0x01020304, false).unwrap();
            assert_eq!(request.packet.message_type, typ);
            assert_eq!(request.packet.total_len(), size);
        }
    }
    #[test]
    fn negotiated_pair_selects_the_official_encoder() {
        assert_eq!(LoginProfile::from_apcp_pair(1, 9), LoginProfile::Legacy);
        assert_eq!(
            LoginProfile::from_apcp_pair(2, 40),
            LoginProfile::Compatibility
        );
        assert_eq!(LoginProfile::from_apcp_pair(2, 41), LoginProfile::Modern);
    }
    #[test]
    fn response_variants_preserve_status_and_appliance_random() {
        let legacy = AvctPacket::beef(0x8300, Bytes::from(vec![0, 0, 0, 1, 1, 2, 3, 4, 0]));
        assert_eq!(
            parse_login_response(&legacy).unwrap(),
            LoginResult::Accepted(LoginResponse {
                status: 0,
                flags: 1,
                appliance_random: 0x01020304
            })
        );
        let mut modern = vec![0; 11];
        modern[0] = 4;
        modern[4..8].copy_from_slice(&0x05060708u32.to_be_bytes());
        modern[8..10].copy_from_slice(&0x20u16.to_be_bytes());
        assert_eq!(
            parse_login_response(&AvctPacket::beef(0x8305, Bytes::from(modern))).unwrap(),
            LoginResult::Rejected(LoginResponse {
                status: 4,
                flags: 0x20,
                appliance_random: 0x05060708
            })
        );
    }
}
