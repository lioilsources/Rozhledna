# Rozhledna

Hub obrazovek a oken všech počítačů na LAN (pracovní název). Plán: [`Prompts/00-ROZHLEDNA_PLAN.md`](Prompts/00-ROZHLEDNA_PLAN.md).

**Stav: Fáze 0 — spike a měření.** Rozhodnutí a měření: [`DECISIONS.md`](DECISIONS.md), [`LATENCY.md`](LATENCY.md), co dál: [`docs/dogfood/phase-0.md`](docs/dogfood/phase-0.md).

## Struktura

| cesta | obsah |
|---|---|
| `proto/rozhledna.proto` | protokol v1 |
| `crates/rz-proto` | prost typy, rámcování, hlavička video framu, datagramy, hodiny + clock sync |
| `crates/rz-net` | QUIC (quinn), dev certifikát, frame-per-stream, control stream |
| `crates/rz-capture` | `trait Capture`: `testpattern`, `macos_sck` (displej, okno); příklad `sck-probe` |
| `crates/rz-codec` | `trait Encoder/Decoder`, Annex B, VideoToolbox; příklad `vt-bench` |
| `crates/xtask` | `cargo xtask gen-proto | lint | bundle <spike> | flutter-spike` |
| `spikes/spike-agent` | capture → VT → QUIC |
| `spikes/spike-viewer` | QUIC → VT → CALayer, overlay, CSV (lib sdílená s Flutter spikem) |
| `spikes/rz-spike-ffi`, `spikes/flutter_spike` | Flutter texture spike (T0.6) |
| `tools/latency` | `rz-latency report`, `counter.html` |
| `docs/probes`, `docs/dogfood` | průzkumy a checklisty |

## Rychlý start (bez oprávnění, jeden stroj)

```
cargo xtask lint
cargo build --release -p spike-agent -p spike-viewer
./target/release/spike-agent --insecure-dev --source testpattern --bind 127.0.0.1:47471 &
./target/release/spike-viewer 127.0.0.1:47471 --insecure-dev --verify-testpattern --duration 10
```

`--insecure-dev` je ve Fázi 0 povinné: žádné párování, jen důvěryhodná LAN.

Licence vlastního kódu: MIT OR Apache-2.0 (D2), viz [`LICENSE-MIT`](LICENSE-MIT) a [`LICENSE-APACHE`](LICENSE-APACHE). Kód z GPL/AGPL projektů se nekopíruje.
