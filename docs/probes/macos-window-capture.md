# T0.7 — Capture jednotlivých oken na macOS (ScreenCaptureKit)

Cíl: zjistit, kdy `SCContentFilter(desktopIndependentWindow:)` dává použitelný obraz, než se na tom postaví Fáze 3 (T3.2).
Stav: **S1–S3, S5 změřeno na M2; S4, S6–S9 zbývá (ručně), S10 N/A.**

## Nástroj

```
cargo build -p rz-capture --example sck-probe
B=./target/debug/examples/sck-probe
$B permissions                 # stav TCC bez vyvolání dialogu
$B windows                     # okna: vrstva 0, ≥ 64×64 pt (filtr z T3.1); --all = vše
$B window <ID> --secs 3 --dump /tmp/rz-probe              # výchozí includeChildWindows
$B window <ID> --secs 3 --children on  --dump /tmp/rz-probe
$B window <ID> --secs 3 --children off --dump /tmp/rz-probe
```
Výstup: počet framů za dobu běhu (SCK posílá framy jen při změně obsahu → u statického okna může být 1–2), zpoždění prvního framu,
velikosti framů, průměrný jas a podíl černých pixelů posledního framu, BMP posledního framu.
Při spuštění z Terminálu se oprávnění Screen Recording přidělí **Terminálu** — to pro průzkum stačí.

## Postup

Pro každý scénář: připrav stav okna, spusť `window <ID>`, během běhu v okně něco měň (psaní v Terminalu / přehrávání videa), zapiš výsledek.
Testovací okna: Terminal (AppKit), Safari (WebKit), VS Code nebo Slack (Electron), Finder.

| # | scénář | jak připravit | očekávání (hypotéza) |
|---|---|---|---|
| S1 | viditelné, v popředí | — | obraz OK |
| S2 | zakryté jiným oknem | překryj jiným oknem | obraz OK (desktop-independent) |
| S3 | minimalizované | ⌘M | žádné nové framy / zamrzlé |
| S4 | na jiném Space | přesuň okno na Space 2, zůstaň na 1 | ? |
| S5 | fullscreen (vlastní Space) | zelené tlačítko, pak přepni zpět | ? |
| S6 | otevřené menu aplikace | během běhu otevři menu v liště | menu v obraze jen s `--children on`? |
| S7 | kontextové menu / popover | pravé tlačítko v okně | dtto |
| S8 | sheet (Uložit…) | ⌘S v TextEdit | sheet je child window |
| S9 | změna velikosti během capture | táhni rohem | nová velikost framu? škálování? |
| S10 | okno na druhém monitoru s jiným scale | přetáhni | velikost/scale |

## Výsledky

macOS 27.0.1, Apple M2 (Mac mini), jeden displej 2560×1080 @ 1× (ne Retina), 2026-11-22.
Testovací okno: Safari s `tools/latency/counter.html` (mění se každý vsync), 1000×700 pt. Spouštěno z terminálu s povoleným Screen Recording, řízeno přes AppleScript.

**Oprava nástroje:** `ScCapture::window` padal na `Assertion failed: (did_initialize), function CGS_REQUIRE_INIT` (SCK potřebuje inicializované spojení s window serverem a cesta pro okno nevolala žádnou CG funkci, cesta pro displej ano přes `CGMainDisplayID`). Opraveno voláním `CGMainDisplayID()` na začátku `window()`. Týká se i budoucího agenta bez NSApplication (T3.2).

**Měřítko:** `--scale 2` na 1× displeji dá framy 2000×1400 s obsahem jen v levé horní čtvrtině (zbytek černý). Agent musí brát backing scale z displeje, na kterém okno je, a ne předpokládat 2.

| # | aplikace | children | framů / 3 s | 1. frame | velikost | jas / černá % | pozn. |
|---|---|---|---|---|---|---|---|
| S1 | Safari | výchozí | 174 | 93 ms | 1000×700 | 24 / 77 | obraz OK (~58 fps) |
| S2 | Safari | výchozí | 72 | 67 ms | 1000×700 | 24 / 78 | obraz OK, bez překrývajícího okna (Activity Monitor). Méně framů škrtí nejspíš WebKit u zakrytého okna, ne SCK |
| S3 | Safari | výchozí | 0 | — | — | — | minimalizované: žádné framy. Hub musí zobrazit poslední frame + stav „minimalizováno“ |
| S4 | — | — | — | — | — | — | neměřeno (přesun okna na jiný Space nejde spolehlivě automatizovat) |
| S5 | Safari | výchozí | 169 (v popředí) / 2 (uživatel na jiném Space) | 62 ms | 2560×1080 | 13 / 86 | fullscreen v popředí OK. Když je jeho Space neaktivní, obraz zamrzne (2 framy), aplikace nekreslí |
| S6 | — | on / off | — | — | — | — | neměřeno (menu je potřeba otevřít ručně během běhu) |
| S7 | — | on / off | — | — | — | — | neměřeno (ručně) |
| S8 | — | on / off | — | — | — | — | neměřeno (ručně) |
| S9 | Safari | výchozí | 233 / 4 s | 59 ms | 600×400 | 23 / 68 | nepřesvědčivé: okno se zmenšilo ještě před startem capture. Zopakovat s delší prodlevou |
| S10 | — | výchozí | — | — | — | — | N/A, M2 má jeden displej |

## Závěr pro Fázi 3

(Doplnit po měření: které stavy fungují; zda je potřeba fallback „ořez z capture displeje“ (plán kap. 13); výchozí hodnota `includeChildWindows`.)
