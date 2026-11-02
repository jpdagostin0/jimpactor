//! Copies an app's container to a zip file and back.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use idevice::IdeviceError;
use idevice::afc::AfcClient;
use idevice::afc::opcode::AfcFopenMode;
use serde::{Deserialize, Serialize};
use zip::ZipArchive;
use zip::write::SimpleFileOptions;

use crate::device::{HOUSE_ARREST_LABEL, open_app_container};
use crate::{Device, Error};

/// Describes a backup; stored in the archive beside `Documents` and `Library`.
pub const BACKUP_MANIFEST: &str = ".jimpactor-backup.json";
const CHUNK: usize = 1 << 20;
/// What a restore may write. iOS recreates everything else in a container.
const RESTORABLE: [&str; 2] = ["Documents", "Library"];
/// Rebuilt by the app, and often most of a container's size.
const NOT_BACKED_UP: &str = "Library/Caches";

/// Which part of a container an archive holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveScope {
    /// What a restore can use: `Documents` and `Library`, without caches.
    Backup,
    /// Every readable file, including `tmp` and caches.
    Everything,
}

impl ArchiveScope {
    /// Whether the file or folder at `path` (relative to the container) belongs in the archive.
    fn includes(self, path: &str) -> bool {
        match self {
            ArchiveScope::Everything => true,
            ArchiveScope::Backup => {
                let top = path.split('/').next().unwrap_or(path);
                RESTORABLE.contains(&top)
                    && path != NOT_BACKED_UP
                    && !path.starts_with("Library/Caches/")
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    pub bundle_id: String,
    pub app_version: Option<String>,
    pub created: DateTime<Utc>,
    pub files: usize,
}

/// How an archive or restore went. `skipped` lists files iOS would not let us touch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransferSummary {
    pub files: usize,
    pub bytes: u64,
    pub skipped: Vec<String>,
}

/// Reads the manifest of a backup archive.
pub fn read_backup_manifest(path: &Path) -> Result<BackupManifest, Error> {
    let mut archive = ZipArchive::new(File::open(path)?)?;
    let mut entry = archive
        .by_name(BACKUP_MANIFEST)
        .map_err(|_| Error::Other(format!("{} is not a Jimpactor backup", path.display())))?;
    let mut json = String::new();
    entry.read_to_string(&mut json)?;
    serde_json::from_str(&json).map_err(|e| Error::Other(format!("Damaged backup: {e}")))
}

/// Where a zip entry goes in the container, or `None` for entries that must not be restored.
/// Archives can come from anywhere, so nothing may escape `Documents` or `Library`.
fn restore_path(name: &str) -> Option<String> {
    let mut parts = name.split('/');
    if !RESTORABLE.contains(&parts.next()?) {
        return None;
    }
    let safe = parts.all(|part| {
        !part.is_empty() && part != "." && part != ".." && !part.contains(['\\', '\0'])
    });
    (safe && name.contains('/')).then(|| format!("/{name}"))
}

/// A failed file is skipped only when iOS refused it. Anything else means the connection is
/// gone, and carrying on would report a nearly empty archive as a success.
fn refused(error: &IdeviceError) -> bool {
    matches!(error, IdeviceError::Afc(_))
}

async fn copy_to_zip(
    afc: &mut AfcClient,
    zip: &mut zip::ZipWriter<File>,
    path: &str,
    options: SimpleFileOptions,
) -> Result<u64, IdeviceError> {
    let mut file = afc.open(format!("/{path}"), AfcFopenMode::RdOnly).await?;
    zip.start_file(path, options).map_err(io_error)?;
    let mut total = 0;
    let copied = loop {
        // `read_n` keeps AFC errors typed, unlike the `AsyncRead` implementation.
        match file.read_n(CHUNK).await {
            Ok(chunk) if chunk.is_empty() => break Ok(()),
            Ok(chunk) => {
                zip.write_all(&chunk).map_err(IdeviceError::Socket)?;
                total += chunk.len() as u64;
            }
            Err(error) => break Err(error),
        }
    };
    if let Err(error) = copied {
        zip.abort_file().map_err(io_error)?;
        return Err(error);
    }
    file.close().await?;
    Ok(total)
}

fn io_error(error: zip::result::ZipError) -> IdeviceError {
    IdeviceError::Socket(std::io::Error::other(error))
}

impl Device {
    /// Zips this app's container into `destination`. The file only appears once it is
    /// complete, so an interrupted run never leaves a partial archive behind.
    /// `on_file` is told each file as it starts.
    pub async fn archive_app_files(
        &self,
        bundle_id: &str,
        app_version: Option<String>,
        scope: ArchiveScope,
        destination: &Path,
        on_file: impl Fn(&str) + Send + 'static,
    ) -> Result<TransferSummary, Error> {
        let identifier = bundle_id.to_owned();
        let destination = destination.to_owned();
        let partial = partial_path(&destination);
        let working = partial.clone();
        let result = self
            .with_provider(HOUSE_ARREST_LABEL, move |provider| async move {
                let mut afc = open_app_container(provider.as_ref(), &identifier).await?;
                let mut zip = zip::ZipWriter::new(File::create(&working)?);
                let options = SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .large_file(true);
                let mut summary = TransferSummary::default();

                let mut pending = vec![String::new()];
                while let Some(folder) = pending.pop() {
                    let names = match afc.list_dir(format!("/{folder}")).await {
                        Ok(names) => names,
                        Err(error) if refused(&error) && !folder.is_empty() => {
                            log::debug!("Could not list {folder}: {error}");
                            summary.skipped.push(format!("{folder}/"));
                            continue;
                        }
                        Err(error) => return Err(error.into()),
                    };
                    for name in names {
                        if name == "." || name == ".." {
                            continue;
                        }
                        let path = if folder.is_empty() {
                            name
                        } else {
                            format!("{folder}/{name}")
                        };
                        if !scope.includes(&path) {
                            continue;
                        }
                        let info = match afc.get_file_info(format!("/{path}")).await {
                            Ok(info) => info,
                            Err(error) if refused(&error) => {
                                summary.skipped.push(path);
                                continue;
                            }
                            Err(error) => return Err(error.into()),
                        };
                        match info.st_ifmt.as_str() {
                            "S_IFDIR" => pending.push(path),
                            // Links lead outside the container or back into it.
                            "S_IFREG" => {
                                on_file(&path);
                                match copy_to_zip(&mut afc, &mut zip, &path, options).await {
                                    Ok(bytes) => {
                                        summary.files += 1;
                                        summary.bytes += bytes;
                                    }
                                    Err(error) if refused(&error) => {
                                        log::debug!("Skipped {path}: {error}");
                                        summary.skipped.push(path);
                                    }
                                    Err(error) => return Err(error.into()),
                                }
                            }
                            _ => {}
                        }
                    }
                }

                if scope == ArchiveScope::Backup {
                    let manifest = BackupManifest {
                        bundle_id: identifier,
                        app_version,
                        created: Utc::now(),
                        files: summary.files,
                    };
                    zip.start_file(BACKUP_MANIFEST, options)?;
                    zip.write_all(
                        &serde_json::to_vec_pretty(&manifest)
                            .map_err(|e| Error::Other(e.to_string()))?,
                    )?;
                }
                zip.finish()?;
                Ok(summary)
            })
            .await;

        match &result {
            Ok(_) => std::fs::rename(&partial, &destination)?,
            Err(_) => {
                let _ = std::fs::remove_file(&partial);
            }
        }
        result
    }

    /// Writes a backup's `Documents` and `Library` into this app's container. Files in the
    /// backup replace those on the device; anything else on the device is left alone.
    pub async fn restore_app_files(
        &self,
        bundle_id: &str,
        archive: &Path,
        on_file: impl Fn(&str) + Send + 'static,
    ) -> Result<TransferSummary, Error> {
        read_backup_manifest(archive)?;
        let identifier = bundle_id.to_owned();
        let archive = archive.to_owned();
        self.with_provider(HOUSE_ARREST_LABEL, move |provider| async move {
            let mut afc = open_app_container(provider.as_ref(), &identifier).await?;
            let mut zip = ZipArchive::new(File::open(&archive)?)?;
            let mut summary = TransferSummary::default();
            let mut folders = std::collections::HashSet::new();

            for index in 0..zip.len() {
                // The entry borrows the archive and cannot be held across an await.
                let (name, is_file) = {
                    let entry = zip.by_index(index)?;
                    (entry.name().to_owned(), entry.is_file())
                };
                if name == BACKUP_MANIFEST || !is_file {
                    continue;
                }
                let Some(path) = restore_path(&name) else {
                    log::warn!("Not restoring {name}: it is outside the app's data");
                    summary.skipped.push(name);
                    continue;
                };
                on_file(&name);
                let data = {
                    let mut entry = zip.by_index(index)?;
                    let mut data = Vec::with_capacity(entry.size() as usize);
                    entry.read_to_end(&mut data)?;
                    data
                };

                let written = async {
                    for (end, _) in path.match_indices('/').skip(1) {
                        if folders.insert(path[..end].to_owned()) {
                            afc.mk_dir(&path[..end]).await?;
                        }
                    }
                    let mut file = afc.open(&path, AfcFopenMode::Wr).await?;
                    file.write_entire(&data).await?;
                    file.close().await
                }
                .await;
                match written {
                    Ok(_) => {
                        summary.files += 1;
                        summary.bytes += data.len() as u64;
                    }
                    Err(error) if refused(&error) => {
                        log::debug!("Could not restore {name}: {error}");
                        summary.skipped.push(name);
                    }
                    Err(error) => return Err(error.into()),
                }
            }
            Ok(summary)
        })
        .await
    }
}

fn partial_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_owned();
    name.push(".part");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backups_hold_app_data_without_caches() {
        let backup = ArchiveScope::Backup;
        assert!(backup.includes("Documents"));
        assert!(backup.includes("Documents/save.dat"));
        assert!(backup.includes("Library/Preferences/app.plist"));
        assert!(!backup.includes("Library/Caches"));
        assert!(!backup.includes("Library/Caches/big.bin"));
        assert!(backup.includes("Library/CachesButNotReally/keep.bin"));
        assert!(!backup.includes("tmp/scratch"));
        assert!(!backup.includes("SystemData"));
        assert!(ArchiveScope::Everything.includes("tmp/scratch"));
    }

    #[test]
    fn restores_cannot_leave_the_app_data() {
        assert_eq!(
            restore_path("Documents/a/b.txt").as_deref(),
            Some("/Documents/a/b.txt")
        );
        for name in [
            "Documents",
            "Documents/",
            "Documents/../../etc/passwd",
            "Documents/./a",
            "Documents//a",
            "Documents/a\\..\\b",
            "tmp/a",
            "/Documents/a",
            "../Documents/a",
            ".jimpactor-backup.json",
        ] {
            assert_eq!(restore_path(name), None, "{name}");
        }
    }

    #[test]
    fn manifests_are_required_to_restore() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("plain.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("Documents/a", SimpleFileOptions::default())
            .unwrap();
        zip.finish().unwrap();
        assert!(read_backup_manifest(&path).is_err());

        let path = temp.path().join("backup.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file(BACKUP_MANIFEST, SimpleFileOptions::default())
            .unwrap();
        let manifest = BackupManifest {
            bundle_id: "a.b".into(),
            app_version: Some("1.0".into()),
            created: Utc::now(),
            files: 3,
        };
        zip.write_all(&serde_json::to_vec(&manifest).unwrap())
            .unwrap();
        zip.finish().unwrap();
        assert_eq!(read_backup_manifest(&path).unwrap().bundle_id, "a.b");
    }

    #[test]
    fn partial_archives_sit_beside_the_destination() {
        assert_eq!(
            partial_path(Path::new("dir/app.zip")),
            PathBuf::from("dir/app.zip.part")
        );
    }
}
