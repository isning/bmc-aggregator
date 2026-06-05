use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::sync::broadcast;
use webrtc::api::interceptor_registry::register_default_interceptors;
use webrtc::api::media_engine::MediaEngine;
use webrtc::api::APIBuilder;
use webrtc::interceptor::registry::Registry;
use webrtc::peer_connection::configuration::RTCConfiguration;
use webrtc::peer_connection::peer_connection_state::RTCPeerConnectionState;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;



use bmc_aggregator::bmc::encoder::Codec;
use bmc_aggregator::bmc::stream::encoder::{VideoStreamEncoder, SoftwareFfmpegEncoder};
use bmc_aggregator::bmc::stream::pipeline::SharedVideoPipeline;
use bmc_aggregator::bmc::webrtc::session::WebRtcSession;
use bmc_aggregator::bmc::webrtc::config::WebRtcConfig;

/// Create a test frame (solid blue RGBA)
fn test_frame(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            pixels[idx] = 0;     // R
            pixels[idx + 1] = 0; // G
            pixels[idx + 2] = 255; // B
            pixels[idx + 3] = 255; // A
        }
    }
    pixels
}

#[tokio::test]
async fn test_webrtc_signaling_flow() {
    // Initialize ffmpeg once
    let _ = ffmpeg_next::init();

    // ── Create a test pipeline with H264 encoder ──
    let encoder: Box<dyn VideoStreamEncoder> = Box::new(
        SoftwareFfmpegEncoder::new(Codec::H264, 64, 48, 500, 30).unwrap(),
    );
    let pipeline = SharedVideoPipeline::new(encoder);

    // Push a test frame so the pipeline has data to broadcast
    let frame = test_frame(64, 48);
    pipeline.push_frame(&frame, 64, 48);
    // Give the encoder time to encode
    tokio::time::sleep(Duration::from_millis(100)).await;
    pipeline.push_frame(&frame, 64, 48);

    // ── Create a WebRTC session (server side) ──
    let session = WebRtcSession::new("test-device".into(), WebRtcConfig::default())
        .with_pipeline(pipeline.tx(), Codec::H264);

    // ── Create a webrtc-rs based client to generate a real SDP offer ──
    let mut m = MediaEngine::default();
    m.register_default_codecs().unwrap();

    let registry = Registry::new();
    let registry = register_default_interceptors(registry, &mut m).unwrap();

    let api = APIBuilder::new()
        .with_media_engine(m)
        .with_interceptor_registry(registry)
        .build();

    let pc = api.new_peer_connection(RTCConfiguration::default()).await.unwrap();

    // Create a data channel (simulating the browser's SPA behavior)
    let _dc = pc.create_data_channel("kvm", None).await.unwrap();
    eprintln!("Data channel created: label=kvm");

    // Add a receive-only video track (simulating the browser's SPA behavior)
    use webrtc::rtp_transceiver::rtp_codec::RTPCodecType;
    use webrtc::rtp_transceiver::rtp_transceiver_direction::RTCRtpTransceiverDirection;
    use webrtc::rtp_transceiver::RTCRtpTransceiverInit;
    pc.add_transceiver_from_kind(
        RTPCodecType::Video,
        Some(RTCRtpTransceiverInit {
            direction: RTCRtpTransceiverDirection::Recvonly,
            send_encodings: vec![],
        }),
    ).await.unwrap();

    let offer = pc.create_offer(None).await.unwrap();
    pc.set_local_description(offer.clone()).await.unwrap();

    // Wait for ICE candidates
    tokio::time::sleep(Duration::from_millis(500)).await;

    let offer_sdp = offer.sdp;
    eprintln!("Client offer SDP:\n{}", &offer_sdp[..offer_sdp.len().min(500)]);

    // ── Send offer to server-side session ──
    let offer_req = bmc_aggregator::bmc::webrtc::signaling::WebRtcOfferRequest {
        sdp: offer_sdp.clone(),
    };

    let response = session.handle_offer(&offer_req).await.unwrap();
    eprintln!(
        "Server response: session_id={}, candidates={}, sdp_len={}",
        response.session_id,
        response.candidates.len(),
        response.sdp.len(),
    );

    // Log the candidates
    for (i, c) in response.candidates.iter().enumerate() {
        eprintln!("  candidate[{}]: {}", i, c.candidate);
    }

    assert!(!response.session_id.is_empty(), "should have session_id");
    assert!(!response.sdp.is_empty(), "should have answer SDP");
    eprintln!("Answer SDP:\n{}", &response.sdp);
    if response.sdp.contains("a=inactive") {
        eprintln!("WARNING: answer has a=inactive — client recvonly not matched correctly");
    }
    assert!(response.sdp.contains("a=sendonly"), "answer should be sendonly for recvonly offer");

    // ── Set the answer on the client PeerConnection ──
    let answer_desc = RTCSessionDescription::answer(response.sdp.clone()).unwrap();
    pc.set_remote_description(answer_desc).await.unwrap();

    // Add server ICE candidates to the client
    for c in &response.candidates {
        let init = webrtc::ice_transport::ice_candidate::RTCIceCandidateInit {
            candidate: c.candidate.clone(),
            sdp_mid: c.sdp_mid.clone(),
            sdp_mline_index: c.sdp_mline_index,
            username_fragment: c.username_fragment.clone(),
        };
        pc.add_ice_candidate(init).await.unwrap();
    }

    // Wait for ICE connection to establish (up to 10 s)
    let connected = Arc::new(AtomicBool::new(false));
    let c = connected.clone();
    pc.on_peer_connection_state_change(Box::new(move |s| {
        eprintln!("Client peer connection state: {s:?}");
        if s == RTCPeerConnectionState::Connected {
            c.store(true, Ordering::SeqCst);
        }
        Box::pin(async {})
    }));

    // Listen for incoming video track data
    let track_received = Arc::new(tokio::sync::Notify::new());
    let tr = track_received.clone();
    pc.on_track(Box::new(move |track, _receiver, _transceiver| {
        let tr = tr.clone();
        eprintln!("Client received track: kind={}, id={}", track.kind(), track.id());
        tokio::spawn(async move {
            let mut count = 0u32;
            loop {
                match track.read_rtp().await {
                    Ok((pkt, _attr)) => {
                        count += 1;
                        if count == 1 {
                            eprintln!("First RTP packet received! seq={}, ts={}, len={}, pt={}",
                                pkt.header.sequence_number, pkt.header.timestamp,
                                pkt.payload.len(), pkt.header.payload_type);
                            tr.notify_one();
                        }
                        if count >= 5 {
                            eprintln!("Received 5 RTP packets — media flowing");
                            break;
                        }
                    }
                    Err(e) => {
                        eprintln!("RTP read error: {e}");
                        break;
                    }
                }
            }
        });
        Box::pin(async {})
    }));

    // Push frames continuously so the video track has data to send when ICE connects
    let pipeline_clone = pipeline.clone();
    let frame_clone = frame.clone();
    tokio::spawn(async move {
        for _ in 0..20 {
            pipeline_clone.push_frame(&frame_clone, 64, 48);
            tokio::time::sleep(Duration::from_millis(33)).await;
        }
    });

    for attempt in 0..20 {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if connected.load(Ordering::SeqCst) {
            eprintln!("ICE connection established after {}s!", (attempt + 1) / 2);
            break;
        }
        if attempt == 19 {
            eprintln!("WARNING: ICE connection did not establish within 10s");
        }
    }

    // Wait for RTP data to be received (up to 5 s)
    tokio::time::timeout(Duration::from_secs(5), track_received.notified()).await.ok();
    eprintln!("Media flow test completed");

    // Close the peer connections
    pc.close().await.unwrap();
    session.close().await;
}

