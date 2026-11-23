# Dogfood checklist — Fáze 1 (MVP „zeď obrazovek“ macOS ↔ macOS)

Odkliká Ol1n. Agent = Mac mini M2, hub = MacBook (nebo naopak). Fáze 2 začne až po odkliknutí.
`export PATH=/opt/homebrew/opt/rustup/bin:$PATH`, na obou strojích `git pull`.

## 1. Agent (M2)
- [ ] `cargo xtask bundle rz-agent` → `open "dist/Rozhledna Agent.app"` (výchozí: SCK, port 47470, mDNS).
- [ ] V menu baru je ikona rozhledny. „✗ Nahrávání obrazovky“ → klik → povolit (pak „Quit & Reopen“). Totéž „✗ Zpřístupnění“.
- [ ] Po restartu agenta obě položky ✓.
- [ ] Log agenta: `~/Library/Logs/Rozhledna/agent.log` (Console.app ho najde taky). `--log-file` nesměrovat na
      externí svazek — macOS se pak při startu ptá na přístup k „removable volume“ a agent do odpovědi visí.

## 2. Hub (MacBook)
- [ ] `cargo xtask hub` → `open dist/Rozhledna.app`. macOS se zeptá na Local Network → Povolit.
- [ ] Vlevo „V síti“ je M2 (mDNS). „Přidat“.
- [ ] Hub ukáže kód, na M2 vyskočí dialog se stejným kódem → Povolit. Hub dialog sám zavře.
- [ ] Mřížka ukazuje všechny displeje M2; overlay: jméno, rozlišení, fps, Mb/s, P50.

## 3. Ovládání (akceptace T1.3/T1.6)
- [ ] Klik do dlaždice = fokus (modrý rámeček). Pohyb myši nad dlaždicí hýbe kurzorem na M2.
- [ ] Kliky, dvojklik (výběr slova), pravé tlačítko (kontextové menu), tažení okna.
- [ ] Scroll kolečkem i trackpadem (plynulý, setrvačnost), správný směr.
- [ ] Psaní včetně české klávesnice: „ěščřžýáíé ů ú“, velká písmena, Shift+šipky, ⌘C / ⌘V na M2.
- [ ] Kurzor na hubu mění tvar podle M2 (I-beam nad textem, ruka nad odkazem).
- [ ] Klik mimo dlaždici / přepnutí na jinou aplikaci → na M2 nezůstane stisknutý modifikátor.
- [ ] Grab: hub potřebuje Zpřístupnění (System Settings → Soukromí → Zpřístupnění → Rozhledna). Ve fokusované dlaždici ⌃⌥↩ (nebo ikona klávesnice) → oranžový rámeček.
- [ ] V grabu ⌘Tab přepíná aplikace **na M2** (ne na MacBooku), ⌘Space otevře Spotlight na M2, ⌘Q zavře aplikaci na M2. ⌃⌥⎋ grab uvolní. (DECISIONS T1.7: OVĚŘIT, že session tap dostane ⌘Tab.)
- [ ] Přepnutí na jinou aplikaci na MacBooku (myší) grab ukončí.

## 4. Odolnost (akceptace)
- [ ] Uspat M2 (nebo zamknout obrazovku) → dlaždice ukáže „obrazovka zamčená / odpojeno — zkouším znovu“; po odemčení se obraz sám vrátí.
- [ ] Uspat MacBook a probudit → spojení se samo obnoví, dlaždice živé.
- [ ] Vytáhnout síť (Wi-Fi off) na 10 s, při tom držet ⌘ na hubu → na M2 nezůstane ⌘ stisknutý (agent uvolní po 1 s ticha).
- [ ] Restart agenta (Ukončit v menu, znovu otevřít) → hub se připojí sám, bez nového párování.
- [ ] Restart hubu → stroje i dlaždice se obnoví (agents.json).

## 5. Týden dogfoodu
- [ ] Ovládat Mac mini z MacBooku jen přes Rozhlednu, zapisovat sem, co drhne:

| datum | co drhne | závažnost |
|---|---|---|
| | | |
