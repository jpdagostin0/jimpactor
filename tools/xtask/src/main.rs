//! `cargo xtask dist [--target <triple>]`
//!
//! Builds release binaries for the target (the host by default) and packages them into `dist/`:
//!
//! - every target: `jimpactor-<os>-<arch>` and `jimpactor-cli-<os>-<arch>`
//! - Linux: `jimpactor-linux-<arch>.AppImage` (downloads linuxdeploy)
//! - Windows: `jimpactor-windows-<arch>-setup.exe` (needs `makensis` on `PATH`)
//! - macOS: always universal (needs both Apple targets installed), plus `Jimpactor.app`

use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LINUX_ID: &str = "io.github.jpdagostin0.jimpactor";
const BINS: &[&str] = &["jimpactor", "jimpactor-cli"];
const MAC_TARGETS: &[&str] = &["aarch64-apple-darwin", "x86_64-apple-darwin"];
const NOTICE_FILES: &[&str] = &["LICENSE", "Attributions.md"];

/// Loaded at runtime by the tray icon, so linuxdeploy can't find them on its own.
const APPIMAGE_LIBS: &[&str] = &[
    "libayatana-appindicator3.so.1",
    "libappindicator3.so.1",
    "libayatana-ido3-0.4.so.0",
    "libdbusmenu-glib.so.4",
    "libdbusmenu-gtk3.so.4",
];
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
    let args: Vec<String> = env::args().skip(1).collect();
    let target = match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["dist"] => host_triple()?,
        ["dist", "--target", target] => target.to_string(),
        _ => return Err("usage: cargo xtask dist [--target <triple>]".into()),
    };

    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    env::set_current_dir(&root)?;
    fs::create_dir_all("dist")?;
    copy_notices(Path::new("dist"))?;
    let icon = genicons::Icon::from_svg(&fs::read_to_string(genicons::SVG_PATH)?);

    if target.contains("-apple-") {
        macos(&icon)
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
    for lib in APPIMAGE_LIBS {
        match find_library(lib) {
            Some(path) => cmd.arg("--library").arg(path),
            None => {
                eprintln!("warning: {lib} not found; the tray icon may not load");
                &mut cmd
            }
        };
    }
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

fn macos(icon: &genicons::Icon) -> Result {
    let slices = MAC_TARGETS
        .iter()
        .map(|target| build(target))
        .collect::<Result<Vec<_>>>()?;
    for bin in BINS {
        let mut lipo = Command::new("lipo");
        lipo.args(["-create", "-output", &format!("dist/{bin}-macos-universal")]);
        for slice in &slices {
            lipo.arg(slice.join(bin));
        }
        run(&mut lipo)?;
    }

    let app = PathBuf::from("dist/Jimpactor.app");
    let _ = fs::remove_dir_all(&app);
    copy_dir(Path::new("package/macos/Jimpactor.app"), &app)?;
    copy_notices(&app.join("Contents/Resources"))?;
    fs::write(app.join("Contents/Resources/AppIcon.icns"), icon.icns())?;
    let exe = app.join("Contents/MacOS/jimpactor");
    let _ = fs::remove_file(app.join("Contents/MacOS/.keep"));
    // Mark the binary as built against the macOS 26 SDK so it gets the current system UI.
    run(Command::new("vtool")
        .args(["-arch", "x86_64", "-arch", "arm64"])
        .args(["-set-build-version", "1", "10.12", "26.0", "-output"])
        .arg(&exe)
        .arg("dist/jimpactor-macos-universal"))?;

    let plist_path = app.join("Contents/Info.plist");
    let mut plist = fs::read_to_string(&plist_path)?;
    for key in ["CFBundleShortVersionString", "CFBundleVersion"] {
        plist = set_plist_string(&plist, key, VERSION)?;
    }
    fs::write(plist_path, plist)?;
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
    run(Command::new(env::var("CARGO").unwrap_or("cargo".into()))
        .args(["build", "--release", "--target", target])
        .args(BINS.iter().flat_map(|bin| ["-p", bin])))?;
    Ok(Path::new("target").join(target).join("release"))
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

fn find_library(name: &str) -> Option<String> {
    let out = Command::new("ldconfig").arg("-p").output().ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .find_map(|line| {
            let (lib, path) = line.trim().split_once(" => ")?;
            (lib.split_whitespace().next()? == name).then(|| path.to_string())
        })
}

/// Replaces the `<string>` value that follows `<key>{key}</key>`.
fn set_plist_string(plist: &str, key: &str, value: &str) -> Result<String> {
    let key_at = plist
        .find(&format!("<key>{key}</key>"))
        .ok_or_else(|| format!("Info.plist has no {key}"))?;
    let start = key_at + plist[key_at..].find("<string>").unwrap() + "<string>".len();
    let end = start + plist[start..].find("</string>").unwrap();
    Ok(format!("{}{value}{}", &plist[..start], &plist[end..]))
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
