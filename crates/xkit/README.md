# XKit for Rust

`xkit` is a workspace library for Apple developer authentication, provisioning,
code signing, and installation. It uses native Rust libraries for Windows, macOS,
and Linux. This is a Rust port of XKit from
[xtool-org/xtool](https://github.com/xtool-org/xtool), originally authored by
Kabir Oberai. The application and
CLI can adopt this crate separately.

| Swift area | Rust API |
| --- | --- |
| GrandSlam / Anisette | `grand_slam::DeveloperServicesLoginManager`, `Account`, `AnisetteDataProvider`; shared `plume_core` SRP, SMS/trusted-device 2FA, app-token crypto, and `omnisette` providers |
| HTTPClientProtocol | `http::HTTPClientProtocol`, `HTTPClient`, injectable requests/responses; anisette websocket provisioning stays in `omnisette` |
| DeveloperServices / OpenAPI | `DeveloperServicesClient` for legacy plists; `DeveloperAPIClient` for JSON:API, Xcode authentication or ASC ES256 JWTs, pagination and structured errors |
| Provisioning operations | `register_device`, `fetch_certificate`, `upsert_app`, `assign_app_groups`, `fetch_profile`, `DeveloperServicesProvisioningOperation` |
| Model / Entitlements | `Certificate`, `Keypair`, `PrivateKey`, `Mobileprovision`, typed `Entitlement` and lossless `Entitlements` |
| Signer | `SigningContext`, `Signer`, `AutoSigner`; native `apple-codesign`, including separate extension entitlements |
| Installation | `Connection`, `ClientDevice`, `IPAUploader`, `IPAInstaller`, `AppInstaller`, `ProfileManager`, `HeartbeatHandler`, `Debugger` |
| Integration | `IntegratedInstaller`, `IntegratedInstallerDelegate`; unpack → prepare device → provision → sign → package → install |
| Utilities | `ProvisioningIdentifiers`, memory/directory storage, Apple `KeychainStorage`, `KeyValueSigningInfoManager`, `ZIPCompressor` |

Rust futures, ownership, and `tokio::time::sleep` replace the Swift task and C
lifetime helpers. Custom connection lookup uses an `IdeviceProvider` passed to
`Connection::with_provider`. Discovery returns an async stream of changed device
snapshots. Dropping a discovery stream stops it; dropping a heartbeat stops its
task. AFC uploads stream in 1 MiB chunks and clean up staged files on completion,
failure, or cancellation. Profile enumeration supports the pre-iOS 9.3 protocol.
The upstream DDI mounter was disabled and has no active API to port.

## Sign an app

```rust,no_run
use std::{path::Path, sync::Arc};
use xkit::{ASCKey, AutoSigner, DeveloperAPIAuthData, DeveloperAPIClient, SigningContext};
use xkit::utilities::{DirectoryStorage, KeyValueSigningInfoManager, persistent_directory};

async fn sign(app: &Path, key: ASCKey) -> xkit::Result<String> {
    let client = DeveloperAPIClient::new(DeveloperAPIAuthData::AppStoreConnect(key), None)?;
    let manager = Arc::new(KeyValueSigningInfoManager {
        storage: Arc::new(DirectoryStorage::new(persistent_directory()?)),
    });
    let signer = AutoSigner { context: SigningContext::new(client, manager) };
    signer.sign(
        app,
        &|_certificates| Box::pin(async { false }),
        &|stage| println!("{stage}"),
        &|_progress| {},
        &|| Ok(()),
    ).await
}
```

For Xcode login, `DeveloperServicesLoginManager::log_in` accepts an anisette
configuration and a 2FA callback, returning a login token and anisette provider.
Use them with `DeveloperAPIAuthData::Xcode(XcodeAuthData { login_token, team_id })`
and `DeveloperAPIClient::new(auth, Some(provider))`. Existing logged-in
`plume_core::auth::Account` values can be adapted with `from_account`.

`SigningContext::target_device` optionally registers a device before provisioning.
`IntegratedInstaller` supplies it automatically when installing an IPA or app.
`AutoSigner` signs in place; `IntegratedInstaller::install` works in a temporary
staging directory. The integration delegate controls certificate revocation and
receives pairing/unlock messages and stage/progress updates. The default delegate
revocation policy matches Swift (accept); override it to present a confirmation.

Directory storage persists private keys as files; callers can instead select
Apple keychain storage or another `KeyValueStorage`. The default
`KeyValueSigningInfoManager` stores data in memory. Certificate serials compare
numerically, so inconsistent leading zeros from Apple's API do not prevent reuse.
Unknown entitlement keys survive normal updates; free-team filtering removes
unknown and paid entitlements, matching the source policy. CSRs use RSA-2048 with
SHA-256, matching the repository's existing Rust certificate implementation.

## Verification

```text
cargo check -p xkit
cargo test -p xkit
cargo clippy -p xkit --all-targets --no-deps -- -D warnings
cargo fmt -p xkit --check
```

Tests run offline using injected HTTP responses, generated test certificates,
and a freestanding ARM64 Mach-O fixture. They cover request transformations,
JWT signatures/renewal, pagination, entitlement rewrites, certificate reuse and
revocation cancellation, profile creation, ZIP round trips, separate extension
entitlements, and a complete provisioning/signing flow. The fixture's source and
LLVM regeneration commands are in `tests/fixtures/minimal.c`.

Live Apple authentication, USB/network devices, the native Apple keychain, and
installation require validation with the corresponding account/hardware/platform.
No live accounts or devices are accessed by the test suite.

## Attribution and license

**XKit: Copyright © 2019 Kabir Oberai. All rights reserved.**
Individual source files also carry copyright notices dated 2020 and 2021.

XKit is part of [XTool](https://github.com/xtool-org/xtool), which is distributed
under the **MIT license**. XTool's MIT license notice is
Copyright (c) 2024 Kabir Oberai.

The original Swift files have been removed.
XTool's MIT notice is preserved in [LICENSE](LICENSE) and in the repository's
[Attributions.md](../../Attributions.md).
