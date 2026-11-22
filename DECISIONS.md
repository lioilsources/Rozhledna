# DECISIONS.md

Záznamy rozhodnutí a výsledků experimentů (plán kap. 0, bod 2). Formát: datum — co se měřilo/rozhodovalo — závěr.
Stroj pro všechna měření z 2026-11-22: MacBook Pro M1 Pro, 32 GB, macOS 27.0.1 (26A434), Xcode 27.2, Rust 1.100.0.

---

## 2026-11-22 — Toolchain a verze závislostí (plán kap. 0, bod 3)

- Rust 1.100.0 přes Homebrew `rustup` (`/opt/homebrew/opt/rustup/bin`, bez úprav shell profilu). Flutter 3.47.9 stable.
- Verze ověřené na crates.io API ve stejný den (všechny aktivně udržované, poslední release ≤ 12 měsíců, kromě `tracing` 12/2025):
  `quinn 0.11.12`, `rustls 0.23.46` (provider `ring`), `rcgen 0.14.10`, `prost 0.14.6`, `prost-build 0.14.6`, `protox 0.10.0`,
  `tokio 1.54.1`, `bytes 1.12.2`, `clap 4.6.9`, `anyhow 1.0.104`, `thiserror 2.0.21`, `tracing 0.1.44`, `tracing-subscriber 0.3.23`,
  `objc2 0.6.5`, `block2 0.6.2`, `dispatch2 0.3.1`, `objc2-* framework crates 0.3.2`.
- **Protobuf bez `protoc`:** `cargo xtask gen-proto` = `protox` (čistý Rust parser) + `prost-build::compile_fds`. Vygenerovaný kód je v gitu (`crates/rz-proto/src/gen/`), build nepotřebuje žádný systémový nástroj.

## 2026-11-22 — macOS bindingy: `objc2-*` framework crates, ne crate `screencapturekit` (plán kap. 8, OVĚŘIT)

- Ověřeno: `objc2-screen-capture-kit`, `objc2-video-toolbox`, `objc2-core-media`, `objc2-core-video` 0.3.2 pokrývají vše potřebné pro Fázi 0
  (SCStream + `SCStreamOutput` přes `define_class!`, `SCContentFilter` display i `desktopIndependentWindow`, `includeChildWindows`,
  VTCompression/Decompression, CMBlockBuffer/CMSampleBuffer, H.264 parameter sets).
- **Závěr:** používáme `objc2-*`. Důvod: stejné CF typy (`CFRetained<CVPixelBuffer>`) od SCK až po VideoToolbox → zero-copy bez přetypovávání raw pointerů mezi dvěma ekosystémy. `screencapturekit` 12.0.0 (vydán 2026-11-21) je použitelná alternativa, ale vlastní wrappery by si vynutily interop přes raw pointery.
- Známá díra: feature `objc2-core-video/objc2-io-surface` se v 0.3.2 nepřeloží (chybí `Type` impl pro `IOSurfaceRef`). Obcházíme vlastní `extern "C" CVPixelBufferGetIOSurface` (spike-viewer). Podobně pár CF accessorů pro attachments (`CFArrayGetValueAtIndex`, `CFDictionaryGetValue`) je deklarováno ručně.

## 2026-11-22 — Hodiny

- `rz_proto::clock::now_ns()` na macOS = `mach_absolute_time()` převedené na ns. Je to stejná doména jako `SCStreamFrameInfoDisplayTime` a CoreMedia host time, takže capture timestamp ze SCK jde přímo do hlavičky framu.
- Offset hub↔agent: Ping/Pong (burst 5× po 50 ms, pak každé 2 s), offset ze vzorku s minimálním RTT z posledních 32. Na loopbacku min RTT 0,23–0,30 ms.

## 2026-11-22 — VideoToolbox H.264: parametry a měření (T0.2)

Konfigurace: `EnableLowLatencyRateControl=true` + `RequireHardwareAcceleratedVideoEncoder=true` (encoder specification), `RealTime=true`,
`AllowFrameReordering=false`, `ProfileLevel=H264_High_AutoLevel`, `MaxKeyFrameInterval=i32::MAX`, `MaxKeyFrameIntervalDuration=0`,
`ExpectedFrameRate`, `AverageBitRate` (0,12 bpp při 60 fps). `PrioritizeEncodingSpeedOverQuality` v low-latency režimu vrací
`-12900` (kVTPropertyNotSupportedErr) → nenastavujeme. Výstup AVCC → Annex B, SPS/PPS in-band u každého IDR. Dekodér: BGRA IOSurface, Metal compatible.

