//! Events from the hub core to the UI. Serialized as JSON (`{"type": "...", ...}`) over the FFI.

use rz_proto::pb;
use serde::Serialize;

pub type AgentId = u64;
pub type TileId = u64;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentState {
    Connecting,
    /// Waiting for the agent's user to confirm the pairing code.
    Pairing,
    Connected,
    /// Lost; will retry (backoff).
    Disconnected,
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TileState {
    /// Waiting for the agent connection.
    Waiting,
    Live,
    /// The agent refused or failed (detail says why); retried after reconnect or unlock.
    Error,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct SourceInfo {
    pub id: u64,
    pub kind: &'static str,
    pub title: String,
    pub native_w: u32,
    pub native_h: u32,
    pub refresh_hz: f64,
    pub scale: f64,
    /// Global points on the agent.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl From<&pb::Source> for SourceInfo {
    fn from(s: &pb::Source) -> Self {
        let r = s.rect.unwrap_or_default();
        Self {
            id: s.id,
            kind: if s.kind == pb::SourceKind::Window as i32 { "window" } else { "display" },
            title: s.title.clone(),
            native_w: s.native_w,
            native_h: s.native_h,
            refresh_hz: s.refresh_hz,
            scale: s.scale,
            x: r.x,
            y: r.y,
            w: r.w,
            h: r.h,
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Seen over mDNS (not necessarily added).
    AgentFound {
        key: String,
        name: String,
        os: String,
        fp_short: String,
        addrs: Vec<String>,
    },
    AgentLost {
        key: String,
    },
    AgentState {
        agent: AgentId,
        state: AgentState,
        detail: String,
    },
    /// After a successful handshake.
    AgentInfo {
        agent: AgentId,
        name: String,
        fingerprint: String,
        granted: Vec<String>,
        newly_paired: bool,
    },
    /// Show this code; the agent's user confirms there.
    PairCode {
        agent: AgentId,
        name: String,
        sas: String,
    },
    /// The agent trusts us but we do not know it: answer with `Hub::answer(request, bool)`.
    ConfirmAgent {
        request: u64,
        agent: AgentId,
        name: String,
        fingerprint: String,
        sas: String,
    },
    Sources {
        agent: AgentId,
        sources: Vec<SourceInfo>,
    },
    Status {
        agent: AgentId,
        screen_recording: bool,
        accessibility: bool,
        screen_locked: bool,
        display_asleep: bool,
    },
    TileState {
        tile: TileId,
        agent: AgentId,
        state: TileState,
        w: u32,
        h: u32,
        fps: u32,
        detail: String,
    },
    /// Once a second per live tile.
    TileStats {
        tile: TileId,
        fps: f32,
        bitrate_kbps: u32,
        latency_p50_ms: f32,
        latency_p95_ms: f32,
        skipped: u32,
        keyframe_requests: u32,
        agent_fps: u32,
        agent_dropped: u32,
    },
    /// PNG as base64 when `kind` is empty.
    CursorShape {
        agent: AgentId,
        shape_id: u32,
        kind: String,
        png_base64: String,
        hotspot_x: u32,
        hotspot_y: u32,
    },
}

impl Event {
    pub fn to_json(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|e| format!(r#"{{"type":"error","detail":"{e}"}}"#))
    }
}

/// Standard base64 (RFC 4648) without a dependency.
pub fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape_and_base64() {
        let e = Event::AgentState { agent: 3, state: AgentState::Connected, detail: String::new() };
        assert_eq!(e.to_json(), r#"{"type":"agent_state","agent":3,"state":"connected","detail":""}"#);
        assert_eq!(base64(b"Man"), "TWFu");
        assert_eq!(base64(b"Ma"), "TWE=");
        assert_eq!(base64(b"M"), "TQ==");
    }
}
