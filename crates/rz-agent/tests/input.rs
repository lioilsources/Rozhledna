//! Input end-to-end on loopback (T1.3) with the null injector: keys + modifiers, click and datagram motion mapped to
//! global coordinates, stuck-key release after silence, release on disconnect.

use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;
use rz_agent::sources::{Backend, Registry};
use rz_agent::{Agent, Limits};
use rz_input::{Action, NullInjector};
use rz_net::control::Control;
use rz_net::handshake::{AgentUi, HubUi, LocalInfo, PairAsk, hub_handshake};
use rz_net::trust::TrustStore;
use rz_net::{Connection, Identity, client_endpoint_mtls, connect};
use rz_proto::datagram::Datagram;
use rz_proto::framing::write_msg;
use rz_proto::pb::{self, envelope::Msg, input_event::Ev};

struct Accept;
impl AgentUi for Accept {
    async fn confirm_hub(&self, _: PairAsk) -> bool {
        true
    }
}
struct HubSide;
impl HubUi for HubSide {
    fn show_code(&self, _: PairAsk) {}
    async fn confirm_agent(&self, _: PairAsk) -> bool {
        true
    }
}

async fn setup() -> Result<(NullInjector, Connection, Control)> {
    let null = NullInjector::default();
    // Testpattern displays are 1344×756 pt side by side: display 2 starts at x = 1344.
    let registry = Registry::new(Backend::Testpattern { w: 1344, h: 756, count: 2 });
    let agent = Agent::new(
        Identity::generate()?,
        "a",
        TrustStore::memory(),
        registry,
        Limits::default(),
        Accept,
        Box::new(null.clone()),
    );
    let ep = rz_net::server_endpoint_mtls("127.0.0.1:0".parse()?, &agent.identity)?;
    let addr = ep.local_addr()?;
    tokio::spawn(agent.clone().accept_loop(ep));
    let hub = Identity::generate()?;
    let conn = connect(&client_endpoint_mtls(&hub)?, addr).await?;
    let (s, r) = conn.open_bi().await?;
    let mut ctl = Control::new(s, r);
    let info = LocalInfo { name: "h".into(), is_agent: false, is_hub: true, features: vec![] };
    hub_handshake(&mut ctl, &conn, &info, &hub.fingerprint, &Mutex::new(TrustStore::memory()), &HubSide).await?;
    Ok((null, conn, ctl))
}

fn ev(source_id: u64, e: Ev) -> pb::InputEvent {
    pb::InputEvent { source_id, ev: Some(e) }
}

fn key(hid: u32, down: bool) -> Ev {
    Ev::Key(pb::Key { hid_usage: hid, down, modifiers: 0 })
}

async fn wait_for(null: &NullInjector, n: usize) -> Vec<(Action, rz_input::Mods)> {
    for _ in 0..100 {
        if null.log.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    null.log.lock().unwrap().clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn keys_click_motion_and_release_on_disconnect() -> Result<()> {
    let (null, conn, _ctl) = setup().await?;
    let mut input = conn.open_uni().await?;
    write_msg(&mut input, &ev(2, key(0xE3, true))).await?; // Cmd down
    write_msg(&mut input, &ev(2, key(0x06, true))).await?; // C down
    write_msg(&mut input, &ev(2, Ev::Button(pb::Button { button: 0, down: true, x: 0.5, y: 0.5, click_count: 1 })))
        .await?;
    let log = wait_for(&null, 3).await;
    assert_eq!(log[1].0, Action::Key { hid: 0x06, down: true });
    assert!(log[1].1.cmd, "C carries Cmd");
    let Action::Button { x, y, down: true, .. } = log[2].0 else { panic!("{:?}", log[2]) };
    assert!((x - (1344.0 + 0.5 * 1343.5)).abs() < 0.01 && (y - 0.5 * 755.5).abs() < 0.01, "({x},{y}) on display 2");

    conn.send_datagram(Datagram::PointerMotion { source_id: 2, seq: 5, x: 0.25, y: 0.0 }.encode().into())?;
    conn.send_datagram(Datagram::PointerMotion { source_id: 2, seq: 4, x: 0.9, y: 0.9 }.encode().into())?; // stale
    let log = wait_for(&null, 4).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    let moves: Vec<_> =
        null.log.lock().unwrap().iter().filter(|(a, _)| matches!(a, Action::Move { .. })).cloned().collect();
    assert_eq!(moves.len(), 1, "older datagram ignored: {log:?}");
    assert!(matches!(moves[0].0, Action::Move { buttons: 1, .. }), "drag while the button is held");

    // Disconnect with Cmd, C and the left button held → all released.
    conn.close(0u32.into(), b"bye");
    let log = wait_for(&null, 7).await;
    let tail: Vec<Action> = log[4..].iter().map(|(a, _)| a.clone()).collect();
    assert!(tail.contains(&Action::Key { hid: 0x06, down: false }));
    assert!(tail.contains(&Action::Key { hid: 0xE3, down: false }));
    assert!(tail.iter().any(|a| matches!(a, Action::Button { button: 0, down: false, .. })));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn stuck_key_released_after_silence() -> Result<()> {
    let (null, conn, mut ctl) = setup().await?;
    let mut input = conn.open_uni().await?;
    write_msg(&mut input, &ev(1, key(0xE1, true))).await?; // Shift down, then silence
    wait_for(&null, 1).await;
    // Pings within the window keep it held.
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_millis(400)).await;
        ctl.send(Msg::Ping(pb::Ping { t0_ns: 0 })).await?;
    }
    assert_eq!(null.log.lock().unwrap().len(), 1, "keep-alive pings: still held");
    tokio::time::sleep(Duration::from_millis(1600)).await;
    let log = null.log.lock().unwrap().clone();
    assert_eq!(log.last().unwrap().0, Action::Key { hid: 0xE1, down: false }, "released after > 1 s of silence");
    Ok(())
}
