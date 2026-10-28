use std::path::PathBuf;

/// Settings for the signer process.
#[derive(Clone, Debug)]
pub struct SignerOptions {
    /// Custom app name override.
    pub custom_name: Option<String>,
    /// Custom bundle identifier override.
    pub custom_identifier: Option<String>,
    /// Custom version override.
    pub custom_version: Option<String>,
    /// Replacement URL schemes; an empty list removes all registered schemes.
    pub custom_url_schemes: Option<Vec<String>>,
    /// Tweak loader and placement settings.
    pub tweak_injection: TweakInjection,
    pub custom_icon: Option<PathBuf>,
    /// Custom entitlements plist to embed (only used when single_profile is set).
    pub custom_entitlements: Option<PathBuf>,
    /// Feature support options.
    pub features: SignerFeatures,
    /// Embedding options.
    pub embedding: SignerEmbedding,
    /// Mode.
    pub mode: SignerMode,
    /// Installation mode.
    pub install_mode: SignerInstallMode,
    /// Tweaks to apply before signing.
    pub tweaks: Option<Vec<PathBuf>>,
    /// App type.
    pub app: SignerApp,
    /// Apply autorefresh
    pub refresh: bool,
}

impl Default for SignerOptions {
    fn default() -> Self {
        SignerOptions {
            custom_name: None,
            custom_identifier: None,
            custom_version: None,
            custom_url_schemes: None,
            tweak_injection: TweakInjection::default(),
            custom_icon: None,
            custom_entitlements: None,
            features: SignerFeatures::default(),
            embedding: SignerEmbedding::default(),
            mode: SignerMode::default(),
            install_mode: SignerInstallMode::default(),
            tweaks: None,
            app: SignerApp::Default,
            refresh: false,
        }
    }
}

impl SignerOptions {
    pub fn new_for_app(app: SignerApp) -> Self {
        let mut settings = Self {
            app,
            ..Self::default()
        };

        match app {
            SignerApp::LiveContainer | SignerApp::LiveContainerAndSideStore => {
                settings.embedding.single_profile = true;
            }
            _ => {}
        }

        settings
    }
}

#[derive(Clone, Debug, Default)]
pub struct SignerFeatures {
    pub support_minimum_os_version: bool,
    pub support_file_sharing: bool,
    pub support_ipad_fullscreen: bool,
    pub support_game_mode: bool,
    pub support_pro_motion: bool,
    pub support_liquid_glass: bool,
    pub support_ellekit: bool,
    pub remove_url_schemes: bool,
}

/// Embedding options.
#[derive(Clone, Debug, Default)]
pub struct SignerEmbedding {
    pub single_profile: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignerInstallMode {
    #[default]
    Install,
    Export,
}

impl std::fmt::Display for SignerInstallMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignerInstallMode::Install => write!(f, "Install"),
            SignerInstallMode::Export => write!(f, "Export"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SignerMode {
    #[default]
    Pem,
    Adhoc,
    ModifyOnly,
    None,
}

impl std::fmt::Display for SignerMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignerMode::Pem => write!(f, "Apple ID"),
            SignerMode::Adhoc => write!(f, "Adhoc"),
            SignerMode::ModifyOnly => write!(f, "Modify without signing"),
            SignerMode::None => write!(f, "No Modify"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerAppReal {
    pub app: SignerApp,
    pub bundle_id: Option<String>,
    pub installed_name: Option<String>,
    pub expiration: Option<plist::Date>,
}

impl SignerAppReal {
    pub fn from_bundle_identifier(identifier: Option<&str>) -> Self {
        let app = SignerApp::from_bundle_identifier(identifier);
        Self {
            app,
            bundle_id: identifier.map(|s| s.to_string()),
            installed_name: None,
            expiration: None,
        }
    }

    pub fn from_bundle_identifier_and_name(identifier: Option<&str>, name: Option<&str>) -> Self {
        let app = SignerApp::from_bundle_identifier_or_name(identifier, name);
        Self {
            app,
            bundle_id: identifier.map(|s| s.to_string()),
            installed_name: None,
            expiration: None,
        }
    }
}

/// Supported app types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignerApp {
    Default,
    Antrag,
    Feather,
    Protokolle,
    AltStore,
    SideStore,
    LiveContainer,
    LiveContainerAndSideStore,
    StikDebug,
    SparseBox,
    EnsWilde,
    ByeTunes,
    StikStore,
    Reynard,
    Ksign,
    AutoCapture,
}

impl std::fmt::Display for SignerApp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use SignerApp::*;
        let name = match self {
            Default => "Default",
            Antrag => "Antrag",
            Feather => "Feather",
            Protokolle => "Protokolle",
            AltStore => "AltStore",
            SideStore => "SideStore",
            LiveContainer | LiveContainerAndSideStore => "LiveContainer",
            StikDebug => "StikDebug",
            SparseBox => "SparseBox",
            EnsWilde => "EnsWilde",
            ByeTunes => "ByeTunes",
            StikStore => "StikStore",
            Reynard => "Reynard",
            Ksign => "Ksign",
            AutoCapture => "Dev Auto Capture",
        };
        write!(f, "{}", name)
    }
}

