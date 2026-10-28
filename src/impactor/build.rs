use std::path::PathBuf;
use std::{env, fs, io};
use winresource::WindowsResource;

fn main() -> io::Result<()> {
    let svg_path = format!("../../{}", genicons::SVG_PATH);
    println!("cargo:rerun-if-changed={svg_path}");

    let icon = genicons::Icon::from_svg(&fs::read_to_string(&svg_path)?);
    let out_dir = PathBuf::from(env::var("OUT_DIR").unwrap());
    fs::write(out_dir.join("icon.png"), icon.png(64))?;
    fs::write(out_dir.join("tray.png"), icon.tray_png(64))?;

    if env::var("CARGO_CFG_TARGET_FAMILY").unwrap() == "windows" {
        let ico_path = out_dir.join("icon.ico");
        fs::write(&ico_path, icon.ico())?;

        let mut res = WindowsResource::new();
        match env::var("CARGO_CFG_TARGET_ENV").unwrap().as_str() {
            "gnu" => {
                let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
                let prefix = match arch.as_str() {
                    "x86" => "i686-w64-mingw32",
                    "x86_64" => "x86_64-w64-mingw32",
                    _ => panic!("unsupported Windows GNU architecture: {arch}"),
                };
                res.set_ar_path(&format!("{prefix}-ar"))
                    .set_windres_path(&format!("{prefix}-windres"));
            }
            "msvc" => {}
            _ => panic!("unsupported env"),
        };
        res.set_icon(ico_path.to_str().unwrap());
        res.compile()?;
    }
    Ok(())
}
