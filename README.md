# Rozhledna

Hub obrazovek a oken všech počítačů na LAN (pracovní název). Plán: [`Prompts/00-ROZHLEDNA_PLAN.md`](Prompts/00-ROZHLEDNA_PLAN.md).

**Stav: Fáze 1 — MVP „zeď obrazovek“ macOS ↔ macOS** (T1.1–T1.9 v kódu, čeká na dogfood [`docs/dogfood/phase-1.md`](docs/dogfood/phase-1.md)).
Fáze 0 uzavřena: [`DECISIONS.md`](DECISIONS.md), [`LATENCY.md`](LATENCY.md), [`docs/dogfood/phase-0.md`](docs/dogfood/phase-0.md).

## Rychlý start (dva Macy)

```
export PATH=/opt/homebrew/opt/rustup/bin:$PATH
# agent (stroj, který sdílí obrazovky)
cargo xtask bundle rz-agent && open "dist/Rozhledna Agent.app"      # menu bar: povolit Nahrávání obrazovky + Zpřístupnění
# hub (stroj, ze kterého se díváš a ovládáš)
cargo xtask hub && open dist/Rozhledna.app                          # vlevo „V síti“ → Přidat → potvrdit kód na agentovi
```

Bez oprávnění a bez druhého stroje: `rz-agent --headless --source testpattern --bind 127.0.0.1:47493`, pak
`RZ_HUB_ADD=127.0.0.1:47493 open dist/Rozhledna.app` (párování potvrdíš v terminálu agenta písmenem `a`).

Diagnostika: `rzctl discover | pair | sources | stats | poke | trusted | forget` (`cargo run -p rz-cli -- …`).

## Struktura

| cesta | obsah |
|---|---|
| `proto/rozhledna.proto` | protokol v1 |
| `crates/rz-proto` | prost typy, rámcování, hlavička video framu, datagramy, hodiny + clock sync |
| `crates/rz-net` | QUIC (quinn), identita, trust store, mTLS, SAS párování, mDNS, backoff, video/control |
| `crates/rz-capture` | `trait Capture`: `testpattern`, `macos_sck`; příklad `sck-probe` |
| `crates/rz-codec` | `trait Encoder/Decoder`, Annex B, VideoToolbox; příklad `vt-bench` |
| `crates/rz-input` | HID tabulky, stav vstupu per hub, injektory `null` a macOS CGEvent |
| `crates/rz-agent` | agent: zdroje, pipeline, session, vstup, kurzor, stav, menu bar |
| `crates/rz-hub-core` | jádro hubu: session s agenty, reconnect, příjem + dekódování, vstup, události |
| `crates/rz-hub-ffi` | C ABI pro Flutter (`include/rz_hub.h`) |
| `crates/rz-cli` | `rzctl` |
| `crates/xtask` | `cargo xtask gen-proto | lint | bundle <rz-agent…> | hub | notarize <app> | flutter-spike` |
| `hub/` | Flutter hub (Swift plugin: textury, události, key grab) |
| `spikes/` | Fáze 0 (spike-agent, spike-viewer, Flutter spike) |
| `tools/latency` | `rz-latency report`, `counter.html` |
| `docs/probes`, `docs/dogfood` | průzkumy a checklisty |

Licence vlastního kódu: MIT OR Apache-2.0 (D2), viz [`LICENSE-MIT`](LICENSE-MIT) a [`LICENSE-APACHE`](LICENSE-APACHE). Kód z GPL/AGPL projektů se nekopíruje.
