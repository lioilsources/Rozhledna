//! `rz-agent`: the Rozhledna agent binary (T1.2).
//!
//! macOS default: menu-bar (tray) app; pairing requests come as a dialog. `--headless`: no UI, pairing is
//! confirmed in the terminal (or pre-trusted with `--trust-hub`).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use rz_agent::sources::{Backend, Registry};
use rz_agent::{Agent, Limits, PairAsk};
use rz_net::handshake::AgentUi;
use rz_net::identity::{Identity, default_dir};
use rz_net::trust::{ALL_GRANTS, TrustStore};

#[derive(Copy, Clone, ValueEnum, Debug)]
enum Source {
    /// ScreenCaptureKit displays (needs Screen Recording).
    Sck,
    /// Two synthetic 2560×1440 displays (no permissions; development, tests).
    Testpattern,
}

#[derive(Parser, Debug)]
#[command(about = "Rozhledna agent: shares this machine's screens with paired hubs on the LAN")]
struct Args {
    #[arg(long, default_value = "0.0.0.0:47470")]
    bind: SocketAddr,
    #[arg(long, value_enum, default_value = "sck")]
    source: Source,
    /// Name shown to hubs (default: host name).
    #[arg(long)]
    name: Option<String>,
    /// Identity + trust store directory (default: ~/Library/Application Support/Rozhledna/agent).
    #[arg(long)]
    state_dir: Option<PathBuf>,
    /// Pre-trust a hub fingerprint (`sha256:<hex>`), e.g. for a headless agent. Repeatable.
    #[arg(long)]
    trust_hub: Vec<String>,
    /// No menu-bar UI; pairing is confirmed in this terminal.
    #[arg(long)]
    headless: bool,
    /// Do not advertise over mDNS.
    #[arg(long)]
    no_mdns: bool,
    #[arg(long, default_value_t = 3840)]
    max_w: u32,
    #[arg(long, default_value_t = 2160)]
    max_h: u32,
    #[arg(long, default_value_t = 60)]
    max_fps: u32,
    /// 0 = auto (0.12 bpp).
    #[arg(long, default_value_t = 0)]
    bitrate_kbps: u32,
    /// SCK `minimumFrameInterval` = 1/N s (default 2×fps).
    #[arg(long)]
    sck_interval_hz: Option<u32>,
    #[arg(long)]
    log_file: Option<PathBuf>,
    /// Print this agent's fingerprint and exit.
    #[arg(long)]
    print_fingerprint: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    init_logging(args.log_file.as_deref())?;
    let dir = match &args.state_dir {
        Some(d) => d.clone(),
        None => default_dir("agent")?,
    };
    let identity = Identity::load_or_create(&dir)?;
    if args.print_fingerprint {
        println!("{}", identity.fingerprint);
        return Ok(());
    }
    let mut trust = TrustStore::load(&dir)?;
    for fp in &args.trust_hub {
        let fp = rz_net::Fingerprint::parse(fp)?;
        if trust.get(&fp).is_none() {
            let all: Vec<String> = ALL_GRANTS.iter().map(|s| s.to_string()).collect();
            trust.insert(&fp, "pre-trusted (--trust-hub)", &all)?;
        }
    }
    let backend = match args.source {
        Source::Sck => Backend::Sck { interval_hz: args.sck_interval_hz },
        Source::Testpattern => Backend::Testpattern { w: 2560, h: 1440, count: 2 },
    };
    let limits =
        Limits { max_w: args.max_w, max_h: args.max_h, max_fps: args.max_fps, bitrate_kbps: args.bitrate_kbps };
    let name = args.name.clone().unwrap_or_else(rz_net::discovery::hostname);
    let st = rz_agent::status::current();
    tracing::info!(
        fp = %identity.fingerprint, %name, state = %dir.display(),
        screen_recording = st.screen_recording, accessibility = st.accessibility, "agent starting"
    );

    #[cfg(target_os = "macos")]
    if !args.headless {
        return tray::run(args, identity, trust, Registry::new(backend), limits, name);
    }
    let agent = Agent::new(identity, &name, trust, Registry::new(backend), limits, Ui::Terminal);
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(agent.serve(args.bind, !args.no_mdns))
}

