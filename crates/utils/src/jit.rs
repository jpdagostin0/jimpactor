//! Enabling JIT for an app (iOS 17+): launch it suspended through process control, then
//! attach and detach debugserver so the kernel allows it to map executable memory.

use std::path::{Path, PathBuf};

use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::debug_proxy::{DebugProxyClient, DebugserverCommand};
use idevice::dvt::process_control::ProcessControlClient;
use idevice::dvt::remote_server::RemoteServerClient;
use idevice::lockdown::LockdownClient;
use idevice::mobile_image_mounter::ImageMounter;
use idevice::provider::IdeviceProvider;
use idevice::rsd::RsdHandshake;
use idevice::IdeviceService;

use crate::{Device, Error};

pub const JIT_LABEL: &str = "jimpactor_jit";

/// Apple's personalized developer disk image, mirrored by the idevice community.
const DDI_BASE_URL: &str = "https://github.com/doronz88/DeveloperDiskImage/raw/refs/heads/main/PersonalizedImages/Xcode_iOS_DDI_Personalized";
const DDI_FILES: [&str; 3] = ["Image.dmg", "Image.dmg.trustcache", "BuildManifest.plist"];

impl Device {
    /// Launches `bundle_id` with JIT enabled and returns its process ID. Mounts the
    /// developer disk image first if needed, downloading it into `ddi_dir` once.
    pub async fn launch_with_jit(&self, bundle_id: &str, ddi_dir: PathBuf) -> Result<u64, Error> {
        let bundle_id = bundle_id.to_owned();
        self.with_provider(JIT_LABEL, move |provider| async move {
            ensure_developer_image(provider.as_ref(), &ddi_dir).await?;
            debug_attach(provider.as_ref(), Target::Launch(bundle_id)).await
        })
        .await
    }

    /// Enables JIT for a process that is already running.
    pub async fn attach_jit(&self, pid: u64, ddi_dir: PathBuf) -> Result<u64, Error> {
        self.with_provider(JIT_LABEL, move |provider| async move {
            ensure_developer_image(provider.as_ref(), &ddi_dir).await?;
            debug_attach(provider.as_ref(), Target::Pid(pid)).await
        })
        .await
    }
}

enum Target {
    Launch(String),
    Pid(u64),
}

/// Port of an RSD service. Services are opened by port rather than through
/// `RsdService::connect_rsd`, whose future the compiler cannot prove `Send`.
fn service_port(handshake: &RsdHandshake, name: &str) -> Result<u16, Error> {
    handshake
        .services
        .get(name)
        .map(|service| service.port)
        .ok_or_else(|| {
            Error::Other(format!(
                "The device does not offer {name}. Mount the developer disk image and turn on Developer Mode."
            ))
        })
}

async fn debug_attach(provider: &dyn IdeviceProvider, target: Target) -> Result<u64, Error> {
    let proxy = CoreDeviceProxy::connect(provider).await.map_err(|error| {
        Error::Other(format!(
            "Could not open the developer tunnel ({error}). JIT requires iOS 17 or later with Developer Mode on."
        ))
    })?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;
    let mut adapter = proxy.create_software_tunnel()?.to_async_handle();
    let stream = adapter.connect(rsd_port).await?;
    let handshake = RsdHandshake::new(stream).await?;

    let pid = match target {
        Target::Pid(pid) => pid,
        Target::Launch(bundle_id) => {
            let port = service_port(&handshake, "com.apple.instruments.dtservicehub")?;
            let mut remote = RemoteServerClient::new(adapter.connect(port).await?);
            remote.read_message(0).await?;
            let mut process_control = ProcessControlClient::new(&mut remote).await?;
            process_control
                .launch_app(bundle_id, None, None, true, true)
                .await?
        }
    };

    let port = service_port(&handshake, "com.apple.internal.dt.remote.debugproxy")?;
    let mut debugger = DebugProxyClient::new(adapter.connect(port).await?);
    let attached = debugger
        .send_command(DebugserverCommand::new(format!("vAttach;{pid:x}"), Vec::new()))
        .await?;
    log::debug!("debugserver attach to {pid}: {attached:?}");
    if attached.as_deref().is_some_and(|reply| reply.starts_with('E')) {
        return Err(Error::Other(format!(
            "debugserver could not attach to process {pid} ({})",
            attached.unwrap_or_default()
        )));
    }
    // Detaching resumes the process with the debugger's code-signing exemption in place.
    debugger
        .send_command(DebugserverCommand::new("D".into(), Vec::new()))
        .await?;
    Ok(pid)
}

async fn ensure_developer_image(provider: &dyn IdeviceProvider, ddi_dir: &Path) -> Result<(), Error> {
    let mut mounter = ImageMounter::connect(provider).await?;
    if !mounter.copy_devices().await?.is_empty() {
        return Ok(());
    }
    log::info!("Mounting the developer disk image");

    let mut lockdown = LockdownClient::connect(provider).await?;
    lockdown
        .start_session(&provider.get_pairing_file().await?)
        .await?;
    let unique_chip_id = lockdown
        .get_value(Some("UniqueChipID"), None)
        .await?
        .as_unsigned_integer()
        .ok_or_else(|| Error::Other("Device did not report its chip ID".into()))?;

    let files = download_developer_image(ddi_dir).await?;
    let [image, trust_cache, manifest] = files;
    mounter
        .mount_personalized(provider, image, trust_cache, &manifest, None, unique_chip_id)
        .await
        .map_err(|error| {
            Error::Other(format!(
                "Could not mount the developer disk image ({error}). Turn on Developer Mode in Settings → Privacy & Security and retry."
            ))
        })
}

/// Returns the image, trust cache and build manifest, downloading any not yet cached.
async fn download_developer_image(ddi_dir: &Path) -> Result<[Vec<u8>; 3], Error> {
    tokio::fs::create_dir_all(ddi_dir).await?;
    let client = reqwest::Client::builder()
        .user_agent(concat!("Jimpactor/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| Error::Other(e.to_string()))?;
    let mut files = Vec::with_capacity(DDI_FILES.len());
    for name in DDI_FILES {
        let path = ddi_dir.join(name);
        if let Ok(data) = tokio::fs::read(&path).await
            && !data.is_empty()
        {
            files.push(data);
            continue;
        }
        log::info!("Downloading developer disk image file {name}");
        let response = client
            .get(format!("{DDI_BASE_URL}/{name}"))
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| Error::Other(format!("Could not download {name}: {e}")))?;
        let data = response
            .bytes()
            .await
            .map_err(|e| Error::Other(format!("Could not download {name}: {e}")))?
            .to_vec();
        // Write beside the target first so an interrupted download is never reused.
        let partial = path.with_extension("partial");
        tokio::fs::write(&partial, &data).await?;
        tokio::fs::rename(&partial, &path).await?;
        files.push(data);
    }
    files
        .try_into()
        .map_err(|_| Error::Other("Developer disk image is incomplete".into()))
}
