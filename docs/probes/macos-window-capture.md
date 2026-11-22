# T0.7 — Capture jednotlivých oken na macOS (ScreenCaptureKit)

Cíl: zjistit, kdy `SCContentFilter(desktopIndependentWindow:)` dává použitelný obraz, než se na tom postaví Fáze 3 (T3.2).
Stav: **nástroj hotový, měření čeká na Ol1na** (session Claude nemá Screen Recording).

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

macOS verze: ____   stroj: ____   datum: ____

| # | aplikace | children | framů / 3 s | 1. frame | velikost | jas / černá % | pozn. (podívat se do BMP) |
|---|---|---|---|---|---|---|---|
| S1 | | výchozí | | | | | |
| S2 | | výchozí | | | | | |
| S3 | | výchozí | | | | | |
| S4 | | výchozí | | | | | |
| S5 | | výchozí | | | | | |
| S6 | | on / off | | | | | |
| S7 | | on / off | | | | | |
| S8 | | on / off | | | | | |
| S9 | | výchozí | | | | | |
| S10 | | výchozí | | | | | |

## Závěr pro Fázi 3

(Doplnit po měření: které stavy fungují; zda je potřeba fallback „ořez z capture displeje“ (plán kap. 13); výchozí hodnota `includeChildWindows`.)
