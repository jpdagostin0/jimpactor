# Feature triage — 2026-10-28

Scope: 11 feature requests. Excluded by request: updater #25, `[Todo]` #2, and JIT #1. Implementation stays on `main`; every commit bumps the workspace version and has a subject only.

| Issue | Assessment | Status |
| --- | --- | --- |
| [#16](https://github.com/jpdagostin0/jimpactor/issues/16) | Remote IP + RemotePairing needs a persistent RSD/tunnel provider, saved endpoint settings, reconnect behavior, and refresh integration. Local Wi-Fi lockdown discovery does not satisfy this. | Needs integration design and a reachable remote device. |
| [#17](https://github.com/jpdagostin0/jimpactor/issues/17) | Apple TV requires PIN pairing and an RSD tunnel, real device identity, and tvOS registration/provisioning throughout both signing paths. Current higher-level operations still select IOS. Upstream PR 226 is useful prior art, but is not a drop-in patch for this fork. | Needs platform integration and Apple TV validation. |
| [#18](https://github.com/jpdagostin0/jimpactor/issues/18) | Current disconnect handling can choose the first other device. Added a lock using the stable UDID that disables installation while the target is unavailable, including cached Wi-Fi devices. | Implemented in 0.1.3. |
| [#19](https://github.com/jpdagostin0/jimpactor/issues/19) | Relaunch now queries the running version and executable, offers restart/use-running/cancel, and waits for the single-instance handoff. Active installation and refresh prevent restart. | Implemented in 0.1.4. Older show-only builds cannot report their version or restart themselves; the dialog explains manual exit for that first upgrade. |
| [#20](https://github.com/jpdagostin0/jimpactor/issues/20) | `Device::pairing_file(true, ...)` already merges lockdown and RemotePairing dictionaries; Settings → Pairing exposes the remote-pairing option and export. | Existing implementation; dual-consumer format regression passes. Live iOS 17.4+ validation remains manual. |
| [#21](https://github.com/jpdagostin0/jimpactor/issues/21) | Existing No Modify is intentionally unchanged. Added a separate Modify without signing mode, export-only in the GUI, with CLI --modify-only. | Implemented in 0.1.2; metadata changes preserve executable bytes unless a requested tweak/SDK edit changes them. |
| [#22](https://github.com/jpdagostin0/jimpactor/issues/22) | Add validated replacement URL schemes or removal in the GUI and CLI; preserve existing schemes by default. | Implemented in 0.1.2. |
| [#23](https://github.com/jpdagostin0/jimpactor/issues/23) | Device misagent can enumerate installed profiles. Match exact app IDs before wildcard profiles and show unknown when a reliable expiration is unavailable. | Implemented in 0.1.3; ambiguous or unavailable profile data is shown as unavailable. |
| [#24](https://github.com/jpdagostin0/jimpactor/issues/24) | Implement enable/disable rather than promising deletion. Apple restricts this to Account Holders/Admins, does not return annual device slots, and invalidates affected profiles when disabling. | Implemented in 0.1.3 with explicit restrictions and server error handling. |
| [#26](https://github.com/jpdagostin0/jimpactor/issues/26) | Default ElleKit behavior remains; None omits the loader and substrate patching. Configure app-relative folder and @rpath/@executable_path; explicit rpath placement adds the required search path. | Implemented in 0.1.2, with bounded load-command insertion and traversal rejection. |
| [#29](https://github.com/jpdagostin0/jimpactor/issues/29) | The login state machine already finishes without prompting for 2FA when Apple's success response has no secondary-auth requirement. The burner-account failure needs its actual returned auth state; do not invent a protocol or treat it as a bad verification code. | Needs a scrubbed response/status from an affected account. |

Apple device-management constraints: [Disable or enable a device](https://developer.apple.com/help/account/devices/disable-or-enable-a-device/), [Devices overview](https://developer.apple.com/help/account/devices/devices-overview/).

Validation for 0.1.2 includes modification-only executable preservation, unchanged No Modify behavior, URL replacement/removal, custom dylib/framework placement, loader omission, invalid paths, insufficient Mach-O padding, and CLI argument conflicts. No live account mutations, app installation, or pairing are performed by these tests.

Validation for 0.1.3 covers stable-UDID locking across disconnect/reconnect, unavailable targets, profile identity/expiration matching, ambiguous profiles, both hybrid pairing parsers, and mocked Apple device status requests including permission errors. No live device status was changed.

Validation for 0.1.4 covers loopback version queries, distinct show/restart commands, legacy show-only responses, version choices, and single-instance ownership handoff. Native restart dialogs were compiled on Windows; no running user application was restarted during validation.