**Experiment přepínačů** (1080p a 4K, 60 fps, 240 framů, encode P50 / P95 ms):

| varianta | 1080p | 4K |
|---|---|---|
| výchozí (LL + RealTime) | 8,4 / 14,8 | 20,3 / 25,2 |
| bez low-latency rate control | 9,6 / 24,6 | **125,7 / 129,5** |
| `MaxFrameDelayCount=0` | 9,1 / 16,4 | 20,9 / 27,9 |
| bez `RealTime` / `RealTime=false` | ≈ výchozí | ≈ výchozí |

**Závěr:** low-latency rate control je nutný; ostatní přepínače nic nemění. Zůstává výchozí konfigurace.

**Benchmark** (`cargo run --release -p rz-codec --example vt-bench`), 60 fps, 300 framů, real-time tempo, testpattern:

| rozlišení | encode P50 / P95 | decode P50 / P95 | max. propustnost enkodéru |
|---|---|---|---|
| 1920×1080 | 7,5 / 9,4 ms | 2,1 / 2,5 ms | ≥ 120 fps |
| 2560×1440 | 9,5 / 10,1 ms | 3,1 / 3,8 ms | ~100 fps |
| 3840×2160 | 19,0 / 20,2 ms | 5,5 / 7,0 ms | **~51 fps** |

Se šumem přes 25 % plochy (`--noise 0.25`, realističtější bitrate): encode stejně, decode 3,2 / 5,5 / 9,5 ms (1080p / 1440p / 4K).

**Zjištění, která mění návrh:**
1. **4K60 H.264 v low-latency režimu M1 Pro nezvládne** — enkodér zpracovává framy sériově (~19–22 ms/frame → ~45–51 fps). Pro FOCUS dlaždici z 4K/5K displeje je potřeba buď strop 1440p@60 (bude ostřejší díky `resize-remote`/`native` později), nebo 4K@≤45 fps, nebo HEVC. **Otevřené: změřit HEVC low-latency na M1 Pro (D4 říká až Fáze 6 — doporučuji krátký test před Fází 1) a výsledek na M2/M3/M4, pokud je k dispozici.** Výchozí strop agenta pro Fázi 1 navrhuji 2560×1440@60.
2. **LL rate control při nedostatečném bitrate zahazuje framy** (se šumem 172/300 zakódováno). Proto čísla framů na drátě přiděluje agent až **po** enkódování (souvislá řada); enkodér hlásí `EncoderOutput::Dropped`. Jinak by hub viděl díry a zbytečně žádal keyframe.
3. Testpattern je pro enkodér extrémně snadný (0,3–0,8 Mb/s místo cílových 15–60). Bitrate a čitelnost textu (G3) se musí měřit na reálné ploše.

**Benchmark na Apple M2** (Mac mini, macOS 27.0.1, rustc 1.100.0), stejný příkaz, 60 fps, 300 framů:

| rozlišení | encode P50 / P95 | decode P50 / P95 | enc fps (testpattern) | se šumem 0,25: encode P50 / P95, decode P50 / P95, zakódováno |
|---|---|---|---|---|
| 1920×1080 | 7,5 / 8,3 ms | 2,7 / 3,3 ms | 60,0 | 8,2 / 9,6 ms, 3,0 / 4,1 ms, 170/300 |
| 2560×1440 | 10,0 / 11,4 ms | 4,1 / 4,6 ms | 60,1 | 10,4 / 11,1 ms, 6,2 / 7,2 ms, 169/300 |
| 3840×2160 | 19,6 / 20,8 ms | 8,3 / 9,3 ms | **50,7** | 20,2 / 21,4 ms, 11,5 / 14,0 ms, 164/300 |

**Závěr M2:** enkodér je stejně rychlý jako M1 Pro (stejný media engine), **4K@60 H.264 LL nezvládne ani M2** (~51 fps). Decode je na M2 o 30–50 % pomalejší (méně GPU jader), na hubu to nevadí, hub je MacBook. Výchozí strop 2560×1440@60 platí i pro M2. Se šumem LL rate control znovu zahazuje ~45 % framů při všech rozlišeních.