fn init_logging(file: Option<&std::path::Path>) -> Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into());
    let b = tracing_subscriber::fmt().with_env_filter(filter);
    match file {
        Some(p) => {
            let f = std::fs::File::create(p).with_context(|| format!("create {}", p.display()))?;
            b.with_ansi(false).with_writer(Mutex::new(f)).init()
        }
        None => b.init(),
    }
    Ok(())
}

fn format_code(sas: &str) -> String {
    format!("{} {}", &sas[..3], &sas[3..])
}

enum Ui {
    Terminal,
    #[cfg(target_os = "macos")]
    Tray(Mutex<tao::event_loop::EventLoopProxy<tray::UserEvent>>),
}

impl AgentUi for Ui {
    async fn confirm_hub(&self, ask: PairAsk) -> bool {
        match self {
            Ui::Terminal => {
                let prompt = format!(
                    "\nHub „{}“ se chce spárovat.\n  otisk: {}\n  kód:   {}  (musí být stejný jako na hubu)\nPovolit? [a/N] ",
                    ask.peer_name,
                    ask.peer_fp,
                    format_code(&ask.sas)
                );
                tokio::task::spawn_blocking(move || {
                    use std::io::{BufRead, Write};
                    eprint!("{prompt}");
                    let _ = std::io::stderr().flush();
                    let mut line = String::new();
                    let _ = std::io::stdin().lock().read_line(&mut line);
                    matches!(line.trim().to_lowercase().as_str(), "a" | "ano" | "y" | "yes")
                })
                .await
                .unwrap_or(false)
            }
            #[cfg(target_os = "macos")]
            Ui::Tray(p) => {
                let (tx, rx) = tokio::sync::oneshot::channel();
                if p.lock().unwrap().send_event(tray::UserEvent::Pair(ask, tx)).is_err() {
                    return false;
                }
                rx.await.unwrap_or(false)
            }
        }
    }
}

