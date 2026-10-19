//! `eris` - the Kallisti 1.0 daemon.
//!
//! Wave 0 stub. This binary prints one version line and exits. It binds no
//! socket, opens no database, reads no config, and installs nothing.
//!
//! The real command surface (`serve`, `probe`, `setup`, `device`, `doctor`,
//! `eval`, `version`) arrives with `clap` in Wave 3 (P5). The admin listener is
//! a literal `127.0.0.1:7340` and stays loopback-only.

fn main() {
    let version = env!("CARGO_PKG_VERSION");
    let core_version = core_domain::VERSION;
    println!("eris {version} (core-domain {core_version})");
}