## 2026-11-22 — Backpressure na agentovi (plán 6.4, upřesnění)

Plán: „> 2 framy neodeslané → drop před enkódováním + bitrate −20 %“. Na 4K je ale v enkodéru běžně ~2 framy (encode > 1 frame interval), takže původní implementace (počítala i framy v enkodéru) chybně škrtila bitrate.
**Závěr:** dva čítače — `unsent` (zakódované, ještě nepředané QUIC) > 2 → drop + −20 % bitrate (síťový backpressure dle plánu); `in_encoder` > 3 → jen drop bez změny bitrate (přetížení enkodéru). Návrat +5 %/s po 1 s klidu.

## 2026-11-22 — Hub: řazení a ztráty (plán 6.4)

- `Reorder` (spike-viewer) vydává framy striktně podle `frame_no`; čekání na chybějící `max(2 × interval, 40 ms)` nebo > 3 ve frontě → skok na keyframe ve frontě, jinak `RequestKeyframe` a zahazování P-framů do keyframe. Žádosti o keyframe rate-limit 250 ms (4K IDR ~1 MB potřebuje čas na enkódování a přenos).
- `stop()` rozpracovaných streamů starších než příští keyframe zatím není (Fáze 1, T1.5).

## 2026-11-22 — Podpis a bundle id (plán kap. 8)

- V keychainu jsou jen identity **Apple Development** (2×) a **Apple Distribution**, chybí **Developer ID Application**. Pro lokální TCC stačí Apple Development (stabilní podpis → oprávnění přežijí rebuild — ověřit v T0.8). Pro notarizaci a distribuci (T1.9) je potřeba Developer ID — Ol1n vytvoří v developer portálu.
- `cargo xtask bundle` bere `$RZ_SIGN_IDENTITY`, jinak první „Developer ID Application“, jinak první „Apple Development“. Podepisuje s hardened runtime.
- Bundle id: `dev.ol1n.rozhledna.spike-agent`, `dev.ol1n.rozhledna.spike-viewer` (produkt později `dev.ol1n.rozhledna.agent` / `.hub`, až padne O1 — název).
- Pozor: spuštěno z terminálu přiřadí macOS oprávnění terminálu (responsible process), ne binárce. Pro vlastní TCC identitu spouštět bundle přes `open`.

## 2026-11-22 — Flutter spike (T0.6)

- Rust `staticlib` (`rz-spike-ffi`) s C ABI, Swift ji volá přes `@_silgen_name` (bez bridging headeru); plugin je v `MainFlutterWindow.swift`, takže vygenerovaný Xcode projekt není třeba upravovat. Linkování přes `macos/Runner/Configs/Rust.xcconfig`.
- Jen `arm64` (macOS 27 je Apple-silicon-only). App Sandbox ve spiku vypnutý (CSV kamkoli, bez síťových entitlementů).
- `textureFrameAvailable` se volá přímo z dekódovacího vlákna (embedder `MarkExternalTextureFrameAvailable` je thread-safe) — bez skoku na main thread.
- `present_ns` ve Flutteru = okamžik, kdy si raster thread vyzvedne buffer v `copyPixelBuffer`; v nativním vieweru = po `CATransaction.commit`. Ani jedno nezahrnuje kompozici WindowServeru → **G1 rozhodne až glass-to-glass měření (T0.9 metoda)**.

## 2026-11-22 — Uzavření Fáze 0 (Ol1n: „ok pokračuj“)

