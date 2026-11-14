# Tauri Settings retrofit and release checklist

Updated 2026-11-12 against HEAD `47f20f7` plus the uncommitted migration working
tree. The Tauri Settings app is now the default launcher; GTK remains installed
as `focaldesk-settings-gtk` for rollback. The user confirmed the migrated Settings
works in their active FocalDesk session before requesting the default switch.
Cross-distribution packaging and compatibility still need their normal release
review.

## Completed feature work

- [x] **P1 — Appearance.** Typed draft/apply/discard controls for themes, font/glow,
  shader chrome, work-area glass, rail backdrop, nullable panel darkness and shelf
  visibility; stale edits and compositor rejection preserve saved state.
- [x] **P2 — Displays implementation.** Arrangement, modes, fractional scaling,
  rotation, primary output, enable/disable and Desktop/Control Panel roles;
  layout/focus preferences; ICC chooser/clear and file validation/fingerprinting;
  HDR appearance presets/parameters and owned calibration patterns. Disruptive
  display edits use compositor-owned 20-second Keep/Revert trials. Experimental
  exclusive HDR has a separate confirmed logout workflow with stale-state checks
  and truthful reporting when automatic logout is unavailable. Both DRM backends
  implement trial/rollback overrides and connected-but-disabled inventory.
- [x] **P3 — Privacy.** Saved AI/location permission inventory and revocation;
  exact reviewed decisions are resolved again before removal. Store errors are
  reported individually. Recent activity clearing requires confirmation. Saved
  notification retention is sent to the live notification service, whose failure
  is reported independently of persistence; the page offers a live-update retry.
- [x] **P4 — Power.** Live batteries/source/profile snapshot plus confirmed suspend,
  hibernate, restart and shutdown. Unavailable/rejected services are errors; an
  accepted request is described as queued, not as a completed system action.
- [x] **P5 — Input.** Capability-aware enable/disable with the existing
  keyboard-capable-device guard; failed updates restore enabled state. Pointer
  and gesture defaults reset only the draft. Shortcuts offer local key capture
  and text entry, including a text fallback for compositor-reserved combinations.
- [x] **P6 — Sound.** Explicit one-second stereo speaker test on the selected
  output, Stop, four-second Rust deadline and parent-death cleanup. Blur, page
  exit, routing changes and window destruction stop playback. Output edits retain
  GTK's 0–150% range; input edits are 0–100%. Opening the page changes no volume.
- [x] **P7 — About and compatibility.** Visible alpha/preview status, fixed
  project/license/credits links and technology acknowledgments; build details
  remain in Debug. Legacy `--panel` names map to their Tauri pages. The default
  launcher routes to Tauri, while the installed GTK binary remains available for
  rollback.

The other pages already implement Accessibility, Theme Editor, Network,
Calendar, Bluetooth, Printers, Applications, Chrome, Workspaces and Debug. Their
Rust/service ownership and fixture coverage are described in [README.md](README.md).
Shared limitations remain: neither frontend provides per-app microphone/camera
portal switches, a full VPN editor, or a custom acceleration-curve editor.
Calendar event editing belongs to the Calendar application.

## Local validation evidence

Recorded passes are local checks, not a GitHub CI result. Temporary evidence may
be removed by the OS; archive it with the eventual release commit.

| Check | Recorded result and evidence | Scope |
| --- | --- | --- |
| Settings Rust library (`tauri-ui`) | 30 passed, 1 ignored; `/tmp/settings-finish-tests.log` | The ignored location-store round trip needs an isolated permission-store service |
| Settings core | 30 passed; `/tmp/settings-finish-tests.log` | Persistence/defaults/model round trips |
| Audio library | 7 passed; `/tmp/settings-finish-tests.log` | Device identity, fixed commands, output boost and input limits |
| Frontend | 8 test files passed and build/Svelte checks clean; `/tmp/settings-finish-js.log`, `/tmp/settings-finish-web.log` | Includes panel routing and shortcut capture helpers |
| Settings Clippy | Passed with warnings denied; `/tmp/settings-finish-clippy.log` | Tauri all targets |
| GTK compatibility | Passed; `/tmp/settings-finish-gtk.log` | Existing GTK executable still compiles |
| Engine DRM/Vulkan | 282 passed, 1 ignored; `/tmp/settings-finish-engine.log` | Includes input enablement/rollback guards; kernel-fence test ran outside sandbox |
| Engine Clippy | Passed; `/tmp/settings-finish-engine-clippy.log` | DRM/Vulkan all targets |
| Native WebKit | Full suite passed; `/tmp/settings-finish-native.log`, artifacts `/tmp/focaldesk-settings-ui-dgds_hri` | Private headless KWin/session bus and fixture services; includes permission stale/revoke, power rejection, speaker Stop/page-exit |
| Real nested display/theme recovery | Passed; `/tmp/settings-finish-display-runtime.log`, artifacts `/tmp/focaldesk-display-runtime-2kx5nyln` | Deadlines after client exit, stale state, ICC changes, confirmation/rollback and persistence isolation; no physical monitor changes |
| Release staging | Passed; `/tmp/settings-finish-stage.log`, `/tmp/focaldesk-settings-stage` | Both binaries and a preview launcher; does not install to the running system |

The final extended UI run passed local capture, draft-only pointer reset, retention
failure/retry and the speaker backend deadline. Evidence:
`/tmp/settings-finish-native-final.log`, `/tmp/focaldesk-settings-ui-frcuhb0f`.
Audio lifecycle checks also passed (`/tmp/settings-finish-audio-runtime.log`,
`/tmp/focaldesk-audio-runtime-w90xlahs`), including the real 20-second deadline,
parent-crash cleanup, stalled capture and command timeout. Rustfmt, markdown links
and whitespace checks passed.

