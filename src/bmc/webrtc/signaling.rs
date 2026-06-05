use serde::{Deserialize, Serialize};

/// Candidate object as the SPA sends/receives it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IceCandidate {
    pub candidate: String,
    #[serde(rename = "sdpMid", skip_serializing_if = "Option::is_none")]
    pub sdp_mid: Option<String>,
    #[serde(rename = "sdpMLineIndex", skip_serializing_if = "Option::is_none")]
    pub sdp_mline_index: Option<u16>,
    #[serde(rename = "usernameFragment", skip_serializing_if = "Option::is_none")]
    pub username_fragment: Option<String>,
}

/// Incoming POST /webrtc/offer
#[derive(Debug, Deserialize)]
pub struct WebRtcOfferRequest {
    pub sdp: String,
}

/// Outgoing answer for POST /webrtc/offer
#[derive(Debug, Serialize)]
pub struct WebRtcOfferResponse {
    pub session_id: String,
    pub sdp: String,
    #[serde(rename = "ice_candidates", skip_serializing_if = "Vec::is_empty")]
    pub candidates: Vec<IceCandidate>,
}

/// Incoming POST /webrtc/ice
#[derive(Debug, Deserialize)]
pub struct WebRtcIcePayload {
    pub session_id: String,
    pub candidate: IceCandidate,
}

/// Outgoing POST /webrtc/close & /webrtc/ice success
#[derive(Debug, Serialize)]
pub struct WebRtcSuccessResponse {
    pub success: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Outgoing GET /webrtc/ice-servers
#[derive(Debug, Serialize)]
pub struct IceServersResponse {
    pub ice_servers: Vec<IceServerEntry>,
    pub mdns_mode: String,
}

#[derive(Debug, Serialize)]
pub struct IceServerEntry {
    pub urls: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}
