//! Build and packaging tasks (`cargo xtask <cmd>`). Plan rule: no shell scripts.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(about = "Rozhledna build tasks")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Regenerate crates/rz-proto/src/gen from proto/rozhledna.proto (pure Rust, no protoc).
    GenProto,
    /// rustfmt --check + clippy -D warnings + tests.
    Lint,
    /// Build a spike binary in release mode and wrap it in a signed .app bundle in dist/.
    ///
    /// Signing identity: $RZ_SIGN_IDENTITY, else the first non-revoked Developer ID / Apple Development one.
    /// A stable signature matters: TCC permissions are bound to it (plan chapter 8).
    Bundle {
        /// rz-agent | spike-agent | spike-viewer
        name: String,
        /// Skip codesign (permissions will NOT persist across rebuilds).
        #[arg(long)]
        no_sign: bool,
    },
    /// Build the hub: rz-hub-ffi staticlib + Flutter app (hub/) → signed dist/Rozhledna.app (T1.6).
    Hub {
        /// Build in debug mode instead of release.
        #[arg(long)]
        debug: bool,
        /// Keep Flutter's ad-hoc signature (Local Network permission will not persist across rebuilds).
        #[arg(long)]
        no_sign: bool,
    },
    /// Build the Rust staticlib and the Flutter macOS spike app (T0.6).
    FlutterSpike {
        /// Build in debug mode instead of release.
        #[arg(long)]
        debug: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::GenProto => gen_proto(),
        Cmd::Lint => lint(),
        Cmd::Bundle { name, no_sign } => bundle(&name, !no_sign),
        Cmd::FlutterSpike { debug } => flutter_spike(!debug),
        Cmd::Hub { debug, no_sign } => hub(!debug, !no_sign),
    }
}

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_path_buf()
}

fn run(cmd: &mut Command) -> Result<()> {
    eprintln!("» {cmd:?}");
    let st = cmd.status().with_context(|| format!("spawning {cmd:?}"))?;
    if !st.success() {
        bail!("command failed ({st}): {cmd:?}");
    }
    Ok(())
}

fn cargo() -> Command {
    let mut c = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
    c.current_dir(root());
    c
}

fn gen_proto() -> Result<()> {
    let root = root();
    let fds = protox::compile(["rozhledna.proto"], [root.join("proto")])?;
    let out = root.join("crates/rz-proto/src/gen");
    std::fs::create_dir_all(&out)?;
    prost_build::Config::new().out_dir(&out).compile_fds(fds)?;
    run(Command::new("rustfmt").arg("--edition=2024").arg(out.join("rozhledna.v1.rs")))?;
    eprintln!("generated {}", out.display());
    Ok(())
}

fn lint() -> Result<()> {
    run(cargo().args(["fmt", "--all", "--check"]))?;
    run(cargo().args(["clippy", "--workspace", "--all-targets", "--", "-D", "warnings"]))?;
    run(cargo().args(["test", "--workspace"]))?;
    Ok(())
}

struct BundleSpec {
    bin: &'static str,
    app: &'static str,
    id: &'static str,
    usage: &'static [(&'static str, &'static str)],
    ui_element: bool,
}

const BUNDLES: &[BundleSpec] = &[
    BundleSpec {
        bin: "rz-agent",
        app: "Rozhledna Agent",
        id: "dev.ol1n.rozhledna.agent",
        usage: &[
            ("NSScreenCaptureUsageDescription", "Rozhledna streams this screen to your paired hubs."),
            ("NSLocalNetworkUsageDescription", "Rozhledna talks to your hubs on the local network."),
        ],
        ui_element: true,
    },
    BundleSpec {
        bin: "spike-agent",
        app: "Rozhledna Spike Agent",
        id: "dev.ol1n.rozhledna.spike-agent",
        usage: &[
            ("NSScreenCaptureUsageDescription", "Rozhledna streams this screen to your hub."),
            ("NSLocalNetworkUsageDescription", "Rozhledna talks to your hub on the local network."),
        ],
        ui_element: true,
    },
    BundleSpec {
        bin: "spike-viewer",
        app: "Rozhledna Spike Viewer",
        id: "dev.ol1n.rozhledna.spike-viewer",
        usage: &[("NSLocalNetworkUsageDescription", "Rozhledna connects to agents on the local network.")],
        ui_element: false,
    },
];

fn bundle(name: &str, sign: bool) -> Result<()> {
    let spec = BUNDLES
        .iter()
        .find(|b| b.bin == name)
        .with_context(|| format!("unknown bundle {name}; known: rz-agent, spike-agent, spike-viewer"))?;
    run(cargo().args(["build", "--release", "-p", spec.bin]))?;
    let root = root();
    let app = root.join("dist").join(format!("{}.app", spec.app));
    let _ = std::fs::remove_dir_all(&app);
    let macos = app.join("Contents/MacOS");
    std::fs::create_dir_all(&macos)?;
    std::fs::copy(root.join("target/release").join(spec.bin), macos.join(spec.bin))?;
    let mut extra = String::new();
    for (k, v) in spec.usage {
        extra.push_str(&format!("  <key>{k}</key><string>{v}</string>\n"));
    }
    if spec.ui_element {
        extra.push_str("  <key>LSUIElement</key><true/>\n");
    }
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleIdentifier</key><string>{id}</string>
  <key>CFBundleName</key><string>{app}</string>
  <key>CFBundleExecutable</key><string>{bin}</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>{ver}</string>
  <key>CFBundleVersion</key><string>{ver}</string>
  <key>LSMinimumSystemVersion</key><string>14.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSBonjourServices</key><array><string>_rozhledna._udp</string></array>
{extra}</dict>
</plist>
"#,
        id = spec.id,
        app = spec.app,
        bin = spec.bin,
        ver = env!("CARGO_PKG_VERSION"),
    );
    std::fs::write(app.join("Contents/Info.plist"), plist)?;
    if sign {
        sign_bundle(&app)?;
    } else {
        eprintln!("WARNING: unsigned bundle — TCC permissions will not survive a rebuild");
    }
    eprintln!("bundle: {}", app.display());
    Ok(())
}

