//! WebRTC E2E tests.
//!
//! Server = GstWebRtcSession (encoder + pay + webrtcbin)
//! Client = recvonly webrtcbin (models a browser offer)
//! Flow: recvonly offer → server.handle_offer() → exchange ICE → verify both peers.

use std::sync::Arc;
use std::time::Duration;

use bmc_aggregator::bmc::encoder::Codec;
use bmc_aggregator::bmc::stream::gst_webrtc::GstWebRtcSession;
use gst::glib;
use gst::prelude::*;
use gstreamer as gst;
use gstreamer_sdp as gst_sdp;
use gstreamer_webrtc as gst_webrtc;

fn gst_init_once() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        gstreamer::init().expect("GStreamer init");
    });
}

/// Exchange SDP/ICE between an offerer (client) and an answerer (server).
/// Each test owns an isolated receiver pipeline and server session.
async fn connect_offer_answer(
    offerer: &gst::Element,
    answerer: &GstWebRtcSession,
) -> Result<String, String> {
    // Subscribe before creating the offer: ICE gathering can start as soon
    // as the local description is set.
    let offer_cands = Arc::new(std::sync::Mutex::new(Vec::<(String, u16)>::new()));
    let oc = offer_cands.clone();
    let webrtc = offerer.clone();
    let signal = webrtc.connect("on-ice-candidate", false, move |args| {
        let index = args[1].get::<u32>().expect("ICE media index");
        let candidate = args[2].get::<String>().expect("ICE candidate");
        if !candidate.is_empty() {
            oc.lock().unwrap().push((
                candidate,
                u16::try_from(index).expect("ICE media index fits u16"),
            ));
        }
        None
    });

    let (tx, rx) = tokio::sync::oneshot::channel();
    let promise = gst::Promise::with_change_func(move |reply| {
        let offer = reply.ok().flatten().and_then(|value| {
            value
                .get::<gst_webrtc::WebRTCSessionDescription>("offer")
                .ok()
        });
        let _ = tx.send(offer);
    });
    webrtc.emit_by_name::<()>("create-offer", &[&None::<gst::Structure>, &promise]);
    let offer = rx
        .await
        .map_err(|error| error.to_string())?
        .ok_or("receiver did not create an offer")?;
    webrtc.emit_by_name::<()>("set-local-description", &[&offer, &None::<gst::Promise>]);
    let offer_sdp = offer.sdp().as_text().map_err(|error| error.to_string())?;
    let (answer_sdp, server_cands) = answerer.handle_offer(&offer_sdp).await?;
    for cand in &server_cands {
        webrtc.emit_by_name::<()>(
            "add-ice-candidate",
            &[&(cand.sdp_mline_index.unwrap_or(0) as u32), &cand.candidate],
        );
    }

    // Parse answer SDP and set on offerer
    {
        use glib::prelude::ToValue;
        let parsed = gst_sdp::SDPMessage::parse_buffer(answer_sdp.as_bytes())
            .map_err(|e| format!("parse answer: {e}"))?;
        let answer =
            gst_webrtc::WebRTCSessionDescription::new(gst_webrtc::WebRTCSDPType::Answer, parsed);
        let args: Vec<&dyn ToValue> = vec![&answer, &None::<gst::Promise>];
        webrtc.emit_by_name::<()>("set-remote-description", &args);
    }

    // 5. Send offerer's ICE candidates to answerer
    tokio::time::sleep(Duration::from_millis(500)).await;
    {
        let cands = offer_cands.lock().unwrap();
        for (candidate, index) in cands.iter() {
            answerer.add_ice_candidate(candidate, *index)?;
        }
    }

    webrtc.disconnect(signal);
    Ok(answer_sdp)
}

