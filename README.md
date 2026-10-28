# Jimpactor

Jimpactor is an independently maintained iOS sideloading app for Windows, macOS, and Linux.

**This project is not affiliated with, endorsed by, or maintained by the original PlumeImpactor / Impactor project or its authors.** It is a separate fork of [PlumeImpactor, now Impactor](https://github.com/claration/Impactor). Upstream code and contributors are credited in [Attributions.md](Attributions.md).

Report Jimpactor issues and request features in [this repository](https://github.com/jpdagostin0/jimpactor/issues).

## Features

- Sign and sideload iOS apps using your Apple Account, with GUI and CLI interfaces.
- Refresh installed apps from the system tray.
- Choose local or remote anisette providers, including custom servers and Sideloadly connections.
- Browse app sources and install release IPAs.
- Export signing certificates and generate or install device pairing files.
- Manage accounts, developer teams, registered devices, app identifiers, and logs.
- Customize apps and inject supported tweaks and frameworks.

Jimpactor can supply pairing files to apps such as StikDebug. It does not include a JIT server or enable JIT itself.

## Downloads

Use artifacts published under [Jimpactor releases](https://github.com/jpdagostin0/jimpactor/releases), when available, or build from source. The original project's releases, Homebrew cask, and Flathub listing distribute the original project.

On Windows, install Apple's device drivers through [iTunes](https://support.apple.com/en-us/106372). On Linux, install and run `usbmuxd` before connecting your device.

## Build

Install Rust, CMake, and the native dependencies listed in the [contributing guide](.github/CONTRIBUTING.md).

```sh
# GUI
cargo run --locked --bin jimpactor

# CLI
cargo run --locked --bin jimpactor-cli -- --help

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