/// Sign with the first candidate identity whose certificate Gatekeeper does not report as revoked.
///
/// `security find-identity -v` is not enough: it may list a revoked cert with no `CSSMERR_` suffix
/// (revocation is only learned via OCSP). A bundle signed with a revoked cert is flagged as malware
/// and moved to Bin on launch, so after signing we ask `spctl`, which does the OCSP check.
fn sign_bundle(app: &Path) -> Result<()> {
    sign(app, false)
}

/// `deep`: also re-sign nested frameworks (Flutter apps embed FlutterMacOS.framework and App.framework).
fn sign(app: &Path, deep: bool) -> Result<()> {
    for identity in signing_identities()? {
        run(Command::new("codesign")
            .args(["--force", "--options", "runtime", "--timestamp=none"])
            .args(if deep { &["--deep"][..] } else { &[][..] })
            .arg("--sign")
            .arg(&identity)
            .arg(app))?;
        // Exit status is irrelevant: an un-notarized Apple Development bundle is always "rejected".
        let out = Command::new("spctl").args(["-a", "-vv", "-t", "exec"]).arg(app).output()?;
        let verdict = String::from_utf8_lossy(&out.stderr);
        if verdict.contains("CSSMERR_TP_CERT_REVOKED") {
            eprintln!("WARNING: identity {identity} has a revoked certificate, trying the next one");
            continue;
        }
        return Ok(());
    }
    // Do not leave a revoked-signed bundle around: launching it gets it trashed as malware.
    let _ = std::fs::remove_dir_all(app);
    bail!("every codesigning identity is revoked; set RZ_SIGN_IDENTITY or pass --no-sign")
}

/// Candidate identities (SHA-1 hashes), best first: $RZ_SIGN_IDENTITY alone if set,
/// else "Developer ID Application", then "Apple Development".
fn signing_identities() -> Result<Vec<String>> {
    if let Ok(id) = std::env::var("RZ_SIGN_IDENTITY") {
        return Ok(vec![id]);
    }
    let out = Command::new("security").args(["find-identity", "-v", "-p", "codesigning"]).output()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut ids = Vec::new();
    for pref in ["Developer ID Application", "Apple Development"] {
        // Lines look like `  1) <SHA1> "name"`, optionally suffixed with `(CSSMERR_...)` for
        // certs the keychain already knows are revoked/expired; skip those up front.
        for line in text.lines().filter(|l| l.contains(pref) && !l.contains("CSSMERR_")) {
            if let Some(hash) = line.split_whitespace().nth(1) {
                ids.push(hash.to_string());
            }
        }
    }
    if ids.is_empty() {
        bail!("no codesigning identity found; set RZ_SIGN_IDENTITY or pass --no-sign");
    }
    Ok(ids)
}

fn flutter_spike(release: bool) -> Result<()> {
    let profile = if release { "release" } else { "debug" };
    let mut b = cargo();
    b.args(["build", "-p", "rz-spike-ffi"]);
    if release {
        b.arg("--release");
    }
    run(&mut b)?;
    let root = root();
    let app_dir = root.join("spikes/flutter_spike");
    let lib_dir = app_dir.join("macos/rust");
    std::fs::create_dir_all(&lib_dir)?;
    std::fs::copy(root.join("target").join(profile).join("librz_spike_ffi.a"), lib_dir.join("librz_spike_ffi.a"))?;
    run(Command::new("flutter").current_dir(&app_dir).args(["build", "macos", &format!("--{profile}")]))?;
    eprintln!(
        "app: {}",
        app_dir
            .join(format!("build/macos/Build/Products/{}/flutter_spike.app", if release { "Release" } else { "Debug" }))
            .display()
    );
    Ok(())
}

fn hub(release: bool, sign_it: bool) -> Result<()> {
    let profile = if release { "release" } else { "debug" };
    let mut b = cargo();
    b.args(["build", "-p", "rz-hub-ffi"]);
    if release {
        b.arg("--release");
    }
    run(&mut b)?;
    let root = root();
    let app_dir = root.join("hub");
    let lib_dir = app_dir.join("macos/rust");
    std::fs::create_dir_all(&lib_dir)?;
    std::fs::copy(root.join("target").join(profile).join("librz_hub_ffi.a"), lib_dir.join("librz_hub_ffi.a"))?;
    run(Command::new("flutter").current_dir(&app_dir).args(["build", "macos", &format!("--{profile}")]))?;
    let built =
        app_dir.join(format!("build/macos/Build/Products/{}/Rozhledna.app", if release { "Release" } else { "Debug" }));
    let dist = root.join("dist/Rozhledna.app");
    let _ = std::fs::remove_dir_all(&dist);
    std::fs::create_dir_all(root.join("dist"))?;
    run(Command::new("ditto").arg(&built).arg(&dist))?;
    if sign_it {
        sign(&dist, true)?;
    }
    eprintln!("hub: {}", dist.display());
    Ok(())
}
