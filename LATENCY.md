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
| SCK 2560×1080, nativní viewer, agent 11ef4fb, okno viditelné | — | 0,7 / 1,5 | 3,4 / 4,2 | 0,3 / 0,4 | **4,4 / 7,0** | 60,0 | G1 dvojice, `run/m2-visible-native.csv` |
| SCK 2560×1080, Flutter, agent 11ef4fb, okno viditelné | — | 0,8 / 2,4 | 3,5 / 4,7 | 12,3 / 14,7 | **16,8 / 18,5** | 59,5 | G1 dvojice, `run/m2-visible-flutter.csv` |

Max total 168,7 ms (první frame, inicializace). Encode je proti testpatternu nižší (6,0 vs 11,2 ms), protože frame má o 25 % méně pixelů a obsah se mezi snímky mění málo.
Hodnota **57,5 fps není vada pipeline:** SCK posílá frame jen při změně obsahu, takže na statických místech plochy fps klesá. Pro G2 je potřeba scéna, která se mění v každém framu (video nebo `counter.html` přes celou obrazovku).

#### Experiment `--sck-interval-hz` (M2 lokálně, counter fullscreen, 2560×1080, displej 60 Hz, 60 s na běh)

| `minimumFrameInterval` | fps | total P50/P95/max | „encode“ P50/P95 | agent sent | dropped_before_encode |
|---|---|---|---|---|---|
| 1/60 (dosavadní výchozí) | 57,2 | 10,1 / 16,1 / 181 | 6,1 / 11,2 | 57 fps | 1 |
| 1/120 | **60,1** | **3,3 / 5,6** / 159 | −0,7 / 0,5 | 61 fps | 0 |
| 0 (bez limitu) | **60,1** | **3,3 / 5,3** / 165 | −0,7 / 0,5 | 60 fps | 1 |
| 1/66 (fps×1,1) | 54,9 ✗ | 4,2 / 15,5 | — / 9,0 | občas 30 fps | 1 |
| **1/120 + throttle v agentovi (výchozí od 11ef4fb)** | **60,2** | **4,0 / 6,5** / 196 | −0,1 / 1,5 | 60 fps | 1 |

Ve všech bězích 0 keyframe requestů a 0 přeskočených framů. CSV jsou na M2 v `run/m2-sck-hz-*.csv`.

1. **Limit přesně 1/fps zahazuje framy.** Když dva vsyncy přijdou těsně pod 16,67 ms, SCK frame vynechá. S 1/120 nebo 0 je to 60 fps.
2. **Proč je „encode“ záporný:** `capture_ts` je `SCStreamFrameInfoDisplayTime`, okamžik, kdy se frame **objeví** na displeji agenta. Bez limitu SCK předá frame ~7 ms (encode ≈ 6 ms + 0,7) **před** tímto okamžikem, protože compositor kreslí dopředu. S limitem 1/60 ho předává až ~1 frame po něm.
   Segment „encode“ tedy neměří čistou dobu enkódování. **Total je ale měřený proti okamžiku zobrazení na agentovi**, takže je to správná reference pro glass-to-glass a zlepšení o ~7 ms je skutečné. Potvrdí kamera.
3. **Total P50 3,3 ms** znamená, že hub ukáže frame ~3,3 ms po tom, co ho ukázal displej agenta (bez kompozice a scanoutu hubu).
4. **Interval 1/66 je horší:** SCK s intervalem 15,15 ms občas bere jen každý druhý vsync. Interval kolem 1/fps je nespolehlivý, takže limit patří do agenta.
   **Nové výchozí nastavení agenta:** SCK `1/(2×fps)` + vlastní throttle na fps (frame se přijme, když od předchozího uplynulo ≥ ¾ intervalu).
   Ověřeno: loopback testpattern 60,9 fps a M2 60 Hz 60,2 fps (network 0,5 / 1,0, decode 3,3 / 3,9, present 0,3 / 0,4).
   Proti hz=0 je total o ~0,7 ms horší (throttle občas pustí frame z pozdějšího vsyncu), ale pořád ~6 ms pod původním 1/60. Na 120Hz displeji zatím neověřeno (MacBook ProMotion, potřebuje Screen Recording).

