# T0.8 — Oprávnění na macOS 26/27

Cíl: zjistit, co agent potřebuje, aby streamoval dlouhodobě bez zásahu, a zda žádat o `com.apple.developer.persistent-content-capture`.
Stav: **protokol připravený, měření čeká na Ol1na** (vyžaduje klikání v System Settings a dlouhodobé sledování).

## Výchozí znalosti (z plánu, kap. 4.2)

- macOS 15+ periodicky (měsíčně) znovu žádá potvrzení Screen Recording u aplikací, které nepoužívají systémový picker — i přes ScreenCaptureKit.
- Výjimka: omezený entitlement `com.apple.developer.persistent-content-capture` (pro VNC/remote desktop aplikace), žádá se u Applu.
- macOS 27 má v Privacy & Security samostatnou položku **„Remote Desktop“**, oddělenou od „Screen & System Audio Recording“.
- TCC váže oprávnění na podpis (designated requirement). Ad-hoc podpis = ztráta oprávnění při každém rebuildu.

## Pozorováno v session Claude (2026-11-22, macOS 27.0.1)

- `sck-probe permissions` z terminálu: Screen Recording **NOT granted**, Accessibility **NOT granted** (oprávnění by se přidělila terminálu jako responsible process).
- `cargo xtask bundle spike-agent` podepíše bundle identitou „Apple Development“ (Team ID stabilní), hardened runtime. Developer ID chybí (DECISIONS.md).

## Pozorováno na Apple M2 (2026-11-22, macOS 27.0.1)

1. **Revokovaný podpis = „Malware Blocked and Moved to Bin“.** `xtask` vybral první „Apple Development“ z `security find-identity -v`, ale `-v` vypisuje i revokované certifikáty (přípona `(CSSMERR_TP_CERT_REVOKED)`). Bundle podepsaný revokovaným certifikátem macOS při spuštění označil za malware a přesunul do Koše. Na MacBooku navíc `find-identity -v` ukazoval revokovaný certifikát bez přípony `CSSMERR_` (revokaci zjistí až OCSP). Oprava: `xtask` přeskakuje řádky s `CSSMERR_`, po podpisu spustí `spctl -a -vv -t exec` a při `CSSMERR_TP_CERT_REVOKED` zkusí další identitu (platný Apple Development vrací jen „rejected“, což je u nenotarizovaného bundlu normální). Když jsou revokované všechny, bundle smaže.
2. Po přepodepsání platnou identitou: start agenta, `TCC state screen_recording=false`, **žádný dialog**. První `Subscribe` (SCK `SCShareableContent`) okamžitě selže: „The user declined TCCs for application, window, display capture“. **Ani tady se dialog nezobrazil** (P1).
3. `tccutil reset ScreenCapture dev.ol1n.rozhledna.spike-agent` + nový subscribe: stejná chyba, opět bez dialogu.
4. Povolení ručně v System Settings → dialog „… may not be able to record the contents of your screen until it is quit. Quit & Reopen / Later“. „Quit & Reopen“ by agenta spustil bez argumentů, proto Later a ruční restart s parametry. Po restartu `screen_recording=true`, capture funguje. V seznamu je jen „Screen & System Audio Recording“, samostatnou položku „Remote Desktop“ k povolení nebylo potřeba (P2: stačí Screen Recording).
5. **P3 ✓:** po `git pull` + `cargo xtask bundle spike-agent` (nový build, stejná platná identita) zůstává `screen_recording=true` bez dotazu.

## Experimenty

Agent spouštěj jako bundle, aby měl vlastní TCC identitu:
```
cargo xtask bundle spike-agent
open "dist/Rozhledna Spike Agent.app" --args --insecure-dev --log-file /tmp/rz-agent.log
grep "TCC state" /tmp/rz-agent.log
```

| # | otázka | postup | výsledek |
|---|---|---|---|
| P1 | Kde se agent objeví při prvním SCK volání? | spusť bundle se `--source sck`, připoj viewer | „Screen & System Audio Recording“ / „Remote Desktop“ / obojí? text dialogu? |
| P2 | Stačí jedna z položek? | povol jen jednu, otestuj capture; pak jen druhou | |
| P3 | Přežije oprávnění rebuild se stejným podpisem? | `cargo xtask bundle spike-agent` znovu, spustit | |
| P4 | Přežije změnu verze v Info.plist? | změň `version` ve workspace Cargo.toml, rebuild | |
| P5 | Co se stane bez podpisu? | `cargo xtask bundle spike-agent --no-sign` | očekávání: nový dotaz |
| P6 | Přežije restart stroje? | restart, spusť agenta | |
| P7 | Měsíční dotaz | nech agenta běžet / spouštět ~5 týdnů, zapiš datum dialogu | datum povolení: ____ datum dotazu: ____ |
| P8 | Accessibility (pro Fázi 1, injekce vstupu) | zatím jen zapsat, zda se objeví dotaz při `AXIsProcessTrusted` (neobjeví — preflight) | |
| P9 | Local Network dotaz | připojení vieweru z jiného stroje → dialog? (Info.plist má `NSLocalNetworkUsageDescription`, `NSBonjourServices`) | |

## Rozhodnutí o `persistent-content-capture`

Doplnit po P1–P7. Doporučení předem: **požádat hned** (žádost nic nestojí, schvalování trvá), účel = osobní remote desktop/KVM pro vlastní stroje na LAN.
Bez entitlementu počítat s měsíčním potvrzením → agent musí stav oprávnění hlásit hubu a hub ho zobrazit na dlaždici (plán kap. 13).
