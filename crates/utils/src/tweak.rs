use std::{
    env,
    path::{Path, PathBuf},
};

use plume_core::MachO;
use uuid::Uuid;

use crate::{
    Bundle, Error, PlistInfoTrait, TweakInjection, TweakLoadPath, TweakLoader, copy_dir_recursively,
};

const ELLEKIT_BYTES: &[u8] = include_bytes!("./ellekit.deb");

pub struct Tweak {
    path: PathBuf,
    app_bundle: PathBuf,
    stage_dir: PathBuf,
    options: TweakInjection,
}

impl Tweak {
    pub async fn install_ellekit(app_bundle: &Bundle) -> Result<(), Error> {
        let stage_dir = env::temp_dir().join(format!("plume_ellekit_{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&stage_dir).await?;

        let deb_path = stage_dir.join("ellekit.deb");
        tokio::fs::write(&deb_path, ELLEKIT_BYTES).await?;

        let tweak = Tweak::new(&deb_path, app_bundle).await?;
        tweak.install_deb().await?;

        tokio::fs::remove_dir_all(&stage_dir).await.ok();

        Ok(())
    }

    pub async fn new<P: AsRef<Path>>(tweak_path: P, app_bundle: &Bundle) -> Result<Self, Error> {
        Self::new_with_options(tweak_path, app_bundle, TweakInjection::default()).await
    }

    pub async fn new_with_options<P: AsRef<Path>>(
        tweak_path: P,
        app_bundle: &Bundle,
        options: TweakInjection,
    ) -> Result<Self, Error> {
        options.folder_path()?;
        let path = tweak_path.as_ref();
        if !path.exists() {
            return Err(Error::TweakInvalidPath);
        }

        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(Error::TweakInvalidPath)?;

        if !file_name.ends_with(".deb")
            && !file_name.ends_with(".dylib")
            && !file_name.ends_with(".framework")
            && !file_name.ends_with(".bundle")
            && !file_name.ends_with(".appex")
        {
            return Err(Error::UnsupportedFileType(file_name.to_string()));
        }

        let stage_dir = env::temp_dir().join(format!("plume_tweak_{}", Uuid::new_v4()));
        tokio::fs::create_dir_all(&stage_dir).await?;

        Ok(Self {
            path: path.to_path_buf(),
            app_bundle: app_bundle.bundle_dir().clone(),
            stage_dir,
            options,
        })
    }

    pub async fn apply(&self) -> Result<(), Error> {
        let file_name = self
            .path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or(Error::TweakInvalidPath)?;

        if file_name.ends_with(".deb") {
            self.install_deb().await?;
        } else if file_name.ends_with(".framework") {
            self.install_framework(&self.path).await?;
        } else if file_name.ends_with(".bundle") {
            self.install_bundle(&self.path).await?;
        } else if file_name.ends_with(".appex") {
            self.install_appex(&self.path).await?;
        } else if file_name.ends_with(".dylib") {
            self.install_dylib(&self.path).await?;
        }

        tokio::fs::remove_dir_all(&self.stage_dir).await.ok();

        Ok(())
    }

    async fn install_deb(&self) -> Result<(), Error> {
        let extract_dir = self.stage_dir.join("deb_contents");
        let source = self.path.clone();
        let destination = extract_dir.clone();
        tokio::task::spawn_blocking(move || extract_deb(&source, &destination))
            .await
            .map_err(|err| Error::TweakExtractionFailed(err.to_string()))?
            .map_err(|err| Error::TweakExtractionFailed(err.to_string()))?;

        self.scan_and_install(&extract_dir).await
    }

    async fn scan_and_install(&self, root: &Path) -> Result<(), Error> {
        let search_paths = [
            "Library/MobileSubstrate/DynamicLibraries",
            "usr/lib",
            "Library/Frameworks",
            "Library/Application Support",
            "var/jb/Library/MobileSubstrate/DynamicLibraries",
            "var/jb/usr/lib",
            "var/jb/Library/Frameworks",
            "var/jb/Library/Application Support",
        ];

        for search_path in search_paths {
            let dir = root.join(search_path);
            if dir.exists() {
                self.scan_directory(&dir).await?;
            }
        }

        Ok(())
    }

    async fn scan_directory(&self, dir: &Path) -> Result<(), Error> {
        use futures::future::BoxFuture;

        fn scan_recursive<'a>(tweak: &'a Tweak, dir: &'a Path) -> BoxFuture<'a, Result<(), Error>> {
            Box::pin(async move {
                let mut entries = tokio::fs::read_dir(dir).await?;

                while let Some(entry) = entries.next_entry().await? {
                    let path = entry.path();
                    let metadata = tokio::fs::symlink_metadata(&path).await?;

                    if metadata.is_symlink() {
                        continue;
                    }

                    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                        if path.is_file() && name.ends_with(".dylib") {
                            tweak.install_dylib(&path).await?;
                        } else if path.is_dir() {
                            if name.ends_with(".framework") {
                                tweak.install_framework(&path).await?;
                            } else if name.ends_with(".bundle") {
                                tweak.install_bundle(&path).await?;
                            } else if name.ends_with(".appex") {
                                tweak.install_appex(&path).await?;
                            } else {
                                // Recursively scan subdirectories
                                scan_recursive(tweak, &path).await?;
                            }
                        }
                    }
                }

                Ok(())
            })
        }

        scan_recursive(self, dir).await
    }

    async fn install_dylib(&self, dylib_path: &Path) -> Result<(), Error> {
        let frameworks_dir = self.app_bundle.join(self.options.folder_path()?);
        tokio::fs::create_dir_all(&frameworks_dir).await?;

        let dylib_name = dylib_path.file_name().ok_or(Error::TweakInvalidPath)?;
        let dest = frameworks_dir.join(dylib_name);

        tokio::fs::copy(dylib_path, &dest).await?;

        if self.options.loader == TweakLoader::ElleKit {
            Self::patch_cydiasubstrate(&dest);
        }
        self.inject_dylib(&dest).await
    }

    async fn install_framework(&self, framework_path: &Path) -> Result<(), Error> {
        let frameworks_dir = self.app_bundle.join(self.options.folder_path()?);
        tokio::fs::create_dir_all(&frameworks_dir).await?;

        let framework_name = framework_path.file_name().ok_or(Error::TweakInvalidPath)?;
        let dest = frameworks_dir.join(framework_name);

        copy_dir_recursively(framework_path, &dest).await?;

        if let Ok(bundle) = Bundle::new(&dest)
            && let Some(exec_name) = bundle.get_executable()
        {
            let exec_path = dest.join(exec_name);
            if exec_path.exists() {
                if self.options.loader == TweakLoader::ElleKit {
                    Self::patch_cydiasubstrate(&exec_path);
                }
                self.inject_dylib(&exec_path).await?;
            }
        }

        Ok(())
    }

    async fn install_bundle(&self, bundle_path: &Path) -> Result<(), Error> {
        let bundle_name = bundle_path.file_name().ok_or(Error::TweakInvalidPath)?;
        let dest = self.app_bundle.join(bundle_name);

        copy_dir_recursively(bundle_path, &dest).await
    }

    async fn install_appex(&self, appex_path: &Path) -> Result<(), Error> {
        let plugins_dir = self.app_bundle.join("PlugIns");
        tokio::fs::create_dir_all(&plugins_dir).await?;

        let appex_name = appex_path.file_name().ok_or(Error::TweakInvalidPath)?;
        let dest = plugins_dir.join(appex_name);

        copy_dir_recursively(appex_path, &dest).await
    }

    async fn inject_dylib(&self, dylib_path: &Path) -> Result<(), Error> {
        let bundle = Bundle::new(&self.app_bundle)?;
        let executable_name = bundle
            .get_executable()
            .ok_or(Error::BundleInfoPlistMissing)?;

        let executable_path = self.app_bundle.join(&executable_name);
        if !executable_path.exists() {
            return Err(Error::BundleInfoPlistMissing);
        }

        let relative = dylib_path
            .strip_prefix(&self.app_bundle)
            .map_err(|_| Error::TweakInvalidPath)?;
        let inject_path = self.options.library_path(relative)?;

        let mut macho = MachO::new(&executable_path)?;
        if self.options.folder.is_some() && self.options.load_path == TweakLoadPath::Rpath {
            macho.add_rpath("@executable_path")?;
        }
        macho.add_dylib(&inject_path)?;

        Ok(())
    }

    fn patch_cydiasubstrate(binary_path: &Path) {
        if let Ok(mut macho) = MachO::new(binary_path) {
            let _ = macho.replace_dylib(
                "/Library/Frameworks/CydiaSubstrate.framework/CydiaSubstrate",
                "@rpath/CydiaSubstrate.framework/CydiaSubstrate",
            );
        }
    }
}

// A Debian package is an ar container holding a compressed data.tar member.
// Delegate parsing, decoding and safe filesystem extraction to upstream crates.
fn extract_deb(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::{
        fs::File,
        io::{self, BufReader, Read},
    };

    let mut archive = ar::Archive::new(BufReader::new(File::open(source)?));
    while let Some(entry) = archive.next_entry() {
        let entry = entry?;
        let reader: Box<dyn Read + '_> = match entry.header().identifier() {
            b"data.tar" => Box::new(entry),
            b"data.tar.gz" => Box::new(flate2::read::GzDecoder::new(entry)),
            b"data.tar.bz2" => Box::new(bzip2::read::BzDecoder::new(entry)),
            b"data.tar.xz" => Box::new(xz2::read::XzDecoder::new_multi_decoder(entry)),
            b"data.tar.lzma" => Box::new(xz2::read::XzDecoder::new_stream(
                entry,
                xz2::stream::Stream::new_lzma_decoder(u64::MAX)?,
            )),
            _ => continue,
        };
        return tar::Archive::new(reader).unpack(destination);
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "Debian package has no supported data archive",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read, Write};

