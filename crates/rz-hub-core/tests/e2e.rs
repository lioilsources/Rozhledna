//! Hub core ↔ real agent (testpattern) on loopback (plan 12): pairing, sources, two tiles decoding frames with
//! correct burned-in numbers, input reaching the agent's injector, and automatic reconnect + resubscribe after the
//! agent restarts. macOS (VideoToolbox), no TCC permissions.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use rz_agent::sources::{Backend, Registry};
use rz_agent::{Agent, Limits, PairAsk};
use rz_hub_core::{AgentState, Event, Hub, TileParams, TileState};
use rz_input::{Action, NullInjector};
use rz_net::Identity;
use rz_net::handshake::AgentUi;
use rz_net::trust::TrustStore;
use rz_proto::pb;

struct Accept;
impl AgentUi for Accept {
    async fn confirm_hub(&self, _: PairAsk) -> bool {
        true
    }
}

struct RunningAgent {
    rt: tokio::runtime::Runtime,
    ep: rz_net::Endpoint,
}

fn start_agent(dir: &std::path::Path, bind: SocketAddr, null: &NullInjector) -> Result<(RunningAgent, SocketAddr)> {
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build()?;
    let id = Identity::load_or_create(dir)?;
    let trust = TrustStore::load(dir)?;
    let agent = Agent::new(
        id,
        "e2e-agent",
        trust,
        Registry::new(Backend::Testpattern { w: 1344, h: 756, count: 2 }),
        Limits::default(),
        Accept,
        Box::new(null.clone()),
    );
    let ep = rt.block_on(async { rz_net::server_endpoint_mtls(bind, &agent.identity) })?;
    let addr = ep.local_addr()?;
    rt.spawn(agent.accept_loop(ep.clone()));
    Ok((RunningAgent { rt, ep }, addr))
}

#[derive(Default)]
struct Frames {
    ok: AtomicU32,
    bad: AtomicU32,
}

fn wait<F: FnMut() -> bool>(what: &str, secs: u64, mut f: F) -> Result<()> {
    let t = Instant::now();
    while !f() {
        if t.elapsed() > Duration::from_secs(secs) {
            bail!("timed out waiting for {what}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

#[test]
fn pair_stream_input_reconnect() -> Result<()> {
    let dir = std::env::temp_dir().join(format!("rz-hub-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let null = NullInjector::default();
    let (agent, addr) = start_agent(&dir, "127.0.0.1:0".parse()?, &null)?;

    let frames: Arc<Mutex<HashMap<u64, Arc<Frames>>>> = Arc::default();
    let f2 = frames.clone();
    let sink: rz_hub_core::FrameSink = Arc::new(move |tile, frame, rec| {
        let f = f2.lock().unwrap().entry(tile).or_default().clone();
        let n = frame.pixel_buffer.with_luma(|l| rz_capture::testpattern::decode_frame_no(l));
        assert_eq!((frame.width, frame.height), (rec.width as u32, rec.height as u32));
        if n.is_some() {
            f.ok.fetch_add(1, Ordering::Relaxed)
        } else {
            f.bad.fetch_add(1, Ordering::Relaxed)
        };
    });
    let hub = Arc::new(Hub::new(Identity::generate()?, TrustStore::memory(), "e2e-hub", sink)?);
    let events: Arc<Mutex<Vec<Event>>> = Arc::default();
    {
        let (hub, events) = (hub.clone(), events.clone());
        std::thread::spawn(move || {
            loop {
                if let Some(e) = hub.next_event(Duration::from_millis(100)) {
                    events.lock().unwrap().push(e);
                }
                if Arc::strong_count(&hub) == 1 {
                    break;
                }
            }
        });
    }
    let has = |p: &dyn Fn(&Event) -> bool| events.lock().unwrap().iter().any(p);
    let count = |p: &dyn Fn(&Event) -> bool| events.lock().unwrap().iter().filter(|e| p(e)).count();

    let a = hub.add_agent(addr);
    wait("pairing + sources", 10, || has(&|e| matches!(e, Event::Sources { sources, .. } if sources.len() == 2)))?;
    assert!(has(&|e| matches!(e, Event::PairCode { .. })), "first contact shows a code");
    assert!(has(&|e| matches!(e, Event::AgentInfo { name, newly_paired: true, .. } if name == "e2e-agent")));
    assert_eq!(hub.sources(a).len(), 2);

    let params = |src| TileParams { source_id: src, max_w: 1344, max_h: 756, fps: 30, focus: false, bitrate_kbps: 0 };
    let t1 = hub.subscribe(a, params(1))?;
    let t2 = hub.subscribe(a, params(2))?;
    let ok = |t: u64| frames.lock().unwrap().get(&t).map_or(0, |f| f.ok.load(Ordering::Relaxed));
    wait("frames on both tiles", 10, || ok(t1) >= 15 && ok(t2) >= 15)?;
    assert!(has(
        &|e| matches!(e, Event::TileState { tile, state: TileState::Live, w: 1344, h: 756, .. } if *tile == t1)
    ));
    wait("tile stats", 3, || has(&|e| matches!(e, Event::TileStats { tile, fps, .. } if *tile == t1 && *fps > 10.0)))?;
    wait("agent status", 3, || has(&|e| matches!(e, Event::Status { .. })))?;

    // Input: a key on tile 2, pointer motion; the agent's null injector sees them on display 2 (x ≥ 1344).
    hub.input(t2, pb::input_event::Ev::Key(pb::Key { hid_usage: 0x04, down: true, modifiers: 0 }));
    hub.pointer(t2, 0.5, 0.5);
    hub.release_all(t2);
    wait("input on the agent", 5, || {
        let log = null.log.lock().unwrap();
        log.iter().any(|(a, _)| *a == Action::Key { hid: 0x04, down: false })
            && log.iter().any(|(a, _)| matches!(a, Action::Move { x, .. } if *x > 1344.0))
    })?;

    // Agent restart (same identity and trust store, same port) → reconnect and resubscribe on its own.
    let connected_before = count(&|e| matches!(e, Event::AgentState { state: AgentState::Connected, .. }));
    let RunningAgent { rt, ep } = agent;
    ep.close(0u32.into(), b"restart");
    drop(ep);
    drop(rt);
    wait("disconnect noticed", 15, || {
        has(&|e| matches!(e, Event::AgentState { state: AgentState::Disconnected, .. }))
    })?;
    let (before1, before2) = (ok(t1), ok(t2));
    let (_agent2, _) = start_agent(&dir, addr, &null)?;
    wait("reconnected", 20, || {
        count(&|e| matches!(e, Event::AgentState { state: AgentState::Connected, .. })) > connected_before
    })?;
    assert_eq!(count(&|e| matches!(e, Event::PairCode { .. })), 1, "reconnect needs no new pairing");
    wait("frames after reconnect", 10, || ok(t1) >= before1 + 15 && ok(t2) >= before2 + 15)?;

    let bad: u32 = frames.lock().unwrap().values().map(|f| f.bad.load(Ordering::Relaxed)).sum();
    assert_eq!(bad, 0, "every decoded frame carries a readable frame number");
    drop(hub);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
