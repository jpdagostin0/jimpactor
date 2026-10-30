//! Account-free local provisioning check. Prints metadata, never OTPs or machine IDs.
//! cargo run -p omnisette --features remote-anisette-v3 --example local_mac -- <scratch-directory>
use omnisette::{AnisetteConfiguration, AnisetteHeaders, AnisetteProvider, mac_libraries};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("provide a scratch directory")?,
    );
    mac_libraries::ensure_library(&root).await?;
    let config = AnisetteConfiguration::new()
        .set_provider(AnisetteProvider::Local)
        .set_configuration_path(root);
    let mut provider = AnisetteHeaders::get_anisette_headers_provider(config)?;
    for _ in 0..2 {
        let headers = provider.provider.get_authentication_headers().await?;
        println!("Client: {}", headers["X-Mme-Client-Info"]);
        println!(
            "OTP present: {}; machine ID present: {}; Apple routing present: {}",
            !headers["X-Apple-I-MD"].is_empty(),
            !headers["X-Apple-I-MD-M"].is_empty(),
            !headers["X-Apple-I-MD-RINFO"].is_empty()
        );
    }
    Ok(())
}
