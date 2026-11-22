//! Control stream: `u32 LE length` + `Envelope` on the first bidi stream.

use anyhow::{Result, bail};
use quinn::{RecvStream, SendStream};
use rz_proto::clock::{ClockSample, ClockSync, now_ns};
use rz_proto::framing::{read_msg, write_msg};
use rz_proto::pb::{Envelope, Ping, Pong, envelope::Msg};

pub struct Control {
    send: SendStream,
    recv: RecvStream,
    seq: u64,
}

impl Control {
    pub fn new(send: SendStream, recv: RecvStream) -> Self {
        Self { send, recv, seq: 0 }
    }

    /// Sends a message, returns its seq.
    pub async fn send(&mut self, msg: Msg) -> Result<u64> {
        self.reply(0, msg).await
    }

    pub async fn reply(&mut self, reply_to: u64, msg: Msg) -> Result<u64> {
        self.seq += 1;
        write_msg(&mut self.send, &Envelope { seq: self.seq, reply_to, msg: Some(msg) }).await?;
        Ok(self.seq)
    }

    /// `Ok(None)` on clean EOF.
    pub async fn recv(&mut self) -> Result<Option<Envelope>> {
        Ok(read_msg(&mut self.recv).await?)
    }

    /// Splits into independent halves (e.g. reader task + writer task).
    pub fn split(self) -> (ControlSender, ControlReceiver) {
        (ControlSender { send: self.send, seq: self.seq }, ControlReceiver { recv: self.recv })
    }
}

pub struct ControlSender {
    send: SendStream,
    seq: u64,
}

impl ControlSender {
    /// The seq the next `send` will use (register a reply handler before sending).
    pub fn next_seq(&self) -> u64 {
        self.seq + 1
    }

    pub async fn send(&mut self, msg: Msg) -> Result<u64> {
        self.reply(0, msg).await
    }
    pub async fn reply(&mut self, reply_to: u64, msg: Msg) -> Result<u64> {
        self.seq += 1;
        write_msg(&mut self.send, &Envelope { seq: self.seq, reply_to, msg: Some(msg) }).await?;
        Ok(self.seq)
    }
}

pub struct ControlReceiver {
    recv: RecvStream,
}

impl ControlReceiver {
    pub async fn recv(&mut self) -> Result<Option<Envelope>> {
        Ok(read_msg(&mut self.recv).await?)
    }
}

/// Builds a Ping for now.
pub fn ping_now() -> Msg {
    Msg::Ping(Ping { t0_ns: now_ns() })
}

/// Builds the Pong for a received Ping. `t1` must be taken as soon as the Ping arrived.
pub fn pong_for(ping: &Ping, t1: u64) -> Msg {
    Msg::Pong(Pong { t0_ns: ping.t0_ns, t1_ns: t1, t2_ns: now_ns() })
}

/// Turns a received Pong into a sample (call right after receiving).
pub fn sample_from_pong(p: &Pong) -> ClockSample {
    ClockSample { t0: p.t0_ns, t1: p.t1_ns, t2: p.t2_ns, t3: now_ns() }
}

/// If `env` is a Ping, answers it.
pub async fn answer_ping(ctl: &mut Control, env: &Envelope) -> Result<bool> {
    if let Some(Msg::Ping(p)) = &env.msg {
        let t1 = now_ns();
        ctl.reply(env.seq, pong_for(p, t1)).await?;
        return Ok(true);
    }
    Ok(false)
}

/// Sends one Ping and waits for the Pong (only for simple request/response use, e.g. tests).
pub async fn ping_once(ctl: &mut Control, sync: &mut ClockSync) -> Result<()> {
    ctl.send(ping_now()).await?;
    match ctl.recv().await? {
        Some(Envelope { msg: Some(Msg::Pong(p)), .. }) => {
            sync.add(sample_from_pong(&p));
            Ok(())
        }
        other => bail!("expected Pong, got {other:?}"),
    }
}
