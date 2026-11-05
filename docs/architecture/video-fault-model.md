# Video Fault Model

Video uses the shared `FeatureFault` contract in `bmc/feature/fault.rs`; it
does not define a video-specific fault type or a compatibility wrapper.

```text
VideoSource -> VideoRuntime -> VideoProcessor -> {MJPEG, WebRTC, WebTransport}
    source fault       shared FeatureFaultStore       child faults
                              |
                 status / trace / HTTP / device WebSocket
```

`FeatureConnection` owns the store. `VideoRuntime` gives its exact store to
`VideoProcessor`, so lifecycle failures, decoder/encoder failures, and browser
transport failures have one identity and one sanitization boundary.

- Source lifecycle failures are `Feature` or `Operation` scope and appear in
  `GET /feature/video/status` as `fault`.
- MJPEG/WebRTC/WebTransport faults are `Child` scope (`mjpeg`, `webrtc`, or
  `webtransport`) and remain in trace history without taking an otherwise
  healthy source offline.
- A known source fault makes the MJPEG endpoint return its structured JSON
  failure before opening a multipart response, preventing an infinite
  “Waiting for first frame” state.
- `POST /feature/video/retry` clears a retryable active fault and wakes the
  video lifecycle. `POST /feature/video/reset` additionally resets browser
  stream state and requests a full source frame where supported.

Public fault text never includes BMC credentials, APCP/AVCT payloads,
SDP/ICE, raw frame bytes, or backend error text. The native iDRAC APCP EOF
case is mapped to `lifecycle/apcp_rejected` with the message “The BMC closed
the AVCT APCP setup connection”.
