# LATENCY.md

Výsledky měření latence po fázích (plán kap. 12). Segmenty: **encode** (capture → konec enkódování), **network** (→ celý frame přijat),
**decode** (→ dekódováno), **present** (→ předáno k zobrazení), **total** (capture → present). Časy agenta převedeny do hodin hubu přes clock sync.

## Fáze 0

### Loopback, jeden stroj (Claude, 2026-11-22)

M1 Pro, macOS 27.0.1. `spike-agent --source testpattern` → `spike-viewer` na 127.0.0.1, 60 fps, 6–8 s, ověření vypálených čísel framů 100 %.
Síť tu prakticky nehraje roli; čísla jsou dno pipeline bez SCK.

| scénář | encode P50/P95 | network P50/P95 | decode P50/P95 | present P50/P95 | **total P50/P95** | fps |
|---|---|---|---|---|---|---|
| headless 1920×1080 | 8,5 / 9,1 | 0,3 / 0,4 | 2,8 / 3,3 | — | **11,6 / 12,8** | 61,7 |
| headless 2560×1440 | 11,2 / 12,8 | 0,3 / 0,6 | 3,5 / 4,8 | — | **15,1 / 19,6** | 61,2 |
| headless 3840×2160 | 22,5 / 24,8 | 0,4 / 1,0 | 6,0 / 7,5 | — | **28,8 / 33,9** | **44,5** ⚠ |
| nativní okno 2560×1440 (CALayer) | 11,3 / 12,1 | 0,3 / 0,4 | 3,8 / 4,8 | 0,2 / 0,3 | **15,6 / 19,7** | 62,2 |
| Flutter Texture 2560×1440 | 11,4 / 12,6 | 0,3 / 0,4 | 3,7 / 4,4 | 4,8 / 7,4 | **20,1 / 24,5** | 61,9 |

⚠ 4K: propustnost HW enkodéru (viz DECISIONS.md). „present“ u nativního okna a u Flutteru neměří totéž místo — viz DECISIONS.md, Flutter spike.
Max hodnoty (60–350 ms) jsou vždy první frame (inicializace enkodéru/dekodéru + IDR).

Reprodukce:
```
cargo build --release -p spike-agent -p spike-viewer
./target/release/spike-agent --insecure-dev --source testpattern --tp-size 2560x1440 --bind 127.0.0.1:47471 &
./target/release/spike-viewer 127.0.0.1:47471 --insecure-dev --headless --verify-testpattern --duration 8 --csv loop.csv
```

### Lokálně na M2, skutečná plocha přes SCK (session Rozhledna-M2, 2026-11-22)

Mac mini M2 (Mac14,3), displej **2560×1080** (ultrawide), takže stream `--max-w 2560 --max-h 1440` je ve skutečnosti 2560×1080. Agent `--source sck` jako podepsaný bundle,
viewer na 127.0.0.1:47470, 60 s, v prohlížeči běží `counter.html`.

| scénář | encode P50/P95 | network P50/P95 | decode P50/P95 | present P50/P95 | **total P50/P95** | fps | pozn. |
|---|---|---|---|---|---|---|---|
| SCK 2560×1080, nativní viewer | 6,0 / 11,1 | 0,7 / 1,5 | 3,4 / 4,6 | 0,3 / 0,5 | **10,3 / 16,5** | 57,5 | n=3417, 3,8 Mb/s skutečně (cíl 19,9), 0 dropů, 1 keyframe |
| SCK 2560×1080, Flutter | | | | | | | čeká (upgrade Flutteru) |

Max total 168,7 ms (první frame, inicializace). Encode je proti testpatternu nižší (6,0 vs 11,2 ms), protože frame má o 25 % méně pixelů a obsah se mezi snímky mění málo.
Hodnota **57,5 fps není vada pipeline:** SCK posílá frame jen při změně obsahu, takže na statických místech plochy fps klesá. Pro G2 je potřeba scéna, která se mění v každém framu (video nebo `counter.html` přes celou obrazovku).

### Wi-Fi, dva Macy — orientační (drát není k dispozici, 2026-11-22)

MacBook (hub, 192.168.88.32) ↔ M2 (agent) na domácí Wi-Fi. **Neplatí pro bránu G2**, ta je definovaná pro drát. Přes iPhone hotspot neměřit (ping 4–78 ms).

| scénář | encode | network | decode | present | total | fps | pozn. |
|---|---|---|---|---|---|---|---|
| SCK 2560×1080, nativní viewer, Wi-Fi | | | | | | | |

### Drátová LAN, dva Macy (Ol1n) — ČEKÁ

Vyplnit podle `docs/dogfood/phase-0.md`. Brána **G2**: P50 total ≤ 25 ms při 60 Hz a ≥ 58 fps při 1440p.

| scénář | encode | network | decode | present | total | fps | pozn. |
|---|---|---|---|---|---|---|---|
| SCK 1440p, nativní viewer | | | | | | | |
| SCK 1440p, Flutter | | | | | | | |
| glass-to-glass Rozhledna (kamera) | | | | | | | |
| glass-to-glass Moonlight+Sunshine | | | | | | | |
| glass-to-glass Screen Sharing (High Performance) | | | | | | | |

### Brány Fáze 0 — předběžný stav

| brána | stav | podklad |
|---|---|---|
| G1 Flutter ≤ 1 frame navíc | **předběžně splněno** (present P95 7,4 ms < 16,7 ms) — potvrdit kamerou | loopback výše |
| G2 P50 ≤ 25 ms, ≥ 58 fps @1440p, drát | **předběžně splněno na loopbacku** (15,6 ms + kompozice ~½–1 frame) — rozhodne LAN | čeká na Ol1na |
| G3 čitelnost textu 4:2:0 | **neměřeno** — testpattern není text; nutné na reálné ploše | čeká na Ol1na |