| bod | rozhodnutí | podklad |
|---|---|---|
| G1 | **Hub ve Flutteru.** Flutter +12,4 ms P50 proti nativnímu (limit 16,7 ms). Pozor: zakryté okno Flutteru padá na 30 fps (macOS throttluje vsync); hub je fullscreen, takže to nevadí. | LATENCY.md, M2 lokálně |
| G2 | **Frame-per-stream stačí**, datagramový režim ne. Lokálně total P50 4,0 ms @60 fps; přes hotspot 17 ms P50 / 66 ms P95 (jitter sítě). Ověření na drátu odloženo, drát není. | LATENCY.md |
| G3 | **H.264 4:2:0 je čitelné** (VS Code, 2560×1080, auto bitrate ~20 Mb/s) → HEVC/4:4:4 zůstává ve Fázi 6. | LATENCY.md, G3 |
| 4K FOCUS | **Strop 1440p@60.** Enkodér M1 Pro i M2 dává 4K jen ~51 fps. Hub (TierController) omezí FOCUS na 2560×1440; agent sám limituje 3840×2160 (hub může chtít 4K@30). | výše VideoToolbox, vt-bench M2 |
| SCK interval | `minimumFrameInterval` = 1/(2×fps) + vlastní throttle v agentovi (≥ ¾ intervalu). Přesně 1/fps ztrácí framy (57 fps), 1/66 je ještě horší (55). | LATENCY.md, experiment |
| Odloženo | glass-to-glass kamerou (T0.9), test 120 Hz displeje, drátová LAN, Developer ID (→ T1.9). Nic z toho nemění architekturu. | docs/dogfood/phase-0.md |
| O4 | **Open-source od prvního dne:** repo je veřejné (github.com/lioilsources/Rozhledna), MIT OR Apache-2.0. | |

## 2026-11-22 — T1.1: identita a párování

- **Klíč v souboru (0600), ne v Keychainu.** Plán 6.3 dovoluje obojí. Keychain u nenotarizovaného bundlu s měnícím se podpisem při každém rebuildu ukazuje dialog „povolit přístup“, což by blokovalo bezobslužný agent. Soubor funguje stejně na Linuxu a Windows. Adresář `~/Library/Application Support/Rozhledna/<role>/` (0700). Keychain lze přidat později za `trait`.
- **`node_id` = prvních 16 B SHA-256 otisku certifikátu** místo samostatného UUID: nemůže se rozejít s klíčem; při nové identitě vznikne nový uzel, což je správně.
- **Vzájemné TLS:** agent vyžaduje klientský certifikát (`server_crypto_mtls`), obě strany akceptují jakýkoli certifikát, ale kontrolují podpis TLS 1.3 handshaku → druhá strana prokazatelně drží klíč k otisku. Důvěra se řeší až v `handshake` podle trust store.
- **SAS** podle plánu: 6 číslic z `SHA-256(min(fp)‖max(fp)‖exporter("rozhledna-pair", 32 B))`; `u32 BE % 10⁶` (zanedbatelná modulo odchylka).
- **`HelloAck.agent`** (nové pole): agent posílá svůj `Hello` i nespárovanému hubu, aby hub ukázal jméno stroje v párovacím dialogu.
- **Agent zná hub, hub agenta ne** (přeinstalovaný hub): agent se znovu neptá, hub se zeptá svého uživatele (`HubUi::confirm_agent`). Útočník bez klíče agenta se takhle neprotlačí, protože hub ověřuje podpis TLS.
- mDNS přes `mdns-sd` 0.22 (bez async runtime, vlastní vlákno). Instance `<name>-<fp8>`, aby dva stroje se stejným hostname nekolidovaly. Hub vidí jen LAN adresy (`lan::is_lan`: RFC 1918, link-local, loopback, CGNAT 100.64/10, ULA).
- Ověřeno: crates na crates.io 2026-11-22 — `mdns-sd 0.22.0`, `sha2 0.11.1`, `hex 0.4.3`, `serde 1.0.229`, `serde_json 1.0.154`, `tray-icon 0.26.1`, `tao 0.37.3`.

## 2026-11-22 — T1.3: vstup

- **Keep-alive pro držené klávesy:** agent uvolní vše, co hub drží, po **1 s** bez jakékoli zprávy od hubu (control, vstup, datagram). QUIC keep-alive se nepočítá (aplikace ho nevidí). Hub proto musí, dokud něco drží, poslat aspoň každých 500 ms `Ping` (hub-core T1.5).
- **ISO klávesy:** tabulka je inverzní k mapování Flutteru (`kVK_ISO_Section` ↔ HID 0x64, `kVK_ANSI_Grave` ↔ 0x35), takže Mac → Mac sedí bez ohledu na to, jak Apple ISO klávesnice prohazuje ty dvě klávesy na úrovni HID. Na Linux/Windows hubu ověřit ve Fázi 4/5.
- **Potlačení lokálního vstupu 0 s** (`CGEventSourceSetLocalEventsSuppressionInterval`): výchozích 250 ms by zablokovalo myš člověka u agenta po každém syntetickém eventu. Kolize lokálního a vzdáleného vstupu řeší až `LocalActivity` (plán 6.7).
- **Modifikátory jako `kCGEventFlagsChanged`** s plnými příznaky včetně levé/pravé strany. Každý event nese příznaky podle stavu tohoto hubu, ne podle globálního stavu systému.
- **Scroll:** kladné `dy` = kolečko od uživatele (CG konvence), `precise` = pixely + `NSEventPhase`. Převod z Flutteru (`scrollDelta.dy` kladné = dolů) dělá hub.
- **Bez Accessibility** macOS eventy tiše zahodí. Agent to hlásí v `AgentStatus.accessibility` a v menu baru.