The staged release executable also passed the entire extended WebKit suite:
`/tmp/settings-finish-release-ui.log`, `/tmp/focaldesk-settings-ui-chrznxtj`.
A focused staged-release check passed exclusive-HDR validation, active-trial and
stale-state rejection, acknowledged logout and rejected-logout/manual activation
without changing canonical settings: `/tmp/settings-finish-exclusive.log`,
`/tmp/focaldesk-settings-ui-ud13vqx7`. Its logout service is a private fixture;
physical HDR session recovery remains a manual gate.

## Reproduce validation

From the repository root:

```sh
npm ci --prefix apps/focaldesk-settings/frontend
npm run build --prefix apps/focaldesk-settings/frontend
npm test --prefix apps/focaldesk-settings/frontend
cargo clippy -p focaldesk-settings --no-default-features --features tauri-ui --all-targets -- -D warnings
cargo test -p focaldesk-settings --no-default-features --features tauri-ui --lib
cargo test -p focaldesk-settings-core -p focaldesk-audio
cargo check -p focaldesk-settings --bin focaldesk-settings
cargo build -p focaldesk-settings --no-default-features --features tauri-ui --bin focaldesk-settings-next
bash scripts/test-settings-ui.sh
FOCALDESK_SETTINGS_TEST_SCOPE=exclusive bash scripts/test-settings-ui.sh
cargo test -p focaldesk-engine --features drm-vulkan --lib
cargo clippy -p focaldesk-engine --features drm-vulkan --all-targets -- -D warnings
cargo build -p focaldesk-audio --example audio_probe
python3 apps/focaldesk-settings/tests/audio_runtime.py
bash scripts/test-display-trial.sh
bash scripts/check-markdown-links.sh
just settings-stage /tmp/focaldesk-settings-stage
```

The UI harness needs WebKitWebDriver and either Weston or KWin. It creates an
independent desktop/session bus, private XDG directories and fixture services.
CI now explicitly builds the Tauri executable, runs that harness and uploads
screenshots/logs. The nested recovery harness starts its own FocalDesk compositor.
Neither harness exercises real monitor modes, accounts, wireless devices or power
actions. Kernel-fence and local socket tests require access outside the sandbox.

## Physical-device release gates

For every gate record tester/date, release SHA, distribution, backend, hardware,
driver, scale factors, steps, observed results and artifact paths. Use a dedicated
test account/session and restore changes afterward. Unchecked means unverified.

- [ ] **Displays:** physical multi-monitor DRM on GLES and Vulkan; mixed scaling,
  rotation and HDR/SDR; disable/re-enable, Control Panel roles, multi-GPU and
  hot-unplug. Compare visible output with saved/live state. Verify Keep, Revert,
  timeout, Settings crash, suspend and failed modeset recovery. Verify actual ICC
  rendering, HDR calibration patterns and exclusive-HDR session recovery.
- [ ] **Input/accessibility:** physical device capability/reconnect behavior,
  keyboard-only traversal, accessible labels/focus, screen reader, IME/Maliit,
  magnifier/focus following, Sticky/Slow/Bounce Keys and key capture.
- [ ] **WebKit/rendering:** 100/125/150/200% scaling and mixed-DPI moves; dark/high
  contrast, reduced motion, portal file dialogs/cancellation and clipboard copy.
- [ ] **Sound:** real default routes, output/input volume/mute, boost and reconnect;
  microphone/speaker explicit tests and cleanup on every exit path.
- [ ] **Network/Bluetooth:** real adapters, credentials, radio changes,
  authorization rejection, PIN/comparison pairing and disconnect/helper cleanup.
- [ ] **Calendar/printers:** Google/Microsoft OAuth/callback/consent and secrets
  broker; account selection/sync/disconnect. Disposable CUPS queue, drivers,
  authorization, jobs and physical print page.
- [ ] **Applications/Chrome/Workspaces:** real file/link opening, new-window rules,
  next-login startup, split restore/ratios, wallpaper inheritance, names,
  transitions, indicator ordering and stale-client rejection.
- [ ] **Privacy/Power/Debug:** disposable saved permissions/recent activity and live
  retention; lock-screen privacy, idle/lid/battery behavior, power authorization,
  confirmed disruptive actions and real logging/clipboard behavior.

## Commit, packaging and rollout gates

- [ ] Put the complete migration in a reviewable commit, including new Rust,
  frontend, lockfile, capabilities, icons, fixtures, scripts and shared tokens.
  Keep unrelated Archiver work out of the migration review.
- [ ] Run full repository CI on that commit and link its results. Local passes
  cannot validate a future commit or establish that unrelated workspace CI passes.
- [ ] Test a clean staged release on each supported distro, including matching
  compositor/connector APIs, WebKitGTK/GTK/portal dependencies and launcher assets.
  Verify an older compositor produces a clear error and preserves preferences.
- [ ] Verify install/upgrade/uninstall and panel callers, preserve user data and
  test rollback by launching GTK without a config migration or JSON repair.
- [ ] Switch the installed default only after the physical-device and CI/package
  gates have evidence, or an explicitly documented accepted exception. Record the
  rollout commit and rollback procedure. Retiring GTK is a separate decision.

**Next step:** run the physical-device matrix in a recovery-capable test session. Feature implementation
is complete; production default rollout remains gated by the evidence above.