impl SignerApp {
    pub fn from_bundle_identifier(identifier: Option<impl AsRef<str>>) -> Self {
        let id = match identifier {
            Some(id) => id.as_ref().to_owned(),
            None => return SignerApp::Default,
        };

        const KNOWN_APPS: &[(&str, SignerApp)] = &[
            ("com.kdt.livecontainer", SignerApp::LiveContainer),
            ("thewonderofyou.syslog", SignerApp::Protokolle),
            ("thewonderofyou.antrag2", SignerApp::Antrag),
            ("thewonderofyou.Feather", SignerApp::Feather),
            ("com.SideStore.SideStore", SignerApp::SideStore),
            ("com.rileytestut.AltStore", SignerApp::AltStore),
            ("com.stik.sj", SignerApp::StikDebug),
            ("com.kdt.SparseBox", SignerApp::SparseBox),
            ("com.yangjiii.EnsWilde", SignerApp::EnsWilde),
            ("com.EduAlexxis.MusicManager", SignerApp::ByeTunes),
            ("me.stik.store", SignerApp::StikStore),
            ("app.stik.store", SignerApp::StikStore),
            ("com.minh-ton.Reynard", SignerApp::Reynard),
            ("nya.asami.ksign", SignerApp::Ksign),
            ("com.halfeatentoast.devcapture", SignerApp::AutoCapture),
        ];

        for &(known_id, app) in KNOWN_APPS {
            if id.contains(known_id) {
                return app;
            }
        }

        SignerApp::Default
    }

    pub fn from_bundle_identifier_or_name(
        identifier: Option<impl AsRef<str>>,
        name: Option<impl AsRef<str>>,
    ) -> Self {
        let app = Self::from_bundle_identifier(identifier);
        if app != SignerApp::Default {
            return app;
        }

        let name = match name {
            Some(name) => name.as_ref().to_owned(),
            None => return SignerApp::Default,
        };

        let normalized = name
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>();

        const KNOWN_APP_NAMES: &[(&str, SignerApp)] = &[
            ("livecontainer", SignerApp::LiveContainer),
            ("sidestore", SignerApp::SideStore),
            ("altstore", SignerApp::AltStore),
            ("feather", SignerApp::Feather),
            ("antrag", SignerApp::Antrag),
            ("protokolle", SignerApp::Protokolle),
            ("stikdebug", SignerApp::StikDebug),
            ("sparsebox", SignerApp::SparseBox),
            ("enswilde", SignerApp::EnsWilde),
            ("byetunes", SignerApp::ByeTunes),
            ("stikstore", SignerApp::StikStore),
            ("reynard", SignerApp::Reynard),
            ("ksign", SignerApp::Ksign),
            ("dev auto capture", SignerApp::AutoCapture),
        ];

        for &(needle, app) in KNOWN_APP_NAMES {
            if normalized.contains(needle) {
                return app;
            }
        }

        SignerApp::Default
    }

