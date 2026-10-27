# Attributions

Jimpactor is an independently maintained fork of **Plume Impactor**. It is not affiliated with, endorsed by, or maintained by the original project or its authors. It relies on several third-party projects, some vendored into this repository with changes.

## Original project

- **Plume Impactor** (later renamed **Impactor**): https://github.com/claration/Impactor (originally https://github.com/khcrysalis/PlumeImpactor)
  - Author: [CLARATION](https://github.com/claration) (khcrysalis), with contributions from the Impactor community
  - License: MIT, Copyright 2025 Samara M. The original license notice is reproduced below.
  - The crates in `crates/core`, `crates/gestalt`, `crates/store` and `crates/utils`, and the applications in `src/impactor` and `src/cli`, come from this project. `crates/core` is licensed under MPL-2.0.
  - Icon and Flatpak distribution by [Paige](https://github.com/ssalggnikool).

## Vendored third-party crates

| Path                    | Upstream                                                                                                     | License    | Notes                                                                                                                                                                                                                      |
| ----------------------- | ------------------------------------------------------------------------------------------------------------ | ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/apple-codesign` | [apple-platform-rs](https://github.com/indygreg/apple-platform-rs) by Gregory Szorc (`apple-codesign` 0.29.0) | MPL-2.0    | Changed in place: it sets 0o755 permissions on signed Mach-O files, skips the required bundle-identifier check, exports everything from `macho_universal`, and adds `MachFile::iter_macho_mut` and `nth_macho_mut`. |
| `crates/omnisette`      | [SideStore/apple-private-apis](https://github.com/SideStore/apple-private-apis) (originally by Dadoum)        | MPL-2.0    | Generates anisette data. Changed to support remote anisette v1 and v3 providers.                                                                                                                                           |
| `crates/decompress`     | [rusty-ferris-club/decompress](https://github.com/rusty-ferris-club/decompress)                              | Apache-2.0 | Copied and changed for archive extraction.                                                                                                                                                                                 |

Each vendored crate keeps its upstream `LICENSE` file in its own directory.

## References and inspiration

These are credited in the original Plume Impactor project:

- [SideStore](https://github.com/SideStore/apple-private-apis): Grandslam auth and Omnisette.
- [gms.py](https://gist.github.com/JJTech0130/049716196f5f1751b8944d93e73d3452): Grandslam auth API references.
- [isideload](https://github.com/nab138/isideload): code for getting the Xcode token correctly.
- [Sideloader](https://github.com/Dadoum/Sideloader): Apple Developer API references.
- [PyDunk](https://github.com/nythepegasus/PyDunk): `v1` Apple Developer API references.

## Notable dependencies

- [idevice](https://github.com/jkcoxson/idevice): talks to `installd` and other device services.
- [iced](https://github.com/iced-rs/iced): GUI framework.
- [android-loader](https://github.com/Dadoum/android-loader): loads Apple's Android libraries for local anisette.

## Original project's MIT license notice

The following notice applies to code inherited from Plume Impactor / Impactor:

```text
Copyright 2025 Samara M

Permission is hereby granted, free of charge, to any person obtaining a
copy of this software and associated documentation files (the
"Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish,
distribute, sublicense, and/or sell copies of the Software, and to
permit persons to whom the Software is furnished to do so, subject to
the following conditions:

The above copyright notice and this permission notice shall be included
in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS
OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY
CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT,
TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE
SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
```
