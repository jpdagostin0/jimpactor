//! `cargo xtask dist [--target <triple>]`
//!
//! Builds release binaries for the target (the host by default) and packages them into `dist/`:
//!
//! - every target: `jimpactor-<os>-<arch>` and `jimpactor-cli-<os>-<arch>`
//! - Linux: `jimpactor-linux-<arch>.AppImage` (downloads linuxdeploy)
//! - Windows: `jimpactor-windows-<arch>-setup.exe` (NSIS; cargo-xwin for MSVC on non-Windows)
//! - macOS: per-architecture or universal binaries, `.app` and `.dmg` (Zig + cargo-bundle)

mod dmg;
mod macos;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LINUX_ID: &str = "io.github.jpdagostin0.jimpactor";
const BINS: &[&str] = &["jimpactor", "jimpactor-cli"];
const NOTICE_FILES: &[&str] = &["LICENSE", "Attributions.md"];

/// System libraries that must come from the host, not the AppImage.
const APPIMAGE_EXCLUDES: &[&str] = &[
    "libglib-2.0.so*",
    "libgobject-2.0.so*",
    "libgio-2.0.so*",
    "libgmodule-2.0.so*",
    "libgthread-2.0.so*",
    "libxkbcommon.so*",
    "libxkbcommon-x11.so*",
    "libX11.so*",
    "libxcb.so*",
];

fn main() -> Result {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    env::set_current_dir(&root)?;
    let args: Vec<String> = env::args().skip(1).collect();
    let (operation, target) = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["setup-macos"] => {
            println!("SDKROOT={}", macos::sdk()?.display());
            return Ok(());
        }
        [operation @ ("dist" | "build")] => (operation, host_triple()?),
        [operation @ ("dist" | "build"), "--target", target] => (operation, target.to_string()),
        _ => {
            return Err("usage: cargo xtask {dist|build} [--target <triple>] | setup-macos".into());
        }
    };

    if operation == "build" {
        build(&target)?;
        return Ok(());
    }
    fs::create_dir_all("dist")?;
    copy_notices(Path::new("dist"))?;
    let icon = genicons::Icon::from_svg(&fs::read_to_string(genicons::SVG_PATH)?);

    if target.ends_with("-apple-darwin") {
        // The default macOS distribution remains universal, as before.
        let target = if args.len() == 1 {
            "universal2-apple-darwin"
        } else {
            &target
        };
        macos::dist(target)
    } else if target.contains("-windows-") {
        windows(&icon, &target)
    } else if target.contains("-linux-") {
        linux(&icon, &target)
    } else {
        Err(format!("unsupported target {target}").into())
    }
}

fn linux(icon: &genicons::Icon, target: &str) -> Result {
    let arch = arch(target);
    let built = build(target)?;
    for bin in BINS {
        fs::copy(built.join(bin), format!("dist/{bin}-linux-{arch}"))?;
    }

    let appdir = PathBuf::from("target/appimage/AppDir");
    let _ = fs::remove_dir_all(&appdir);
    let usr = appdir.join("usr");
    fs::create_dir_all(usr.join("bin"))?;
    fs::copy(built.join("jimpactor"), usr.join("bin/jimpactor"))?;
    copy_notices(&usr.join("share/doc/jimpactor"))?;
    for &size in genicons::LINUX_SIZES {
        let dir = usr.join(format!("share/icons/hicolor/{size}x{size}/apps"));
        fs::create_dir_all(&dir)?;
        fs::write(dir.join(format!("{LINUX_ID}.png")), icon.png(size))?;
    }

    let linuxdeploy = PathBuf::from(format!("target/appimage/linuxdeploy-{arch}.AppImage"));
    if !linuxdeploy.exists() {
        run(Command::new("curl").args(["-fsSL", "-o"]).arg(&linuxdeploy).arg(format!(
            "https://github.com/linuxdeploy/linuxdeploy/releases/download/continuous/linuxdeploy-{arch}.AppImage"
        )))?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&linuxdeploy, fs::Permissions::from_mode(0o755))?;
    }

    let mut cmd = Command::new(&linuxdeploy);
    cmd.arg("--appimage-extract-and-run")
        .arg("--appdir")
        .arg(&appdir)
        .arg("--executable")
        .arg(usr.join("bin/jimpactor"))
        .arg("--desktop-file")
        .arg(format!("package/linux/{LINUX_ID}.desktop"))
        .args(["--output", "appimage"])
        .env("NO_STRIP", "true");
    // Older appimage plugins read OUTPUT, newer ones LDAI_OUTPUT.
    let appimage = format!("dist/jimpactor-linux-{arch}.AppImage");
    cmd.env("OUTPUT", &appimage).env("LDAI_OUTPUT", &appimage);
    for exclude in APPIMAGE_EXCLUDES {
        cmd.arg(format!("--exclude-library={exclude}"));
    }
    run(&mut cmd)
}

