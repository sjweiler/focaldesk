# FocalDesk AI Console — Tauri 2

React/TypeScript frontend for the existing FocalDesk Rust AI daemon. The compositor, FocalVector, inference, RAG, permissions, and guardrails stay in their existing services. This app introduces no replacement backend.

## Run

From this directory:

```bash
npm ci
npm run tauri dev -- --no-default-features
```

Browser design preview: `npm run dev`. The console starts with real local history or an empty conversation; no example conversation is seeded. Sending messages and discovering providers/models require the desktop app and the existing AI daemon. Desktop errors are displayed with a retry action; preview mode never fabricates successful daemon responses.

## Connected features

The console calls the existing Rust AI daemon and Chromium browser service. Production never substitutes fixture responses when a service fails.

- Chat streams guarded complete lines and supports Stop, retry, Markdown/GFM, citations, provider/model selection, temperature and token limits.
- Conversations and workspace settings persist atomically to `$XDG_DATA_HOME/focaldesk/ai-console/state.json` (0600), with a previous-state backup. Rename, delete, move, search, JSON import/export and explicit migration from the old GTK `ai_console.json` are available.
- Workspaces can share retrieval or restrict RAG to selected indexed sources. An empty selection supplies no retrieval evidence. The shared index is retained when a workspace is deleted.
- Knowledge offers native file/folder pickers, document/folder/HTTPS website indexing, tracked background progress, cooperative cancellation, reindex/remove, memory search/remember/forget and retrieval evaluation. Cancellation finishes the current document before stopping. Website indexing uses the bounded static crawler; interactive browsing uses Chromium.
- Text and PDF text attachments are extracted locally. PNG/JPEG/WebP attachments use provider vision formats, requiring a vision-capable model. Limits: 8 MB per file, 8 MB of image bytes per request, 10 MB combined selected attachments. Extracted text is capped at 128,000 characters per file with a visible truncation label. Attachments are included in history and backups.
- Agents support start, cancel, dry-run, retry, native action confirmation, custom definition import/export/edit/install/rollback, enable/disable and schedule/event triggers.
- Controls expose live workflows and runs, Mission Control, packages/signers, scenario capture/evaluation, context grants/suggestions, event processing, attention routines, capability leases and connector lifecycle operations. Advanced manifests use JSON editors and native JSON file import/export. Existing daemon validation and confirmation requirements apply.
- Voice controls connect to existing microphone/STT and speech services, including optional wake phrases, blocked applications, dictation, read-aloud and Stop speech. Closing the console releases its microphone leases and cancels its active chat streams.
- Settings save supported non-secret provider settings while preserving unrelated environment entries. API keys go only to the credential broker under a narrow write-only ACL; the form reports when that broker is absent. Provider changes require the explicit backend restart button.
- GPU/VRAM telemetry comes from `nvidia-smi`; unavailable hardware is reported. Last-request tokens/sec includes retrieval and request latency. Provider counters come from the daemon.
- Browser supports dedicated Chromium tabs, current-tab navigation, Back/Forward/Reload, rendered text, selectors, typing/clicking, screenshots and optional live screenshots. Markdown web links populate Browser's address field. Browser Assistant uses the same real browser tools. Downloads are denied.
- Midnight, Slate Blue and High Contrast themes are persisted.

Conversation history and attachments go to the selected provider, which can be remote. Browser and retrieved website text remain untrusted evidence. See [browser service documentation](../../docs/browser-service.md).

## Validation

```bash
npm run build
npm test
cargo test -p focaldesk-ai --lib --offline
FOCALDESK_AI_PERMISSION=allow-session cargo test -p focaldesk-ai --test console_parity --offline -- --ignored --nocapture
cargo build -p focaldesk-ai --example console_fixture --offline
cargo build -p focaldesk-ai-console3 -p focaldesk-browserd --offline
python3 tests/native_ui.py
python3 tests/live_inference.py # requires local gemma4:12b
```

The isolated IPC regression verifies folder indexing, project-only evidence, streamed deltas, cancellation, split-chunk secret redaction and cooperative job cancellation. Provider contract tests verify vision payload formats. `tests/native_ui.py` drives the real Tauri window using WebKitWebDriver and temporary Rust fixture storage; the fixture model and embedder are intentionally synthetic, and no Tauri calls are mocked. It exercises native IPC, workspace/history persistence, indexing, scoped requests, streaming/Stop, migration, settings and actual Chromium navigation/screenshots. It requires a graphical session, WebKitWebDriver and network access to example.com. Physical microphone capture and audio playback require manual hardware testing.

## Installed identity and live verification

Normal Cargo builds embed the frontend (`custom-protocol` is enabled by default), so the installed app does not depend on a Vite server. Use `--no-default-features` for Tauri development mode.

The package is named `focaldesk-ai-console3`; its executable is `focaldesk-ai-console`, the canonical identity authorized by the AI daemon. Release daemons reject user-owned development executables. Install the built executable root-owned with mode 0755 before testing against a production daemon. Do not disable IPC peer checks.

```bash
cargo build -p focaldesk-ai-console3 --offline
sudo install -o root -g root -m 0755 ../../target/debug/focaldesk-ai-console /usr/bin/focaldesk-ai-console
/usr/bin/focaldesk-ai-console --check-services
/usr/bin/focaldesk-ai-console --check-chat
/usr/bin/focaldesk-ai-console
```

`--check-services` queries real providers, daemon health, memory, agents/runs, connectors and indexed documents. `--check-chat` sends one short real model prompt and fails if no nonempty answer is returned. Neither mode requires a webview. The GTK and Tauri packages share the canonical executable name, so build/install the intended package explicitly during migration.

`--check-stream-stop` verifies real configured-model deltas and cancellation. `--check-image /tmp/synthetic-red.png` checks vision inference on a synthetic red PNG. These diagnostics fail when the relevant model capability is unavailable.

The desktop launcher starts `focaldesk-ai-console-app.service`, preserving the broker's systemd-unit identity. Install its unit from `packaging/systemd/user/` at the repository root. The broker and voice daemons remain separate services; installing this frontend does not provision their encryption keys or physical audio devices.

## Verified on 2026-11-10

135 AI library tests passed, alongside the isolated IPC regression and native WebKit UI run. The native run also read all advanced control panels and exercised actual Chromium history navigation and PNG screenshots. A separate isolated-service run used the installed `gemma4:12b` model: it emitted a guarded streamed delta, accepted cancellation and ended as cancelled; vision inference identified a synthetic red PNG as “Red”. Neither test changed the user's production index or conversations. Audio hardware capture/playback and remote-provider credentials were not exercised. The local credential broker was absent, which is reflected by disabled API-key fields.

Production installation was completed and the AI daemon, Chromium daemon and console were restarted. All three systemd units reported active/running. Installed executable hashes matched the tested build outputs and were root-owned (0755). Production `--check-services`, `--check-stream-stop` and `--check-image` passed against the actual daemon and Gemma model. The production FocalVector index reported 4,212 memories; tests did not populate or clear that index.
