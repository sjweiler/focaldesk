# FocalDesk Settings

The Tauri 2 / Svelte application is now the default `focaldesk-settings` launcher.
The GTK application remains available as `focaldesk-settings-gtk`, and the Tauri
binary can also be launched directly as `focaldesk-settings-next`. It includes
Appearance, Accessibility, Theme Editor, Network, Calendar, Bluetooth, Printers,
Displays, Sound, Applications, Chrome, Workspaces, Keyboard & Input, Privacy
preferences, Power preferences, Debug, and About. All sidebar sections have
Tauri pages. Mode validation, persistence, hardware interaction, and display
recovery remain in Rust. The default launcher is a small wrapper that preserves
command-line arguments and starts the Tauri binary; GTK remains the rollback path.

See the [parity audit and release checklist](RELEASE-CHECKLIST.md) for the
implementation evidence, manual test gates, and rollout requirements.
The P1–P7 feature work is implemented; physical-device and rollout validation remain.

## Connecting to a release compositor

A production/release compositor accepts only root-owned executable inodes (not
group/world-writable), or explicitly approved service/executable pairs. A staged
release launched directly from `/tmp` is normally user-owned and will be rejected,
even when the running compositor supports `focaldesk-settings-next`. The symptom
may be `Connection reset by peer`; the compositor journal records the rejected
executable and service identity. Do not bypass peer checks to run the preview.

Install it as the default through the root-owned target:

```sh
just install-settings
/usr/local/bin/focaldesk-settings
```

An already matching compositor does not require a session restart for this
installation. Close the old staged preview when ready. An older compositor that
does not recognize the preview/protocol still requires an update and session
restart. The isolated UI fixtures do not establish production IPC trust.

## Run the preview against a development compositor

From the repository root, run `just settings-preview`, or use the commands below:

```sh
npm ci --prefix apps/focaldesk-settings/frontend
npm run build --prefix apps/focaldesk-settings/frontend
cargo run -p focaldesk-settings --no-default-features --features tauri-ui --bin focaldesk-settings-next
```

Development still runs the Tauri binary directly. Keep GTK installed for the
advanced controls still linked from Tauri and as a rollback option. `just
settings-preview` also builds the GTK sibling. Apply requires a compositor built
from this checkout: its desktop IPC
allowlist now recognizes `focaldesk-settings-next`. An older running compositor
can reject the preview client until the compositor is rebuilt and the session is
restarted. Opening and previewing themes do not require that restart.

It requires the same Linux WebKitGTK 4.1 dependencies as Mail and AI Console. GTK4
is optional in a Tauri-only build.

## Architecture

`src/appearance.rs` is the frontend-independent Rust API. It validates editable
fields and resolves theme choices from validated system, packaged, and installed
theme documents. Appearance previews project colors and show wallpaper thumbnails;
EXR wallpapers are tone-mapped to SDR, and the preview does not show HDR brightness.
Previewing neither installs a theme nor writes configuration. Apply installs a selected collection
package if needed, reloads current configuration, rejects stale appearance edits,
merges only the exposed appearance fields, and submits the result to the existing
compositor IPC. A rejected or unavailable compositor never triggers a direct-save
fallback. Concurrent hardware settings remain owned by existing services.

The Tauri bridge exposes page-specific Rust commands, including owned display and
theme trials, bounded hardware operations, diagnostics, and opening an allowlisted
existing Settings page. It exposes
no general command execution, arbitrary file writes, or remote web content. The
preview executable is authorized only for the desktop IPC endpoint it needs.

Framework-neutral CSS tokens live in `packages/theme` as `@focaldesk/theme` and are
imported by Settings, Mail, and AI Console. Svelte-specific components remain local
until their APIs have been exercised by more pages. Tokens do not replace the
compositor's Rust theme model.

## Appearance controls

Appearance includes theme selection, text size, glow strength, focused-display
highlighting, shader chrome, work-area glass, rail background, panel darkness,
and task-shelf visibility. High contrast and reduced motion are in Accessibility.
All Appearance edits stay in a draft until Apply; Discard restores saved values.
The local illustration previews theme colors, text size, and glow rather than
simulating the compositor's surface shaders or shelf policy.

