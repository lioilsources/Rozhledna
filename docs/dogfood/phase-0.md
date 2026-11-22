# Dogfood checklist — Fáze 0

Odkliká Ol1n. Potřeba: dva Macy (agent = A, hub = H) na **drátové** LAN, Rust (`rustup`), Flutter, repo na obou.
Výsledky zapisuj do `LATENCY.md` a `docs/probes/*.md`. Fáze 1 začne až po odkliknutí.

Pokud `cargo` není v PATH: `export PATH=/opt/homebrew/opt/rustup/bin:$PATH`.

## 0. Sanity (oba stroje)
- [ ] `cargo xtask lint` projde (fmt, clippy, 24 testů včetně VT roundtripu).
- [ ] `cargo run --release -p rz-codec --example vt-bench` — zapsat tabulku do DECISIONS.md, pokud je stroj jiný než M1 Pro.
- [ ] `cargo run --release -p rz-codec --example vt-bench -- --noise 0.25` — dtto.

## 1. Agent se skutečnou obrazovkou (A)
- [ ] `cargo xtask bundle spike-agent`
- [ ] `open "dist/Rozhledna Spike Agent.app" --args --insecure-dev --log-file /tmp/rz-agent.log`
- [ ] Povolit Screen Recording (zapsat do `docs/probes/macos-permissions.md`, P1/P2: kterou položku systém nabídl).
- [ ] Ukončit (`pkill spike-agent`) a spustit znovu; v `/tmp/rz-agent.log` je `TCC state screen_recording=true`.
- [ ] `--list-displays`: `"dist/Rozhledna Spike Agent.app/Contents/MacOS/spike-agent" --list-displays` (z Terminálu — použije oprávnění Terminálu).

## 2. Nativní viewer přes LAN (H) — T0.4/T0.5, brána G2
- [ ] `cargo run --release -p spike-viewer -- <IP-A>:47470 --insecure-dev --max-w 2560 --max-h 1440 --csv lan-1440-native.csv --duration 60`
- [ ] Obraz plochy A je vidět, plynulý; overlay ukazuje P50/P95.
- [ ] Na A hýbat okny, scrollovat web, přehrát video (aby byl obsah dynamický).
- [ ] Po skončení zapsat report (stdout) do `LATENCY.md` → řádek „SCK 1440p, nativní viewer“.
- [ ] Zopakovat s `--max-w 3840 --max-h 2160` (čekáme ~45 fps kvůli enkodéru; ověřit) a s Wi-Fi (jen pro zajímavost).
- [ ] Odpojit kabel na 5 s a zase zapojit: viewer skončí s chybou (reconnect je až T1.1) — OK pro Fázi 0, jen zapsat chování.

## 3. Flutter spike (H) — T0.6, brána G1
- [ ] `cargo xtask flutter-spike`
- [ ] Spustit: `RZ_SPIKE_ADDR=<IP-A>:47470 RZ_SPIKE_MAX=2560x1440 RZ_SPIKE_AUTOSTART=1 RZ_SPIKE_DURATION=60 RZ_SPIKE_QUIT=1 RZ_SPIKE_CSV=$PWD/lan-1440-flutter.csv spikes/flutter_spike/build/macos/Build/Products/Release/flutter_spike.app/Contents/MacOS/flutter_spike`
  (nebo app otevřít normálně, zadat adresu a kliknout Connect / Stop + report).
- [ ] Zapsat report do `LATENCY.md` → „SCK 1440p, Flutter“.

## 4. Glass-to-glass (T0.9, ručně)
- [ ] Na A otevřít `tools/latency/counter.html` v prohlížeči, fullscreen.
- [ ] Na H vedle sebe: nativní viewer a Flutter spike (nebo postupně), oba ukazují A.
- [ ] Telefon v slow-motion (240 fps) natočit monitor A a monitor H naráz, ~10 s. Latence = číslo na A − číslo na H; vzít 10 snímků, medián.
- [ ] Totéž pro **Moonlight + Sunshine** a pro vestavěné **Screen Sharing (High Performance)** na stejné dvojici.
- [ ] Zapsat do `LATENCY.md`. **G1:** Flutter ≤ nativní + 1 frame (16,7 ms @60 Hz)? **G2:** P50 ≤ 25 ms a ≥ 58 fps @1440p?

## 5. Čitelnost textu (G3)
- [ ] Na A otevřít hustý text (Terminal s malým fontem, kód ve VS Code, tabulka). Ve vieweru @1440p při auto bitrate (0,12 bpp ≈ 26 Mb/s) posoudit čitelnost.
- [ ] Zkusit `--bitrate-kbps 15000` a `40000` na agentovi. Zapsat verdikt: H.264 4:2:0 přijatelné → HEVC/4:4:4 zůstává ve Fázi 6.

## 6. Průzkumy
- [ ] T0.7: `docs/probes/macos-window-capture.md` — vyplnit tabulku S1–S10.
- [ ] T0.8: `docs/probes/macos-permissions.md` — P1–P6, P9 hned; P7 (měsíční dotaz) průběžně. Rozhodnout o žádosti `persistent-content-capture`.

## 7. Rozhodnutí před Fází 1
- [ ] G1 → hub ve Flutteru, nebo nativní renderer (přepsat kap. 9).
- [ ] G2 → frame-per-stream stačí, nebo datagramový režim.
- [ ] G3 → HEVC/4:4:4 ve Fázi 6, nebo předsunout.
- [ ] 4K FOCUS: strop 1440p@60 / 4K@45 / test HEVC (DECISIONS.md, zjištění 1).
- [ ] Developer ID certifikát pro notarizaci (T1.9).
- [ ] Otevřené body O1 (název), O3, O4, O6.

## Zjištění z dogfoodu pro Fázi 1 (2026-11-22, MacBook + M2 přes iPhone hotspot)
- **Zamčená/uspaná obrazovka agenta:** SCShareableContent nevrací displej → `Subscribe` selže „display not found“. Agent má hlásit stav `locked/asleep` (ne chybu) a hub ho ukázat na dlaždici; po odemčení se sám znovu přihlásit k odběru.
- **Flutter spike po chybě Subscribe nekončil** (čekal na RZ_SPIKE_DURATION). Opraveno ve spiku (skončí po chybě, ověřeno: nedostupná adresa → konec za 11 s místo 60). Ve Fázi 1 musí chyby dojít do UI.
- **Decode při shlucích po Wi-Fi:** dekódovat vše, zobrazit jen poslední frame (LATENCY.md, hotspot).
- **SCK `minimumFrameInterval`:** nikdy ne ≈1/fps; 1/(2×fps) + throttle v agentovi (LATENCY.md, experiment).
- **Statická plocha = skoro žádné framy** — na idle spojení spoléhat na keepalive; měřicí scény musí mít pohyb.
