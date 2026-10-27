//! Renders every platform icon from `package/icon.svg`.
//!
//! Used by `src/impactor/build.rs` for the icons compiled into the app, and by `cargo xtask` for
//! the icons that ship alongside it in packages.

use resvg::tiny_skia::{Pixmap, Transform};
use resvg::usvg::{Options, Tree};

/// Sizes of the freedesktop `hicolor` theme icons installed on Linux.
pub const LINUX_SIZES: &[u32] = &[16, 32, 48, 64, 128, 256, 512];
const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

/// macOS (Big Sur+) icon grid: an 824px shape centered on a 1024px canvas.
const MAC_SCALE: f32 = 824.0 / 1024.0;

/// PNG-backed ICNS entry types, as (OSType, pixel size).
const ICNS_TYPES: &[(&[u8; 4], u32)] = &[
    (b"icp4", 16),
    (b"ic11", 32), // 16@2x
    (b"icp5", 32),
    (b"ic12", 64), // 32@2x
    (b"ic07", 128),
    (b"ic13", 256), // 128@2x
    (b"ic08", 256),
    (b"ic14", 512), // 256@2x
    (b"ic09", 512),
    (b"ic10", 1024), // 512@2x
];

/// Path of the source SVG, relative to the workspace root.
pub const SVG_PATH: &str = "package/icon.svg";

pub struct Icon {
    icon: Tree,
    glyph: Tree,
}

impl Icon {
    /// Parses the icon. The SVG must contain a `<g id="glyph">` group covering the 600px box at
    /// (212, 212), which is used on its own for the tray icon.
    pub fn from_svg(svg: &str) -> Self {
        Self {
            icon: parse(svg),
            glyph: parse(&glyph_svg(svg)),
        }
    }

    /// The full icon, edge to edge.
    pub fn png(&self, size: u32) -> Vec<u8> {
        png(&self.icon, size, 1.0)
    }

    /// The bare glyph, for tray icons. macOS treats it as a template and only uses its alpha.
    pub fn tray_png(&self, size: u32) -> Vec<u8> {
        png(&self.glyph, size, 1.0)
    }

    /// Windows icon with PNG-compressed entries (supported since Windows Vista).
    pub fn ico(&self) -> Vec<u8> {
        let images: Vec<_> = ICO_SIZES.iter().map(|&s| (s, self.png(s))).collect();

        let mut out = Vec::new();
        out.extend_from_slice(&[0, 0, 1, 0]);
        out.extend_from_slice(&(images.len() as u16).to_le_bytes());

        let mut offset = 6 + 16 * images.len() as u32;
        for (size, data) in &images {
            let dim = if *size >= 256 { 0 } else { *size as u8 };
            out.extend_from_slice(&[dim, dim, 0, 0]);
            out.extend_from_slice(&1u16.to_le_bytes()); // color planes
            out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(&offset.to_le_bytes());
            offset += data.len() as u32;
        }
        for (_, data) in &images {
            out.extend_from_slice(data);
        }
        out
    }

    /// macOS icon, laid out on the macOS icon grid. All lengths are big-endian and include their
    /// 8-byte headers.
    pub fn icns(&self) -> Vec<u8> {
        let images: Vec<_> = ICNS_TYPES
            .iter()
            .map(|&(ty, s)| (ty, png(&self.icon, s, MAC_SCALE)))
            .collect();

        let total: usize = 8 + images.iter().map(|(_, d)| 8 + d.len()).sum::<usize>();
        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(b"icns");
        out.extend_from_slice(&(total as u32).to_be_bytes());
        for (ty, data) in &images {
            out.extend_from_slice(*ty);
            out.extend_from_slice(&(8 + data.len() as u32).to_be_bytes());
            out.extend_from_slice(data);
        }
        out
    }
}

fn parse(svg: &str) -> Tree {
    Tree::from_str(svg, &Options::default()).expect("invalid svg")
}

/// Just the `#glyph` group, cropped to its 600px box so it fills the canvas.
fn glyph_svg(svg: &str) -> String {
    let start = svg
        .find(r#"<g id="glyph""#)
        .expect("icon.svg has no #glyph group");
    let end = start
        + svg[start..]
            .find("</g>")
            .expect("unterminated #glyph group")
        + "</g>".len();
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="600" height="600" viewBox="212 212 600 600">{}</svg>"#,
        &svg[start..end]
    )
}

/// Renders `tree` into a `size`x`size` PNG, shrunk by `scale` around the center.
fn png(tree: &Tree, size: u32, scale: f32) -> Vec<u8> {
    let mut pixmap = Pixmap::new(size, size).unwrap();
    let s = size as f32 / tree.size().width() * scale;
    let offset = size as f32 * (1.0 - scale) / 2.0;
    resvg::render(
        tree,
        Transform::from_scale(s, s).post_translate(offset, offset),
        &mut pixmap.as_mut(),
    );
    pixmap.encode_png().unwrap()
}