## 2026-11-22 — T1.4: kurzor (OVĚŘENO experimentem)

- `+[NSCursor currentSystemCursor]` je v SDK deprecated („v budoucí verzi vždy nil“), ale **na macOS 27.0.1 funguje**: `cursor-probe` vrátil systémový kurzor 28×40 pt, hotspot (5,5), a jeho hash TIFF dat se shoduje s `NSCursor.arrowCursor`. Poznávání standardních tvarů podle hashe tedy funguje.
- Bez `NSApplication` vrací `arrowCursor` NULL (objc2 panikaří), proto agent vytváří `NSApplication.sharedApplication` v `main` i v headless režimu a tracker má `catch_unwind`.
- Když `currentSystemCursor` jednou začne vracet nil, tracker to zaloguje a posílá jen pozici (hub ukáže šipku). Náhrada: sledovat kurzor z ScreenCaptureKit (`showsCursor=true` do druhého, malého streamu) nebo privátní `CGSCopyRegisteredCursorImages`. Zatím se nedělá.
- Ověřit ručně na M2: tvar se mění v jiných aplikacích (I-beam nad textem, ruka nad odkazem).

## 2026-11-22 — T1.5: hub-core a FFI

- **FFI = ruční C ABI + JSON události, ne `flutter_rust_bridge`** (D1 ho uvádí). Důvody: textury stejně potřebují nativní Swift plugin (`FlutterTexture`), který C ABI volá přímo (ověřeno ve Fázi 0, T0.6); API je malé (~20 funkcí) a události jsou jeden JSON kanál; FRB by přidal codegen nástroj s vlastní verzí a build krok. Pokud API naroste (Fáze 2+), přechod na FRB je mechanický — `Hub` je čisté Rust API.
- **Dekódovací vlákno na dlaždici** a nejnovější frame v jednom slotu: při shluku framů (Wi-Fi) se dekóduje vše (P-framy na sobě závisí), Flutter si vezme jen poslední (zjištění z dogfoodu).
- **Dlaždice přežívají reconnect:** UI drží id dlaždic; session je po připojení přihlásí znovu, nový stream začíná DISCONTINUITY → reorder se resetuje, dekodér se přestaví podle nových SPS/PPS.
- **Reconnect na novou adresu:** zkouší se nastavená adresa a pak adresy, které mDNS hlásí pro stejný otisk (`fp=` v TXT) — agent, který dostal jinou IP (DHCP, jiná síť), se najde sám.

## 2026-11-22 — T1.7: grab

- **Spouštění grabu ⌃⌥↩, ne holý Enter** (plán 9.6: „Enter na fokusované dlaždici / dvojklik / zkratka“). Enter musí normálně chodit do vzdálené aplikace a dvojklik vybírá slovo, takže zbývá zkratka s hub modifikátorem (výchozí ⌃⌥, plán 9.6) a tlačítko na dlaždici. Uvolnění ⌃⌥⎋ podle plánu.
- **Tap `cgSessionEventTap` + `headInsertEventTap`**, klávesy se posílají ze Swiftu rovnou do Rustu (žádný skok přes Dart = žádná latence UI vlákna). Kód kláves z tapu je `kVK_*`; na HID ho převádí Rust (`from_mac`), takže tabulka je jen jedna.
- **OVĚŘIT ručně:** zda session tap dostane ⌘Tab dřív než Dock na macOS 27 (akceptace Fáze 1). Pokud ne, náhrada je `kCGHIDEventTap` (také jen s Accessibility).
