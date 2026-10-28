//! Zig handles the compiler, archiver, linker and universal binary; cargo-bundle
//! handles the app layout. Only the Apple SDK is an external platform input.

use crate::{BINS, Result, cargo, copy_dir, copy_notices, run, target_dir};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, io};

const SDK_VERSION: &str = "26.1";
const SDK_SHA256: &str = "beee7212d265a6d2867d0236cc069314b38d5fb3486a6515734e76fa210c784c";
const SDK_URL: &str =
    "https://github.com/joseluisq/macosx-sdks/releases/download/26.1/MacOSX26.1.sdk.tar.xz";
const TARGETS: &[&str] = &[
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "universal2-apple-darwin",
];

pub fn sdk() -> Result<PathBuf> {
    if let Some(path) = env::var_os("SDKROOT") {
        let path = fs::canonicalize(path)?;
        validate_sdk(&path)?;
        return Ok(path);
    }
    if cfg!(target_os = "macos") {
        let output = Command::new("xcrun")
            .args(["--sdk", "macosx", "--show-sdk-path"])
            .output()?;
        if output.status.success() {
            let path = PathBuf::from(String::from_utf8(output.stdout)?.trim());
            validate_sdk(&path)?;
            return Ok(path);
        }
    }
    let cache = target_dir()?.join("macos-sdk");
    let path = cache.join(format!("MacOSX{SDK_VERSION}.sdk"));
    let ready = cache.join("verified-sdk.sha256");
    if ready.is_file() && fs::read_to_string(&ready)? == SDK_SHA256 {
        validate_sdk(&path)?;
        return Ok(path);
    }
    fs::create_dir_all(&cache)?;
    let archive = cache.join(format!("MacOSX{SDK_VERSION}.sdk.tar.xz"));
    if !archive.is_file() {
        eprintln!("Downloading macOS {SDK_VERSION} SDK ({SDK_URL})");
        let temporary = cache.join("download.partial");
        let mut response = reqwest::blocking::Client::new()
            .get(SDK_URL)
            .send()?
            .error_for_status()?;
        let mut file = File::create(&temporary)?;
        io::copy(&mut response, &mut file)?;
        file.flush()?;
        fs::rename(temporary, &archive)?;
    }
    let mut hash = Sha256::new();
    let mut input = File::open(&archive)?;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = input.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    if hex::encode(hash.finalize()) != SDK_SHA256 {
        return Err(format!(
            "SDK checksum mismatch: {}; remove it and retry",
            archive.display()
        )
        .into());
    }
    eprintln!("Extracting macOS SDK to {}", cache.display());
    let reader = lzma_rust2::XzReader::new(File::open(&archive)?, false);
    let mut archive = tar::Archive::new(reader);
    let manuals = PathBuf::from(format!("MacOSX{SDK_VERSION}.sdk/usr/share/man"));
    for entry in archive.entries()? {
        let mut entry = entry?;
        // Tcl man pages contain ':' in their filenames, which Windows cannot
        // represent. They are documentation, not headers or link stubs.
        if entry.path()?.starts_with(&manuals) {
            continue;
        }
        if !entry.unpack_in(&cache)? {
            return Err("SDK archive entry would escape the cache directory".into());
        }
    }
    validate_sdk(&path)?;
    fs::write(ready, SDK_SHA256)?;
    Ok(path)
}

fn validate_sdk(path: &Path) -> Result {
    for file in [
        "SDKSettings.json",
        "usr/lib/libMobileGestalt.tbd",
        "System/Library/Frameworks/AppKit.framework",
    ] {
        if !path.join(file).exists() {
            return Err(format!("macOS SDK is missing {file}: {}", path.display()).into());
        }
    }
    Ok(())
}

pub fn build(target: &str) -> Result<PathBuf> {
    if !TARGETS.contains(&target) {
        return Err(format!(
            "unsupported macOS target {target}; expected {}",
            TARGETS.join(", ")
        )
        .into());
    }
    let sdk = sdk()?;
    let mut command = cargo()?;
    command
        .args(["zigbuild", "--locked", "--release", "--target", target])
        .args(BINS.iter().flat_map(|bin| ["-p", bin]))
        .env("SDKROOT", sdk)
        .env("MACOSX_DEPLOYMENT_TARGET", "11.0")
        // Packed Apple debug info invokes dsymutil, which is not part of Zig.
        // Keep it embedded until the release profile strips the executable.
        .env("CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO", "off");
    run(&mut command)?;
    Ok(target_dir()?.join(target).join("release"))
}

