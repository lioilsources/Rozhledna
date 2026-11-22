//! `rzctl` (T1.8): headless diagnostics from the hub's point of view.
//!
//! ```text
//! rzctl discover [--secs 5]              list agents/hubs seen over mDNS
//! rzctl pair <host:port>                 pair with an agent (shows the code; confirm on the agent)
//! rzctl sources <host:port>              list the agent's sources
//! rzctl stats <host:port> [--source N]   subscribe, receive frames, print fps / bitrate / latency per second
//! rzctl poke <host:port> [--source N]    input test: circle the pointer, type via HID keys and as Unicode text
//! rzctl trusted | forget <sha256:…>      the hub's trust store
//! rzctl fingerprint                      this hub's fingerprint
//! ```
//! Uses the hub identity (`~/Library/Application Support/Rozhledna/hub`, or `--state-dir`).

use std::net::{SocketAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rz_net::control::{Control, ping_now, sample_from_pong};
use rz_net::discovery::Discovery;
use rz_net::handshake::{Authenticated, HubUi, LocalInfo, PairAsk, hub_handshake};
use rz_net::identity::{Identity, default_dir};
use rz_net::trust::TrustStore;
use rz_net::{Connection, Fingerprint, client_endpoint_mtls, connect, video};
use rz_proto::clock::{ClockSync, now_ns};
use rz_proto::pb::{self, envelope::Msg};

#[derive(Parser)]
#[command(name = "rzctl", about = "Rozhledna diagnostics")]
struct Args {
    #[arg(long, global = true)]
    state_dir: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    Discover {
        #[arg(long, default_value_t = 5)]
        secs: u64,
    },
    Pair {
        addr: String,
    },
    Sources {
        addr: String,
    },
    Stats {
        addr: String,
        /// Source id (default: the first one).
        #[arg(long)]
        source: Option<u64>,
        #[arg(long, default_value_t = 10)]
        secs: u64,
        #[arg(long, default_value_t = 2560)]
        max_w: u32,
        #[arg(long, default_value_t = 1440)]
        max_h: u32,
        #[arg(long, default_value_t = 60)]
        fps: u32,
    },
    Poke {
        addr: String,
        #[arg(long)]
        source: Option<u64>,
    },
    Trusted,
    Forget {
        fingerprint: String,
    },
    Fingerprint,
}

struct Terminal;

impl HubUi for Terminal {
    fn show_code(&self, ask: PairAsk) {
        eprintln!(
            "Párování s „{}“\n  otisk agenta: {}\n  kód: {} {}\nPotvrď na agentovi (stejný kód)…",
            ask.peer_name,
            ask.peer_fp,
            &ask.sas[..3],
            &ask.sas[3..]
        );
    }

    async fn confirm_agent(&self, ask: PairAsk) -> bool {
        let q = format!(
            "Agent „{}“ tenhle hub zná, ale hub jeho otisk ne:\n  {}\n  kód: {} {}\nDůvěřovat? [a/N] ",
            ask.peer_name,
            ask.peer_fp,
            &ask.sas[..3],
            &ask.sas[3..]
        );
        tokio::task::spawn_blocking(move || {
            use std::io::{BufRead, Write};
            eprint!("{q}");
            let _ = std::io::stderr().flush();
            let mut l = String::new();
            let _ = std::io::stdin().lock().read_line(&mut l);
            matches!(l.trim().to_lowercase().as_str(), "a" | "ano" | "y" | "yes")
        })
        .await
        .unwrap_or(false)
    }
}

struct Hub {
    id: Identity,
    trust: Mutex<TrustStore>,
    info: LocalInfo,
}

impl Hub {
    fn open(dir: Option<PathBuf>) -> Result<Self> {
        let dir = match dir {
            Some(d) => d,
            None => default_dir("hub")?,
        };
        let id = Identity::load_or_create(&dir)?;
        let trust = Mutex::new(TrustStore::load(&dir)?);
        let name = format!("{} (rzctl)", rz_net::discovery::hostname());
        Ok(Self { id, trust, info: LocalInfo { name, is_agent: false, is_hub: true, features: vec![] } })
    }

    async fn connect(&self, addr: &str) -> Result<(Connection, Control, Authenticated)> {
        let addr = resolve(addr)?;
        let ep = client_endpoint_mtls(&self.id)?;
        let conn = connect(&ep, addr).await?;
        let (send, recv) = conn.open_bi().await?;
        let mut ctl = Control::new(send, recv);
        let auth = hub_handshake(&mut ctl, &conn, &self.info, &self.id.fingerprint, &self.trust, &Terminal).await?;
        Ok((conn, ctl, auth))
    }
}

fn resolve(s: &str) -> Result<SocketAddr> {
    let s = if s.contains(':') { s.to_string() } else { format!("{s}:{}", rz_proto::DEFAULT_PORT) };
    s.to_socket_addrs()?.next().with_context(|| format!("cannot resolve {s}"))
}

/// Next reply to `seq`; `AgentStatus` and stats are printed, other unsolicited messages skipped.
async fn reply(ctl: &mut Control, seq: u64) -> Result<Msg> {
    loop {
        let e =
            tokio::time::timeout(Duration::from_secs(10), ctl.recv()).await?.context("agent closed")?.context("eof")?;
        match e.msg {
            Some(Msg::AgentStatus(s)) => print_status(&s),
            Some(m) if e.reply_to == seq => return Ok(m),
            _ => {}
        }
    }
}

fn print_status(s: &pb::AgentStatus) {
    let m = |b| if b { "✓" } else { "✗" };
    eprintln!(
        "agent: screen recording {}  accessibility {}{}{}",
        m(s.screen_recording),
        m(s.accessibility),
        if s.screen_locked { "  ZAMČENO" } else { "" },
        if s.display_asleep { "  displej spí" } else { "" }
    );
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn".into()))
        .with_writer(std::io::stderr)
        .init();
    let args = Args::parse();
    match args.cmd {
        Cmd::Discover { secs } => {
            let d = Discovery::new()?;
            let mut rx = d.browse()?;
            let end = tokio::time::sleep(Duration::from_secs(secs));
            tokio::pin!(end);
            let mut seen = std::collections::BTreeMap::new();
            loop {
                tokio::select! {
                    _ = &mut end => break,
                    ev = rx.recv() => match ev {
                        Some(rz_net::discovery::Discovered::Found(p)) => { seen.insert(p.fullname.clone(), p); }
                        Some(rz_net::discovery::Discovered::Lost { fullname }) => { seen.remove(&fullname); }
                        None => break,
                    }
                }
            }
            for p in seen.values() {
                let addrs: Vec<String> = p.addrs.iter().map(|a| a.to_string()).collect();
                println!("{:<24} {:<8} {:<12} fp={}  {}", p.name, p.os, p.roles.join(","), p.fp_short, addrs.join(" "));
            }
            if seen.is_empty() {
                eprintln!("nic nenalezeno (Local Network povoleno? stejná síť?)");
            }
        }
        Cmd::Pair { addr } => {
            let hub = Hub::open(args.state_dir)?;
            let (conn, _ctl, auth) = hub.connect(&addr).await?;
            println!(
                "{} „{}“ {} — oprávnění: {}",
                if auth.newly_paired { "spárováno s" } else { "už spárováno:" },
                auth.peer.name,
                auth.peer_fp,
                auth.granted.join(", ")
            );
            conn.close(0u32.into(), b"bye");
        }
        Cmd::Sources { addr } => {
            let hub = Hub::open(args.state_dir)?;
            let (conn, mut ctl, auth) = hub.connect(&addr).await?;
            eprintln!("agent „{}“", auth.peer.name);
            let seq = ctl.send(Msg::ListSources(pb::ListSources { include_windows: false })).await?;
            match reply(&mut ctl, seq).await? {
                Msg::SourceList(l) => {
                    for s in l.sources {
                        let r = s.rect.unwrap_or_default();
                        println!(
                            "{:>10}  {:<40} {}×{} px @{:.0} Hz  at ({:.0},{:.0}) {:.0}×{:.0} pt ×{:.1}",
                            s.id, s.title, s.native_w, s.native_h, s.refresh_hz, r.x, r.y, r.w, r.h, s.scale
                        );
                    }
                }
                Msg::Error(e) => bail!("agent error {}: {}", e.code, e.message),
                m => bail!("unexpected {m:?}"),
            }
            conn.close(0u32.into(), b"bye");
        }
        Cmd::Stats { addr, source, secs, max_w, max_h, fps } => {
            let hub = Hub::open(args.state_dir)?;
            let (conn, mut ctl, auth) = hub.connect(&addr).await?;
            eprintln!("agent „{}“", auth.peer.name);
            let source = match source {
                Some(s) => s,
                None => {
                    let seq = ctl.send(Msg::ListSources(pb::ListSources { include_windows: false })).await?;
                    match reply(&mut ctl, seq).await? {
                        Msg::SourceList(l) => l.sources.first().context("agent has no sources")?.id,
                        Msg::Error(e) => bail!("agent error {}: {}", e.code, e.message),
                        m => bail!("unexpected {m:?}"),
                    }
                }
            };
            let sub = pb::Subscribe { source_id: source, max_w, max_h, max_fps: fps, ..Default::default() };
            let seq = ctl.send(Msg::Subscribe(sub)).await?;
            let ack = match reply(&mut ctl, seq).await? {
                Msg::SubscribeAck(a) => a,
                Msg::Error(e) => bail!("agent error {}: {}", e.code, e.message),
                m => bail!("unexpected {m:?}"),
            };
            eprintln!("stream {}: {}×{} @{} fps", ack.stream_id, ack.w, ack.h, ack.fps);
            stats_loop(conn, ctl, secs).await?;
        }
        Cmd::Poke { addr, source } => {
            let hub = Hub::open(args.state_dir)?;
            let (conn, mut ctl, auth) = hub.connect(&addr).await?;
            eprintln!("agent „{}“ — oprávnění {}", auth.peer.name, auth.granted.join(", "));
            let source = match source {
                Some(s) => s,
                None => {
                    let seq = ctl.send(Msg::ListSources(pb::ListSources { include_windows: false })).await?;
                    match reply(&mut ctl, seq).await? {
                        Msg::SourceList(l) => l.sources.first().context("agent has no sources")?.id,
                        m => bail!("unexpected {m:?}"),
                    }
                }
            };
            poke(&conn, &mut ctl, source).await?;
            conn.close(0u32.into(), b"bye");
        }
        Cmd::Trusted => {
            let hub = Hub::open(args.state_dir)?;
            println!("tento hub: {}", hub.id.fingerprint);
            for p in hub.trust.lock().unwrap().peers() {
                println!("{}  {}", p.fingerprint, p.name);
            }
        }
        Cmd::Forget { fingerprint } => {
            let hub = Hub::open(args.state_dir)?;
            let fp = Fingerprint::parse(&fingerprint)?;
            let had = hub.trust.lock().unwrap().remove(&fp)?;
            println!("{}", if had { "zapomenuto" } else { "nebyl v trust store" });
        }
        Cmd::Fingerprint => println!("{}", Hub::open(args.state_dir)?.id.fingerprint),
    }
    Ok(())
}

/// Receives frames, pings every second, prints a line per second.
async fn stats_loop(conn: Connection, ctl: Control, secs: u64) -> Result<()> {
    let (mut tx, mut rx) = ctl.split();
    let sync = std::sync::Arc::new(Mutex::new(ClockSync::default()));
    let s2 = sync.clone();
    tokio::spawn(async move {
        while let Ok(Some(e)) = rx.recv().await {
            match e.msg {
                Some(Msg::Pong(p)) => s2.lock().unwrap().add(sample_from_pong(&p)),
                Some(Msg::AgentStatus(s)) => print_status(&s),
                Some(Msg::StreamStats(s)) => eprintln!(
                    "  agent: {} fps sent, {} kb/s, dropped {}",
                    s.fps, s.bitrate_kbps, s.dropped_before_encode
                ),
                _ => {}
            }
        }
    });
    let end = Instant::now() + Duration::from_secs(secs);
    let mut window_start = Instant::now();
    let (mut n, mut bytes, mut kf) = (0u32, 0u64, 0u32);
    let mut lat: Vec<f64> = Vec::new();
    tx.send(ping_now()).await?;
    while Instant::now() < end {
        let Ok(s) = tokio::time::timeout(Duration::from_millis(200), conn.accept_uni()).await else {
            continue;
        };
        let (h, p) = video::recv_frame(s?).await?;
        let recv = now_ns();
        n += 1;
        bytes += p.len() as u64;
        kf += h.is_keyframe() as u32;
        if let Some(t) = sync.lock().unwrap().remote_to_local(h.capture_ts_ns) {
            lat.push(recv.saturating_sub(t) as f64 / 1e6);
        }
        if window_start.elapsed() >= Duration::from_secs(1) {
            let dt = window_start.elapsed().as_secs_f64();
            lat.sort_by(f64::total_cmp);
            let p50 = lat.get(lat.len() / 2).copied().unwrap_or(f64::NAN);
            println!(
                "{:5.1} fps  {:6.2} Mb/s  keyframes {}  capture→recv P50 {:.1} ms",
                n as f64 / dt,
                bytes as f64 * 8.0 / dt / 1e6,
                kf,
                p50
            );
            (n, bytes, kf) = (0, 0, 0);
            lat.clear();
            window_start = Instant::now();
            tx.send(ping_now()).await?;
        }
    }
    conn.close(0u32.into(), b"bye");
    Ok(())
}

/// Circles the pointer around the centre of `source` for 2 s, then types "ahoj " with HID keys (layout dependent)
/// and "Rozhledna ěščřžýáíé ✓" as Unicode text. Watch the agent's screen; needs Accessibility on the agent.
async fn poke(conn: &Connection, ctl: &mut Control, source: u64) -> Result<()> {
    use rz_proto::datagram::Datagram;
    use rz_proto::framing::write_msg;
    use rz_proto::pb::input_event::Ev;
    let ev = |e: Ev| pb::InputEvent { source_id: source, ev: Some(e) };
    let t0 = Instant::now();
    let mut seq = 0u32;
    while t0.elapsed() < Duration::from_secs(2) {
        let a = t0.elapsed().as_secs_f32() * std::f32::consts::TAU;
        seq += 1;
        let d = Datagram::PointerMotion { source_id: source, seq, x: 0.5 + 0.1 * a.cos(), y: 0.5 + 0.1 * a.sin() };
        conn.send_datagram(d.encode().into())?;
        tokio::time::sleep(Duration::from_millis(8)).await;
    }
    ctl.send(ping_now()).await?;
    let mut s = conn.open_uni().await?;
    // a h o j space (HID usages 0x04 0x0B 0x12 0x0D 0x2C).
    for u in [0x04u32, 0x0B, 0x12, 0x0D, 0x2C] {
        for down in [true, false] {
            write_msg(&mut s, &ev(Ev::Key(pb::Key { hid_usage: u, down, modifiers: 0 }))).await?;
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
    }
    write_msg(&mut s, &ev(Ev::Text(pb::Text { utf8: "Rozhledna ěščřžýáíé ✓".into() }))).await?;
    write_msg(&mut s, &ev(Ev::ReleaseAll(true))).await?;
    s.finish()?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    eprintln!(
        "hotovo: kurzor kroužil uprostřed zdroje {source}, napsáno „ahoj Rozhledna ěščřžýáíé ✓“ do aktivního okna"
    );
    Ok(())
}