#[tokio::test]
async fn test_webrtc_session_create_and_close() {
    let (tx, _rx) = broadcast::channel(64);
    let session = WebRtcSession::new("test".into(), WebRtcConfig::default())
        .with_pipeline(tx, Codec::H264);

    assert!(!session.id.is_empty());

    // Closing without an active peer connection should not panic
    session.close().await;
}

#[test]
fn test_h264_payloader_with_encoder_output() {
    let _ = ffmpeg_next::init();
    let width = 1280;
    let height = 720;
    let mut encoder: Box<dyn VideoStreamEncoder> = Box::new(
        SoftwareFfmpegEncoder::new(Codec::H264, width, height, 2000, 30).unwrap(),
    );

    // Create a test frame with some variation
    let mut frame = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            frame[idx] = (x % 255) as u8;
            frame[idx + 1] = (y % 255) as u8;
            frame[idx + 2] = 128u8;
            frame[idx + 3] = 255u8;
        }
    }

    // Encode two frames (first is keyframe, gets SPS/PPS+IDR)
    let packets1 = encoder.encode(&frame, width, height).unwrap();
    eprintln!("Keyframe encoder output: {} packets", packets1.len());
    for (i, pkt) in packets1.iter().enumerate() {
        eprintln!("  Packet[{}]: {} bytes, starts with {:02x} {:02x} {:02x} {:02x}",
            i, pkt.len(),
            pkt.get(0).copied().unwrap_or(0),
            pkt.get(1).copied().unwrap_or(0),
            pkt.get(2).copied().unwrap_or(0),
            pkt.get(3).copied().unwrap_or(0),
        );
        // Check for H264 start code or NAL type
        if pkt.len() >= 4 {
            if pkt[0] == 0 && pkt[1] == 0 && pkt[2] == 0 && pkt[3] == 1 {
                eprintln!("    → Annex B, NAL types found:");
                let mut offset = 4;
                while offset < pkt.len() {
                    if offset + 4 <= pkt.len() && pkt[offset] == 0 && pkt[offset+1] == 0 && pkt[offset+2] == 0 && pkt[offset+3] == 1 {
                        eprint!(" [start code at {offset}]");
                        offset += 4;
                        continue;
                    }
                    if offset + 3 <= pkt.len() && pkt[offset] == 0 && pkt[offset+1] == 0 && pkt[offset+2] == 1 {
                        eprint!(" [3-byte start code at {offset}]");
                        offset += 3;
                        continue;
                    }
                    let nal_type = pkt[offset] & 0x1F;
                    let nri = (pkt[offset] >> 5) & 0x03;
                    eprint!(" NAL(type={nal_type}, nri={nri})");
                    // Skip past this NALU (find next start code)
                    let mut found = false;
                    for j in (offset+1)..pkt.len() {
                        if j+3 < pkt.len() && pkt[j] == 0 && pkt[j+1] == 0 && pkt[j+2] == 0 && pkt[j+3] == 1 {
                            offset = j;
                            found = true;
                            break;
                        }
                        if j+2 < pkt.len() && pkt[j] == 0 && pkt[j+1] == 0 && pkt[j+2] == 1 {
                            offset = j;
                            found = true;
                            break;
                        }
                    }
                    if !found { break; }
                }
                eprintln!();
            } else {
                let nal_type = pkt[0] & 0x1F;
                eprintln!("    → Raw NAL, type={nal_type}");
            }
        }
    }

    // Feed through H264Payloader
    use rtp::codecs::h264::H264Payloader;
    use rtp::packetizer::Payloader;
    use bytes::Bytes;

    let mut payloader = H264Payloader::default();
    let mtu = 1400 - 12; // 1388 payload MTU (matches packetizer)

    for (i, pkt) in packets1.iter().enumerate() {
        let data = Bytes::copy_from_slice(pkt);
        let result = payloader.payload(mtu, &data).unwrap();
        eprintln!("  Payloader output for packet[{}] ({} bytes input): {} RTP payloads",
            i, pkt.len(), result.len());
        for (j, payload) in result.iter().enumerate() {
            let first_byte = payload[0];
            let nal_type = first_byte & 0x1F;
            let pkt_desc = match nal_type {
                24 => format!("STAP-A ({} NALUs, {} bytes)", {
                    // count NALUs in STAP-A
                    let mut count = 0;
                    let mut off = 1;
                    while off + 2 < payload.len() {
                        let nalu_len = ((payload[off] as usize) << 8) | payload[off+1] as usize;
                        off += 2 + nalu_len;
                        count += 1;
                    }
                    count
                }, payload.len()),
                28 => {
                    let is_start = (payload[1] & 0x80) != 0;
                    let is_end = (payload[1] & 0x40) != 0;
                    let fu_type = payload[1] & 0x1F;
                    if is_start { format!("FU-A START (type={}, {} bytes)", fu_type, payload.len()) }
                    else if is_end { format!("FU-A END (type={}, {} bytes)", fu_type, payload.len()) }
                    else { format!("FU-A MID (type={}, {} bytes)", fu_type, payload.len()) }
                }
                _ => format!("Single NALU type={} ({} bytes)", nal_type, payload.len()),
            };
            eprintln!("    payload[{}]: {}", j, pkt_desc);
        }
    }

    // Now test with just the encoder output directly (skip pipeline splitting)
    // to verify SPS/PPS are in the payloader output
    let mut combined = Vec::new();
    for pkt in &packets1 {
        combined.extend_from_slice(pkt);
    }
    let combined_data = Bytes::copy_from_slice(&combined);
    let mut payloader2 = H264Payloader::default();
    let result2 = payloader2.payload(mtu, &combined_data).unwrap();
    eprintln!("\nCombined ({}/{} bytes): {} payloads", combined.len(), combined_data.len(), result2.len());
    let mut has_sps_or_pps = false;
    for (j, payload) in result2.iter().enumerate() {
        let nal_type = payload[0] & 0x1F;
        if nal_type == 24 {
            // STAP-A: check for SPS(7) and PPS(8)
            let mut off = 1;
            while off + 2 < payload.len() {
                let nalu_len = ((payload[off] as usize) << 8) | payload[off+1] as usize;
                off += 2;
                if off < payload.len() {
                    let inner_type = payload[off] & 0x1F;
                    eprintln!("  STAP-A contains NAL type={}", inner_type);
                    if inner_type == 7 || inner_type == 8 {
                        has_sps_or_pps = true;
                    }
                }
                off += nalu_len;
            }
        }
        if nal_type == 7 || nal_type == 8 {
            has_sps_or_pps = true;
            eprintln!("  Contains standalone SPS/PPS NAL type={}", nal_type);
        }
    }
    assert!(has_sps_or_pps, "No SPS/PPS found in payloader output!");
    eprintln!("TEST PASSED: SPS/PPS found in payloader output");
}
