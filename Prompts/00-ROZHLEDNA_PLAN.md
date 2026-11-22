# ROZHLEDNA_PLAN.md

**Rozhledna** (pracovní název) — hub obrazovek a oken všech počítačů na LAN, s tiling window managementem a plným ovládáním myší/klávesnicí.

- Verze plánu: 2026-11-22, rev. 2 — zapracovány odpovědi: Rust potvrzen; Linux = GNOME Wayland na openSUSE Leap 16.0; stroje nepoužívá nikdo další
- Autor plánu: Claude (research + architektura), implementace: Opus v Claude Code
- Platformy: macOS (must, první), Linux GNOME Wayland — openSUSE Leap 16.0 x86_64 a DGX OS arm64 (druhý), Windows 11 (třetí)
- Pozn.: Projekt Davida Vávry se nepodařilo veřejně dohledat (žádné repo ani příspěvek ve vyhledávání). Plán vychází z popisu funkcí, ne z jeho implementace.

---

## 0. Jak tento dokument číst (instrukce pro Opus)

1. Postupuj po fázích (kap. 11). Každá fáze je samostatně dogfoodovatelná; nezačínej další, dokud Ol1n neodklikne dogfood checklist.
2. Nic nevymýšlej. Kde plán říká **OVĚŘIT**, udělej malý experiment, výsledek zapiš do `DECISIONS.md` (datum, co se měřilo, závěr) a teprve pak pokračuj.
3. Verze crates/balíčků nepinuj z hlavy. Před přidáním závislosti ověř aktuální verzi a stav údržby na crates.io / pub.dev.
4. Žádný bash. Buildovací a balicí úlohy patří do `cargo xtask` (Rust).
5. Claude Code session neumí odkliknout systémová oprávnění (TCC na macOS, portály na Waylandu). Proto existuje zdroj `testpattern` a injektor `null` (kap. 12): celý pipeline musí jít vyvíjet a testovat bez oprávnění. Reálný capture/input testuje Ol1n.
6. Jedna feature = jedna izolovaná změna, kterou jde samostatně vyzkoušet. Po každé feature aktualizuj `CHANGELOG.md` a příslušný dogfood checklist.
7. Kód z GPL/AGPL projektů (Sunshine, Moonlight, RustDesk, TwinDesk, xpra, Lan Mouse, Deskflow) **nekopíruj**. Smíš z nich číst principy. Viz rozhodnutí D2.

---

## 1. Cíl a ne-cíle

### Cíl
Jedna aplikace (**hub**), ve které vidím a ovládám displeje i jednotlivá okna všech svých počítačů v LAN:

- živý obraz v nejvyšší rozumné obnovovací frekvenci, latence na úrovni „jako bych u toho seděl“,
- se vzdálenými plochami a okny zacházím jako s okny: přesun, resize, scroll, stack, tile, přeskládání,
- přepnu do dlaždice a pracuji myší a klávesnicí,
- dock s živými náhledy (inspirace Enlightenment: pager, iconbox, zoom při hoveru),
- workspaces, automatický tiling, více tiling layoutů (inspirace i3/sway/dwm/bspwm/niri).

### Ne-cíle pro v1
- Přístup přes internet (řeší později Tailscale/WireGuard, žádný vlastní relay).
- Hraní her (gamepady, relativní myš) — až jako rozšíření.
- Login obrazovka / pre-boot (to je práce pro hardware KVM typu PiKVM).
- Sledování cizích lidí. Agent vždy viditelně signalizuje, že je sledován (kap. 6.8).

---

## 2. Slovník

| Pojem | Význam |
|---|---|
| **agent** | Proces na sledovaném počítači: výčet zdrojů, capture, enkódování, injekce vstupu, správa oken. |
| **hub** | Prohlížeč a „window manager“ nad vzdálenými zdroji. Stroj může být agent i hub zároveň. |
| **source (zdroj)** | Co lze streamovat: `DISPLAY` (celý monitor/plocha) nebo `WINDOW` (jedno okno). |
| **tile (dlaždice)** | Umístění zdroje v layoutu hubu. |
| **tier** | Kvalitativní úroveň streamu: `PREVIEW` (náhled do docku), `LIVE` (viditelná dlaždice), `FOCUS` (aktivní dlaždice). |
| **workspace** | Pojmenovaná sada dlaždic s vlastním layoutem. |
| **focus / grab** | Focus = kam jde klávesnice. Grab = hub zachytí i systémové zkratky (Cmd+Tab…) a pošle je vzdálenému stroji. |
| **fit mód** | Jak se zdroj vejde do dlaždice: `scale`, `crop`, `native`, `resize-remote`. |

---

## 3. Rozhodnutí

### Přijatá výchozí (lze změnit před Fází 0)

| # | Rozhodnutí | Proč |
|---|---|---|
| D1 | **Rust** pro agenta a sdílené jádro, **Flutter** pro UI hubu (most přes `flutter_rust_bridge` + nativní texture plugin). | Agent je z 80 % lepení OS API (ScreenCaptureKit, VideoToolbox, AX, WinRT Graphics Capture, PipeWire, portály, libei). Rust má pro všechna hotové bindingy (`objc2-*`, oficiální `windows` crate, `pipewire`, `ashpd`, `reis`, `quinn`). Go by znamenalo cgo na všechno, problémy s main-thread afinitou (AX, CFRunLoop) a bolestivý WinRT. Stejnou kombinaci Rust jádro + Flutter UI používá RustDesk. Flutter dává známý UI stack a později tablet/telefon jako hub. **Potvrzeno Ol1nem 2026-11-22.** |
| D2 | Vlastní kód pod **MIT/Apache-2.0**, bez kopírování GPL kódu. FFmpeg jen jako dynamicky linkovaná LGPL knihovna. | Nechává otevřenou cestu k open-source i komerčnímu produktu. |
| D3 | Transport **QUIC** (`quinn`), jedno spojení hub↔agent, video „jeden frame = jeden unidirectional stream“. | Šifrování a multiplexing zdarma, nezávislé streamy pro víc dlaždic, možnost zahodit zastaralé framy. Bez WebRTC složitosti (SDP/ICE/jitter buffer), která na LAN nepřináší užitek. |
| D4 | První kodek **H.264 High** hardwarově; HEVC/AV1 a 4:4:4 až ve Fázi 6. | Dekóduje a enkóduje vše, nejmenší riziko. |
| D5 | Hub nikdy nedekóduje víc pixelů, než kolik zobrazuje („pixel budget“). | Jinak N strojů × 4K × 120 Hz neudekóduje nic. |
| D6 | Kurzor se kreslí lokálně na hubu, ne ve videu. | Ukazatel má nulovou vnímanou latenci. |
| D7 | Klávesy se posílají jako USB HID usage kódy (fyzická klávesa), ne znaky. | Nezávislé na rozložení klávesnice na hubu. |
| D8 | Linux cíl = **GNOME na Waylandu** (openSUSE Leap 16.0 s GNOME 48; DGX OS na Sparku). Primární backend je přímé D-Bus API Mutteru, portál jen jako fallback; správa oken přes vlastní GNOME Shell extension. X11, wlroots a KDE jsou v backlogu. | Leap 16 nabízí v instalátoru jen Waylandové desktopy. Mutter API funguje bez potvrzovacího dialogu (bezobslužný provoz); stejnou cestou jde gnome-remote-desktop, RustDesk i FreeRDP. |
| D9 | Výchozí oprávnění hub→agent: vše zapnuto, bez ask-on-connect. Indikátor sledování a panic zkratka zůstávají. | Všechny stroje používá jen Ol1n. |

### Otevřená (potřebuji od Ol1na)

- **O1 — název** (Rozhledna je pracovní).
- **O2 — vyřešeno** (viz D8).
- **O3 — Windows stroj** pro Fázi 5 (edice, GPU).
- **O4 — open-source od prvního dne, nebo až po MVP?**
- **O5 — vyřešeno** (viz D9).
- **O6 — jakou GPU má stroj s Leapem** (NVIDIA / Intel / AMD)? Určuje hardwarový enkodér, viz T4.3.

---

## 4. Research: co existuje a co z toho plyne

### 4.1 Existující projekty