pub fn dist(target: &str) -> Result {
    let built = build(target)?;
    let label = if target == "universal2-apple-darwin" {
        "universal"
    } else {
        crate::arch(target)
    };
    for bin in BINS {
        verify_architectures(&built.join(bin), target)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(built.join(bin), fs::Permissions::from_mode(0o755))?;
        }
        fs::copy(built.join(bin), format!("dist/{bin}-macos-{label}"))?;
    }
    // --binary-path is supported by cargo-bundle >= 0.12. It prevents a second
    // cargo build from reverting to the host linker after cargo-zigbuild ran.
    let bundle_target = if label == "universal" {
        "aarch64-apple-darwin"
    } else {
        target
    };
    run(cargo()?
        .args([
            "bundle",
            "--release",
            "--format",
            "osx",
            "--package",
            "jimpactor",
            "--target",
            bundle_target,
            "--binary-path",
        ])
        .arg(built.join("jimpactor")))?;
    let source = target_dir()?
        .join(bundle_target)
        .join("release/bundle/osx/Jimpactor.app");
    let destination = if label == "universal" {
        PathBuf::from("dist/Jimpactor.app")
    } else {
        PathBuf::from(format!("dist/macos-{label}/Jimpactor.app"))
    };
    if destination.exists() {
        fs::remove_dir_all(&destination)?;
    }
    copy_dir(&source, &destination)?;
    copy_notices(&destination.join("Contents/Resources"))?;
    verify_architectures(&destination.join("Contents/MacOS/jimpactor"), target)?;
    let plist = plist::Value::from_file(destination.join("Contents/Info.plist"))?;
    for (key, expected) in [
        ("CFBundleIdentifier", "io.github.jpdagostin0.jimpactor"),
        ("CFBundleExecutable", "jimpactor"),
        ("LSMinimumSystemVersion", "11.0"),
        ("CFBundleShortVersionString", crate::VERSION),
    ] {
        if plist
            .as_dictionary()
            .and_then(|dict| dict.get(key))
            .and_then(plist::Value::as_string)
            != Some(expected)
        {
            return Err(format!("app bundle has incorrect {key}").into());
        }
    }
    if plist
        .as_dictionary()
        .and_then(|dict| dict.get("LSUIElement"))
        .and_then(plist::Value::as_boolean)
        != Some(true)
    {
        return Err("app bundle is missing LSUIElement".into());
    }
    crate::dmg::create(
        &destination,
        Path::new(&format!("dist/jimpactor-macos-{label}.dmg")),
    )?;
    archive_app(
        &destination,
        Path::new(&format!("dist/jimpactor-macos-{label}.app.tar.gz")),
    )?;
    Ok(())
}

fn archive_app(app: &Path, output: &Path) -> Result {
    let encoder =
        flate2::write::GzEncoder::new(File::create(output)?, flate2::Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let parent = app.parent().ok_or("app has no parent")?;
    for source in crate::dmg::entries(app)? {
        let metadata = fs::symlink_metadata(&source)?;
        let name = source.strip_prefix(parent)?;
        let mut header = tar::Header::new_gnu();
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        if metadata.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_mode(0o755);
            header.set_size(0);
            archive.append_data(&mut header, name, io::empty())?;
        } else if metadata.file_type().is_symlink() {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_mode(0o777);
            header.set_size(0);
            archive.append_link(&mut header, name, fs::read_link(&source)?)?;
        } else {
            header.set_mode(
                if name
                    .to_string_lossy()
                    .replace('\\', "/")
                    .contains("/Contents/MacOS/")
                {
                    0o755
                } else {
                    0o644
                },
            );
            header.set_size(metadata.len());
            archive.append_data(&mut header, name, File::open(&source)?)?;
        }
    }
    archive.into_inner()?.finish()?;
    Ok(())
}

fn verify_architectures(path: &Path, target: &str) -> Result {
    use goblin::mach::{
        Mach,
        constants::cputype::{CPU_TYPE_ARM64, CPU_TYPE_X86_64},
    };
    let bytes = fs::read(path)?;
    let mut actual = match Mach::parse(&bytes)? {
        Mach::Binary(binary) => vec![binary.header.cputype],
        Mach::Fat(binary) => binary.arches()?.iter().map(|arch| arch.cputype).collect(),
    };
    actual.sort_unstable();
    let mut expected = match target {
        "aarch64-apple-darwin" => vec![CPU_TYPE_ARM64],
        "x86_64-apple-darwin" => vec![CPU_TYPE_X86_64],
        "universal2-apple-darwin" => vec![CPU_TYPE_ARM64, CPU_TYPE_X86_64],
        _ => return Err(format!("unexpected Apple target {target}").into()),
    };
    expected.sort_unstable();
    if actual != expected {
        return Err(format!(
            "{} has architectures {actual:?}, expected {expected:?}",
            path.display()
        )
        .into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_archive_preserves_executable_permissions_on_every_host() -> Result {
        let temporary = tempfile::tempdir()?;
        let app = temporary.path().join("Jimpactor.app");
        fs::create_dir_all(app.join("Contents/MacOS"))?;
        fs::write(app.join("Contents/MacOS/jimpactor"), b"executable fixture")?;
        fs::write(app.join("Contents/Info.plist"), b"plist fixture")?;
        let output = temporary.path().join("app.tar.gz");
        archive_app(&app, &output)?;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(File::open(output)?));
        let mut checked = 0;
        for entry in archive.entries()? {
            let entry = entry?;
            let path = entry.path()?.to_string_lossy().replace('\\', "/");
            if path == "Jimpactor.app/Contents/MacOS/jimpactor" {
                assert_eq!(entry.header().mode()?, 0o755);
                checked += 1;
            } else if path == "Jimpactor.app/Contents/Info.plist" {
                assert_eq!(entry.header().mode()?, 0o644);
                checked += 1;
            }
        }
        assert_eq!(checked, 2);
        Ok(())
    }
}
