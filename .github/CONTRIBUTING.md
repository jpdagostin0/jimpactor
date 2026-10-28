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
sudo apt-get install build-essential cmake nasm
```

Native dependencies such as liblzma, libbz2, libzstd and libdbus are built from
Cargo-managed sources and statically linked. GTK and AppIndicator are not used.
The `tray` crate supplies the icon and Iced renders its popup menu, following
[upstream's Iced example](https://github.com/nobane/tray-rs/blob/main/crates/tray/examples/iced-popup.rs).
Its Linux tray backend uses X11; an X11 tray host is needed (including on Wayland
desktops using XWayland). The display/graphics drivers, Secret Service, and
`usbmuxd` remain runtime services, rather than build dependencies.

### macOS

Install Xcode or its Command Line Tools.

### Windows

Install Visual Studio Build Tools with the C++ workload and the Windows SDK. Install Apple's device drivers through iTunes for device communication.

## Packaging

```sh
cargo xtask dist
cargo xtask dist --target <triple>
```

Artifacts are written to `dist/`. Linux packaging downloads linuxdeploy to create an AppImage. Windows packaging requires NSIS (`makensis`) on `PATH`.

The [release workflow](workflows/release.yml) builds distributions with `cargo xtask dist` on pushes to `main`, `v*` tags, and manual dispatches. Successful builds upload distributions as workflow artifacts. Tag pushes create a draft GitHub release from the available artifacts even if another platform or architecture fails. If no distributions succeed, release creation is skipped. Build failures remain visible in the workflow results, and cancelling the workflow prevents release creation.

| Distribution | Build environment |
| --- | --- |
| Linux x86_64 and aarch64 AppImages and binaries | Native architecture, Ubuntu 24.04 runner |
| Windows x86_64, i686 and ARM64 installers and binaries | LLVM/cargo-xwin cross-compilation to MSVC targets and NSIS, Ubuntu 24.04 runner |
| macOS universal app archive, DMG, and binaries | Zig cross-compilation, Ubuntu 24.04 runner |

Builds run directly on GitHub's Ubuntu runners. Build tools and packaging dependencies are installed with `--no-install-recommends`. AppImage tools run with `APPIMAGE_EXTRACT_AND_RUN=1` so packaging does not require FUSE. The macOS builder installs OpenSSL development files for the host-side `cargo-bundle` tool.

Every distribution builds on a Linux runner. Cargo downloads are shared across jobs. Source cache warming is optional: if it fails, the builds fetch their own dependencies. Compiled-output and host-tool caches identify the Ubuntu environment. Each Windows builder caches its Cargo tool and downloads only its target architecture's Windows SDK/CRT libraries, with a separate SDK cache. Missing distribution artifacts fail that build without blocking release of other successful artifacts.

### Windows cross-compilation and packaging

On Debian/Ubuntu, install LLVM, CMake, Ninja and NSIS, then install the Rust target and [cargo-xwin](https://github.com/rust-cross/cargo-xwin):

```sh
sudo apt-get install build-essential clang llvm lld cmake ninja-build nasm pkg-config nsis
rustup target add x86_64-pc-windows-msvc i686-pc-windows-msvc aarch64-pc-windows-msvc
cargo install --locked cargo-xwin@0.23.1
XWIN_ARCH=x86_64 cargo xtask dist --target x86_64-pc-windows-msvc
XWIN_ARCH=x86 cargo xtask dist --target i686-pc-windows-msvc
XWIN_ARCH=aarch64 cargo xtask dist --target aarch64-pc-windows-msvc
```

When targeting Windows MSVC from a non-Windows host, xtask invokes `cargo xwin build`. Cargo-xwin supplies the Windows SDK/CRT and configures clang-cl, lld-link and native dependency builds; winresource uses llvm-rc for the executable's resources. NSIS creates the installer on Linux. A Windows host, Visual Studio installation and Wine are not needed for building or packaging. Running the resulting Windows application is a separate validation step.

### macOS cross-compilation and packaging

Use the same Cargo commands on Linux, Windows, or macOS. Install [Zig 0.17.0](https://ziglang.org/download/), the two Rust targets, and the Cargo tools:

```sh
rustup target add aarch64-apple-darwin x86_64-apple-darwin
cargo install --locked cargo-zigbuild@0.24.0 cargo-bundle@0.12.0

# Both architectures, Jimpactor.app and a compressed DMG
cargo xtask dist --target universal2-apple-darwin

# Architecture-specific binaries, app bundles and DMGs
cargo xtask dist --target aarch64-apple-darwin
cargo xtask dist --target x86_64-apple-darwin

# Compile only
cargo xtask build --target aarch64-apple-darwin
```

Cargo-zigbuild compiles the GUI/CLI and their native dependencies. Cargo-bundle creates the app and its icons from `package/icon.svg`; Rust libraries create and verify the HFS+ DMG, including the Applications link and executable permissions. No macOS disk utilities or signing credentials are used. Builds use a consistent minimum deployment target of macOS 11.0 and embed debug information until stripping, avoiding a dependency on Apple's `dsymutil`.

The xtask uses `SDKROOT` when supplied, or the installed Xcode SDK on a Mac. Otherwise it downloads the pinned macOS 26.1 SDK, checks its SHA-256, and extracts it into `target/macos-sdk`. `cargo xtask setup-macos` prepares just the SDK. On Windows, enable Developer Mode so SDK symlinks can be created. SDK man pages with Windows-incompatible filenames are omitted.

The universal output is `dist/Jimpactor.app`, `dist/jimpactor-macos-universal.dmg`, the GUI/CLI binaries, and an `.app.tar.gz` archive that preserves Unix permissions when downloaded through CI artifacts. Per-architecture app bundles live under `dist/macos-aarch64/` or `dist/macos-x86_64/`, with correspondingly named DMGs and binaries. Packaging checks the Mach-O architectures, app metadata and every file inside the DMG before succeeding.

See [release.yml](workflows/release.yml) for the Linux release builders. Default-branch builds populate shared Cargo download caches; compiled outputs remain specific to the host/compiler/target. Nix is not required.

## Changes

Use a separate branch, keep changes focused, and explain how they were checked. Commit messages must contain a single subject line, with no body or trailers. Do not commit build outputs, credentials, pairing records, or local application state.
