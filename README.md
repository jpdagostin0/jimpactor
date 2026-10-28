# Jimpactor

Jimpactor is an independently maintained iOS sideloading app for Windows, macOS, and Linux.

**This project is not affiliated with, endorsed by, or maintained by the original PlumeImpactor / Impactor project or its authors.** It is a separate fork of [PlumeImpactor, now Impactor](https://github.com/claration/Impactor). Upstream code and contributors are credited in [Attributions.md](Attributions.md).

Report Jimpactor issues and request features in [this repository](https://github.com/jpdagostin0/jimpactor/issues).

## Features

- Sign and sideload iOS apps using your Apple Account, with GUI and CLI interfaces.
- Refresh installed apps from the system tray.
- Discover paired iPhones and iPads over Wi-Fi and install or refresh apps wirelessly.
- Choose local or remote anisette providers, including custom servers and Sideloadly connections.
- Browse app sources and install release IPAs.
- Export signing certificates and generate or install device pairing files.
- Manage accounts, developer teams, registered devices, app identifiers, and logs.
- Customize apps and inject supported tweaks and frameworks.

Jimpactor can supply pairing files to apps such as StikDebug. It does not include a JIT server or enable JIT itself.

## Downloads

Use artifacts published under [Jimpactor releases](https://github.com/jpdagostin0/jimpactor/releases), when available, or build from source. The original project's releases, Homebrew cask, and Flathub listing distribute the original project.

## Connect your device

On Windows, install Apple's device drivers through [iTunes](https://support.apple.com/en-us/106372). On Linux, install and run `usbmuxd` before connecting your device.

Pair your iPhone or iPad with this computer over USB first and accept **Trust This Computer** on the device. For wireless connections, keep the computer and device on the same Wi-Fi network. Jimpactor automatically discovers paired devices; select the entry labeled `[WiFi]` in the device dropdown to install or refresh apps and manage pairing files wirelessly.

Wi-Fi devices stay in the list for five minutes after disconnecting. Jimpactor reconnects automatically when a device returns, preserving your selection. Before starting a device operation, it waits briefly for a cached device to reconnect. If the device is unavailable, wake and unlock it, check its Wi-Fi connection, and retry.

## Signing and troubleshooting

For Apple IDs with multiple developer teams, choose a team in **Settings → Account** before signing or exporting a certificate. In CLI scripts, pass the team explicitly:

```sh
jimpactor-cli sign --apple-id --team-id TEAM_ID --package App.ipa --output Signed.ipa
```

Use the device's full UDID with `--udid`; the number assigned to a USB connection changes when it reconnects. Reinstalling an app with another account updates its device's Auto Refresh account after installation succeeds.

Jimpactor checks provisioning profiles against the app and destination device before installation. If a profile or certificate has expired, does not match, or has been revoked, re-sign with a valid identity and a profile that includes your device. A CoreTrust-patched executable with data after its signature cannot be re-signed; use an original decrypted IPA.

Apple verification errors distinguish an incorrect code from a temporary lockout or service failure. Wait before retrying a locked challenge. For anisette connection errors, check the selected server in Settings and your system's proxy and certificate settings.

Startup errors are written to the `logs` folder in the application data directory below. If Windows exits with a native access violation, include the Event Viewer faulting module and exception code with your report.

## Export and tweak options

Choose **Modify without signing** to apply app customizations and export an IPA without replacing executable signatures. Modified apps generally need signing before installation. **No Modify** keeps its existing behavior and skips customizations.

URL schemes can be replaced with a comma-separated list or removed in the app options. For tweaks, choose **ElleKit** (the default) or **None** for libraries that provide their own hooking. The library folder is relative to the app: leave it blank for the existing `Frameworks` placement, or enter `/` for the app root. Load paths support `@rpath` and `@executable_path`.

```sh
jimpactor-cli sign --package App.ipa --output Modified.ipa --modify-only --url-scheme duplicate-app
jimpactor-cli sign --package App.ipa --output Signed.ipa --tweaks Custom.dylib --tweak-loader none --tweak-inject-folder / --tweak-inject-path @executable_path
```

## Build

Install Rust, CMake, and the native dependencies listed in the [contributing guide](.github/CONTRIBUTING.md).

```sh
# GUI
cargo run --locked --bin jimpactor

# CLI
cargo run --locked -p jimpactor-cli -- --help

# Release binaries and platform packages in dist/
cargo xtask dist

# Cross-compile and package macOS on Linux or Windows (requires Zig and Cargo tools)
cargo xtask dist --target universal2-apple-darwin
```

The workspace contains the GUI in `src/impactor`, the CLI in `src/cli`, shared libraries in `crates`, and build tools in `tools`. All platform icons are generated from `package/icon.svg`.

## Application data

Jimpactor uses its own application identifier, `io.github.jpdagostin0.jimpactor`, and stores configuration in `%APPDATA%/Jimpactor` on Windows or `${XDG_CONFIG_HOME:-$HOME/.config}/Jimpactor` on macOS and Linux. GUI and CLI share this directory. Account tokens use the `Jimpactor` service in the OS credential store when available.

Earlier development builds used the original project's `PlumeImpactor` directory and credential-store name. Sign in and configure Jimpactor again when moving from one of those builds; the original project's data is left in place.

## License and credits

Jimpactor is by JP D'Agostino and is licensed under the [MIT license](LICENSE). The original project's license notice is preserved in [Attributions.md](Attributions.md). Some components use other licenses, including MPL-2.0 and Apache-2.0; see their individual license files.
