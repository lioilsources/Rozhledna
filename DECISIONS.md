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
