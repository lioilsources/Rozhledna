//! Hub → agent input (plan 6.7, T1.3): reliable events on uni streams (`u32 LE` + `InputEvent`), pointer motion
//! in datagrams. One [`rz_input::InputState`] per hub; everything that hub holds is released when it disconnects,
//! sends `release_all`, or goes silent for [`STUCK_AFTER`] while something is pressed.
//!
//! Keep-alive: while a key or button is held, the hub must send something (a `Ping` is enough) at least every
//! 500 ms; any control message, input event or datagram counts.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rz_input::{Injector, InputState, SourceRect};
use rz_net::Connection;
use rz_proto::clock::now_ns;
use rz_proto::datagram::{Datagram, seq_newer};
use rz_proto::framing::read_msg;
use rz_proto::pb;

pub const STUCK_AFTER: Duration = Duration::from_secs(1);

pub type SharedInjector = Arc<Mutex<Box<dyn Injector>>>;
pub type Rects = Arc<Mutex<HashMap<u64, SourceRect>>>;

/// Input side of one hub connection.
pub struct HubInput {
    state: Mutex<InputState>,
    injector: SharedInjector,
    rects: Rects,
    /// `now_ns()` of the last message from the hub.
    last_activity: AtomicU64,
    allowed: bool,
    last_motion_seq: Mutex<Option<u32>>,
}

impl HubInput {
    pub fn new(injector: SharedInjector, rects: Rects, allowed: bool) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(InputState::default()),
            injector,
            rects,
            last_activity: AtomicU64::new(now_ns()),
            allowed,
            last_motion_seq: Mutex::new(None),
        })
    }

    pub fn touch(&self) {
        self.last_activity.store(now_ns(), Ordering::Relaxed);
    }

    fn rect(&self, source_id: u64) -> Option<SourceRect> {
        self.rects.lock().unwrap().get(&source_id).copied()
    }

    pub fn event(&self, ev: &pb::InputEvent) {
        self.touch();
        if !self.allowed {
            return;
        }
        let rect = self.rect(ev.source_id);
        let mut st = self.state.lock().unwrap();
        let actions = st.event(ev, rect);
        st.apply(&mut **self.injector.lock().unwrap(), &actions);
    }

    pub fn datagram(&self, d: &[u8]) {
        self.touch();
        if !self.allowed {
            return;
        }
        let Some(Datagram::PointerMotion { source_id, seq, x, y }) = Datagram::decode(d) else { return };
        {
            // Datagrams can arrive out of order; an older position must not jump the pointer back.
            let mut last = self.last_motion_seq.lock().unwrap();
            if let Some(l) = *last
                && !seq_newer(seq, l)
            {
                return;
            }
            *last = Some(seq);
        }
        let Some(rect) = self.rect(source_id) else { return };
        let mut st = self.state.lock().unwrap();
        let actions = st.motion(rect, x, y);
        st.apply(&mut **self.injector.lock().unwrap(), &actions);
    }

    pub fn release_all(&self, why: &str) {
        let mut st = self.state.lock().unwrap();
        if !st.has_pressed() {
            return;
        }
        let actions = st.release_all();
        tracing::info!(n = actions.len(), "releasing held input: {why}");
        st.apply(&mut **self.injector.lock().unwrap(), &actions);
    }

    fn stuck(&self) -> bool {
        let idle = now_ns().saturating_sub(self.last_activity.load(Ordering::Relaxed));
        idle > STUCK_AFTER.as_nanos() as u64 && self.state.lock().unwrap().has_pressed()
    }
}

/// Runs until the connection closes: input streams, datagrams, stuck-key watchdog. Releases everything at the end.
pub async fn run(conn: Connection, input: Arc<HubInput>) {
    let streams = {
        let (conn, input) = (conn.clone(), input.clone());
        async move {
            while let Ok(mut s) = conn.accept_uni().await {
                let input = input.clone();
                tokio::spawn(async move {
                    loop {
                        match read_msg::<_, pb::InputEvent>(&mut s).await {
                            Ok(Some(ev)) => input.event(&ev),
                            Ok(None) => break,
                            Err(e) => {
                                tracing::debug!("input stream: {e}");
                                break;
                            }
                        }
                    }
                });
            }
        }
    };
    let datagrams = {
        let (conn, input) = (conn.clone(), input.clone());
        async move {
            while let Ok(d) = conn.read_datagram().await {
                input.datagram(&d);
            }
        }
    };
    let watchdog = {
        let input = input.clone();
        async move {
            let mut iv = tokio::time::interval(Duration::from_millis(250));
            loop {
                iv.tick().await;
                if input.stuck() {
                    input.release_all("hub silent for > 1 s");
                }
            }
        }
    };
    tokio::select! {
        _ = streams => {}
        _ = datagrams => {}
        _ = watchdog => {}
    }
    input.release_all("hub disconnected");
}
