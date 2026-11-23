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
- Flutter spike: v automatickém režimu skončí hned po chybě spojení/odběru (dřív čekal na RZ_SPIKE_DURATION).
- Flutter spike: report se flushne na stdout; dogfood příkaz má RZ_SPIKE_QUIT=1.
- `cargo xtask bundle`: nepodepisuje revokovaným certifikátem. Přeskočí identity s `CSSMERR_` a po podpisu ověří `spctl`. Při `CSSMERR_TP_CERT_REVOKED` zkusí další identitu, a když jsou revokované všechny, bundle smaže (jinak ho Gatekeeper označí za malware a přesune do Koše).
- `vt-bench` na Apple M2 (DECISIONS.md): 4K60 H.264 LL nezvládne ani M2 (~51 fps).

## Unreleased — Fáze 1 (MVP zeď obrazovek)

### T1.1 rz-net: identita, párování, discovery
- `identity`: trvalý Ed25519 klíč + self-signed cert v `~/Library/Application Support/Rozhledna/<role>/` (soubory 0600, adresář 0700; `RZ_STATE_DIR` přepíše), otisk SHA-256, `node_id` = prvních 16 B otisku.
- `trust`: trust store `trusted.json` (0600), oprávnění hubu na agentovi (výchozí vše, D9).
- `tls`: vzájemné TLS 1.3 (`server_crypto_mtls`, `client_crypto(Some(id))`), verifiery pouští libovolný cert, ale kontrolují podpis handshaku.
- `pair`: SAS 6 číslic z otisků + TLS exporteru `rozhledna-pair`.
- `handshake`: Hello → HelloAck → (PairRequest → PairResult) na control streamu; `AgentUi`/`HubUi` traity; případ „agent hub zná, hub agenta ne“ potvrdí uživatel hubu.
- Protokol: `HelloAck.agent` (Hello agenta — jméno, OS, kodeky; posílá se i nespárovanému hubu).
- `discovery`: mDNS `_rozhledna._udp.local.` (mdns-sd), TXT `id name os v fp roles`, jen LAN adresy (`lan::is_lan`); příklad `mdns`.
- `backoff`: 250 ms ×2 → 10 s, ±20 % jitter (reconnect smyčka přijde se SessionManagerem v T1.5).
- Testy: loopback párování (první kontakt + shodný SAS, reconnect bez dotazu, odmítnutí, hub zapomněl agenta, klient bez certifikátu odmítnut).

### T1.2 rz-agent
- Nový crate `rz-agent` (lib + bin): mTLS server jen pro LAN adresy, handshake/párování, mDNS, `SourceRegistry` (displeje přes SCK, nebo N testpattern displejů), libovolně streamů na hub (`Subscribe` / `UpdateSubscription` = restart streamu pod stejným id / `Unsubscribe` / `RequestKeyframe`), backpressure z Fáze 0, `StreamStats`.
- Protokol: `AgentStatus` (Screen Recording, Accessibility, zamčená obrazovka, spící displej) po handshaku a při každé změně; `Error.code` (`rz_agent::err`): zamčeno, bez oprávnění `view`, neznámý zdroj, chyba capture, neznámý stream.
- Zamčená obrazovka: `Subscribe` vrátí `SCREEN_LOCKED` místo nejasného „display not found“ (zjištění z dogfoodu).
- Během streamování drží `IOPMAssertion` PreventUserIdleDisplaySleep (plán kap. 8).
- Throttle na mřížce fps s tolerancí ¼ intervalu (`pipeline::Throttle`, testy 120 Hz → 60, jitter, pauza).
- Menu bar (`tray-icon` + `tao`): stav oprávnění (klik = požádat / otevřít Nastavení), zamčeno, připojené huby a počet sledovaných obrazovek, „Odpojit vše“, spárované huby se „Zapomenout“, Ukončit. Párování = NSAlert s kódem. `--headless`: potvrzení v terminálu, `--trust-hub sha256:…`.
- `cargo xtask bundle rz-agent` → `dist/Rozhledna Agent.app` (`dev.ol1n.rozhledna.agent`, LSUIElement).
- Test loopback: párování, 2 zdroje, 2 streamy naráz, RequestKeyframe, Unsubscribe, neznámý zdroj.

### T1.8 rzctl (základ)
- `rzctl discover | pair | sources | stats | trusted | forget | fingerprint` s identitou hubu (`~/Library/Application Support/Rozhledna/hub`).