**G1 (M2 lokálně):** Flutter je o **12,4 ms** (P50) pomalejší, celý rozdíl je v present. To je pod limitem 16,7 ms, ale těsně (~0,75 frame).
„present“ ale u každé cesty měří něco jiného: nativní viewer měří jen odeslání `layer.contents`, Flutter včetně čekání na raster/vsync. Skutečný rozdíl ukáže až glass-to-glass.
Zakryté okno Flutteru (pod Safari ve fullscreen Space) padá na 29,7 fps, protože macOS omezuje vsync neviditelných oken. Nativní viewer drží 60 fps. Tenhle běh se do G1 nepočítá.

### Wi-Fi, dva Macy — orientační (drát není k dispozici, 2026-11-22)

Ve skutečnosti **iPhone Personal Hotspot**: MacBook přes iPhone USB (en8 172.20.10.4), M2 přes Wi-Fi hotspotu (172.20.10.3). Ping 4,6–93 ms. **Do brány G2 se nepočítá.**

| scénář | encode P50/P95 | network P50/P95 | decode P50/P95 | present P50/P95 | total P50/P95 | fps | pozn. |
|---|---|---|---|---|---|---|---|
| SCK 2560×1080, headless, statická plocha, 20 s | 7,6 / 12,8 | 3,3 / 12,1 | 3,7 / 33,8 | — | 15,6 / 110 | 6,2 | SCK neposílá framy beze změny obsahu; min RTT 4,6 ms |
| SCK 2560×1080, nativní okno | 7,1 / 13,5 | 3,7 / 14,2 | 4,9 / 229 | 0,2 / 3,6 | 19,1 / 239 | 24 | spojení po ~4 s spadlo („timed out“), n=85; souběžně s restartem agenta na M2 |
| SCK 2560×1080, nativní okno, counter fullscreen, 30 s | 6,9 / 12,5 | 4,1 / 42,2 | 3,5 / 14,1 | 0,2 / 0,4 | **15,9 / 67,9** | 57,8 | n=1686, 2,0 Mb/s, 0 keyframe requestů |
| SCK 2560×1080, headless, counter fullscreen, 60 s | 8,1 / 13,1 | 4,4 / 43,4 | 3,7 / 13,3 | — | **17,3 / 65,8** | 57,1 | n=3199, 2 keyframe requesty, 15 přeskočených framů, min RTT 4,4 ms |

Shrnutí hotspotu: P50 je jen o ~5 ms horší než lokálně, ale P95 je ~4× horší, a to kvůli jitteru sítě. Vyšší P95 u decode (13 vs 4,6 ms lokálně) není pomalý dekodér. Je to fronta za předchozími framy (segment decode = `decode_done − recv`). Z CSV headless 60 s:
z 288 framů s decode > 8 ms se jich 249 dekódovalo < 6 ms po předchozím framu (Wi-Fi doručí shluk framů naráz a sériový dekodér je zpracuje po ~3,5 ms).
U 56 předchozí frame přišel později (čekání v reorder bufferu). Pro Fázi 1 z toho plyne: při shluku dekódovat všechno (P-framy na sobě závisí), ale zobrazit jen poslední.
fps je stejné jako lokálně (~57), takže strop není v síti.

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
| G1 Flutter ≤ 1 frame navíc | **splněno těsně:** M2 + SCK +12,4 ms P50 (+11,5 P95) proti nativnímu, limit 16,7 ms. Potvrdit kamerou, rozhoduje Ol1n | M2 lokálně výše |
| G2 P50 ≤ 25 ms, ≥ 58 fps @1440p, drát | **uzavřeno podle lokálního měření (rozhodnutí Ol1na, varianta b, 2026-11-22):** M2 + SCK total P50 10,3–11,0 ms, síť lokálně ~0,6 ms. Drát není k dispozici, ověření na drátu je odložené. Otevřené: fps 57 místo 60 (experiment `--sck-interval-hz`) | M2 lokálně výše |
| G3 čitelnost textu 4:2:0 | **neměřeno** — testpattern není text; nutné na reálné ploše | čeká na Ol1na |