    pub fn supports_pairing_file(&self) -> bool {
        use SignerApp::*;
        !matches!(self, Default | LiveContainer | AltStore)
    }

    pub fn supports_pairing_file_alt(&self) -> bool {
        use SignerApp::*;
        !matches!(self, Default | AltStore)
    }

    pub fn pairing_file_path(&self) -> Option<&'static str> {
        use SignerApp::*;
        match self {
            Antrag | Feather | Protokolle | StikDebug | SparseBox | EnsWilde | StikStore
            | Reynard | Ksign => Some("/Documents/pairingFile.plist"),
            SideStore => Some("/Documents/ALTPairingFile.mobiledevicepairing"),
            LiveContainerAndSideStore | LiveContainer => {
                Some("/Documents/SideStore/Documents/ALTPairingFile.mobiledevicepairing")
            }
            ByeTunes => Some("/Documents/pairing file/pairingFile.plist"),
            AutoCapture => Some("/Documents/rpPairingFile.plist"),
            _ => None,
        }
    }

    /// Additional locations the app reads a pairing file from, besides `pairing_file_path`.
    /// Newer SideStore builds look for separate remote (RPPairing) and lockdown files.
    pub fn extra_pairing_file_paths(&self) -> &'static [&'static str] {
        use SignerApp::*;
        match self {
            SideStore => &[
                "/Documents/PairingFile_RemoteRP.plist",
                "/Documents/PairingFile_Lockdown.plist",
            ],
            LiveContainerAndSideStore | LiveContainer => &[
                "/Documents/SideStore/Documents/PairingFile_RemoteRP.plist",
                "/Documents/SideStore/Documents/PairingFile_Lockdown.plist",
            ],
            _ => &[],
        }
    }
}

/// Loader defaults preserve the existing automatic ElleKit behavior.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TweakLoader {
    #[default]
    ElleKit,
    None,
}
impl std::fmt::Display for TweakLoader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::ElleKit => "ElleKit",
            Self::None => "None",
        })
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum TweakLoadPath {
    #[default]
    Rpath,
    ExecutablePath,
}
impl std::fmt::Display for TweakLoadPath {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Rpath => "@rpath",
            Self::ExecutablePath => "@executable_path",
        })
    }
}
#[derive(Clone, Debug, Default)]
pub struct TweakInjection {
    pub loader: TweakLoader,
    /// None keeps legacy Frameworks placement and @rpath/leaf load strings.
    pub folder: Option<String>,
    pub load_path: TweakLoadPath,
}
impl TweakInjection {
    pub fn folder_path(&self) -> Result<std::path::PathBuf, crate::Error> {
        let Some(folder) = &self.folder else {
            return Ok("Frameworks".into());
        };
        let folder = folder.replace('\\', "/");
        if folder == "/" || folder.is_empty() {
            return Ok(std::path::PathBuf::new());
        }
        if folder.starts_with('/')
            || folder
                .split('/')
                .any(|part| part == ".." || part.contains(':'))
        {
            return Err(crate::Error::Other(
                "Tweak folder must stay inside the app bundle".into(),
            ));
        }
        Ok(std::path::PathBuf::from(folder))
    }
    pub fn library_path(&self, relative: &std::path::Path) -> Result<String, crate::Error> {
        let relative = if self.folder.is_none() && self.load_path == TweakLoadPath::Rpath {
            relative.strip_prefix("Frameworks").unwrap_or(relative)
        } else {
            relative
        };
        let value = relative
            .to_str()
            .ok_or(crate::Error::TweakInvalidPath)?
            .replace('\\', "/");
        Ok(format!("{}/{value}", self.load_path))
    }
}