Panel darkness is an optional override from 0–100%. Use theme panel darkness saves
`null`, leaving the theme's rail/dock backing opacities in control; editing an
override affects backing opacity only. Shelf choices are Always visible,
Intelligent dodge, and Autohide. Rust validates numeric ranges and typed enums,
checks the complete editable Appearance snapshot (including shelf visibility) for
stale edits, and merges these fields into the latest config. Other dock, display,
and shell settings are retained. A rejected compositor request does not fall back
to saving the configuration directly.

Rust/frontend tests and the isolated WebKit fixture cover the added controls,
draft/discard/apply, explicit/default panel darkness, invalid requests, backend
rejection, and concurrent shelf edits. Actual renderer/shelf behavior and theme
backing defaults still need the release checklist's live-session verification.

## Displays and recovery

Displays also includes top-bar/sidebar visibility across outputs and remembering
the focused display. These use separate Apply/Discard actions, typed Rust
preferences, stale-edit checks and compositor persistence with no direct-save
fallback. Finish or discard monitor edits first; layout controls are disabled
while monitor changes or a trial are pending. Rust also rejects layout saves
during an observed active trial. Focus restoration applies at the next sign-in.

Displays shows live compositor output state and advertised modes. Monitor arrangement
supports dragging with edge snapping, keyboard arrows, and numeric logical coordinates.
Resolution, refresh rate, fractional scale, rotation, primary display, HDR request, and
color profile changes stay in a draft until **Test changes**. Unsupported modes,
unsupported HDR, stale configuration, duplicate connectors, and disconnected layouts
are rejected by Rust before applying anything.

The compositor owns a fixed 20-second trial. Only **Keep changes** writes canonical
display preferences. Timeout, explicit Revert, session suspend, or a failed trial
triggers restoration of the preceding live configuration. The timer runs in the
compositor service loop after the frontend exits. Confirmation requires the current
trial ID, an unexpired deadline, a successful backend rebuild, and matching live state
(including active HDR when requested). Concurrent persisted display edits invalidate
confirmation. Display-only persistence preserves other settings and disconnected
monitor preferences; renderer inventory never stores a transient trial.

Both GLES and Vulkan DRM backends resolve in-memory trial/rollback overrides rather
than writing trial settings to disk. Nested sessions support logical layout trials;
physical monitor modes, enabling/disabling outputs, and HDR remain controlled by
the host compositor. In DRM sessions, the Tauri page now supports output
enable/disable and Desktop/Control Panel roles through the same timed trial.
Connected disabled outputs remain selectable using kernel-probed inventory,
including after confirmation/restart; saved JSON alone never proves a connector
is physically present. At least one enabled primary Desktop output is required.
Disabled outputs do not constrain the active arrangement's connectivity.

Both DRM backends honor the transient enabled/role overrides. Trial confirmation
checks actual enabled state and role; failed rebuilds and expiry retain rollback.
GLES retains the session-owned device fd after a failed destructive rebuild so the
rollback can retry. Window/focus restoration avoids reserving ordinary application
windows on Control Panel outputs. Physical disable/re-enable, hot-unplug, modeset
failure and multi-GPU recovery still require dedicated hardware verification.
ICC file selection and HDR appearance/calibration are native in Tauri and use the
same owned display trial. Calibration patterns require the current trial ID and
an active HDR output. The experimental exclusive HDR10 control requires explicit
confirmation before saving its session mode and requesting logout; a rejected
logout is reported as a saved preference awaiting manual activation.

The existing compositor must be rebuilt and the session restarted to expose the new
display trial protocol. Physical multi-monitor KMS/HDR testing is still required before
making the preview the installed default.

Useful diagnostics:

```sh
cargo run -p focaldesk-cli -- display-configuration
# trial.json contains {"expected": [...current configs...], "outputs": [...draft configs...]}
cargo run -p focaldesk-cli -- preview-displays trial.json
cargo run -p focaldesk-cli -- confirm-displays 1
cargo run -p focaldesk-cli -- revert-displays 1
```