| Projekt | Co dělá | Co si vzít | Licence |
|---|---|---|---|
| **Sunshine + Moonlight** | Nízkolatenční stream celých displejů; HW enkodéry AMD/Intel/NVIDIA; na macOS jen VideoToolbox a omezená podpora; min. macOS 14.2 / Windows 11. | Měřítko latence (Fáze 0), nastavení enkodérů. Neumí per-window ani metadata oken. | GPL-3.0 |
| **Apollo** | Fork Sunshine pro Windows; při startu streamu vytvoří virtuální displej podle klienta. | Nápad „virtuální displej přesně ve velikosti dlaždice“. | GPL-3.0 |
| **RustDesk / SubnetDesk** | Kompletní remote desktop; Rust; SCK / DXGI / PipeWire+X11. SubnetDesk = LAN-first fork. | Důkaz stacku; reference platformních backendů (jen číst). | AGPL-3.0 |
| **Deskhub** | QUIC+TLS, FEC + retransmise, adaptivní bitrate, C++20 jádro; macOS SCK+VideoToolbox+CGEvent; Linux PipeWire+VA-API/NVENC+uinput; 60 fps default, až 240. Bez discovery, jen per-display. | Nejbližší reference transportní vrstvy. | MIT |
| **TwinDesk** | Electron + nativní helpery (Swift / C# / C); sdílená myš a klávesnice, aplikace z druhého počítače ve vlastních oknech, H.264, TLS, UDP discovery. | Důkaz, že „okna z jiného počítače“ jdou na všech třech OS; rozdělení na helpery. | GPL-3.0 |
| **xpra** | Seamless forwarding jednotlivých X11 oken; správu oken dělá WM klienta. Shadow mód i na Windows/macOS, ne na Waylandu. | Model „vzdálené okno = lokální okno“. | GPL |
| **Lan Mouse / Deskflow / Input Leap** | Softwarové KVM bez videa; Lan Mouse podporuje GNOME ≥ 45, KDE ≥ 6.1, wlroots, Windows, macOS. | Vstup na Waylandu (libei), přechod kurzoru přes hranu. | GPL |
| **Jump Desktop** (komerční) | Remote desktop; nově „Remote Desktop for AI Agents“ s přehledem agentních relací. | Potvrzení, že „zeď agentů“ je reálný trh — a konkurence. | proprietární |

### 4.2 Klíčová zjištění, která formují návrh

- **macOS oprávnění jsou největší provozní riziko.** Od macOS 15 systém periodicky (měsíčně) znovu žádá potvrzení Screen Recording u aplikací, které obcházejí systémový picker — i když používají ScreenCaptureKit. Výjimku dává omezený entitlement `com.apple.developer.persistent-content-capture`, o který se u Applu žádá a který je určen pro aplikace typu VNC/remote desktop. Na macOS 27 je navíc v nastavení samostatná položka „Remote Desktop“, oddělená od „Screen & System Audio Recording“. → Fáze 0 má na to vlastní průzkum (T0.8).
- **macOS 27 (Golden Gate) běží jen na Apple silicon**, takže Intel Macy neřešíme.
- **Wayland nemá přenosný seznam oken ani správu oken.** Capture a vstup jdou přes `xdg-desktop-portal` (ScreenCast + RemoteDesktop/libei, restore tokeny pro přeskočení dialogu), ale výčet a přesouvání oken vyžaduje backend pro každý kompozitor zvlášť (GNOME Shell extension, KWin skript, sway/Hyprland/niri IPC, nebo `ext-foreign-toplevel-list` + `ext-image-copy-capture` na wlroots).
- **GNOME má zkratku mimo portál.** Mutter vystavuje na session busu D-Bus API `org.gnome.Mutter.ScreenCast` a `org.gnome.Mutter.RemoteDesktop`: capture i vstup bez potvrzovacího dialogu, GNOME jen ukáže indikátor sdílení. Stejnou cestou jde gnome-remote-desktop a nově i RustDesk a FreeRDP (s portálem jako fallbackem). `RecordWindow` streamuje jedno okno podle Mutter window id, `RecordVirtual` vytvoří virtuální monitor. API je interní, takže se mezi verzemi GNOME může měnit. Jeden projekt naměřil, že `RecordWindow` vyjedná stream ve velikosti obrazovky s oknem v rohu a že vstup nejde omezit na jedno okno → průzkum T4.0.
- **Seznam a ovládání oken na GNOME** jde jen přes Shell extension s D-Bus rozhraním (existují Window Calls a Window Control). Napíšeme vlastní malou.
- **openSUSE Leap 16.0:** GNOME 48, instalátor nabízí jen Waylandové desktopy (Xorg jde doinstalovat ručně), SELinux v enforcing režimu. Kodeky: oficiální Mesa má H.264/H.265 vypnuté, plné FFmpeg je až v Packmanu, repozitář openh264 je zapnutý rovnou. Hardwarové H.264 proto závisí na GPU (O6): NVIDIA přes NVENC, Intel přes intel-media-driver (OVĚŘIT), AMD přes Mesa jen s Packmanem; záloha je softwarový openh264.
- **Windows:** `Windows.Graphics.Capture` umí capture jednoho okna podle HWND, žlutý rámeček jde vypnout (`IsBorderRequired = false`), na novějších buildech lze omezit frekvenci (`MinUpdateInterval`). Pro celé displeje je nejnižší latence přes DXGI Desktop Duplication. Vstup do elevovaných oken blokuje UIPI (potřeba `uiAccess` nebo SYSTEM proces); UAC prompt běží na secure desktopu, kam smí jen SYSTEM.
- **DGX Spark má hardwarový NVENC i NVDEC** (H.264, H.265, AV1 včetně YUV 4:4:4), takže je plnohodnotný agent. OVĚŘIT ve Fázi 4, že NVENC je dostupný z FFmpeg na aarch64 DGX OS.
- **Virtuální displeje:** Windows přes IddCx (Indirect Display Driver), macOS jen přes nedokumentované třídy `CGVirtualDisplay*` (používají je všechny aplikace třetích stran), Linux přes headless výstupy kompozitoru nebo EDID trik u NVIDIA.
- **Latence:** WebRTC má na LAN změřené dno kolem 40 ms; herní stacky (Parsec, Steam Link, Moonlight) uvádějí na drátové LAN 11–20 ms. Cílem je být blíž té druhé skupině — proto vlastní QUIC transport a žádný jitter buffer.

### 4.3 Možné cesty vývoje

| Cesta | Popis | Čas do „wow“ | Strop | Verdikt |
|---|---|---|---|---|
| **A — Lepidlo** | Sunshine na každém stroji + vlastní multi-session klient nad `moonlight-common-c`. | dny | Jen celé displeje, žádná metadata oken, slabší macOS host, GPL-3 na celý hub. | Dobré demo, slepá ulička pro per-window a dock. Použít jako benchmark. |
| **B — Vlastní agent + hub (doporučeno)** | Rust agent, QUIC, Flutter hub. | 1–2 týdny mac↔mac | Žádný; všechny funkce ze zadání. | **Hlavní cesta tohoto plánu.** |
| **C — WebRTC + webový hub** | Agent posílá WebRTC, hub je Svelte v prohlížeči. | ~týden | Prohlížeč nezachytí systémové zkratky, vyšší latence, méně kontroly nad dekódováním. | Později jako sekundární „web viewer“ (rozšíření). |
| **D — Vestavěné protokoly** | RDP/VNC, které OS už mají, + multi-session klient. | dny | Nízké fps na macOS (VNC), Windows Home nemá RDP host, per-window jen na Windows (RAIL). | Jen jako „source plugin“ pro stroje bez agenta. |
| **E — Fork RustDesk** | Přidat „zeď“ a tiling do RustDesku. | ~týden | Architektura není stavěná na víc streamů a per-window; AGPL; velká kódová báze. | Nedoporučuji; číst jako referenci. |
| **F — Go agent** | Jako B, ale agent v Go (quic-go) + malé nativní helpery (Swift/C#/C) pro capture. | o 30–50 % déle | Stejný jako B, víc procesů a IPC. | Zamítnuto — Rust potvrzen. |

---

## 5. Architektura

```
        AGENT (na každém stroji)                               HUB
┌──────────────────────────────────────┐        ┌──────────────────────────────────────┐
│ SourceRegistry  (displeje, okna)     │        │ Flutter UI                           │
│   └ platform backend: SCK / WGC /    │        │  ├ Dock (živé náhledy, pager)        │
│     PipeWire-portal / X11 / wlr      │        │  ├ Workspaces + Layout engine (Dart) │
│ Capture ──► Scaler(GPU) ──► Encoder  │        │  ├ Tile widget = Texture + overlay   │
│              (VideoToolbox / FFmpeg) │        │  └ Input router (focus, grab)        │
│ Injector  (CGEvent / SendInput /     │  QUIC  │ rz-hub-core (Rust)                   │
│            XTest / libei / uinput)   │◄──────►│  ├ SessionManager (1 spojení/agent)  │
│ WindowControl (AX / Win32 / EWMH /   │  mTLS  │  ├ StreamReceiver ► Decoder ► Texture│
│                kompozitor IPC)       │        │  ├ TierController (pixel budget)     │
│ Clipboard, CursorTracker             │        │  └ Discovery (mDNS), TrustStore      │
│ Tray/menu-bar UI, TrustStore         │        │ Nativní pluginy: texture, key-grab   │
└──────────────────────────────────────┘        └──────────────────────────────────────┘

Kanály v jednom QUIC spojení:
  [bidi stream]  control   — protobuf zprávy (zdroje, subscribe, WM příkazy, clipboard)
  [uni  stream]  input     — spolehlivě: klávesy, tlačítka, scroll, text       hub → agent
  [uni  streamy] video     — 1 stream = 1 frame, s prioritou podle tieru     agent → hub
  [uni  stream]  preview   — JPEG náhledy, nízká priorita                    agent → hub
  [datagramy]    pointer   — pohyb myši (hub → agent), pozice kurzoru (agent → hub)
```

---

## 6. LAN stack po vrstvách

### 6.1 Fyzická síť (doporučení, ne kód)
- Drát (1 GbE stačí; 2.5 GbE komfortní). Wi-Fi 6/6E funguje, ale s vyšším rozptylem latence — plán s ní počítá až ve Fázi 6 (FEC/NACK).
- Orientační bitrate H.264 pro desktop (odhad, upřesní Fáze 0): 1440p60 ≈ 15–40 Mb/s, 4K60 ≈ 40–100 Mb/s. Díky tierům běží naplno jen jedna FOCUS dlaždice.

### 6.2 Discovery
- mDNS/DNS-SD, služba `_rozhledna._udp.local.`, výchozí port **UDP 47470**. Crate `mdns-sd`.
- TXT: `id=<uuid>`, `name=<hostname>`, `os=macos|linux|windows`, `v=<protocol>`, `fp=<prvních 16 hex otisku certifikátu>`, `roles=agent,hub`.
- Ruční přidání `host:port` (kvůli VLANům, Tailscale).
- macOS 15+: Info.plist musí mít `NSLocalNetworkUsageDescription` a `NSBonjourServices`; systém zobrazí dotaz na Local Network. OVĚŘIT chování na 26/27.

### 6.3 Identita a párování
- Každý uzel si při prvním startu vygeneruje Ed25519 klíč a self-signed certifikát (`rcgen`), uloží do OS keychainu / souboru s právy 0600.
- QUIC = TLS 1.3 s oboustrannou autentizací. Vlastní verifier pustí libovolný certifikát, ale aplikační vrstva drží spojení ve stavu `UNPAIRED`, dokud otisk není v trust store.
- Párování: obě strany spočítají **SAS** = 6 číslic z `SHA-256(min(fpA,fpB) || max(fpA,fpB) || TLS exporter(„rozhledna-pair“, 32 B))`. Agent ukáže kód a tlačítko Povolit; hub ukáže stejný kód. Po potvrzení se otisky uloží (TOFU).
- Headless agent: `rzctl pair` vypíše kód do terminálu a čeká na `y`; nebo `trusted_hubs = ["sha256:…"]` v konfigu.
- Oprávnění na pár hub→agent: `view`, `control`, `clipboard`, `window_control`. Výchozí: vše zapnuto (D9). Režim `view` + ask-on-connect zůstává jako volba v nastavení agenta.

### 6.4 Transport
- `quinn`, ALPN `rozhledna/1`. Nastavit: `max_concurrent_uni_streams ≥ 4096`, velká receive okna (stream 8 MB, connection 64 MB), keep-alive 2 s, idle timeout 10 s, datagramy zapnuté, velké počáteční congestion window (LAN).
- **Video frame = unidirectional stream**: 44 B hlavička + payload (kap. 7.3). Priorita streamu: FOCUS > LIVE > preview (`SendStream::set_priority`).
- Hub skládá framy podle `frame_no`. Pokud čeká na frame déle než `max(2 × frame interval, 40 ms)` nebo má ve frontě > 3 framy: pošle `RequestKeyframe`, zavolá `stop()` na rozpracované streamy starší než příští keyframe. Agent po `RequestKeyframe` resetuje neodeslané framy a vynutí IDR.
- Backpressure na agentovi: pokud jsou > 2 framy neodeslané, další frame se **před enkódováním** zahodí a cílový bitrate klesne o 20 %; při klidu roste o 5 %/s zpět k cíli.
- Synchronizace hodin: `Ping/Pong` s časovými razítky každé 2 s, offset z minima RTT (NTP styl). Slouží jen k měření latence.

### 6.5 Kodeky
- Wire formát: **Annex B**; SPS/PPS (VPS) in-band u každého IDR.
- Nízkolatenční profil: bez B-framů, „nekonečný“ GOP, IDR jen na vyžádání nebo při změně rozlišení, real-time režim.
- Abstrakce `trait Encoder` / `trait Decoder` v `rz-codec`:
  - `videotoolbox` (macOS, Fáze 0–3): `VTCompressionSession` s low-latency rate control, `RealTime=true`, `AllowFrameReordering=false`, `ProfileLevel=H264_High_AutoLevel`, vstup NV12 IOSurface přímo z ScreenCaptureKit (zero-copy). Dekodér `VTDecompressionSession`, výstup BGRA IOSurface kompatibilní s Metal.
  - `ffmpeg` (Linux, Windows, Fáze 4–5): `h264_nvenc`, `h264_vaapi`, `h264_amf`, `h264_qsv`; dekódování `vaapi`, `d3d11va`, `nvdec`. Dynamicky linkovaný LGPL build.
- Cílový bitrate: bity-na-pixel heuristika (start 0.12 bpp při 60 fps pro FOCUS, 0.06 pro LIVE), upřesnit ve Fázi 0.

### 6.6 Capture
| OS | Displej | Okno | Poznámky |
|---|---|---|---|
| macOS | `SCStream` + `SCContentFilter(display:excludingWindows:)` | `SCContentFilter(desktopIndependentWindow:)` | `showsCursor=false`, `pixelFormat=420v`, `minimumFrameInterval=1/fps`, `queueDepth=5`; změna velikosti/fps za běhu přes `updateConfiguration`. Framy chodí jen při změně obsahu. Vlastní okna hubu vyloučit z capture (stroj, který je hub i agent). |
| Windows | DXGI Desktop Duplication (primárně), WGC `CreateForMonitor` (fallback) | WGC `CreateForWindow(HWND)` | `IsBorderRequired=false`, `IsCursorCaptureEnabled=false`, `IncludeSecondaryWindows` pro menu/popupy. |
| Linux GNOME Wayland (primární) | Mutter D-Bus `ScreenCast.RecordMonitor` → PipeWire | `ScreenCast.RecordWindow` (window id z vlastní Shell extension) | Bez dialogu; GNOME ukazuje indikátor sdílení. Detaily T4.0–T4.1. |
| Linux X11 (backlog) | XShm + XDamage | XComposite `NameWindowPixmap` | Leap 16 je Wayland-only, zatím se nepíše. |
| Linux Wayland ostatní (fallback) | portál ScreenCast → PipeWire (DMA-BUF) s restore tokenem | portál s window source; na wlroots `ext-image-copy-capture` + `ext-foreign-toplevel-list` (backlog) | Dialog jen jednou, pak token. |

### 6.7 Vstup
- Hub → agent: `Key{hid_usage, down}`, `Button{button, down, x, y, click_count}`, `Scroll{dx, dy, precise, phase}`, `Text{utf8}`, `ReleaseAll`. Pohyb myši datagramem (stačí poslední).
- Souřadnice normalizované 0..1 v obsahu zdroje; agent přepočítá na globální body.
- Agent drží množinu stisknutých kláves na hub; při odpojení, uvolnění grabu nebo 1 s bez keep-alive pošle sám key-up na všechny (žádné zaseknuté modifikátory).
- Mapování modifikátorů mezi OS (⌘↔Ctrl/Win) je volitelná tabulka v konfigu hubu, výchozí 1:1 podle fyzické pozice.
- Injekce: macOS `CGEventPost(.cghidEventTap)` (vyžaduje Accessibility); Windows `SendInput` (elevovaná okna a UAC viz Fáze 5); GNOME: Mutter D-Bus `RemoteDesktop` (`NotifyPointer*`, `NotifyKeyboardKeycode`); ostatní Wayland: portál RemoteDesktop + libei (`reis`), fallback `uinput` (backlog); X11 XTest (backlog).
- Priorita lokálního uživatele: pokud agent zaznamená lokální HID aktivitu v posledních 2 s, pošle hubu `LocalActivity` (hub zobrazí ikonu); blokování vzdáleného vstupu je volitelné.

### 6.8 Bezpečnost a soukromí
- Bind jen na privátní rozhraní (RFC 1918, link-local, volitelně Tailscale CGNAT rozsah).
- Agent má v tray ikoně stav „sleduje mě hub X“ a položku „Odpojit vše“ + globální panic zkratku.
- Audit log připojení na agentovi.
- Clipboard sync je opt-in na agenta a limit 1 MB.
- Žádný cloud, žádná telemetrie.

### 6.9 Správa vzdálených oken
| OS | Výčet + události | Přesun/resize/focus |
|---|---|---|
| macOS | `SCShareableContent` + polling 4 Hz s diffem (později `AXObserver`) | Accessibility: `kAXPositionAttribute`, `kAXSizeAttribute`, `kAXRaiseAction`, `kAXMinimizedAttribute`; párování AX elementu s `CGWindowID` přes `_AXUIElementGetWindow` (privátní, ale používá ho AltTab, Rectangle, yabai), fallback podle rámu+titulku. Přesun mezi Spaces není v rozsahu. Resize Electron aplikací přes AX se zadrhává → throttle 10 Hz, přesná hodnota po puštění. |
| Windows | `EnumWindows` + `SetWinEventHook` | `SetWindowPos`, `ShowWindow`, `SetForegroundWindow` |
| X11 | EWMH `_NET_CLIENT_LIST`, property notify | `_NET_MOVERESIZE_WINDOW`, `_NET_ACTIVE_WINDOW` |
| GNOME Wayland | vlastní Shell extension `rozhledna@ol1n` přes D-Bus (`List`, signál `WindowsChanged`) | táž extension: `Activate`, `MoveResize`, `Minimize`, `Close` |
| Wayland ostatní (backlog) | sway/Hyprland/niri IPC, KWin skript | totéž |

---

## 7. Protokol v1

### 7.1 Control stream
První bidi stream, který otevře hub. Rámec: `u32 LE délka` + protobuf `Envelope`.

```proto
syntax = "proto3";
package rozhledna.v1;

message Envelope {
  uint64 seq = 1;
  uint64 reply_to = 2;            // 0 = není odpověď
  oneof msg {
    Hello hello = 10;             HelloAck hello_ack = 11;
    PairRequest pair_request = 12; PairResult pair_result = 13;
    Ping ping = 14;               Pong pong = 15;
    ListSources list_sources = 20; SourceList source_list = 21;
    SourceEvent source_event = 22; GetIcon get_icon = 23; Icon icon = 24;
    Subscribe subscribe = 30;     SubscribeAck subscribe_ack = 31;
    UpdateSubscription update_subscription = 32;
    Unsubscribe unsubscribe = 33; RequestKeyframe request_keyframe = 34;
    StreamStats stream_stats = 35;
    PreviewSubscribe preview_subscribe = 40;
    CursorShape cursor_shape = 50; LocalActivity local_activity = 51;
    WindowCommand window_command = 60; WindowCommandResult window_command_result = 61;
    ClipboardOffer clipboard_offer = 70; ClipboardRequest clipboard_request = 71;
    ClipboardData clipboard_data = 72;
    Error error = 99;
  }
}

enum Os { OS_UNKNOWN = 0; MACOS = 1; LINUX = 2; WINDOWS = 3; }
enum SourceKind { DISPLAY = 0; WINDOW = 1; }
enum Tier { LIVE = 0; FOCUS = 1; }
enum Codec { H264 = 0; HEVC = 1; AV1 = 2; }

message Hello {
  uint32 protocol = 1;            // = 1
  bytes node_id = 2;              // 16 B UUID
  string name = 3; Os os = 4; string os_version = 5; string app_version = 6;
  bool is_agent = 7; bool is_hub = 8;
  repeated Codec encode_codecs = 9; repeated Codec decode_codecs = 10;
  repeated string features = 11;  // "per_window","window_control","clipboard","cursor_shapes","virtual_display","audio"
}
message HelloAck { bool paired = 1; repeated string granted = 2; } // view, control, clipboard, window_control

message Rect { double x = 1; double y = 2; double w = 3; double h = 4; }   // body (points), globální souřadnice agenta

message Source {
  uint64 id = 1; SourceKind kind = 2;
  string title = 3; string app_name = 4; string app_id = 5; uint32 pid = 6;
  uint64 display_id = 7;          // u okna: na kterém displeji leží
  Rect rect = 8; double scale = 9;
  uint32 native_w = 10; uint32 native_h = 11; double refresh_hz = 12;
  bool focused = 13; bool minimized = 14; bool on_screen = 15; uint32 z_order = 16;
  bytes icon_hash = 17;
}
message SourceEvent { oneof ev { Source added = 1; uint64 removed = 2; Source changed = 3; } }

message Subscribe {
  uint64 source_id = 1; Tier tier = 2;
  uint32 max_w = 3; uint32 max_h = 4;   // pixely; agent škáluje se zachováním poměru stran, nikdy nezvětšuje
  uint32 max_fps = 5; Codec codec = 6; uint32 bitrate_kbps = 7; // 0 = auto
}
message SubscribeAck { uint32 stream_id = 1; uint32 w = 2; uint32 h = 3; uint32 fps = 4; Codec codec = 5; }
message UpdateSubscription { uint32 stream_id = 1; Tier tier = 2; uint32 max_w = 3; uint32 max_h = 4; uint32 max_fps = 5; uint32 bitrate_kbps = 6; }

message PreviewSubscribe { repeated uint64 source_ids = 1; uint32 max_w = 2; uint32 display_fps = 3; uint32 window_interval_ms = 4; }

message WindowCommand {
  uint64 source_id = 1;
  oneof cmd { bool focus = 2; Rect move_resize = 3; bool minimize = 4; bool restore = 5; bool close = 6; bool fullscreen = 7; }
}
```
(Zbylé zprávy — `PairRequest`, `Ping`, `StreamStats`, `CursorShape`, clipboard — dodefinuj ve stejném stylu; pole drž minimální.)

### 7.2 Input stream
Uni stream hub → agent otevřený po `HelloAck`. Rámec `u32 LE délka` + `InputEvent`:

```proto
message InputEvent {
  uint64 source_id = 1;           // cíl; u DISPLAY jde o globální vstup na tom displeji
  oneof ev {
    Key key = 2; Button button = 3; Scroll scroll = 4; Text text = 5; bool release_all = 6;
  }
}
message Key { uint32 hid_usage = 1; bool down = 2; uint32 modifiers = 3; } // (usage_page << 16) | usage
message Button { uint32 button = 1; bool down = 2; float x = 3; float y = 4; uint32 click_count = 5; }
message Scroll { float dx = 1; float dy = 2; bool precise = 3; uint32 phase = 4; }
message Text { string utf8 = 1; }
```

### 7.3 Video frame stream
Uni stream agent → hub, jeden na frame. Hlavička 44 B, little-endian:

| Offset | Typ | Pole |
|---|---|---|
| 0 | u32 | magic `RZVF` |
| 4 | u32 | stream_id |
| 8 | u64 | frame_no |
| 16 | u64 | capture_ts_ns (monotonní hodiny agenta) |
| 24 | u64 | encode_done_ts_ns |
| 32 | u16 | flags: bit0 keyframe, bit1 discontinuity |
| 34 | u8 | codec |
| 35 | u8 | reserved |
| 36 | u16 | width |
| 38 | u16 | height |
| 40 | u32 | payload_len |

### 7.4 Preview stream
Uni stream agent → hub, rámce `u32 délka` + `Preview{source_id, jpeg, w, h, ts}`. Displeje: průběžně `display_fps` (výchozí 8). Okna: round-robin screenshot každých `window_interval_ms` (výchozí 1500), fokusované a nedávno změněné okno častěji.

### 7.5 Datagramy (≤ 1200 B)
- `0x01 PointerMotion` hub → agent: `source_id u64, seq u32, x f32, y f32`.
- `0x02 CursorPos` agent → hub: `source_id u64, seq u32, x f32, y f32, shape_id u32, visible u8`.
Starší `seq` se zahazuje.

---

## 8. Platformní backendy — macOS (detail pro Fáze 0–3)

- **Crates:** primárně `objc2` framework crates (`objc2-screen-capture-kit`, `objc2-video-toolbox`, `objc2-core-media`, `objc2-core-video`, `objc2-core-graphics`, `objc2-application-services`, `objc2-app-kit`). Alternativa pro capture: crate `screencapturekit`. OVĚŘIT dostupnost a úplnost před začátkem; rozhodnutí do `DECISIONS.md`.
- **Agent jako aplikace:** `.app` bundle s `LSUIElement=1`, tray přes `tray-icon` + `tao`. Hlavní vlákno drží Cocoa run loop (nutné pro AX a callbacky). Autostart přes `SMAppService`.
- **Podpis:** od prvního dne stabilní bundle id a podpis Developer certifikátem (Ol1n má členství). Nikdy ad-hoc — TCC oprávnění se při změně podpisu ztrácejí.
- **Oprávnění agenta:** Screen Recording (capture), Accessibility (injekce vstupu, správa oken), Local Network. Tray menu ukazuje stav každého (✓/✗) a klik otevře příslušný panel System Settings.
- **Oprávnění hubu:** Local Network; pro grab režim Accessibility/Input Monitoring (CGEventTap).
- **Kurzor:** `showsCursor=false`; `CursorTracker` čte aktuální systémový kurzor (30 Hz + při pohybu), obrázek hashuje a mapuje na `kind` (arrow, ibeam, pointer, resize-h/v/diag, grab, crosshair, wait); neznámé posílá jako PNG + hotspot. Hub nastaví lokální kurzor.
- **Interakce s oknem:** macOS neumí spolehlivě klikat do okna na pozadí. Klik do `WINDOW` dlaždice proto okno na agentovi nejdřív zvedne (`kAXRaiseAction` + aktivace aplikace). Je to očekávané chování, zapsat do dokumentace.
- **Spánek displeje:** když displej usne, capture se zastaví. Během aktivního odběru držet `IOPMAssertion` (PreventUserIdleDisplaySleep), jinak ne.

Linux a Windows backendy jsou popsané ve Fázích 4 a 5.

---

## 9. Hub — specifikace UI a „window manageru“

### 9.1 Dlaždice a tiery
- Stav dlaždice: `Disconnected → Connecting → Preview → Live → Focus` (+ `Error`).
- **TierController** (pixel budget): pro každou dlaždici na viditelném workspace spočítá fyzické pixely dlaždice → `max_w/max_h` (strop = nativní rozlišení zdroje). FOCUS dlaždice: `max_fps = min(refresh hubu, refresh zdroje, 120)`; ostatní viditelné: 30 fps (nastavitelné 15–60); neviditelné workspaces: odhlásit, drží jen preview.
- Při změně layoutu debounce 150 ms, během animace se škáluje stará textura.
- Overlay dlaždice: název stroje + okna, barva stroje, tečka stavu, volitelně fps / latence / bitrate.

### 9.2 Fit módy
| Mód | Chování |
|---|---|
| `scale` (výchozí pro DISPLAY) | Zachová poměr stran, letterbox. |
| `crop` | Vyplní dlaždici, ořízne. |
| `native` | 1:1 pixely, posun (pan) tažením / scrollem s modifikátorem. |
| `resize-remote` (výchozí pro WINDOW, od Fáze 3) | Hub pošle `WindowCommand.move_resize`, aby vzdálené okno mělo přesně velikost dlaždice → ostrý obraz 1:1. Pro DISPLAY až s virtuálními displeji (na GNOME Fáze 4, jinde Fáze 6). |

### 9.3 Layouty (čisté funkce v Dartu: `arrange(tiles, area, params) → Map<TileId, Rect>`)
1. `grid` — automatická mřížka; počet sloupců volit tak, aby byla maximální nejmenší škála dlaždic vzhledem k jejich poměrům stran.
2. `master_stack` — dwm styl; `master_ratio` (0.6), `nmaster` (1), orientace vlevo/nahoře.
3. `bsp` — binární strom; nová dlaždice dělí fokusovanou po delší straně.
4. `columns` — rolovací sloupce (niri/PaperWM); šířky 1/3, 1/2, 2/3, 1/1.
5. `monocle` / `tabbed` — jedna viditelná, pruh záložek s živými náhledy.
6. `stack` — i3 stacking: seznam titulků + jedna viditelná.
7. `float` — volné obdélníky, z-order, přichytávání k hranám.

Parametry: vnitřní/vnější mezery, layout per workspace, sady workspaces per monitor hubu.

### 9.4 Workspaces a pravidla
- Workspaces 1–9 + pojmenované; přepínání, přesun dlaždice, „overview“ všech dlaždic napříč workspaces.
- **Auto-tiling:** odběr „všechna okna stroje X“ / „všechna okna aplikace Y na stroji X“ → nové vzdálené okno se samo objeví jako dlaždice; zavřené okno dlaždici odstraní.
- Pravidla ve stylu i3 `assign`: `{machine, app_id, title_regex} → {workspace, layout_hint, fit}`.
- Persistování: layouty, pravidla a identita dlaždic (`machine + app_id + title`) do JSON; po restartu se dlaždice znovu spárují se zdroji.

### 9.5 Dock (inspirace Enlightenment)
- Pozice dole/vlevo/vpravo, autohide.
- Sekce per stroj (barva, stav, latence), sbalitelné. Položky: displeje (živý náhled 8 fps), okna (náhled ~1,5 s).
- **Hover zoom:** najetím se položka dočasně přepne na LIVE a zvětší (jako zoom pageru v E16).
- Klik = fokus existující dlaždice nebo otevření v aktuálním workspace; drag = umístění na konkrétní místo/workspace; kontextové menu = „odebírat všechna okna aplikace“, připnout, vytvořit pravidlo.
- **Pager:** miniatura workspaces s živými náhledy dlaždic; drag dlaždic mezi workspaces.
- **Iconbox:** skryté/„minimalizované“ dlaždice parkují v docku.

### 9.6 Focus, vstup, grab
- Myš nad dlaždicí: pohyb, kliky a scroll jdou vzdálenému stroji hned (volitelně až po kliknutí).
- Klávesnice jde do fokusované dlaždice. Bez grabu Flutter nevidí systémové zkratky.
- **Grab** (Enter na fokusované dlaždici / dvojklik / zkratka): nativní plugin zachytí všechny klávesy včetně systémových (macOS `CGEventTap`, Windows `WH_KEYBOARD_LL`, Wayland `keyboard-shortcuts-inhibit`, X11 `XGrabKeyboard`). Uvolnění: konfigurovatelný akord, výchozí `Ctrl+Opt+Esc` (macOS) / `Ctrl+Alt+Esc`. Grab viditelně označit rámečkem.
- Zkratky hubu mají konfigurovatelný „hub modifikátor“ (výchozí `Ctrl+Opt` na macOS, `Super` na Linuxu, `Ctrl+Win` na Windows): fokus směrem h/j/k/l, přesun/prohození, resize ±, cyklus layoutů, float, zoom dlaždice na celé okno, workspace 1–9, přesun na workspace, zavřít dlaždici (odhlásí odběr, vzdálené okno nezavírá), zavřít vzdálené okno (explicitní, jiná zkratka).
- Klávesy jako `PhysicalKeyboardKey.usbHidUsage`.

---

## 10. Struktura repozitáře

```
rozhledna/
  ROZHLEDNA_PLAN.md  DECISIONS.md  CHANGELOG.md
  proto/rozhledna.proto
  crates/
    rz-proto/      prost typy, rámcování, hlavička video framu
    rz-net/        quinn endpoint, identita, párování (SAS), mDNS, clock sync
    rz-capture/    trait Source; backendy: testpattern, macos_sck, linux_mutter, linux_portal, win_wgc, win_dxgi (backlog: linux_x11, linux_wlr)
    rz-codec/      trait Encoder/Decoder; backendy: videotoolbox, ffmpeg
    rz-input/      trait Injector; backendy: null, macos_cgevent, linux_mutter, linux_libei, win_sendinput (backlog: linux_xtest, linux_uinput); HID tabulky
    rz-wm/         trait WindowControl; backendy: macos_ax, gnome, win32 (backlog: x11_ewmh, sway, hyprland, kwin)
    rz-agent/      binárka agenta + tray
    rz-hub-core/   SessionManager, StreamReceiver, TierController (logika), FFI pro Flutter
    rz-cli/        rzctl: discover, pair, sources, stats, view (headless diagnostika)
    xtask/         build, bundle, sign, notarize, gen-proto
  hub/             Flutter aplikace (UI, layout engine, dock) + nativní pluginy (texture, key-grab)
  gnome-extension/ rozhledna@ol1n — GNOME Shell extension (GPL-2.0-or-later, samostatná komponenta)
  tools/latency/   okno s milisekundovým čítačem + vyhodnocení logů
  docs/probes/     výsledky průzkumů (window capture, oprávnění)
```

---

## 11. Fáze

Odhady času jsou hrubé a počítají s Opusem jako programátorem a Ol1nem jako testerem.

### Fáze 0 — Spike a měření (2–4 dny)
Cíl: ověřit latenci a tři rizikové předpoklady dřív, než vznikne produktový kód.

| Úkol | Obsah |
|---|---|
| T0.1 | Kostra workspace, `xtask`, `rz-proto` s hlavičkou framu, lint. |
| T0.2 | `rz-codec::videotoolbox` enkodér + dekodér; zdroj `testpattern` (pohyblivý vzor + vypálené číslo framu a čas); test roundtrip; změřit ms enkódování/dekódování pro 1080p, 1440p, 4K. |
| T0.3 | `rz-net` minimum: quinn s dev certifikátem (`--insecure-dev`), frame-per-stream, clock sync. |
| T0.4 | `spike-agent` (SCK displej → enkodér → síť) a `spike-viewer` (nativní okno: dekodér → IOSurface jako `CALayer.contents`). |
| T0.5 | Overlay + CSV: capture→encode→send→recv→decode→present, P50/P95. |
| T0.6 | Flutter spike: minimální macOS Flutter app + Swift texture plugin (`FlutterTexture.copyPixelBuffer`) + Rust staticlib; stejný stream; změřit rozdíl proti nativnímu vieweru. |
| T0.7 | Průzkum capture oken: zakryté okno, jiný Space, minimalizované, fullscreen, menu/popupy/sheety (child windows). Výsledek do `docs/probes/macos-window-capture.md`. |
| T0.8 | Průzkum oprávnění na macOS 26 a 27: Screen Recording vs. „Remote Desktop“, měsíční dotaz, co přežije rebuild při stabilním podpisu, zda žádat o `persistent-content-capture`. Do `docs/probes/macos-permissions.md`. |
| T0.9 | (Ol1n, ručně) Srovnání na stejné dvojici Maců: Sunshine+Moonlight a vestavěný Screen Sharing v režimu High Performance. Metoda: `tools/latency` čítač, zpomalené video telefonem přes oba monitory. |

**Brány:**
- **G1:** Flutter textura přidá ≤ 1 frame proti nativnímu vieweru → hub ve Flutteru. Jinak hub = nativní Rust renderer (wgpu) a Flutter jen pro dock/nastavení; přepsat kap. 9 před Fází 1.
- **G2:** Na drátové LAN P50 pipeline ≤ 25 ms při 60 Hz a ≥ 58 fps při 1440p → frame-per-stream stačí. Jinak hned datagramový režim.
- **G3:** Čitelnost textu v H.264 4:2:0 při cílovém bitrate je přijatelná → HEVC/4:4:4 zůstává ve Fázi 6. Jinak předsunout.

### Fáze 1 — MVP „zeď obrazovek“ macOS↔macOS (5–8 dní)
| Úkol | Obsah |
|---|---|
| T1.1 | `rz-net`: identita, trust store, párování SAS, mDNS discovery, reconnect s backoffem. |
| T1.2 | `rz-agent`: tray app, stav oprávnění, `SourceRegistry` jen pro displeje, `Subscribe/Unsubscribe/UpdateSubscription`, backpressure. |
| T1.3 | `rz-input::macos_cgevent`: myš, klávesy (HID → CGKeyCode tabulka ANSI + ISO), scroll, `ReleaseAll`, hlídač zaseknutých kláves. |
| T1.4 | `CursorTracker` + `CursorShape`/`CursorPos`. |
| T1.5 | `rz-hub-core`: SessionManager, StreamReceiver s pravidly z 6.4, dekodér, FFI události pro Flutter. |
| T1.6 | Hub UI: seznam strojů, párovací dialog, layout `grid`, dlaždice s texturou, fokus, vstup myši/klávesnice bez grabu, overlay statistik. |
| T1.7 | Nativní key-grab plugin pro macOS + akord pro uvolnění. |
| T1.8 | `rzctl`: `discover`, `pair`, `sources`, `stats`. |
| T1.9 | `xtask bundle/sign/notarize` pro agenta i hub. |

**Akceptace:** Dva Macy; hub ukáže oba displeje druhého stroje v mřížce; psaní, klikání a scroll fungují včetně českého rozložení; Cmd+Tab v grabu přepíná aplikace na vzdáleném stroji; po uspání/probuzení se spojení samo obnoví; odpojení kabelu nezanechá stisknutý modifikátor.

**Dogfood:** Ol1n týden ovládá Mac mini z MacBooku jen přes Rozhlednu a zapisuje, co drhne.

### Fáze 2 — Dock, tiery, workspaces, tiling (5–8 dní; jen hub)
| Úkol | Obsah |
|---|---|
| T2.1 | Preview stream na agentovi (displeje) + dock s živými náhledy, hover zoom. |
| T2.2 | TierController + pixel budget; `UpdateSubscription` při změně layoutu. |
| T2.3 | Layout engine: `grid`, `master_stack`, `bsp`, `monocle/tabbed`, `columns`, `stack`, `float` + unit testy (golden rects). |
| T2.4 | Workspaces, pager s náhledy, overview. |
| T2.5 | Zkratky hubu, konfigurační soubor, persistování layoutů. |
| T2.6 | Fit módy `scale`, `crop`, `native`. |

**Akceptace:** 6+ dlaždic ze 3 strojů; přepínání layoutů a workspaces klávesnicí bez zadrhnutí; CPU/GPU hubu při 6 dlaždicích pod dohodnutým stropem (stanovit po Fázi 0); neviditelné workspaces neodebírají video.

### Fáze 3 — Okna jako zdroje + správa vzdálených oken na macOS (7–12 dní)
| Úkol | Obsah |
|---|---|
| T3.1 | `SourceRegistry` pro okna: filtr (vrstva 0, ≥ 64×64 bodů, ne vlastní okna), polling + diff → `SourceEvent`. Ikony aplikací (`GetIcon`). |
| T3.2 | Capture okna (`desktopIndependentWindow`), child windows podle výsledku T0.7. |
| T3.3 | Náhledy oken round-robin screenshotem. |
| T3.4 | `rz-wm::macos_ax`: focus, move/resize, minimize/restore, close. |
| T3.5 | Hub: WINDOW dlaždice, fit `resize-remote` s throttlingem, vstup se souřadnicemi v okně, raise-on-interact. |
| T3.6 | Auto-tiling odběry a pravidla (9.4), spárování dlaždic po restartu. |
| T3.7 | Clipboard: text obousměrně při změně fokusu dlaždice (opt-in). |

**Akceptace:** „Odebírej všechna okna Terminalu na Mac mini“ → každé nové okno se samo zařadí do layoutu; změna velikosti dlaždice změní velikost skutečného okna a obraz je ostrý 1:1; zavření okna na dálku odstraní dlaždici.

### Fáze 4 — Linux agent + Linux hub: GNOME Wayland (2–3 týdny)
Cíle: openSUSE Leap 16.0 (GNOME 48, x86_64) a DGX Spark (DGX OS, GNOME — verzi OVĚŘIT, arm64). Leap 16 nabízí jen Waylandové desktopy, takže X11 backendy se nepíšou; wlroots a KDE jsou v backlogu.

| Úkol | Obsah |
|---|---|
| T4.0 | **Průzkum → `docs/probes/gnome-mutter.md`.** Introspekce `org.gnome.Mutter.ScreenCast` a `org.gnome.Mutter.RemoteDesktop` na GNOME 48 (Leap) i na GNOME ve Sparku. Ověřit: relace vznikne bez dialogu; co je potřeba pro vstup (jeden projekt hlásí odmítnutí „not allowed“, dokud není v GNOME povolené vzdálené ovládání); chování `RecordWindow` (hlášeno: stream má velikost obrazovky a okno je v levém horním rohu); `RecordVirtual`; `cursor-mode`; schránka přes RemoteDesktop relaci; SELinux denials v enforcing režimu. |
| T4.1 | `rz-capture::linux_mutter`: `RemoteDesktop.CreateSession` → `ScreenCast.CreateSession` s `remote-desktop-session-id` → `RecordMonitor` (displej) / `RecordWindow` (okno) → `Start` → PipeWire node. D-Bus přes `zbus`, PipeWire přes crate `pipewire`. Všechna volání musí jít ze stejného D-Bus spojení, které relaci vytvořilo. Kurzor mimo obraz (`cursor-mode` = metadata; tvar a pozice z metadat streamu). Nejdřív MemFd buffery (CPU kopie), DMA-BUF zero-copy až jako optimalizace. |
| T4.2 | `rz-capture::linux_portal` jako fallback (portál ScreenCast + RemoteDesktop, `persist_mode=2`, restore token v souboru 0600) pro případ, že Mutter API chybí nebo odmítne. Detekce za běhu, jedna řádka v logu. |
| T4.3 | `rz-codec::ffmpeg`: řetěz enkodérů `h264_nvenc` → `h264_vaapi` → `libopenh264` (software); dekodéry `nvdec` / `vaapi` → software. `xtask` přibalí vlastní LGPL build FFmpeg (bez libx264), aby agent nezávisel na Packmanu. `rzctl doctor` vypíše, který enkodér a dekodér se skutečně použil a proč ostatní selhaly. **OVĚŘIT NVENC na Sparku (aarch64)** a podle O6 hardwarový enkodér na Leapu. |
| T4.4 | `rz-input::linux_mutter`: `NotifyPointerMotionAbsolute`, `NotifyPointerButton`, `NotifyPointerAxis` / `NotifyPointerAxisDiscrete`, `NotifyKeyboardKeycode` (tabulka HID usage → evdev). Přesné signatury vzít z introspekce v T4.0. Fallback `linux_libei` přes portál. |
| T4.5 | GNOME Shell extension `rozhledna@ol1n` (samostatná komponenta, licence GPL-2.0-or-later, Shell 46–48): D-Bus metody `List`, `Activate`, `MoveResize`, `Minimize`, `Unminimize`, `Close` + signál `WindowsChanged`. Vrací Mutter window id, které bere `RecordWindow`. Rozhraní pustí jen volajícího, jehož proces je binárka agenta (PID odesílatele z D-Bus → cesta k binárce). |
| T4.6 | `rz-wm::gnome` nad extension; `SourceRegistry` pro okna; fit `resize-remote`; raise-on-interact (GNOME neumí poslat vstup jen jednomu oknu, takže klik do WINDOW dlaždice okno nejdřív aktivuje — stejný model jako na macOS). Ořez streamu podle rámu okna, pokud se potvrdí chování z T4.0. |
| T4.7 | Headless a virtuální displeje: `RecordVirtual` vytvoří virtuální monitor v požadované velikosti → Spark bez monitoru dostane plochu ve velikosti dlaždice; fit `resize-remote` pro DISPLAY na GNOME. |
| T4.8 | Balení: RPM (Leap) + `.deb` (DGX OS) přes `xtask`; `systemd --user` unit agenta; služba pro firewalld `rozhledna.xml` (UDP 47470) a povolení `mdns` v zóně; ověřit běh pod SELinuxem v enforcing režimu. |
| T4.9 | Hub na Linuxu: Flutter (GTK, Wayland); textura přes `FlTextureGL` (DMA-BUF → EGLImage), dočasně pixel-buffer textura a zapsat dluh; grab přes `keyboard-shortcuts-inhibit` (GNOME se jednou zeptá na povolení). |

Stroje úplně bez GUI (JODA) řeší rozšíření R1 „terminálové dlaždice“.

**Akceptace:** Leap i Spark jako dlaždice na Mac hubu při 60 fps; ovládání funguje včetně českých kláves; okna z Leapu jako samostatné dlaždice s `resize-remote`; po restartu stroje se agent sám přihlásí bez jakéhokoli dialogu; `rzctl doctor` na obou strojích hlásí hardwarový enkodér, nebo jasně vysvětlí, proč běží softwarový.

### Fáze 5 — Windows agent + hub (2–3 týdny)
- Capture: `win_dxgi` (displeje), `win_wgc` (okna, fallback displeje).
- Kodek: `ffmpeg` (`h264_nvenc` / `h264_amf` / `h264_qsv`), dekódování `d3d11va`.
- Vstup: `SendInput`; pro elevovaná okna helper s `uiAccess` (podepsaný, v zabezpečené cestě); pro UAC secure desktop a Ctrl+Alt+Del služba pod SYSTEM (`SendSAS`) — až jako T5.x po základní funkčnosti.
- WM: `EnumWindows` + `SetWinEventHook`, `SetWindowPos`.
- Hub: Flutter `GpuSurfaceTexture` se sdíleným D3D11 handle; key-grab `WH_KEYBOARD_LL`.
- Instalátor (MSI) + pravidlo firewallu; code-signing certifikát (nutný i pro `uiAccess`).

### Fáze 6 — Kvalita (průběžně)
HEVC a AV1; 4:4:4 tam, kde to hardware umí; 120 Hz end-to-end; HDR a správa barev (sRGB vs. P3); zpřesňovací frame při statickém obsahu; audio (Opus) per dlaždice; přenos souborů a obrázků ve schránce; virtuální displeje (`resize-remote` pro DISPLAY, headless stroje); datagramový režim s FEC/NACK pro Wi-Fi; Tailscale; více hubů na jednoho agenta; auto-update agentů.

### Fáze 7 — Rozšíření
Viz kap. 15.

---

## 12. Testování a měření

- **`testpattern` + `null` injektor:** deterministický zdroj a černá díra pro vstup. Integrační test spustí agenta a `rz-hub-core` na loopbacku, odebere stream, zkontroluje pořadí framů, vypálená čísla, chování při `RequestKeyframe` a reconnectu. Běží bez oprávnění a bez displeje.
- **Unit testy:** layout engine (golden rects), HID tabulky (round-trip), rámcování protokolu, SAS, TierController (pixel budget pro zadané scény).
- **Latence:** vestavěný overlay (P50/P95 po úsecích) + ruční glass-to-glass měření čítačem z `tools/latency`. Výsledky každé fáze do `LATENCY.md`.
- **Dogfood checklisty:** ke každé fázi soubor `docs/dogfood/phase-N.md` s kroky, které Ol1n odklikne.

---

## 13. Rizika

| Riziko | Dopad | Zmírnění |
|---|---|---|
| macOS měsíční dotaz na Screen Recording, změny TCC na 26/27 | Agent po čase tiše přestane streamovat | T0.8; stabilní podpis; žádost o `persistent-content-capture`; agent hlásí hubu stav oprávnění, hub ho zobrazí na dlaždici |
| Wayland mimo GNOME | Na KDE/wlroots nebude správa oken | Backlog; capture + vstup přes portál jako fallback už ve Fázi 4 |
| Interní Mutter D-Bus API se mezi verzemi GNOME mění | Agent po aktualizaci GNOME přestane fungovat | T4.0 introspekce na obou verzích; detekce za běhu; portál jako fallback; extension deklaruje podporované verze Shellu |
| Kodeky na openSUSE (Mesa bez H.264, FFmpeg až v Packmanu) | Leap enkóduje softwarově | Vlastní přibalený LGPL FFmpeg; řetěz NVENC → VA-API → openh264; `rzctl doctor` |
| Flutter textury přidají latenci / na Linuxu jsou pracné | Hub není dost svižný | Brána G1; nativní renderer jako záloha |
| Capture oken má díry (minimalizovaná, jiný Space, popupy) | Per-window dlaždice občas černé | T0.7 dřív než Fáze 3; fallback: ořez z capture displeje |
| Čitelnost textu při 4:2:0 a škálování | Rozmazané písmo | `resize-remote`, `native` mód, dřívější HEVC 4:4:4 (G3) |
| Limity enkodérů (počet relací, 1× NVENC na Sparku) | Málo souběžných streamů z jednoho stroje | Tiery; preview jako JPEG bez enkodéru; sdílení jednoho enkódování pro víc odběratelů |
| Windows secure desktop / UIPI | Nejde odkliknout UAC | SYSTEM služba až jako pozdější krok; do té doby zdokumentovat |
| Rozsah (3 OS × capture × kodek × vstup × WM) | Projekt se rozplizne | Pevné pořadí fází, macOS hotové a dogfoodované před Linuxem |

---

## 14. Na co se v zadání zapomnělo (checklist)

1. **Headless stroje** — bez monitoru není co snímat (virtuální displej, nebo terminálová dlaždice).
2. **Kurzor** — lokální vykreslení, tvary kurzoru.
3. **Klávesnice** — rozložení, mapování ⌘/Ctrl/Win, systémové zkratky (grab), zaseknuté modifikátory, IME/mrtvé klávesy.
4. **Schránka a soubory** — text, obrázky, drag & drop.
5. **Zvuk.**
6. **HiDPI, různá rozlišení, více monitorů, HDR, barevné prostory, rotace.**
7. **Zamčená obrazovka, login, UAC, spánek a probuzení** (Wake-on-LAN z hubu).
8. **Oprávnění a jejich expirace** na macOS; portálové dialogy na Waylandu.
9. **Bezpečnost** — párování, šifrování, role (jen dívat se / ovládat), viditelný indikátor sledování, panic tlačítko, audit log.
10. **Škálování** — pixel budget, tiery, limity enkodérů.
11. **Kolize vstupu** — lokální uživatel vs. hub, dva huby na jednom agentovi.
12. **Chráněný obsah** — DRM okna jsou černá; pole s hesly.
13. **Zrcadlení sebe sama** — stroj, který je hub i agent, musí vyloučit okno hubu z capture.
14. **Persistování** — layouty, identita oken po restartu, auto-reconnect.
15. **Měření latence** vestavěné v aplikaci.
16. **Distribuce** — podpis, notarizace, instalátory, služby, auto-update agentů.
17. **Spotřeba** — capture jen když někdo odebírá; statický obsah = nulový bitrate.
18. **Licence** — pozor na GPL/AGPL při inspiraci existujícími projekty.
19. **Stroje bez agenta** — VM, Raspberry, telefon, BIOS.

---

## 15. Rozšíření (backlog po Fázi 6)

| # | Rozšíření | Popis |
|---|---|---|
| R1 | **Terminálové dlaždice** | SSH/tmux pane jako nativní textová dlaždice vedle video dlaždic; pro stroje bez GUI (JODA) a pro Claude Code relace v tmuxu. Nulová režie videa, ostrý text. |
| R2 | **MCP server hubu** | Nástroje `list_sources`, `screenshot(source)`, `focus`, `type` → Director / jiný agent se umí „podívat“ na libovolnou obrazovku. |
| R3 | **Pozornost a alerty** | Lokální VLM na Sparku průběžně čte náhledy: „okno čeká na vstup“, „build spadl“, „dialog s oprávněním“ → dlaždice zabliká, notifikace přes PromoClown. Fulltext/OCR hledání přes všechny obrazovky. |
| R4 | **Softwarové KVM** | Kurzor přejede přes hranu monitoru na fyzicky sousední stroj (bez videa), jako Universal Control napříč OS. |
| R5 | **Seamless mód** | Vzdálená okna jako skutečná nativní okna na ploše hubu (xpra/RAIL styl), spravuje je lokální WM. |
| R6 | **Virtuální displeje** | Libovolný stroj jako další monitor jiného. Na GNOME hotovo už ve Fázi 4 (T4.7, `RecordVirtual`); zde macOS a Windows. |
| R7 | **Záznam a time-travel** | Kruhový buffer posledních N hodin per stroj na NAS (stream už je enkódovaný); „co se dělo na téhle obrazovce ve 3 ráno“. Výchozí vypnuto, jasný indikátor. |
| R8 | **Broadcast vstupu** | Psaní do více dlaždic najednou (jako tmux synchronize-panes). |
| R9 | **Scény** | Pojmenované rozložení + sada odběrů na jednu zkratku; režim „TV zeď“ pro velkou obrazovku. |
| R10 | **Web/tablet hub** | Flutter hub na iPad/Android; webový viewer přes WebTransport + WebCodecs (OVĚŘIT podporu v Safari). |
| R11 | **Source pluginy** | VNC/RDP/Moonlight zdroje pro stroje bez agenta, scrcpy pro Android, HDMI capture / PiKVM pro BIOS. Moonlight jako samostatný proces kvůli GPL. |
| R12 | **Vzdálený launcher** | Spustit aplikaci na stroji X a rovnou dostat její okno jako dlaždici. |
| R13 | **Napájení** | Wake-on-LAN, uspání, restart agenta z hubu. |
| R14 | **Hry** | Relativní myš, gamepady, 240 Hz. |

---

## 16. Zdroje

- Sunshine — podporované OS, omezení macOS hostu: https://tech-insider.org/apollo-vs-sunshine-game-streaming-2026/ , https://hysenlabs.com/projects/lizardbyte-sunshine
- Deskhub (QUIC, MIT): https://github.com/manhpham90vn/Deskhub
- TwinDesk (helpery per OS, okna aplikací): https://github.com/InukaAsith/TwinDesk/wiki
- RustDesk / SubnetDesk: https://openapps.pro/apps/rustdesk , https://raw.githubusercontent.com/zibo-chen/SubnetDesk/master/README.md
- xpra seamless / shadow: https://github.com/Xpra-org/xpra/
- Lan Mouse: https://github.com/feschber/lan-mouse
- Jump Desktop pro AI agenty: https://jumpdesktop.com/ai-agents.html
- macOS Screen Recording dotazy a entitlement: https://9to5mac.com/2024/08/14/macos-sequoia-screen-recording-prompt-monthly/ , https://mjtsai.com/blog/2024/08/08/sequoia-screen-recording-prompts-and-the-persistent-content-capture-entitlement/
- macOS 27 „Remote Desktop“ TCC položka: https://developer.apple.com/forums/tags/screencapturekit
- macOS Tahoe / Golden Gate (Apple silicon only): https://en.wikipedia.org/wiki/MacOS_Tahoe
- Wayland: capture, okna, portály: https://github.com/githubnext/desktop-tools/issues/136 , https://github.com/zcloudcz/DeskShare/pull/13 , https://github.com/cushycush/wdotool
- Windows Graphics Capture: https://learn.microsoft.com/en-us/uwp/api/windows.graphics.capture.graphicscapturesession.isborderrequired , https://deepwiki.com/qiin2333/foundation-sunshine/4.1.1-windows-capture-methods
- Windows UIPI / secure desktop: https://learn.microsoft.com/en-ie/Windows/Win32/winauto/uiauto-securityoverview , https://learn.microsoft.com/en-us/answers/a/12737194
- DGX Spark NVENC/NVDEC: https://www.nvidia.com/content/dam/en-zz/Solutions/dgx-spark/nvidia-gb10-superchip-whitepaper.pdf
- Virtuální displeje: https://github.com/VirtualDrivers/Virtual-Display-Driver , https://pkg.go.dev/github.com/go-macos/virtualdisplay
- Latence: https://computingforgeeks.com/?p=180693 (WebRTC na LAN), https://tech-insider.org/parsec-vs-moonlight-vs-steam-link-2026/
- macOS AX a správa oken: https://tangled.org/pierrelf.com/mct/blob/main/MCT/API/AccessibilityBridge.swift , https://developer.apple.com/forums/thread/845559
- openSUSE Leap 16.0 (Wayland-only instalátor, GNOME 48, SELinux): https://doc.opensuse.org/release-notes/x86_64/openSUSE/Leap/16.0/yast-html/release-notes.html , https://itsfoss.com/news/opensuse-leap-16-release.md
- Kodeky na openSUSE: https://computingforgeeks.com/opensuse-leap-multimedia-codecs/ , https://forums.opensuse.org/t/reintroduce-some-media-codecs-into-official-repos/184488
- Mutter D-Bus API bez dialogu (RustDesk, FreeRDP): https://github.com/rustdesk/rustdesk/pull/15988 , https://github.com/FreeRDP/FreeRDP/pull/14006
- `RecordWindow` — naměřené chování: https://github.com/sreevarshan-xenoz/Pocketdesk/pull/643
- Mutter headless a `RecordVirtual`: https://gitlab.gnome.org/GNOME/mutter/-/merge_requests/1698/pipelines
- GNOME Shell extension pro okna: https://github.com/ickyicky/window-calls , https://extensions.gnome.org/extension/10886/window-control/
