//! Loopback pairing (T1.1): first contact with SAS, reconnect without prompt, decline, hub-side confirm,
//! client without certificate.

use std::sync::{Arc, Mutex};

use anyhow::Result;
use rz_net::control::Control;
use rz_net::handshake::{AgentUi, Authenticated, HubUi, LocalInfo, PairAsk, agent_handshake, hub_handshake};
use rz_net::trust::TrustStore;
use rz_net::{Identity, client_endpoint, client_endpoint_mtls, connect, server_endpoint_mtls};

#[derive(Default)]
struct Ui {
    accept: bool,
    asked: Mutex<Vec<PairAsk>>,
}

impl AgentUi for Ui {
    async fn confirm_hub(&self, ask: PairAsk) -> bool {
        self.asked.lock().unwrap().push(ask);
        self.accept
    }
}

impl HubUi for Ui {
    fn show_code(&self, ask: PairAsk) {
        self.asked.lock().unwrap().push(ask);
    }
    async fn confirm_agent(&self, ask: PairAsk) -> bool {
        self.asked.lock().unwrap().push(ask);
        self.accept
    }
}

fn info(name: &str, agent: bool) -> LocalInfo {
    LocalInfo { name: name.into(), is_agent: agent, is_hub: !agent, features: vec![] }
}

struct Node {
    id: Identity,
    trust: Mutex<TrustStore>,
}

fn node() -> Node {
    Node { id: Identity::generate().unwrap(), trust: Mutex::new(TrustStore::memory()) }
}

/// One connection: agent handshake on the server side, hub handshake on the client side.
async fn run(
    agent: &Arc<Node>,
    agent_ui: Arc<Ui>,
    hub: &Node,
    hub_ui: &Ui,
) -> (Result<Authenticated>, Result<Authenticated>) {
    let server = server_endpoint_mtls("127.0.0.1:0".parse().unwrap(), &agent.id).unwrap();
    let addr = server.local_addr().unwrap();
    let a = agent.clone();
    let srv = tokio::spawn(async move {
        let conn = server.accept().await.unwrap().await.unwrap();
        let (send, recv) = conn.accept_bi().await.unwrap();
        let mut ctl = Control::new(send, recv);
        let r =
            agent_handshake(&mut ctl, &conn, &info("agent-m2", true), &a.id.fingerprint, &a.trust, &*agent_ui).await;
        // Keep the connection open until the hub has read the last reply.
        let _ = tokio::time::timeout(std::time::Duration::from_secs(2), conn.closed()).await;
        r
    });
    let client = client_endpoint_mtls(&hub.id).unwrap();
    let conn = connect(&client, addr).await.unwrap();
    let (send, recv) = conn.open_bi().await.unwrap();
    let mut ctl = Control::new(send, recv);
    let h = hub_handshake(&mut ctl, &conn, &info("hub-mb", false), &hub.id.fingerprint, &hub.trust, hub_ui).await;
    conn.close(0u32.into(), b"done");
    (srv.await.unwrap(), h)
}

#[tokio::test(flavor = "multi_thread")]
async fn first_contact_then_reconnect() -> Result<()> {
    let agent = Arc::new(node());
    let hub = node();
    let aui = Arc::new(Ui { accept: true, ..Default::default() });
    let hui = Ui::default();

    let (a, h) = run(&agent, aui.clone(), &hub, &hui).await;
    let (a, h) = (a?, h?);
    assert!(a.newly_paired && h.newly_paired);
    assert_eq!(a.peer_fp, hub.id.fingerprint);
    assert_eq!(h.peer_fp, agent.id.fingerprint);
    assert_eq!(h.peer.name, "agent-m2");
    assert_eq!(h.granted, vec!["view", "control", "clipboard", "window_control"]);
    let (asked_a, asked_h) = (aui.asked.lock().unwrap().clone(), hui.asked.lock().unwrap().clone());
    assert_eq!(asked_a.len(), 1);
    assert_eq!(asked_h.len(), 1);
    assert_eq!(asked_a[0].sas, asked_h[0].sas, "both screens show the same code");
    assert_eq!(asked_a[0].peer_name, "hub-mb");
    assert!(agent.trust.lock().unwrap().get(&hub.id.fingerprint).is_some());
    assert_eq!(hub.trust.lock().unwrap().get(&agent.id.fingerprint).unwrap().name, "agent-m2");

    // Second connection: trusted both ways, no prompt.
    let (a, h) = run(&agent, aui.clone(), &hub, &hui).await;
    assert!(!a?.newly_paired && !h?.newly_paired);
    assert_eq!(aui.asked.lock().unwrap().len(), 1);
    assert_eq!(hui.asked.lock().unwrap().len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn agent_declines() -> Result<()> {
    let agent = Arc::new(node());
    let hub = node();
    let (a, h) = run(&agent, Arc::new(Ui { accept: false, ..Default::default() }), &hub, &Ui::default()).await;
    assert!(a.is_err());
    assert!(h.unwrap_err().to_string().contains("declined"));
    assert!(agent.trust.lock().unwrap().peers().is_empty());
    assert!(hub.trust.lock().unwrap().peers().is_empty());
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn hub_forgot_agent() -> Result<()> {
    let agent = Arc::new(node());
    let hub = node();
    let aui = Arc::new(Ui { accept: true, ..Default::default() });
    let (a, h) = run(&agent, aui.clone(), &hub, &Ui::default()).await;
    a?;
    h?;
    hub.trust.lock().unwrap().remove(&agent.id.fingerprint)?;

    // Agent still trusts the hub → the hub's user confirms locally; the agent is not asked again.
    let hui = Ui { accept: true, ..Default::default() };
    let (a, h) = run(&agent, aui.clone(), &hub, &hui).await;
    assert!(!a?.newly_paired);
    assert!(h?.newly_paired);
    assert_eq!(hui.asked.lock().unwrap().len(), 1);
    assert_eq!(aui.asked.lock().unwrap().len(), 1);
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn client_without_certificate_is_rejected() -> Result<()> {
    let agent = node();
    let server = server_endpoint_mtls("127.0.0.1:0".parse()?, &agent.id)?;
    let addr = server.local_addr()?;
    tokio::spawn(async move {
        if let Some(inc) = server.accept().await {
            let _ = inc.await;
        }
    });
    let client = client_endpoint(true)?;
    match connect(&client, addr).await {
        Err(_) => {} // rejected during the handshake
        Ok(conn) => {
            // TLS 1.3: the client can finish before the server checks its (missing) certificate; the server then
            // closes the connection with a TLS alert.
            let reason = tokio::time::timeout(std::time::Duration::from_secs(3), conn.closed()).await?;
            let s = reason.to_string();
            assert!(s.contains("certificate") || s.contains("crypto") || s.contains("TLS"), "unexpected close: {s}");
        }
    }
    Ok(())
}
