//! `cargo run -p rz-net --example mdns -- <name> <port> <secs>`: advertises with an ephemeral identity and prints
//! what it sees. Run two instances (also on two machines) to check discovery.

use std::time::Duration;

use rz_net::discovery::{Advert, Discovery};

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter("info").init();
    let a: Vec<String> = std::env::args().skip(1).collect();
    let name = a.first().cloned().unwrap_or_else(rz_net::discovery::hostname);
    let port: u16 = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(47470);
    let secs: u64 = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let id = rz_net::Identity::generate()?;
    let mut d = Discovery::new()?;
    d.advertise(&Advert { name, fingerprint: id.fingerprint, port, roles: vec!["agent"] })?;
    let mut rx = d.browse()?;
    let end = tokio::time::sleep(Duration::from_secs(secs));
    tokio::pin!(end);
    loop {
        tokio::select! {
            _ = &mut end => break,
            ev = rx.recv() => match ev { Some(e) => println!("{e:?}"), None => break },
        }
    }
    Ok(())
}
