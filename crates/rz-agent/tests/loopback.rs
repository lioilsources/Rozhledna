//! Agent end-to-end on loopback with the testpattern backend (plan 12): pairing, ListSources, two streams at once,
//! RequestKeyframe, Unsubscribe, unknown source, AgentStatus. Needs VideoToolbox (macOS), no TCC permissions.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use rz_agent::sources::{Backend, Registry};
use rz_agent::{Agent, Limits, err};
use rz_net::control::Control;
use rz_net::handshake::{AgentUi, HubUi, LocalInfo, PairAsk, hub_handshake};
use rz_net::trust::TrustStore;
use rz_net::{Identity, client_endpoint_mtls, connect, video};
use rz_proto::pb::{self, envelope::Msg};

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

#[tokio::test(flavor = "multi_thread")]
async fn two_streams_keyframe_unsubscribe() -> Result<()> {
    let registry = Registry::new(Backend::Testpattern { w: 1344, h: 756, count: 2 });
    let agent = Agent::new(
        Identity::generate()?,
        "test-agent",
        TrustStore::memory(),
        registry,
        Limits::default(),
        Accept,
        Box::new(rz_input::NullInjector::default()),
    );
    let ep = rz_net::server_endpoint_mtls("127.0.0.1:0".parse()?, &agent.identity)?;
    let addr = ep.local_addr()?;
    tokio::spawn(agent.clone().accept_loop(ep));

    let hub_id = Identity::generate()?;
    let client = client_endpoint_mtls(&hub_id)?;
    let conn = connect(&client, addr).await?;
    let (send, recv) = conn.open_bi().await?;
    let mut ctl = Control::new(send, recv);
    let info = LocalInfo { name: "test-hub".into(), is_agent: false, is_hub: true, features: vec![] };
    let auth =
        hub_handshake(&mut ctl, &conn, &info, &hub_id.fingerprint, &Mutex::new(TrustStore::memory()), &HubSide).await?;
    assert_eq!(auth.peer.name, "test-agent");
    assert!(auth.newly_paired);

    let (mut tx, mut rx) = ctl.split();
    let (mtx, mut msgs) = tokio::sync::mpsc::unbounded_channel::<pb::Envelope>();
    tokio::spawn(async move {
        while let Ok(Some(e)) = rx.recv().await {
            if mtx.send(e).is_err() {
                break;
            }
        }
    });
    // Waits for a reply to `seq`, skipping (and keeping) unsolicited messages.
    let mut status: Option<pb::AgentStatus> = None;
    macro_rules! reply_to {
        ($seq:expr) => {{
            let seq = $seq;
            loop {
                let e = tokio::time::timeout(Duration::from_secs(5), msgs.recv()).await?.context("control closed")?;
                match e.msg {
                    Some(Msg::AgentStatus(s)) => status = Some(s),
                    Some(Msg::StreamStats(_)) => {}
                    Some(m) if e.reply_to == seq => break m,
                    other => bail!("unexpected {other:?}"),
                }
            }
        }};
    }

    let seq = tx.send(Msg::ListSources(pb::ListSources { include_windows: false })).await?;
    let Msg::SourceList(list) = reply_to!(seq) else { bail!("expected SourceList") };
    assert_eq!(list.sources.iter().map(|s| s.id).collect::<Vec<_>>(), vec![1, 2]);

    let mut acks = Vec::new();
    for src in [1u64, 2] {
        let sub = pb::Subscribe { source_id: src, max_w: 672, max_h: 672, max_fps: 30, ..Default::default() };
        let seq = tx.send(Msg::Subscribe(sub)).await?;
        let Msg::SubscribeAck(a) = reply_to!(seq) else { bail!("expected SubscribeAck") };
        assert_eq!((a.w, a.h, a.fps), (672, 378, 30), "fit 1344×756 into 672×672 keeping aspect");
        acks.push(a.stream_id);
    }
    assert_ne!(acks[0], acks[1]);

    let seq = tx.send(Msg::Subscribe(pb::Subscribe { source_id: 9, ..Default::default() })).await?;
    let Msg::Error(e) = reply_to!(seq) else { bail!("expected Error") };
    assert_eq!(e.code, err::UNKNOWN_SOURCE);
    assert!(status.is_some() || msgs.is_empty(), "AgentStatus arrives right after the handshake");

    // Frames from both streams; the first of each is keyframe + DISCONTINUITY.
    let mut per: HashMap<u32, Vec<(u64, bool)>> = HashMap::new();
    let t = Instant::now();
    while per.values().map(|v| v.len()).min().unwrap_or(0) < 10 || per.len() < 2 {
        if t.elapsed() > Duration::from_secs(10) {
            bail!("frames: {:?}", per.iter().map(|(k, v)| (k, v.len())).collect::<Vec<_>>());
        }
        let s = tokio::time::timeout(Duration::from_secs(5), conn.accept_uni()).await??;
        let (h, p) = video::recv_frame(s).await?;
        assert!(!p.is_empty());
        if h.frame_no == 0 {
            assert!(h.is_keyframe() && h.flags & rz_proto::frame::FLAG_DISCONTINUITY != 0);
        }
        per.entry(h.stream_id).or_default().push((h.frame_no, h.is_keyframe()));
    }
    assert_eq!(per.len(), 2);

    // Cursor: a CursorPos datagram for a streamed source arrives at least once a second (macOS).
    #[cfg(target_os = "macos")]
    {
        let d = tokio::time::timeout(Duration::from_secs(3), conn.read_datagram()).await??;
        match rz_proto::datagram::Datagram::decode(&d) {
            Some(rz_proto::datagram::Datagram::CursorPos { source_id, .. }) => {
                assert!(source_id == 1 || source_id == 2)
            }
            other => bail!("expected CursorPos, got {other:?}"),
        }
    }

    // RequestKeyframe on stream 1 → a later keyframe.
    let s1 = acks[0];
    let last = per[&s1].iter().map(|f| f.0).max().unwrap();
    tx.send(Msg::RequestKeyframe(pb::RequestKeyframe { stream_id: s1, last_frame_no: last })).await?;
    // Unsubscribe stream 2.
    tx.send(Msg::Unsubscribe(pb::Unsubscribe { stream_id: acks[1] })).await?;
    let t = Instant::now();
    let mut got_kf = false;
    let mut after_unsub: Vec<u32> = Vec::new();
    while t.elapsed() < Duration::from_secs(2) {
        let Ok(s) = tokio::time::timeout(Duration::from_millis(500), conn.accept_uni()).await else { continue };
        let (h, _) = video::recv_frame(s?).await?;
        if h.stream_id == s1 && h.frame_no > last && h.is_keyframe() {
            got_kf = true;
        }
        if t.elapsed() > Duration::from_millis(500) {
            after_unsub.push(h.stream_id);
        }
    }
    assert!(got_kf, "keyframe after RequestKeyframe");
    assert!(after_unsub.iter().all(|&s| s == s1), "stream 2 stopped after Unsubscribe: {after_unsub:?}");
    assert_eq!(agent.hubs().len(), 1);
    assert_eq!(agent.hubs()[0].streams, 1);

    conn.close(0u32.into(), b"done");
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(agent.hubs().is_empty(), "hub removed after disconnect");
    Ok(())
}
