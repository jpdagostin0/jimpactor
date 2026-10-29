// tailscale-rs does not support Intel Macs; `tailnet` marks targets that embed it.
fn main() {
    println!("cargo::rustc-check-cfg=cfg(tailnet)");
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    if !(os == "macos" && arch == "x86_64") {
        println!("cargo::rustc-cfg=tailnet");
    }
}
