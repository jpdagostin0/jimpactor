use crate::{Result, invalid};
use std::{
    fs, io,
    path::{Path, PathBuf},
};
use zip::{ZipArchive, ZipWriter, write::SimpleFileOptions};

pub struct ZIPCompressor;
impl ZIPCompressor {
    pub fn decompress(file: &Path, directory: &Path, progress: &dyn Fn(Option<f64>)) -> Result<()> {
        progress(None);
        fs::create_dir_all(directory)?;
        // zip validates enclosed paths and symlink destinations during extraction.
        ZipArchive::new(fs::File::open(file)?)?.extract(directory)?;
        progress(Some(1.0));
        Ok(())
    }
    /// Writes a ZIP containing the supplied directory itself (normally Payload).
    pub fn compress(
        directory: &Path,
        destination: &Path,
        progress: &dyn Fn(Option<f64>),
    ) -> Result<()> {
        let directory = directory.canonicalize()?;
        let parent = directory
            .parent()
            .ok_or_else(|| invalid("Cannot archive filesystem root"))?;
        let destination_parent = destination
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let destination_absolute = destination_parent.canonicalize()?.join(
            destination
                .file_name()
                .ok_or_else(|| invalid("Invalid archive path"))?,
        );
        if destination_absolute.starts_with(&directory) {
            return Err(invalid(
                "Archive destination must be outside the source directory",
            ));
        }
        let mut files = Vec::new();
        collect(&directory, &mut files)?;
        let mut writer = ZipWriter::new(fs::File::create(destination)?);
        let total = files.len().max(1) as f64;
        progress(Some(0.0));
        for (index, path) in files.iter().enumerate() {
            let name = path
                .strip_prefix(parent)
                .map_err(|e| invalid(e.to_string()))?
                .to_string_lossy()
                .replace('\\', "/");
            let metadata = fs::symlink_metadata(path)?;
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                metadata.permissions().mode()
            };
            #[cfg(not(unix))]
            let mode = if metadata.is_dir() { 0o755 } else { 0o644 };
            let options = SimpleFileOptions::default().unix_permissions(mode);
            if metadata.file_type().is_symlink() {
                writer.add_symlink(
                    name,
                    fs::read_link(path)?.to_string_lossy().replace('\\', "/"),
                    options,
                )?;
            } else if metadata.is_dir() {
                writer.add_directory(format!("{name}/"), options)?;
            } else {
                writer.start_file(name, options)?;
                io::copy(&mut fs::File::open(path)?, &mut writer)?;
            }
            progress(Some((index + 1) as f64 / total));
        }
        writer.finish()?;
        progress(Some(1.0));
        Ok(())
    }
}

fn collect(path: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
    result.push(path.to_path_buf());
    if fs::symlink_metadata(path)?.is_dir() {
        let mut entries = fs::read_dir(path)?.collect::<std::result::Result<Vec<_>, _>>()?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            collect(&entry.path(), result)?;
        }
    }
    Ok(())
}

pub fn copy_directory(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let path = entry.path();
        let target = destination.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            let link = fs::read_link(&path)?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(link, target)?;
            #[cfg(windows)]
            {
                if path.is_dir() {
                    std::os::windows::fs::symlink_dir(link, target)?;
                } else {
                    std::os::windows::fs::symlink_file(link, target)?;
                }
            }
            #[cfg(not(any(unix, windows)))]
            return Err(invalid("Symlinks are unsupported on this platform"));
        } else if kind.is_dir() {
            copy_directory(&path, &target)?;
        } else {
            fs::copy(path, target)?;
        }
    }
    Ok(())
}