### T1.3 rz-input
- Nový crate `rz-input`: HID → `CGKeyCode` tabulka (ANSI + ISO + JIS, inverzní k Flutter `PhysicalKeyboardKey`), `InputState` per hub (stisknuté klávesy a tlačítka, modifikátory včetně stran, drag, `release_all` = nejdřív klávesy, pak modifikátory), `SourceRect` mapování 0..1 → globální body (nikdy ne na sousední displej), `NullInjector`.
- `macos::CgEventInjector`: `CGEventPost(kCGHIDEventTap)`, event source s potlačením lokálního vstupu 0 s, modifikátory jako `FlagsChanged`, drag eventy, click count, scroll řádky / pixely s fází, text přes `CGEventKeyboardSetUnicodeString`.
- Agent: vstupní uni streamy (`u32 LE` + `InputEvent`) a datagramy `PointerMotion` (starší seq se zahodí), jen s oprávněním `control`; hlídač: > 1 s ticha s něčím stisknutým → uvolnit; při odpojení uvolnit vše. `--inject cgevent|null` (výchozí cgevent pro sck, null pro testpattern).
- Protokol: zdokumentované konvence `Key` / `Button` / `Scroll` (znaménka, jednotky, fáze).
- `rzctl poke`: test injekce (kruh kurzorem, psaní přes HID i Unicode).
- Testy: tabulka HID, modifikátory a release_all, mapování a drag, loopback vstup (klávesy, klik, datagramy, uvolnění při odpojení, uvolnění po tichu).

### T1.4 Kurzor
- `rz_agent::cursor`: vlákno 60 Hz pozice (`CGEventGetLocation`), 30 Hz tvar (`NSCursor.currentSystemCursor`, hash TIFF dat). Standardní kurzory AppKitu → `kind` (arrow, ibeam, pointer, crosshair, grab, grabbing, resize-h/v, not-allowed, copy), ostatní jako PNG + hotspot v pixelech.
- Agent posílá hubu `CursorShape` (control, jednou za tvar) a `CursorPos` datagram pro každý sledovaný zdroj při změně a 1× za sekundu (normalizovaně ke zdroji, `visible` = uvnitř).
- `rzctl stats` vypisuje tvar a pozici kurzoru; příklad `cursor-probe` (experiment).

### T1.5 rz-hub-core + rz-hub-ffi
- `rz-hub-core::Hub`: agenti (adresa nebo mDNS), dlaždice (`subscribe` / `update_tile` / `unsubscribe`), vstup (`input`, `pointer`, `release_all`), poslední kurzor na dlaždici, odpovědi na párovací dotazy, události (`Event`, JSON).
- Task na agenta: connect (nastavená adresa, pak adresy z mDNS podle otisku) → handshake/párování → `ListSources` → přihlášení všech jeho dlaždic → video / control / datagramy; při ztrátě reconnect s backoffem, dlaždice zůstávají a po připojení se znovu přihlásí. Po odemčení obrazovky (`AgentStatus`) se zopakují neúspěšné odběry.
- Ping každých 500 ms (clock sync + keep-alive pro držené klávesy, DECISIONS T1.3).
- Dekódovací vlákno na dlaždici (přežije reconnect): reorder z Fáze 0 + `floor` / `waiting_for_keyframe` → příjem `stop()`ne streamy, které už nejdou použít (plán 6.4); RequestKeyframe max 1× / 250 ms; statistiky 1× / s (fps, bitrate, latence capture→decode P50/P95, přeskočené, agent fps/drop).
- Vstupní uni stream s prioritou 10 (klik nečeká za keyframem), pohyb myši datagramy se seq.
- `rz-net::video::recv_header` / `recv_payload`, `ControlSender::next_seq`.
- `rz-hub-ffi` (staticlib, C ABI, `include/rz_hub.h`): události jako JSON, nejnovější `CVPixelBuffer` na dlaždici + callback, vstup, kurzor, párování.
- Test e2e hub-core ↔ rz-agent (testpattern): párování, 2 dlaždice s ověřenými čísly framů, vstup na správný displej, restart agenta → automatický reconnect bez nového párování a pokračující framy.

### T1.6 Hub UI (Flutter)
- `hub/` (Flutter macOS, bundle `dev.ol1n.rozhledna.hub`, „Rozhledna.app“): Swift plugin nad `rz-hub-ffi` (method channel + event channel s JSON událostmi + textura na dlaždici).
- Panel strojů: přidané stroje se stavem (oprávnění, zamčeno, chyba), nalezené přes mDNS s „Přidat“, ruční adresa; seznam se ukládá (`agents.json`) a po startu obnoví.
- Párovací dialog s kódem (zavře se sám po potvrzení na agentovi), dialog „neznámý stroj“ s odpovědí do jádra.
- Layout `grid` jako čistá funkce (nejlepší nejmenší škála; testy), letterbox, overlay (stroj, displej, rozlišení, fps, Mb/s, P50), stavové hlášky na dlaždici.
- Vstup bez grabu: pohyb myši (datagramy), tlačítka s počtem kliků, scroll kolečkem (pixely) a trackpadem (fáze), klávesy jako HID usage do fokusované dlaždice, `release_all` při ztrátě fokusu; tvar kurzoru podle agenta.
- TierController (zjednodušený): stream = fyzické pixely dlaždice, fokus ≤ 2560×1440 @ 60, ostatní ≤ 1920×1080 @ 30, debounce 150 ms, změna < 10 % se ignoruje.
- `cargo xtask hub` → `dist/Rozhledna.app` (podepsaný `--deep`, stejná kontrola revokace jako agent). Dogfood checklist `docs/dogfood/phase-1.md`.
- Ověřeno na MacBooku proti agentovi s testpatternem: párování, 2 živé dlaždice 30 fps, latence capture→decode P50 ~10 ms.