## Sound

Sound uses the shared `focaldesk-audio` Rust API. Device inventory and controls use
structured `pactl` metadata through PipeWire-Pulse (or PulseAudio), with fixed
commands and a two-second command timeout. Both output and input support default
device selection, volume, and explicit mute state. Every change resolves the current
device ID again, so disconnected/stale devices are rejected. Volume changes are
limited to 0–150% for outputs and 0–100% for inputs; opening the page does not
change existing boosted volume. Output boost preserves the GTK range.
Speaker-monitor sources are excluded from the microphone list.

Microphone level tests are opt-in and limited to 20 seconds by Rust. `parec` produces
raw float PCM that Rust immediately reduces to peak/RMS levels; raw audio is never
sent to TypeScript or saved as a recording. Tests stop on Stop, page exit, window
blur, window close, device switching, or microphone mute. A parent-death signal
terminates the capture helper if Settings crashes. PipeWire reconnect suppression
keeps the test pinned to its selected source, following the [WirePlumber linking
policy](https://pipewire.pages.freedesktop.org/wireplumber/policies/linking.html).
The UI never starts capture just to show microphone metadata.

Install `pactl` and `parec` for live operation (`pulseaudio-utils` on Ubuntu).
These tools connect to PipeWire's PulseAudio compatibility server; the implementation
does not replace the audio server, modify system services, or invoke arbitrary
commands supplied by the frontend. Sound changes take effect immediately in the
session audio server and use its normal persistence policy.

Audio validation without touching hardware:

```sh
cargo test -p focaldesk-audio
cargo build -p focaldesk-audio --example audio_probe
python3 apps/focaldesk-settings/tests/audio_runtime.py
```

The runtime test creates private command fixtures and synthetic PCM to verify
monitor filtering, the real 20-second deadline, parent-crash cleanup, stalled-stream
cleanup, and command timeouts. The native WebKit test uses the same fixture tools
to verify routing, volumes, mute, errors, level rendering, and capture cleanup.

For an explicitly read-only check against the current session:

```sh
cargo run -p focaldesk-audio --example audio_probe -- snapshot
python3 apps/focaldesk-settings/tests/live_audio.py
```

The live UI check only opens Sound and sends Tab navigation. It saves a screenshot
and a report of device counts, keyboard focus, and device pixel ratio; it never
changes hardware settings or starts microphone capture.

## Keyboard & Input

The Tauri preview includes keyboard layouts and variants, repeat delay/rate,
modifier presets, shortcut overrides, pointer speed, natural scrolling, workspace
swipes, and capability-aware mouse/touchpad controls. The typing test stays local
to the page. Rust compiles the requested XKB keymap and rejects invalid values,
shortcut conflicts, unsupported device options, and stale edits before saving.
Input updates preserve unrelated preferences and unknown configuration fields.
Disconnected device preferences remain intact. Device enable/disable follows
reported hardware capabilities and keeps keyboard-capable devices enabled. Failed
updates restore the preceding enabled state along with other device settings.
Pointer/gesture reset changes only the draft. Shortcuts support local key capture
and text entry; compositor-reserved combinations can still be entered as text. GTK's
device acceleration controls expose speed and profiles, not a custom-curve editor.

This page requires the updated compositor IPC API. Rebuild and restart FocalDesk
before using it in a real session. Physical input changes still require testing
on supported devices before the Tauri preview becomes the installed default.

## Applications

Applications is native in Tauri, with separate tabs for FocalDesk launch defaults,
desktop file/link associations, ordered window rules, and startup applications.
Commands are executable names or paths. Startup arguments are edited individually;
spaces inside an argument remain intact, and commands run without an implicit
shell. Saving never launches an application. Enabled startup changes require
confirmation and take effect at the next sign-in. The first matching window rule
wins for new windows; restored windows retain their saved placement.

Rust validates changes and atomically updates the existing `apps` configuration.
Other settings and unknown application/row fields survive edits and reordering.
Concurrent application changes require reloading. Reset changes the draft first.

File/link associations use GIO's registered desktop applications and XDG MIME
preferences, independently of the FocalDesk launch commands. Each save changes one
type and checks both its effective default and a fingerprint of MIME preference
files. External preference edits require a reload. The UI reports when a desktop
override still takes precedence. The backend never launches the selected app.

The native test uses private XDG desktop entries, MIME preferences, and an execution
canary. It checks saves, row identity, argument boundaries, stale edits, reset,
association updates, and invalid selections. Live file opening, next-sign-in
startup, and real compositor window placement still need a manual session check.
All sidebar sections now have Tauri pages.

## Debug

Debug is native in Tauri: log level, FPS/frame timing, damage statistics, input
events, and verbose protocol logging. Apply/Discard and draft reset use the same
atomic, stale-edit-checked Rust persistence as the other preferences. Unknown
debug fields and unrelated settings are retained. Reload status reports whether
the desktop accepted the saved settings; protocol logging remains conditional on
the runtime support of each component.

The diagnostic preview contains version/build information, OS/architecture,
session details, and local executable/settings/log paths. It does not read log
contents, credentials, or the general environment. Copy uses the native clipboard
on Tauri's GTK event thread and sends nothing externally. Its GTK3 clipboard API
uses the GTK runtime already required by WebKitGTK, rather than the old GTK4 UI.
Open log file resolves a current local log candidate in Rust and requests its
default desktop application through GIO. The frontend cannot specify an arbitrary
path to that operation.

The isolated WebKit test checks all logging controls, report preview, explicit
opening of a fixture log through a private desktop application, preservation,
reset/discard, invalid levels, and stale edits. Clipboard contents are not changed
by the automated test; copy/paste and real-session logging still need manual
verification. All 18 pages being present does not replace the hardware, compositor,
accessibility, and session testing needed before making Tauri the installed default.

## Workspaces

Workspaces is native in Tauri, including visible slots (1–9), transition style,
session and split-layout restoration, maximize-on-launch, split-screen enablement,
and names/wallpapers for workspaces 1–9. Default keyboard hints are shown separately
from customized bindings in Keyboard. Restore settings apply at the next sign-in;
maximize-on-launch affects new windows. Desktop animations must be enabled for
animated transitions.

Rust validates saves and updates the existing workspace section atomically. Saved
split ratios, future fields, entries beyond workspace 9, and other sections are
preserved. Divider updates do not conflict with preference edits; concurrent edits
to workspace preferences require a reload. Reset changes the draft first and
preserves saved split ratios and unlisted entries. No session or window launch is
triggered by saving.

Wallpapers use existing absolute local PNG/JPEG/WebP/EXR paths or a native file
chooser. Clearing a path inherits the theme wallpaper. Unchanged legacy paths are
preserved. The WebKit fixture checks saves, wallpapers, reset/discard, split-ratio
preservation, and stale edits. Native chooser interaction, real wallpaper rendering,
transitions, and session/split restoration still need manual desktop testing.

## Chrome

Chrome is native in Tauri: show or hide the ten top-bar indicators and five fixed
sidebar buttons, and set their ordering using comma-separated numeric IDs. Empty
orders use compositor defaults; unlisted items follow in default order. Workspace
buttons remain dynamic. Changes use Apply/Discard rather than saving every edit.

Rust validates orders, rejects stale edits, and atomically updates only visibility
and ordering. Unknown settings, custom launch definitions (including future
fields), and hidden custom IDs survive. Custom items are listed for reference and
remain configured in `settings.json`; this page never runs their commands. Reset
restores built-in visibility and ordering in the draft while retaining custom
definitions and visibility. Reload status distinguishes saved preferences from
successful desktop reload. Live top-bar/sidebar rendering needs a manual session
check; the isolated WebKit test covers controls, preservation, invalid orders,
reset, and stale edits.

## Printers

Printers is native in Tauri. It includes configured CUPS queues and service state,
explicit device discovery, manual/discovered printer installation, IPP Everywhere,
raw queues where supported, registered legacy drivers, the server default printer,
pause/resume, accepting/rejecting new jobs, and unfinished job lists with individual
cancellation. Existing GTK Settings remains available as the companion.

Rust owns all CUPS commands, parses output under a fixed locale, validates queue
names/device URIs/driver identifiers, and resolves targets against fresh inventories.
Installation rejects names already present in the queue inventory. Discovered
devices are rechecked before installation; vendor-specific URI backends must be
reported by the configured CUPS service. File/command-style backends and embedded
credentials are refused. UI device addresses omit user information and query strings;
discovery IDs retain the full device identity in Rust for installation.

Commands have bounded output and deadlines, and helpers are terminated after a
Settings crash. Discovery and driver enumeration happen only on explicit requests;
background refresh reads configured queue status. Unknown job-acceptance state is
shown as unknown with its toggle disabled. A completed command followed by a failed
status refresh is reported explicitly rather than implying that the command failed.

Changing the default affects the configured print service. Pausing/resuming,
rejecting jobs, and cancelling a job require inline confirmation. Resume can start
existing queued work. Cancellation only targets one freshly resolved unfinished job;
already printed pages cannot be recalled. These operations use normal CUPS
authorization and do not write printer state into FocalDesk's settings.json.

The native WebKit test installs private CUPS executables in its child PATH. It checks
defaults, pause/resume, job acceptance, individual cancellation and stale-job rejection,
discovery, installation, registered/vendor/raw drivers, input/URI validation,
credential redaction, authorization failures, timeouts, partial metadata errors, and
stopped/empty service states. No real printers, queues, or documents are changed, and
no pages are submitted. Real CUPS authorization, device discovery, vendor drivers,
driverless compatibility, and physical printing still need manual session testing.

## Calendar

Calendar is native in the preview. It shows up to 20 cached upcoming events over
the next seven days, account/sync status, provider calendar discovery and selection,
local reminder timing, explicit sync requests, Google/Microsoft browser sign-in,
and account disconnect with optional local-cache deletion. Event editing remains
in the Calendar application. This Settings page does not expose provider event
write commands, although the shared provider authorization requests calendar
read/write permission for the application ecosystem.

Rust owns the connector-host client, validates account/provider/calendar inputs,
and serializes operations. Calendar preference saves use an atomic compare/check
at the connector host against the expected selection and reminder time. Legacy
GTK preference requests remain supported. Removing a calendar from sync clears
its cached events, so the UI asks for confirmation; provider events are unchanged.
Sync responses report queued work, and existing connector consent rules still apply.

Provider URLs are validated against HTTPS Google/Microsoft authorization endpoints
and opened from Rust. The webview receives flow status and optional user codes,
not authorization URLs, access tokens, refresh tokens, or raw OAuth errors. An
optional Google application client secret is transient form input and is cleared
on submission; credential storage remains with the existing Rust services and
secrets broker. Browser launch is a fixed `xdg-open` call with a validated URL.

Sign-in status polling stops on page exit or Stop waiting. This does not cancel
the connector host's already-started browser flow; it can finish until its existing
10-minute deadline. Returning to the page can resume status checks during the
same Settings process. Disconnect and deletion require explicit confirmation.

Rebuild/restart `focald-connectors` before saving preferences: updates use the new
checked request. The connector peer-policy catalog now also recognizes the preview;
the host retains its existing owner-only socket and same-user checks. The native
WebKit fixture uses a private connector socket and fake browser,
testing cached events, selection/reminder drafts, concurrent edits, sync, sign-in,
URL validation, error redaction, disconnect/cache deletion, and service errors.
It does not contact Google/Microsoft or change real accounts. Real provider OAuth,
browser callbacks, consent, and secrets-broker integration still need session testing.

## Theme Editor

Theme Editor is native in Tauri. It includes solid/linear/radial paint, 2–8 gradient
stops, linear-light sRGB/Display P3/Rec.2020 colors, HDR luminance intent, semantic
surface interaction states and inheritance, edges, typography, layout, output color
behavior, wallpaper/tint/processing, backing opacity, contrast checks, and editable
TOML. Rust validates documents, converts gamuts, and projects the local SDR
illustration. Editing the draft does not change the desktop.

Open/Save As/Export and wallpaper selection use native XDG FileChooser portal
dialogs. FocalDesk's portal configuration prefers the GTK fallback for FileChooser;
a working portal backend is required. Rust writes theme documents and packages
atomically, refuses symlink destinations, and checks file hashes before overwriting.
Existing destinations selected in Save As/Export are confirmed by the native dialog.
Packages use the existing asset/checksum/path validation. Open a `.fdtheme` package
and install it to unpack assets; replacing an existing package requires the explicit
replacement checkbox. Installed packages appear in Appearance.

Desktop preview and Apply to this session use new owned theme-editor IPC commands.
An owned preview expires after 30 seconds in the compositor, including after the
requesting process exits; another editor cannot revert it. Session apply checks
the last observed editor revision. Leaving the editor or closing Settings requests
reversion of its own preview. These commands require an updated compositor. Existing
GTK live-preview behavior remains available through the legacy commands.

Session apply does not persist the active-theme selection. Save/export preserves
the authored document; install a package and select it in Appearance to keep the
theme across sessions. The local illustration does not reproduce HDR or wallpaper
processing; use desktop preview to assess the actual renderer.

Native WebKit fixtures test gradient and surface editing, layout/wallpaper metadata,
TOML generation, file/package round trips, stale-file rejection, installation,
rejected runtime changes, session apply, and preview cleanup without changing the
real desktop. Native file chooser interactions still need manual session testing.
The nested-compositor recovery test separately verifies the real 30-second preview
deadline after CLI exit, ownership, stale revisions, and configuration isolation.

## Network and Bluetooth

Network includes wired connect/disconnect, Wi-Fi radio controls, an explicit
rescan, access-point signal/security details, and password-based or saved-profile
connections. Polling reads the NetworkManager cache without requesting scans.
Wi-Fi targets are resolved against a fresh device/BSSID inventory. Passwords go
to `nmcli --ask` on stdin, never into command arguments or settings.json, and are
cleared from the form after each attempt. Failed secret-bearing command transcripts
are replaced with a generic error. Enterprise networks may need an existing
NetworkManager profile; this page does not add a full profile/VPN editor.

Bluetooth includes radio controls, connected/paired devices, 30-second discovery,
pairing, connect/disconnect, and forgetting devices. Pairing uses a dedicated
BlueZ `bluetoothctl` agent: comparison prompts require an explicit user reply;
PIN and passkey prompts accept validated input. The agent stays alive until the
bond is confirmed and stable. Pairing does not automatically trust or connect a
device; Connect remains a separate explicit action.

Rust owns fixed commands, validation, timeouts, bounded output, and helper cleanup.
Discovery and pairing helpers stop on page exit, window close, their deadline,
or parent-process death. Page leases reject stale requests from an older Bluetooth
page. Radio-off, disconnect, and forget actions include an inline confirmation.
Successful connection/pairing state is persisted by NetworkManager or BlueZ.

The native WebKit test installs private `nmcli` and `bluetoothctl` executables in
its child PATH. It exercises escaped SSIDs, explicit scanning, secret delivery and
redaction, radio controls, passkey rejection/confirmation, PIN pairing, connect,
disconnect, forget, and helper cleanup without touching live connections. Real
adapters, authorization agents, and distribution-specific CLI behavior still need
hardware testing before the preview becomes the installed default.

## Accessibility

Accessibility is native in the preview. It includes Sticky, Slow, and Bounce
Keys; delay controls; modifier notifications and the Shift shortcut; magnification
and focus following; pointer size; the Maliit on-screen keyboard preference;
Night Light with manual/custom/solar schedules and local coordinates; color
filters; ambient-light adaptation; per-monitor comfort controls; reference color
plates; and a draft-only reset to neutral color comfort.

Rust validates all ranges before saving visual preferences and keyboard filters
in one atomic transaction. It preserves layouts, shortcuts, other input controls,
unrelated settings, and future fields, including nested Night Light fields.
Stale accessibility drafts are rejected. Changing global magnification clears
saved per-monitor zoom overrides; resetting color comfort clears monitor opt-outs.
Saving requests the same desktop reload used by GTK and reports when the desktop
cannot reload. Installed Maliit and a readable local IIO sensor are required for
the respective runtime features; enabling their preferences does not prove that
those services or hardware are available.

The native WebKit fixture checks keyboard-operated switches, dependent controls,
draft/discard, schedules, atomic saves, preservation of concurrent layout edits,
reset/deletion of overrides, and stale-edit rejection. Physical input, magnifier,
color transforms, sensor behavior, and Maliit still need session testing before
the preview becomes the installed default.

## Complete navigation, Privacy, and Power

The preview now keeps all 18 original Settings sections in their familiar order.
Mouse & Touchpad and Keyboard have separate entries into the native input page.
The sidebar scrolls independently; every entry has an accessible name and tooltip.
Every sidebar section has a Tauri page. GTK remains the installed default and
recovery companion until real-session and rollout validation is completed.

Privacy includes location services, recent activity, notification retention,
lock screen notification privacy, and clearing notification history on logout.
Saved AI/location permissions can be reviewed and individually revoked after
confirmation. Rust resolves the exact reviewed decision again and rejects changed
records. Inventory errors are reported independently, allowing available stores
to remain usable. Recent activity clearing also requires confirmation. Retention
saves send `SetHistoryLimit` to the live notification service and distinguish
persistence from activation failures; Retry live update retries a saved value.
Power includes blank/suspend timeouts, power-button and lid behavior, low-battery
behavior, and performance profiles. Battery/source/profile status comes from the
power service. Suspend, hibernate, restart, and shutdown require confirmation.
Acknowledgments report queued actions rather than claiming completed actions.

Rust validates and atomically saves only the selected section, preserving other
preferences and future fields. Stale drafts are rejected. After saving, Settings
requests a desktop reload and reports when activation is unavailable. Performance
profile changes use the existing power-service IPC; activation failures are
reported separately from successful preference persistence. No system action is
performed merely by opening either page or editing a draft.

## Validation

```sh
npm test --prefix apps/focaldesk-settings/frontend
npm run build --prefix apps/focaldesk-settings/frontend
cargo clippy -p focaldesk-settings --no-default-features --features tauri-ui --all-targets -- -D warnings
cargo test -p focaldesk-settings --no-default-features --features tauri-ui --lib
cargo build -p focaldesk-settings --no-default-features --features tauri-ui --bin focaldesk-settings-next
python3 apps/focaldesk-settings/tests/native_ui.py
bash scripts/test-display-trial.sh
```

The native test requires `WebKitWebDriver` and an available desktop session. It
uses private XDG directories and a compositor IPC fixture, leaving real preferences
untouched. It exercises the actual Tauri bridge, package installation, rejected
changes, stale-edit detection, preservation of unrelated configuration, and the
Displays controls with Test/Revert/Keep, Sound controls and microphone cleanup,
and Keyboard & Input draft/discard/apply, navigation, and typing-text privacy.
The input fixture checks the real Tauri bridge; Rust unit tests separately check
keymap compilation, shortcut conflicts, disconnected-device edits, and atomic
input persistence. The nested runtime test separately exercises
the actual compositor timeout after the requesting CLI has exited, expired IDs,
confirmation, explicit rollback, and concurrent saved-preference conflicts. It can
start headless Weston in CI and is included in the CI workflow.
Screenshots and logs are retained in its printed temporary directory.

Before making Tauri the installed default, verify input methods, keyboard-only
navigation, assistive technology, fractional scaling, focus, and multi-monitor
behavior under FocalDesk. The preview is an SDR illustration; authoritative HDR
calibration stays in the compositor. Display trials now use compositor-owned
timed confirmation and rollback that survives a Settings crash.


## Finishing the retrofit and staging

All P1–P7 implementation items are present. See RELEASE-CHECKLIST.md for exact
verification evidence and the remaining physical-device release gates. The app
continues to identify itself as an alpha preview. About exposes project/license
links and credits; build identity and executable details are in Debug diagnostics.
Legacy `--panel` arguments route to the corresponding Tauri destination, including
GTK's human-readable Theme Editor and Mouse & Touchpad names.

The explicit speaker test generates a quiet one-second stereo tone in a private
temporary WAV file and plays it through `paplay` on the selected output. Its Rust
helper has a four-second deadline and parent-death cleanup; Stop, route changes,
page exit, blur, and window destruction cancel it. Nothing plays on page load.
Install `paplay` alongside `pactl` and `parec` (`pulseaudio-utils` on Ubuntu).

Run the integration suite without competing with the active desktop:

```sh
just settings-test-ui
```

This creates a private headless Weston or KWin desktop and session bus. It needs
WebKitWebDriver; UI screenshots, fixture state, and host logs remain under `/tmp`.
CI builds the Tauri executable, runs this harness, and uploads its artifacts.
Remote CI still needs a run on the eventual migration commit.

Build an opt-in staged release without modifying the running system:

```sh
just settings-stage /tmp/focaldesk-settings-stage
```

The stage contains the default launcher, both GTK and Tauri executables, and the
license. The frontend and bundled theme collections are embedded in the Tauri
binary. This does not modify the running system. GTK rollback remains available
as `focaldesk-settings-gtk`. Verify WebKitGTK 4.1/GTK3, GTK4/libadwaita for the companion, portals,
matching FocalDesk services, and supported device behavior on each target distro.
Rollback means launching `focaldesk-settings-gtk`; no configuration
migration or JSON repair is required.

To run the same UI checks against the staged release binary:

```sh
FOCALDESK_SETTINGS_TEST_BINARY=/tmp/focaldesk-settings-stage/usr/bin/focaldesk-settings-next just settings-test-ui
FOCALDESK_SETTINGS_TEST_SCOPE=exclusive just settings-test-ui
```

The focused exclusive-HDR check uses a fake logout service and private state. It
verifies active-trial/stale-state rejection and saved-mode reporting when logout
is acknowledged or unavailable; it never logs out the active desktop.


## Collection wallpaper integrity

Default and all seven optional collection themes have their wallpaper assets:
Classic, Eagle, Moonbase, Lunar Transport, Venus Aerie, Mars Colony and Deep Space.
Optional `.fdtheme` packages embed the image bytes with size and SHA-256 metadata;
installation writes an absolute wallpaper path inside the installed theme folder.
The system Default document references `Prismara-HDR1400.exr`, which is included in
the source assets and installed alongside the existing HDR wallpaper.

Appearance caches validated collection packages once. Applying a collection
installs its wallpaper. Applying an exact legacy collection install that omitted
its wallpaper repairs it; customized documents are preserved. Tests fully decode
every shipped wallpaper and exercise Apply for every collection in WebKit.

Audit existing user installs without changing them:

```sh
cargo run -p focaldesk-settings --no-default-features --example repair_collection_wallpaper -- --all
```

Add `--apply` to back up and repair unmodified collection installs with missing or
unreadable wallpaper files. Existing valid wallpapers and customized themes are
left unchanged. If the active theme is repaired, the utility requests a normal
settings reload through the installed CLI; it does not change monitor modes.

Use `just install-theme-assets` to update system importable packages, and
`just install-settings-preview` to update the preview's embedded collection. Close
older Settings windows after upgrading so they cannot reinstall old package data.


## Power service compatibility

The running `focaldesk-powerd` must include the Settings preview in its production
IPC policy. Updating only the Settings executable leaves an older running daemon
rejecting requests with a connection reset. The power-service install recipes now
restart an existing daemon after installation, rather than leaving its old binary
running. Use the installed root-owned Settings executable; peer checks remain on.

Power actions are disabled until a live snapshot loads and become unavailable
again if a status refresh fails. Connection failures explain the service update
and Refresh status retry. The read-only production check opens a private Settings
window, queries only `GetSnapshot`, saves its screenshot/report, and closes it:

```sh
python3 apps/focaldesk-settings/tests/live_power.py
```

It never invokes suspend, hibernate, restart, shutdown or profile changes. The
isolated rejection/reconnect check is `FOCALDESK_SETTINGS_TEST_SCOPE=power just
settings-test-ui`.
