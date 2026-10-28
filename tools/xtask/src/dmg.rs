//! HFS+ and UDIF creation without hdiutil, mounts, or host shell scripts.

use crate::Result;
use apple_dmg::{DmgReader, DmgWriter};
use fstool::block::{BlockDevice, FileBackend};
use fstool::fs::Filesystem;
use fstool::fs::hfs_plus::{FormatOpts, HfsPlus};
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::{self, BufWriter, Read};
use std::path::{Path, PathBuf};

pub fn create(app: &Path, output: &Path) -> Result {
    let temporary = tempfile::tempdir()?;
    let raw = temporary.path().join("volume.hfs");
    let entries = entries(app)?;
    // Round every file to allocation blocks and reserve room for the catalog.
    let bytes = entries
        .iter()
        .try_fold(32 * 1024 * 1024_u64, |total, path| {
            Ok::<_, io::Error>(total + fs::symlink_metadata(path)?.len().div_ceil(4096) * 4096)
        })?;
    let mut device = FileBackend::create(&raw, bytes)?;
    let mut volume = HfsPlus::format(
        &mut device,
        &FormatOpts {
            volume_name: "Jimpactor".into(),
            ..FormatOpts::default()
        },
    )?;
    let parent = app.parent().ok_or("app has no parent directory")?;
    for source in &entries {
        let path = image_path(source, parent)?;
        let metadata = fs::symlink_metadata(source)?;
        if metadata.is_dir() {
            volume.create_dir(&mut device, &path, 0o755, 0, 0, 0)?;
        } else if metadata.file_type().is_symlink() {
            let target = fs::read_link(source)?;
            volume.create_symlink(
                &mut device,
                &path,
                target.to_str().ok_or("non-UTF8 symlink")?,
                0o777,
                0,
                0,
                0,
            )?;
        } else {
            // Bundle executables must keep their executable bit even when the
            // packaging host uses Windows filesystem permissions.
            let mode = if path.contains("/Contents/MacOS/") {
                0o755
            } else {
                0o644
            };
            volume.create_file(
                &mut device,
                &path,
                &mut File::open(source)?,
                metadata.len(),
                mode,
                0,
                0,
                0,
            )?;
        }
    }
    volume.create_symlink(
        &mut device,
        "/Applications",
        "/Applications",
        0o777,
        0,
        0,
        0,
    )?;
    volume.flush(&mut device)?;
    device.sync()?;
    drop(device);
    // The Apple_HFS partition name makes this a directly mountable HFS+ UDIF
    // volume, rather than an ISO file renamed to .dmg.
    let partial = output.with_extension("dmg.partial");
    let mut writer = DmgWriter::new(BufWriter::new(File::create(&partial)?));
    writer.add_partition("whole disk (Apple_HFS : 0)", &fs::read(&raw)?)?;
    writer.finish()?;
    verify(app, &partial)?;
    if output.exists() {
        fs::remove_file(output)?;
    }
    fs::rename(partial, output)?;
    eprintln!("Created and verified {}", output.display());
    Ok(())
}

/// Re-open the compressed DMG, extract the HFS+ volume, and compare every app
/// file with its source. Verify the drag-and-drop link separately.
pub fn verify(app: &Path, image: &Path) -> Result {
    let temporary = tempfile::tempdir()?;
    let raw = temporary.path().join("verify.hfs");
    let mut archive = DmgReader::open(image)?;
    let checksum = u32::from(archive.koly().data_fork_digest);
    if archive.data_checksum()? != checksum {
        return Err("DMG data checksum is invalid".into());
    }
    fs::write(&raw, archive.partition_data(0)?)?;
    let mut device = FileBackend::open_read_only(&raw)?;
    let mut volume = HfsPlus::open(&mut device)?;
    if volume.read_symlink_target_path(&mut device, "/Applications")? != "/Applications" {
        return Err("DMG Applications link is invalid".into());
    }
    let parent = app.parent().ok_or("app has no parent directory")?;
    for source in entries(app)? {
        let path = image_path(&source, parent)?;
        let metadata = fs::symlink_metadata(&source)?;
        if metadata.is_dir() {
            volume.list_path(&mut device, &path)?;
        } else if metadata.file_type().is_symlink() {
            if Path::new(&volume.read_symlink_target_path(&mut device, &path)?)
                != fs::read_link(&source)?
            {
                return Err(format!("DMG symlink differs: {path}").into());
            }
        } else {
            if path.contains("/Contents/MacOS/")
                && volume.getattr(&mut device, Path::new(&path))?.mode & 0o111 == 0
            {
                return Err(format!("DMG executable has no execute permission: {path}").into());
            }
            let mut reader = volume.open_file_reader(&mut device, &path)?;
            if digest(&mut reader)? != digest(&mut File::open(&source)?)? {
                return Err(format!("DMG file differs: {path}").into());
            }
        }
    }
    Ok(())
}

fn digest(reader: &mut impl Read) -> Result<Vec<u8>> {
    let mut hash = Sha256::new();
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = reader.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    Ok(hash.finalize().to_vec())
}

fn image_path(source: &Path, parent: &Path) -> Result<String> {
    Ok(format!(
        "/{}",
        source
            .strip_prefix(parent)?
            .to_str()
            .ok_or("non-UTF8 bundle path")?
            .replace('\\', "/")
    ))
}

pub(super) fn entries(root: &Path) -> Result<Vec<PathBuf>> {
    let mut result = vec![root.to_path_buf()];
    if fs::symlink_metadata(root)?.is_dir() {
        let mut children = fs::read_dir(root)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<io::Result<Vec<_>>>()?;
        children.sort();
        for child in children {
            result.extend(entries(&child)?);
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disk_image_contains_complete_app_and_applications_link() -> Result {
        let temporary = tempfile::tempdir()?;
        let app = temporary.path().join("Jimpactor.app");
        fs::create_dir_all(app.join("Contents/MacOS"))?;
        fs::create_dir_all(app.join("Contents/Resources"))?;
        fs::write(app.join("Contents/MacOS/jimpactor"), b"executable fixture")?;
        fs::write(app.join("Contents/Info.plist"), b"plist fixture")?;
        fs::write(
            app.join("Contents/Resources/unicode-é.txt"),
            b"resource fixture",
        )?;
        let image = temporary.path().join("Jimpactor.dmg");
        create(&app, &image)?;
        verify(&app, &image)?;
        // A reader from a separate project must also recognize the UDIF volume.
        let mut independent = udif::DmgArchive::open(&image)?;
        let partition = independent.extract_main_partition()?;
        assert_eq!(&partition[1024..1026], b"H+");
        fs::write(app.join("Contents/MacOS/jimpactor"), b"changed executable")?;
        assert!(
            verify(&app, &image).is_err(),
            "verification must reject changed executable bytes"
        );
        Ok(())
    }
}
