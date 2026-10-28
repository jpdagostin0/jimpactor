use plume_core::{MachO, MachOExt};
use plume_utils::{
    Bundle, PlistInfoTrait, Signer, SignerMode, SignerOptions, Tweak, TweakInjection,
    TweakLoadPath, TweakLoader,
};
use std::{fs, path::Path};

fn app(path: &Path) -> Bundle {
    fs::create_dir_all(path).unwrap();
    let info = plist::Dictionary::from_iter(
        [
            ("CFBundleIdentifier", "com.example.original"),
            ("CFBundleExecutable", "Executable"),
            ("CFBundleName", "Original"),
        ]
        .map(|(key, value)| (key, plist::Value::String(value.into()))),
    );
    plist::to_file_xml(path.join("Info.plist"), &info).unwrap();
    fs::write(
        path.join("Executable"),
        include_bytes!("../../xkit/tests/fixtures/minimal-arm64.macho"),
    )
    .unwrap();
    Bundle::new(path.to_owned()).unwrap()
}

#[tokio::test]
async fn modification_only_changes_metadata_and_preserves_executable_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = app(&dir.path().join("Test.app"));
    let original = fs::read(bundle.bundle_dir().join("Executable")).unwrap();
    let mut signer = Signer::new(
        None,
        SignerOptions {
            mode: SignerMode::ModifyOnly,
            custom_name: Some("Renamed".into()),
            custom_url_schemes: Some(vec!["duplicate-app".into(), "duplicate-login".into()]),
            ..Default::default()
        },
    );
    signer
        .modify_bundle(&bundle, &Some("TEAM123456".into()))
        .await
        .unwrap();
    signer.sign_bundle(&bundle).await.unwrap();
    assert_eq!(bundle.get_name().as_deref(), Some("Renamed"));
    assert_eq!(
        bundle.get_bundle_identifier().as_deref(),
        Some("com.example.original")
    );
    assert_eq!(
        fs::read(bundle.bundle_dir().join("Executable")).unwrap(),
        original
    );
    let value = plist::Value::from_file(bundle.bundle_dir().join("Info.plist")).unwrap();
    let urls = value.as_dictionary().unwrap()["CFBundleURLTypes"]
        .as_array()
        .unwrap();
    let schemes = urls[0].as_dictionary().unwrap()["CFBundleURLSchemes"]
        .as_array()
        .unwrap();
    assert_eq!(
        schemes,
        &vec![
            plist::Value::String("duplicate-app".into()),
            plist::Value::String("duplicate-login".into())
        ]
    );
    bundle.set_url_schemes(&[]).unwrap();
    assert!(
        !plist::Value::from_file(bundle.bundle_dir().join("Info.plist"))
            .unwrap()
            .as_dictionary()
            .unwrap()
            .contains_key("CFBundleURLTypes")
    );
    assert!(bundle.set_url_schemes(&["bad://scheme".into()]).is_err());
}

#[tokio::test]
async fn no_modify_keeps_metadata_and_executable_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = app(&dir.path().join("Test.app"));
    let original = fs::read(bundle.bundle_dir().join("Info.plist")).unwrap();
    let mut signer = Signer::new(
        None,
        SignerOptions {
            mode: SignerMode::None,
            custom_name: Some("Ignored".into()),
            ..Default::default()
        },
    );
    signer.modify_bundle(&bundle, &None).await.unwrap();
    signer.sign_bundle(&bundle).await.unwrap();
    assert_eq!(
        fs::read(bundle.bundle_dir().join("Info.plist")).unwrap(),
        original
    );
}

#[tokio::test]
async fn custom_tweak_paths_copy_and_inject_without_bundling_a_loader() {
    for (folder, prefix, expected) in [
        (
            "/",
            TweakLoadPath::ExecutablePath,
            "@executable_path/Custom.dylib",
        ),
        (
            "Libraries",
            TweakLoadPath::Rpath,
            "@rpath/Libraries/Custom.dylib",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let bundle = app(&dir.path().join("Test.app"));
        let source = dir.path().join("Custom.dylib");
        let data = include_bytes!("../../xkit/tests/fixtures/minimal-arm64.macho");
        fs::write(&source, data).unwrap();
        let options = TweakInjection {
            loader: TweakLoader::None,
            folder: Some(folder.into()),
            load_path: prefix,
        };
        let destination = bundle
            .bundle_dir()
            .join(options.folder_path().unwrap())
            .join("Custom.dylib");
        let mut signer = Signer::new(
            None,
            SignerOptions {
                mode: SignerMode::ModifyOnly,
                tweak_injection: options,
                tweaks: Some(vec![source]),
                ..Default::default()
            },
        );
        signer.modify_bundle(&bundle, &None).await.unwrap();
        assert_eq!(fs::read(destination).unwrap(), data);
        assert!(
            !bundle
                .bundle_dir()
                .join("Frameworks/CydiaSubstrate.framework")
                .exists()
        );
        let binary = MachO::new(bundle.bundle_dir().join("Executable")).unwrap();
        assert!(
            binary
                .macho_file()
                .nth_macho(0)
                .unwrap()
                .dylib_load_paths()
                .unwrap()
                .iter()
                .any(|path| path == expected)
        );
        if prefix == TweakLoadPath::Rpath {
            assert!(
                binary
                    .macho_file()
                    .nth_macho(0)
                    .unwrap()
                    .macho
                    .rpaths
                    .contains(&"@executable_path")
            );
        }
    }
}

#[test]
fn injection_rejects_traversal_and_refuses_to_overwrite_macho_contents() {
    for folder in ["../escape", "C:/escape", "/absolute", "nested/../../escape"] {
        assert!(
            TweakInjection {
                folder: Some(folder.into()),
                ..Default::default()
            }
            .folder_path()
            .is_err()
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let bundle = app(&dir.path().join("Test.app"));
    let path = bundle.bundle_dir().join("Executable");
    let original = fs::read(&path).unwrap();
    let mut binary = MachO::new(&path).unwrap();
    assert!(binary.add_rpath(&"x".repeat(20_000)).is_err());
    assert_eq!(fs::read(path).unwrap(), original);
}

#[tokio::test]
async fn custom_framework_uses_its_executable_name() {
    let dir = tempfile::tempdir().unwrap();
    let bundle = app(&dir.path().join("Test.app"));
    let framework = app(&dir.path().join("Custom.framework"));
    let tweak = Tweak::new_with_options(
        framework.bundle_dir(),
        &bundle,
        TweakInjection {
            loader: TweakLoader::None,
            folder: Some("Libraries".into()),
            load_path: TweakLoadPath::ExecutablePath,
        },
    )
    .await
    .unwrap();
    tweak.apply().await.unwrap();
    let binary = MachO::new(bundle.bundle_dir().join("Executable")).unwrap();
    assert!(
        binary
            .macho_file()
            .nth_macho(0)
            .unwrap()
            .dylib_load_paths()
            .unwrap()
            .contains(&"@executable_path/Libraries/Custom.framework/Executable".into())
    );
}