fn windows(icon: &genicons::Icon, target: &str) -> Result {
    let arch = arch(target);
    let built = build(target)?;
    for bin in BINS {
        fs::copy(
            built.join(format!("{bin}.exe")),
            format!("dist/{bin}-windows-{arch}.exe"),
        )?;
    }

    let nsis = PathBuf::from("target/nsis");
    let _ = fs::remove_dir_all(&nsis);
    fs::create_dir_all(&nsis)?;
    fs::copy("package/windows/installer.nsi", nsis.join("installer.nsi"))?;
    fs::copy(built.join("jimpactor.exe"), nsis.join("jimpactor.exe"))?;
    copy_notices(&nsis)?;
    fs::write(nsis.join("icon.ico"), icon.ico())?;
    run(Command::new("makensis")
        .arg(format!("-DAPPVERSION={VERSION}"))
        .arg("installer.nsi")
        .current_dir(&nsis))?;
    fs::rename(
        nsis.join("installer.exe"),
        format!("dist/jimpactor-windows-{arch}-setup.exe"),
    )?;
    Ok(())
}

fn copy_notices(destination: &Path) -> Result {
    fs::create_dir_all(destination)?;
    for name in NOTICE_FILES {
        fs::copy(name, destination.join(name))?;
    }
    Ok(())
}

/// Builds both binaries in release mode and returns the directory they land in.
fn build(target: &str) -> Result<PathBuf> {
    if target.ends_with("-apple-darwin") {
        return macos::build(target);
    }
    let mut command = cargo()?;
    if target.ends_with("-windows-msvc") && !cfg!(target_os = "windows") {
        command.args(["xwin", "build"]);
    } else {
        command.arg("build");
    }
    run(command
        .args(["--locked", "--release", "--target", target])
        .args(BINS.iter().flat_map(|bin| ["-p", bin])))?;
    Ok(target_dir()?.join(target).join("release"))
}

fn cargo() -> Result<Command> {
    let mut command = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    // Prefer workspace-scoped tools, then ordinary cargo install / PATH tools.
    let mut paths = vec![target_dir()?.join("host-tools/bin")];
    if let Some(path) = env::var_os("PATH") {
        paths.extend(env::split_paths(&path));
    }
    command.env("PATH", env::join_paths(paths)?);
    Ok(command)
}

fn target_dir() -> Result<PathBuf> {
    let output = Command::new(env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--no-deps", "--locked", "--format-version", "1"])
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    Ok(PathBuf::from(
        metadata["target_directory"]
            .as_str()
            .ok_or("Cargo metadata has no target directory")?,
    ))
}

fn host_triple() -> Result<String> {
    let out = Command::new("rustc").arg("-vV").output()?;
    String::from_utf8(out.stdout)?
        .lines()
        .find_map(|l| l.strip_prefix("host: "))
        .map(str::to_string)
        .ok_or_else(|| "could not determine host target from `rustc -vV`".into())
}

fn arch(target: &str) -> &str {
    target.split('-').next().unwrap()
}

fn copy_dir(from: &Path, to: &Path) -> Result {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &dest)?;
        } else {
            fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

fn run(cmd: &mut Command) -> Result {
    eprintln!("> {cmd:?}");
    let status = cmd
        .status()
        .map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if !status.success() {
        return Err(format!("{:?} failed with {status}", cmd.get_program()).into());
    }
    Ok(())
}