    fn package(name: &str, data: &[u8]) -> tempfile::NamedTempFile {
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let mut ar = ar::Builder::new(file.as_file_mut());
        ar.append(
            &ar::Header::new(b"debian-binary".to_vec(), 4),
            Cursor::new(b"2.0\n"),
        )
        .unwrap();
        ar.append(
            &ar::Header::new(name.as_bytes().to_vec(), data.len() as u64),
            Cursor::new(data),
        )
        .unwrap();
        file
    }

    fn encode(mut reader: impl Read) -> Vec<u8> {
        let mut data = Vec::new();
        reader.read_to_end(&mut data).unwrap();
        data
    }

    #[test]
    fn extracts_supported_debian_compressions() {
        let payload = b"tweak contents";
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(payload.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        tar.append_data(&mut header, "usr/lib/test.dylib", payload.as_slice())
            .unwrap();
        let data = tar.into_inner().unwrap();
        let lzma = xz2::stream::Stream::new_lzma_encoder(
            &xz2::stream::LzmaOptions::new_preset(6).unwrap(),
        )
        .unwrap();
        for (name, encoded) in [
            (
                "data.tar.gz",
                encode(flate2::read::GzEncoder::new(
                    data.as_slice(),
                    flate2::Compression::default(),
                )),
            ),
            (
                "data.tar.bz2",
                encode(bzip2::read::BzEncoder::new(
                    data.as_slice(),
                    bzip2::Compression::default(),
                )),
            ),
            (
                "data.tar.xz",
                encode(xz2::read::XzEncoder::new(data.as_slice(), 6)),
            ),
            (
                "data.tar.lzma",
                encode(xz2::read::XzEncoder::new_stream(data.as_slice(), lzma)),
            ),
            ("data.tar", data),
        ] {
            let source = package(name, &encoded);
            let destination = tempfile::tempdir().unwrap();
            extract_deb(source.path(), destination.path()).unwrap();
            assert_eq!(
                std::fs::read(destination.path().join("usr/lib/test.dylib")).unwrap(),
                payload,
                "{name}"
            );
        }
    }

    #[test]
    fn extracts_bundled_ellekit() {
        let mut source = tempfile::NamedTempFile::new().unwrap();
        source.write_all(ELLEKIT_BYTES).unwrap();
        let destination = tempfile::tempdir().unwrap();
        extract_deb(source.path(), destination.path()).unwrap();
        assert!(
            destination
                .path()
                .join("Library/Frameworks/CydiaSubstrate.framework/CydiaSubstrate")
                .is_file()
        );
    }

    #[test]
    fn rejects_missing_or_corrupt_data() {
        for name in ["control.tar", "data.tar.xz"] {
            let source = package(name, b"invalid");
            let destination = tempfile::tempdir().unwrap();
            assert!(extract_deb(source.path(), destination.path()).is_err());
        }
    }
}
