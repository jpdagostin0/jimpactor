# Attributions

Jimpactor is an independently maintained fork of **Plume Impactor**. It is not affiliated with, endorsed by, or maintained by the original project or its authors. It relies on several third-party projects, some vendored into this repository with changes.

## Original project

- **Plume Impactor** (later renamed **Impactor**): https://github.com/claration/Impactor (originally https://github.com/khcrysalis/PlumeImpactor)
  - Author: [CLARATION](https://github.com/claration) (khcrysalis), with contributions from the Impactor community
  - License: MIT, Copyright 2025 Samara M. The original license notice is reproduced below.
  - The crates in `crates/core`, `crates/gestalt`, `crates/store` and `crates/utils`, and the applications in `src/impactor` and `src/cli`, come from this project. `crates/core` is licensed under MPL-2.0.
  - Icon and Flatpak distribution by [Paige](https://github.com/ssalggnikool).

## Upstream authentication contributions

The authentication and Omnisette changes incorporate or adapt the following upstream work:

- [Rogue911](https://github.com/Rogue911): [SideStore/apple-private-apis #27](https://github.com/SideStore/apple-private-apis/pull/27), identifying the AuthKit client as `com.apple.akd/1.0` to avoid GrandSlam HTTP 503 errors.
- [Nicholas Sharp (nab138)](https://github.com/nab138): [SideStore/apple-private-apis #25](https://github.com/SideStore/apple-private-apis/pull/25), app-token decryption, authenticated plist requests, cancellable sign-in callbacks, and dependency updates.
- [Maxim Grischenko (max402)](https://github.com/max402): [Impactor #297](https://github.com/claration/impactor/pull/297), SMS challenge handling and preservation of the anisette provider's client identity and header values.
- [BenwillLopez](https://github.com/BenwillLopez): [Impactor #295](https://github.com/claration/impactor/pull/295), configurable anisette v3 servers and the `PLUMESIGN_ANISETTE_URL` CLI override.
- [2foil](https://github.com/2foil): [Impactor #304](https://github.com/claration/impactor/pull/304), retaining remote Mac client identity, refreshing provisioning state, and offline regression coverage for sign-in after two-factor authentication.
- [icebergOcean](https://github.com/icebergOcean): [SideStore #1448](https://github.com/SideStore/SideStore/issues/1448), documenting `s2k_fo` password derivation using the lowercase hexadecimal ASCII encoding of the SHA-256 password digest as the PBKDF2 input.

## Vendored third-party crates

Local Mac anisette uses **adi-core** by [sacca97](https://github.com/sacca97/os-findmy-e), pinned to the MIT-licensed commit [`374d83854f657a7fdca45f813387d99d491db0e2`](https://github.com/sacca97/os-findmy-e/tree/374d83854f657a7fdca45f813387d99d491db0e2/native/adi-core). Its MIT notice is preserved in [`licenses/adi-core-MIT.txt`](licenses/adi-core-MIT.txt). The emulator runs inside Jimpactor; there is no helper executable. Its dependency **Unicorn** 2.1.5 still declares GPL-2.0, preserved in [`licenses/Unicorn-GPL-2.0.txt`](licenses/Unicorn-GPL-2.0.txt). The MIT pin does not change Unicorn's license. Apple's Mac CoreADI is downloaded directly from Apple at runtime and is not redistributed.

The local Mac identity and provisioning approach follows [sacca97/os-findmy-e's Mac anisette implementation](https://github.com/sacca97/os-findmy-e/blob/374d83854f657a7fdca45f813387d99d491db0e2/tools/machoadi/mac_anisette.py). The pinned iTunes payload offsets and hashes were documented by [lazzurs](https://github.com/lazzurs) in [OpenTagViewer #274](https://github.com/parawanderer/OpenTagViewer/pull/274). [Kabir Oberai's XTool #337](https://github.com/xtool-org/xtool/pull/337) identified the Android-ADI/Mac-client mismatch causing `-80045`.

| Path                    | Upstream                                                                                                     | License    | Notes                                                                                                                                                                                                                      |
| ----------------------- | ------------------------------------------------------------------------------------------------------------ | ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `crates/apple-codesign` | [apple-platform-rs](https://github.com/indygreg/apple-platform-rs) by Gregory Szorc (`apple-codesign` 0.29.0) | MPL-2.0    | Changed in place: it sets 0o755 permissions on signed Mach-O files, skips the required bundle-identifier check, exports everything from `macho_universal`, and adds `MachFile::iter_macho_mut` and `nth_macho_mut`. |
| `crates/omnisette`      | [SideStore/apple-private-apis](https://github.com/SideStore/apple-private-apis) (originally by Dadoum)        | MPL-2.0    | Generates anisette data. Changed to support remote anisette v1 and v3 providers.                                                                                                                                           |
| `crates/xkit` | [XTool / XKit](https://github.com/xtool-org/xtool) by Kabir Oberai | MIT (XTool project) | Rust port of XKit's authentication, developer services, provisioning, signing, and installation APIs. XKit: Copyright © 2019 Kabir Oberai. All rights reserved. The original Swift sources are not included. |

Each vendored crate keeps its upstream `LICENSE` file in its own directory.

**XKit: Copyright © 2019 Kabir Oberai. All rights reserved.**
Individual source files also carry copyright notices dated 2020 and 2021.
XKit is part of [XTool](https://github.com/xtool-org/xtool), which is distributed
under the MIT license. XTool's MIT license notice
(Copyright (c) 2024 Kabir Oberai) is preserved in
[`crates/xkit/LICENSE`](crates/xkit/LICENSE) and reproduced below.

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
- [tray](https://github.com/nobane/tray-rs) (MIT): native system tray icon; its Iced popup example provides the menu integration pattern.
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

## XTool MIT license notice

The following is the MIT license notice for
[XTool](https://github.com/xtool-org/xtool), the project containing XKit, authored
by Kabir Oberai:

```text
MIT License

Copyright (c) 2024 Kabir Oberai

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```