### T1.7 Key grab (macOS)
- Hub: `KeyGrab` (Swift) — `CGEventTap` na session úrovni (head insert) polyká všechny klávesy včetně ⌘Tab, ⌘Space, ⌘Q a posílá je přímo do Rustu (`rz_hub_key_mac`, bez skoku přes Dart); modifikátory z `flagsChanged`, Caps Lock jako stisk; tap se po timeoutu znovu zapne.
- Start: ⌃⌥↩ ve fokusované dlaždici nebo ikona klávesnice; konec: ⌃⌥⎋, přepnutí na jinou aplikaci, fokus jiné dlaždice. Při konci `release_all`. Grab je vidět (oranžový rámeček + pruh s nápovědou).
- Bez Zpřístupnění pro hub tap nevznikne → hláška s návodem.
- `rz_input::hid::from_mac` (inverzní tabulka, test round-trip); `xtask hub` staví staticlib s `MACOSX_DEPLOYMENT_TARGET=12.0` (bez varování linkeru).

### T1.9 Podpis a notarizace
- `cargo xtask notarize <app>`: ověří podpis Developer ID, zip → `notarytool submit --wait` (profil `RZ_NOTARY_PROFILE`) → `stapler staple` → `spctl`. Bez Developer ID skončí srozumitelnou chybou.
- Podpis Developer ID se dělá s bezpečným časovým razítkem (`--timestamp`, nutné pro notarizaci), Apple Development dál bez (rychlé lokální buildy).
- README: rychlý start Fáze 1.

### Zpevnění Fáze 1
- **Oprava:** nová epocha streamu (reconnect, změna velikosti dlaždice) se zasekla na „floor“ staré epochy — framy 1, 2… nové epochy se zahodily, než se zpracoval frame 0, a hub si řekl o keyframe. Reorder teď rozlišuje epochy podle `capture_ts` (pozdní framy staré epochy pryč, předčasné framy nové epochy se ponechají), `Tile::wants` nezahodí frame novější, než co bylo zobrazeno. Test e2e: po reconnectu i po změně velikosti 0 žádostí o keyframe, 0 přeskočených.
- **Oprava:** ukončení agenta (menu) i hubu (⌘Q) neposlalo CONNECTION_CLOSE → druhá strana si všimla až po 10 s idle timeoutu. Agent i hub teď před koncem počkají, až se close odešle; hub uvolní jádro při `willTerminate` (vlákno událostí skončí dřív, než se jádro uvolní).
- Test C ABI (`rz-hub-ffi/tests/ffi.rs`) proti agentovi: JSON události, callback na frame, `take_frame` (CVPixelBuffer správné šířky), `key_mac`, pointer, free.
- **Oprava (nález z M2):** `rzctl poke` hlásil „hotovo, napsáno…“, i když agent neměl Zpřístupnění a vstup zahodil. Teď počká na `AgentStatus` a bez Zpřístupnění skončí chybou s návodem.
- Agent v menu baru (bez `--headless`) loguje do `~/Library/Logs/Rozhledna/agent.log`, pokud není dané `--log-file` (log na externím svazku spouští TCC dialog „removable volume“, agent do odpovědi visí).
- `rzctl stats` vypisuje i P95. `rzctl poke --delay N` (výchozí 3 s): čas přepnout na agentovi do cílového okna (poke píše do aktivního okna).
- **Oprava (dogfood):** hub po prvním spuštění neviděl agenty v síti. macOS zahazuje multicast, dokud uživatel nepovolí Local Network, a mdns-sd opakuje dotaz s intervalem zdvojnásobovaným až na 1 h. Discovery teď restartuje browse po 3, 10, 30 a 60 s, pak každou minutu.
- **Oprava (dogfood):** po krátké změně obrazovky (přepnutí okna, stisk klávesy) mohl hub zůstat na předchozím stavu, protože throttle zahodil poslední snímek a SCK další neposlal. Agent teď poslední zahozený snímek odešle, jakmile na něj přijde řada.
- **Oprava (dogfood, hotspot):** dlaždice natrvalo zamrzla, když se ztratil vyžádaný keyframe. Hub během čekání zahazuje P-framy nepřečtené, takže reorder nic neviděl a o keyframe už znovu nepožádal. Hub teď žádá znovu každých 500 ms a statistiky dlaždice chodí i během čekání. Agent na žádost o keyframe při statické obrazovce znovu zakóduje poslední snímek jako IDR, protože SCK bez změny nic nepošle.
