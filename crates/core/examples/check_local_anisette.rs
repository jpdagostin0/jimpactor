//! Account-free diagnostic: checks Apple's response using a nonexistent Apple ID.
use plume_core::{AnisetteConfiguration, AnisetteProvider, Error, auth::Account};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?
        .block_on(run())
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::path::PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("provide a scratch directory")?,
    );
    let config = AnisetteConfiguration::new()
        .set_provider(AnisetteProvider::Local)
        .set_configuration_path(path);
    let mut account = Account::new(config).await?;
    let username = format!("jimpactor-probe-{}@example.invalid", uuid::Uuid::new_v4());
    match account
        .login_email_pass(&username, "not-a-real-account")
        .await
    {
        Err(Error::AuthSrpWithMessage(code, message)) => {
            println!("Apple returned {code}: {message}");
            if matches!(code, -80045 | -80046) {
                return Err("Apple rejected the local Mac identity".into());
            }
        }
        Err(error) => return Err(error.into()),
        Ok(_) => return Err("unexpected authentication of nonexistent account".into()),
    }
    Ok(())
}
