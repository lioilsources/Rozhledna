# CHANGELOG

## Unreleased — Fáze 0 (spike a měření)

### T0.1 Kostra
- Cargo workspace (`crates/`, `spikes/`, `tools/`), lints, `rustfmt.toml`, `cargo xtask` (`gen-proto`, `lint`, `bundle`, `flutter-spike`).
- `proto/rozhledna.proto` — protokol v1 včetně dodefinovaných zpráv (PairRequest/Result, Ping/Pong, StreamStats, CursorShape, clipboard, Preview, …).
- `rz-proto`: prost typy, `u32 LE` rámcování, 44 B hlavička video framu, datagramy PointerMotion/CursorPos, monotonní hodiny, NTP-style clock sync.

### T0.2 Kodek
- `rz-capture`: `trait Capture`, `testpattern` (pohyblivý vzor, vypálené číslo framu strojově i 7-seg číslicemi), `CVPixelBuffer` helpery.
- `rz-codec`: `trait Encoder/Decoder`, Annex B ↔ AVCC, VideoToolbox H.264 enkodér (low-latency, infinite GOP, IDR na vyžádání) a dekodér (BGRA IOSurface).
- Test roundtrip testpattern → VT → VT → číslo framu sedí. Benchmark `vt-bench` (1080p/1440p/4K, volitelně šum).

### T0.3 Síť
- `rz-net`: quinn endpointy s LAN transport configem, dev self-signed Ed25519 cert + `--insecure-dev`, frame-per-stream, control stream, clock sync. Loopback test.

### T0.4 / T0.5 Spiky a měření
- `spike-agent`: SCK displej nebo testpattern → VT → QUIC; backpressure a řízení bitrate (6.4), RequestKeyframe, StreamStats, výpis TCC stavu.
- `spike-viewer`: QUIC → reorder/loss handling (6.4) → VT → `CALayer.contents = IOSurface`; overlay P50/P95; CSV; `--headless --verify-testpattern`.
- `tools/latency`: `rz-latency report <csv>`, `counter.html` pro glass-to-glass.

### T0.6 Flutter spike
- `rz-spike-ffi` (staticlib, C ABI) + `spikes/flutter_spike` (Swift `FlutterTexture` plugin, overlay, automatizace přes env proměnné).

### T0.7 / T0.8 Průzkumy
- `sck-probe` (oprávnění bez dialogu, seznam oken, capture okna s/bez child windows, BMP dump). Protokoly v `docs/probes/`.

### Dogfood M2
- `spike-agent --sck-interval-hz N` (`ScOptions.min_interval_hz`): experiment s `minimumFrameInterval` u SCK. Na M2 se reálná plocha drží na ~57 fps místo 60.
- Výchozí SCK interval 1/(2×fps) + throttle na fps v agentovi (≥ ¾ intervalu mezi framy).
