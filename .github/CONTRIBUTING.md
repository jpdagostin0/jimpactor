# Contributing to Jimpactor

Jimpactor is an independent fork of PlumeImpactor / Impactor. It is not affiliated with or supported by the original project's maintainers. Open issues and pull requests in [jpdagostin0/jimpactor](https://github.com/jpdagostin0/jimpactor).

## Workspace

| Path | Purpose |
| --- | --- |
| `src/impactor` | Iced GUI |
| `src/cli` | Command-line signing and device tools |
| `crates/core` | Apple authentication, developer services, and signing support |
| `crates/gestalt` | Apple Silicon device identification |
| `crates/store` | Accounts, credentials, anisette settings, and refresh state |
| `crates/utils` | Signing, app modification, and device helpers |
| `crates/decompress` | Vendored archive extraction |
| `crates/omnisette` | Vendored local and remote anisette support |
| `crates/apple-codesign` | Vendored signing library with local changes |
| `tools/genicons` | Platform icons generated from `package/icon.svg` |
| `tools/xtask` | Release builds and packaging |

See [Attributions.md](../Attributions.md) for upstream sources and licenses. Preserve applicable notices when changing vendored code.

## Building

Install [Rust](https://rustup.rs/), [CMake](https://cmake.org/download/), and a C/C++ compiler.

```sh
cargo run --locked --bin jimpactor
cargo run --locked --bin jimpactor-cli -- --help
cargo check --workspace --all-targets --locked
cargo test --workspace --locked
```

### Linux

Install `usbmuxd` for device communication. On Ubuntu/Debian, install build dependencies with:

```sh
sudo apt-get install libclang-dev pkg-config libglib2.0-dev libsecret-1-dev libgtk-3-dev libpng-dev libjpeg-dev libgl1-mesa-dev libglu1-mesa-dev libxkbcommon-dev libexpat1-dev libtiff-dev libxdo-dev libappindicator3-dev
```

### macOS

Install Xcode or its Command Line Tools.

### Windows

Install Visual Studio Build Tools with the C++ workload and the Windows SDK. Install Apple's device drivers through iTunes for device communication.

## Packaging

```sh
cargo xtask dist
cargo xtask dist --target <triple>
```

Artifacts are written to `dist/`. Linux packaging downloads linuxdeploy to create an AppImage. Windows packaging requires NSIS (`makensis`) on `PATH`. macOS packaging creates a universal `Jimpactor.app` and requires both `aarch64-apple-darwin` and `x86_64-apple-darwin` Rust targets. See [release.yml](workflows/release.yml) for the platform build setup.

## Changes

Use a separate branch, keep changes focused, and explain how they were checked. Commit messages must contain a single subject line, with no body or trailers. Do not commit build outputs, credentials, pairing records, or local application state.
