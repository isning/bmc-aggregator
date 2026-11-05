# Frontend Video Lifecycle

The console separates three kinds of state that must not be collapsed into
one set of booleans:

1. **Source signal** comes from the backend (`streaming`, `no_signal`,
   `device_lost`, `device_busy`). Elapsed time never creates a source signal.
2. **Transport connection** belongs to MJPEG, WebRTC, or WebTransport and is
   used only by that transport's connection/recovery implementation.
3. **Presentation lifecycle** is the typed state machine in
   `frontend/src/features/video/lifecycle.ts`. `ConsoleView` renders only its
   presentation policy.

The presentation policy distinguishes mounting from visibility. WebRTC and
WebTransport surfaces are mounted while connecting or waiting for a first
frame so their tracks/decoders can bind, but remain transparent. They are
unmounted for `no_signal`, `device_lost`, `device_busy`, `error`, and `idle`.

The device event WebSocket holds a lightweight video feature lease while the
console is active. This lease is independent of the MJPEG/WebRTC/WebTransport
render surface. It keeps backend signal observation alive after surfaces are
unmounted and is released automatically when the event socket closes. A BMC
can therefore report `streaming` after power-on and drive automatic recovery
without retaining an empty media element.

```text
connect_requested -> connecting -> source_ready -> waiting_first_frame
                                                    |
                                               frame_received
                                                    v
                                                streaming

backend signal -> no_signal | device_lost | device_busy
backend streaming -> waiting_first_frame -> fresh frame -> streaming
fault -> error
cleanup -> idle
```

An authoritative no-signal state cannot be overwritten by cumulative decoder
statistics. The backend must first report `streaming`; only a subsequent fresh
browser frame moves the lifecycle back to `streaming`.

Transport-specific work is registered in strategy tables:

- surface cleanup: remove MJPEG URL, clear WebRTC `srcObject`, or clear the
  WebTransport canvas;
- recovery: refresh MJPEG, rebind/reconnect WebRTC, or await the next
  WebTransport frame.

Adding a transport therefore extends the transport strategy table and its
transport implementation. It must not add another conditional branch to the
console template.