#[cfg(target_os = "macos")]
mod tray {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use anyhow::Result;
    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSAlert, NSAlertFirstButtonReturn, NSApplication};
    use objc2_foundation::NSString;
    use rz_agent::sources::Registry;
    use rz_agent::{Agent, Limits, PairAsk};
    use rz_net::Fingerprint;
    use rz_net::identity::Identity;
    use rz_net::trust::TrustStore;
    use tao::event::Event;
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS};
    use tray_icon::menu::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};
    use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

    use super::{Args, Ui, format_code};

    pub enum UserEvent {
        Refresh,
        Pair(PairAsk, tokio::sync::oneshot::Sender<bool>),
        Menu(MenuEvent),
    }

    #[derive(Clone)]
    enum Action {
        RequestScreenRecording,
        OpenScreenRecording,
        OpenAccessibility,
        DisconnectAll,
        Unpair(Fingerprint),
        Quit,
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    pub fn run(
        args: Args,
        identity: Identity,
        trust: TrustStore,
        registry: Registry,
        limits: Limits,
        name: String,
    ) -> Result<()> {
        let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
        event_loop.set_activation_policy(ActivationPolicy::Accessory);
        let proxy = event_loop.create_proxy();
        let agent = Agent::new(identity, &name, trust, registry, limits, Ui::Tray(Mutex::new(proxy.clone())));

        let p = Mutex::new(proxy.clone());
        MenuEvent::set_event_handler(Some(move |e| {
            let _ = p.lock().unwrap().send_event(UserEvent::Menu(e));
        }));

        // Network + agent on a tokio runtime in the background; the main thread runs the Cocoa event loop.
        let (a, bind, mdns) = (agent.clone(), args.bind, !args.no_mdns);
        let refresh = Mutex::new(proxy);
        std::thread::Builder::new().name("rz-agent-net".into()).spawn(move || {
            let rt = tokio::runtime::Runtime::new().expect("tokio runtime");
            rt.block_on(async move {
                let mut changes = a.subscribe_changes();
                let serve = tokio::spawn(a.clone().serve(bind, mdns));
                let mut tick = tokio::time::interval(Duration::from_secs(3));
                loop {
                    tokio::select! {
                        r = changes.changed() => if r.is_err() { break },
                        _ = tick.tick() => {}
                    }
                    if refresh.lock().unwrap().send_event(UserEvent::Refresh).is_err() {
                        break;
                    }
                    if serve.is_finished() {
                        match serve.await {
                            Ok(Err(e)) => tracing::error!("agent stopped: {e:#}"),
                            Err(e) => tracing::error!("agent task: {e}"),
                            Ok(Ok(())) => {}
                        }
                        break;
                    }
                }
            });
        })?;

        let mut tray: Option<TrayIcon> = None;
        let mut actions: HashMap<MenuId, Action> = HashMap::new();
        let mut last_menu_key = String::new();
        event_loop.run(move |event, _, flow| {
            *flow = ControlFlow::Wait;
            match event {
                Event::NewEvents(tao::event::StartCause::Init) => {
                    let (menu, acts, key) = build_menu(&agent);
                    actions = acts;
                    last_menu_key = key;
                    tray = TrayIconBuilder::new()
                        .with_menu(Box::new(menu))
                        .with_icon_templated(icon())
                        .with_tooltip(format!("Rozhledna agent — {}", agent.info.name))
                        .build()
                        .map_err(|e| tracing::error!("tray icon: {e}"))
                        .ok();
                }
                Event::UserEvent(UserEvent::Refresh) => {
                    let (menu, acts, key) = build_menu(&agent);
                    // Rebuilding an open menu closes it; only replace it when something changed.
                    if key != last_menu_key
                        && let Some(t) = &tray
                    {
                        t.set_menu(Some(Box::new(menu)));
                        actions = acts;
                        last_menu_key = key;
                    }
                }
                Event::UserEvent(UserEvent::Pair(ask, reply)) => {
                    let _ = reply.send(confirm_dialog(&ask));
                }
                Event::UserEvent(UserEvent::Menu(e)) => match actions.get(e.id()).cloned() {
                    Some(Action::RequestScreenRecording) => {
                        let granted = unsafe { CGRequestScreenCaptureAccess() };
                        tracing::info!(granted, "Screen Recording requested");
                    }
                    Some(Action::OpenScreenRecording) => {
                        open_settings("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
                    }
                    Some(Action::OpenAccessibility) => {
                        open_settings("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
                    }
                    Some(Action::DisconnectAll) => agent.disconnect_all(),
                    Some(Action::Unpair(fp)) => {
                        if let Err(e) = agent.unpair(&fp) {
                            tracing::warn!("unpair: {e:#}");
                        }
                    }
                    Some(Action::Quit) => {
                        agent.disconnect_all();
                        *flow = ControlFlow::Exit;
                    }
                    None => {}
                },
                _ => {}
            }
        });
    }

    fn open_settings(url: &str) {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }

    /// Menu + id→action map + a key describing the content (to skip identical rebuilds).
    fn build_menu(agent: &Arc<Agent<Ui>>) -> (Menu, HashMap<MenuId, Action>, String) {
        let st = rz_agent::status::current();
        let hubs = agent.hubs();
        let trusted = agent.trusted_hubs();
        let mut acts = HashMap::new();
        let menu = Menu::new();
        let mark = |b: bool| if b { "✓" } else { "✗" };
        let item = |menu: &dyn Fn(&MenuItem), text: String, enabled: bool| {
            let i = MenuItem::new(text, enabled, None);
            menu(&i);
            i
        };
        let add = |i: &MenuItem| {
            let _ = menu.append(i);
        };
        item(&add, format!("Rozhledna agent — {}", agent.info.name), false);
        item(&add, format!("Otisk {}", agent.identity.fingerprint.short()), false);
        let _ = menu.append(&PredefinedMenuItem::separator());

        let sr = item(&add, format!("{} Nahrávání obrazovky", mark(st.screen_recording)), true);
        acts.insert(
            sr.id().clone(),
            if st.screen_recording { Action::OpenScreenRecording } else { Action::RequestScreenRecording },
        );
        let ax = item(&add, format!("{} Zpřístupnění (ovládání, Fáze 1 T1.3)", mark(st.accessibility)), true);
        acts.insert(ax.id().clone(), Action::OpenAccessibility);
        if st.screen_locked {
            item(&add, "Obrazovka je zamčená — snímání neběží".into(), false);
        }
        let _ = menu.append(&PredefinedMenuItem::separator());

        if hubs.is_empty() {
            item(&add, "Nikdo se nedívá".into(), false);
        } else {
            for h in &hubs {
                let what = match h.streams {
                    0 => "připojen".to_string(),
                    1 => "sleduje 1 obrazovku".to_string(),
                    n => format!("sleduje {n} obrazovky"),
                };
                item(&add, format!("● {} ({}) — {what}", h.name, h.addr.ip()), false);
            }
            let d = item(&add, "Odpojit vše".into(), true);
            acts.insert(d.id().clone(), Action::DisconnectAll);
        }
        let sub = Submenu::new(format!("Spárované huby ({})", trusted.len()), !trusted.is_empty());
        for t in &trusted {
            let i = MenuItem::new(format!("Zapomenout {} ({})", t.name, &t.fingerprint[7..23]), true, None);
            if let Ok(fp) = Fingerprint::parse(&t.fingerprint) {
                acts.insert(i.id().clone(), Action::Unpair(fp));
            }
            let _ = sub.append(&i);
        }
        let _ = menu.append(&sub);
        let _ = menu.append(&PredefinedMenuItem::separator());
        let q = item(&add, "Ukončit".into(), true);
        acts.insert(q.id().clone(), Action::Quit);

        let key = format!(
            "{st:?}|{:?}|{:?}",
            hubs.iter().map(|h| (&h.name, h.addr, h.streams)).collect::<Vec<_>>(),
            trusted.iter().map(|t| &t.fingerprint).collect::<Vec<_>>()
        );
        (menu, acts, key)
    }

    fn confirm_dialog(ask: &PairAsk) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        #[allow(deprecated)]
        NSApplication::sharedApplication(mtm).activateIgnoringOtherApps(true);
        let alert = NSAlert::new(mtm);
        alert.setMessageText(&NSString::from_str(&format!("Hub „{}“ se chce připojit", ask.peer_name)));
        alert.setInformativeText(&NSString::from_str(&format!(
            "Kód: {}\n\nPovol jen tehdy, když hub ukazuje stejný kód.\nPo povolení uvidí tvoje obrazovky a bude je moci ovládat.\n\nOtisk: {}",
            format_code(&ask.sas),
            ask.peer_fp.short()
        )));
        alert.addButtonWithTitle(&NSString::from_str("Povolit"));
        alert.addButtonWithTitle(&NSString::from_str("Odmítnout"));
        alert.runModal() == NSAlertFirstButtonReturn
    }

    /// 22×22 template icon: a lookout tower (rozhledna).
    fn icon() -> Icon {
        const S: usize = 22;
        let mut px = vec![0u8; S * S * 4];
        let mut set = |x: usize, y: usize| {
            if x < S && y < S {
                px[(y * S + x) * 4 + 3] = 255;
            }
        };
        // Platform with roof.
        for x in 6..16 {
            set(x, 4);
            set(x, 5);
        }
        for x in 8..14 {
            set(x, 2);
            set(x, 3);
        }
        // Legs widening towards the ground.
        for y in 6..21 {
            let spread = (y - 6) / 3;
            set(8 - spread, y);
            set(13 + spread, y);
        }
        // Cross braces.
        for y in [10usize, 15] {
            let spread = (y - 6) / 3;
            for x in (8 - spread)..=(13 + spread) {
                set(x, y);
            }
        }
        Icon::from_rgba(px, S as u32, S as u32).expect("icon")
    }
}

#[allow(dead_code)]
fn _assert_send_sync() {
    fn f<T: Send + Sync>() {}
    f::<Ui>();
    f::<Arc<Agent<Ui>>>();
}