/// Wait for peer connection state to reach Connected (up to 15s).
async fn wait_ice_connected(webrtc: &gst::Element) -> Result<(), String> {
    for _ in 0..150 {
        let state = webrtc.property::<gst_webrtc::WebRTCPeerConnectionState>("connection-state");
        if state == gst_webrtc::WebRTCPeerConnectionState::Connected {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let final_state: gst_webrtc::WebRTCPeerConnectionState = webrtc.property("connection-state");
    Err(format!(
        "peer connection not connected after 15s (state={final_state:?})"
    ))
}

/// Check SDP negotiation and ICE connectivity. This does not assert decoded
/// frame order or audio playback; those need a receiving media pipeline.
async fn assert_connection(codec: Codec, backend: &str, audio: bool) {
    gst_init_once();
    let server = if audio {
        GstWebRtcSession::new_with_audio("srv".into(), None, codec, backend, 320, 240, 30, 500)
    } else {
        GstWebRtcSession::new("srv".into(), None, codec, backend, 320, 240, 30, 500)
    }
    .expect("create server session");
    let pipeline = gst::Pipeline::new();
    let client = gst::ElementFactory::make("webrtcbin")
        .property_from_str("bundle-policy", "max-bundle")
        .build()
        .expect("create receiving peer");
    pipeline.add(&client).expect("add receiving peer");
    let encoding = match codec {
        Codec::H264 => "H264",
        Codec::H265 => "H265",
        Codec::VP8 => "VP8",
        Codec::VP9 => "VP9",
        Codec::AV1 => "AV1",
        _ => panic!("unsupported test codec"),
    };
    let caps = gst::Caps::builder("application/x-rtp")
        .field("media", "video")
        .field("encoding-name", encoding)
        .field("clock-rate", 90_000i32)
        .field("payload", 96i32)
        .build();
    client.emit_by_name::<gst_webrtc::WebRTCRTPTransceiver>(
        "add-transceiver",
        &[&gst_webrtc::WebRTCRTPTransceiverDirection::Recvonly, &caps],
    );
    if audio {
        let caps = gst::Caps::builder("application/x-rtp")
            .field("media", "audio")
            .field("encoding-name", "OPUS")
            .field("clock-rate", 48_000i32)
            .field("payload", 97i32)
            .build();
        client.emit_by_name::<gst_webrtc::WebRTCRTPTransceiver>(
            "add-transceiver",
            &[&gst_webrtc::WebRTCRTPTransceiverDirection::Recvonly, &caps],
        );
    }
    pipeline
        .set_state(gst::State::Playing)
        .expect("start receiver");
    server.start_main_loop().expect("start server main loop");

    let result = tokio::time::timeout(Duration::from_secs(25), async {
        let answer = connect_offer_answer(&client, &server).await?;
        let answer = gst_sdp::SDPMessage::parse_buffer(answer.as_bytes()).expect("parse answer");
        for kind in if audio {
            &["video", "audio"][..]
        } else {
            &["video"][..]
        } {
            assert!(
                answer
                    .medias()
                    .any(|media| media.media() == Some(*kind) && media.port() != 0),
                "answer must accept {kind}"
            );
        }
        let sender = server.inner_webrtc();
        tokio::try_join!(wait_ice_connected(&client), wait_ice_connected(&sender))?;
        Ok::<(), String>(())
    })
    .await;
    pipeline.set_state(gst::State::Null).expect("stop receiver");
    server.close();
    result
        .expect("SDP/ICE exchange timed out")
        .expect("both peers must connect");
}

macro_rules! ice_connect_test {
    ($(#[$attribute:meta])* $name:ident, $codec:expr, $backend:expr) => {
        $(#[$attribute])*
        #[tokio::test]
        async fn $name() {
            assert_connection($codec, $backend, false).await;
        }
    };
}

ice_connect_test!(test_ice_connect_h264_software, Codec::H264, "software");
ice_connect_test!(test_ice_connect_h265_software, Codec::H265, "software");
ice_connect_test!(test_ice_connect_vp8, Codec::VP8, "software");
ice_connect_test!(test_ice_connect_vp9, Codec::VP9, "software");
ice_connect_test!(test_ice_connect_av1, Codec::AV1, "software");

ice_connect_test!(
    #[ignore = "requires NVIDIA GPU, driver, and NVENC GStreamer plugins"]
    test_ice_connect_nvenc_h264,
    Codec::H264,
    "nvenc"
);
ice_connect_test!(
    #[ignore = "requires NVIDIA GPU, driver, and NVENC GStreamer plugins"]
    test_ice_connect_nvenc_h265,
    Codec::H265,
    "nvenc"
);
ice_connect_test!(
    #[ignore = "requires NVIDIA GPU, driver, and NVENC GStreamer plugins"]
    test_ice_connect_nvenc_av1,
    Codec::AV1,
    "nvenc"
);

#[tokio::test]
async fn test_audio_session_negotiates_h264() {
    assert_connection(Codec::H264, "software", true).await;
}
