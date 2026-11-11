# Browser automation

`focaldesk-browserd` is a Rust service using chromiumoxide to control an installed Chrome/Chromium process. It provides rendered page text, element selectors, viewport screenshots, clicks and typing. The AI backend remains in `focaldesk-server`; the Tauri console supplies manual browser controls and displays agent activity.

## Use

In the AI Console, open **Browser**, enter an HTTP(S) URL, and select **Open page**. Read the rendered page, capture a screenshot, or select an interactive element. Manual **Type text** and **Click selected element** buttons execute the action on the website.

For model-driven browsing, open **Agents**, choose **Browser Assistant**, and describe the task including a URL. Open/read/screenshot tools run without an interaction approval. Agent-proposed clicks and typing show their exact arguments in the run card and require approval through the existing native confirmation flow. Declining leaves the website unchanged by that proposed action. Chat messages by themselves do not execute browser tools.

## Components

- `services/focaldesk-browserd`: typed Unix-socket protocol/client, tool definitions, browser engine, daemon and live diagnostic.
- `services/focaldesk-server/src/ai_tools.rs`: browser tools alongside the existing MCP tools; read-only execution explicitly refuses click/type.
- `crates/focaldesk-ai/src/agent_definition.rs`: Browser Assistant with a bounded tool allowlist.
- AI provider planning: explicit JSON-completion method; Ollama planning uses JSON output with thinking disabled, while ordinary chat settings are preserved. The managed provider retains retries/deadlines/telemetry for both modes.
- `apps/focaldesk-ai-console3`: Browser page, screenshot display, tab selection, and agent approval controls.

## Runtime

The socket defaults to `$XDG_RUNTIME_DIR/focaldesk/browser.sock`, overridden with `FOCALDESK_BROWSER_SOCKET`. Socket permissions and peer authorization use the existing FocalDesk transport. Allowed clients are the server, console, and browser daemon diagnostics. Production clients must satisfy the existing ownership/service identity checks.

Chromium starts on demand, uses a dedicated `browser.profile` beside the socket (separate socket paths get separate profiles), and retains its default sandbox. It does not use the user's ordinary browser profile. Override detection with `FOCALDESK_CHROMIUM_EXECUTABLE` in `~/.config/focaldesk/browser.env`.

Only HTTP(S) navigation URLs without embedded credentials are accepted. No arbitrary JavaScript execution, file upload, cookie export, or browser-debugging endpoint is exposed to the model. Fixed page-reading JavaScript extracts at most 32,000 characters and 100 visible interactive elements. Website text is marked untrusted and the agent is instructed not to treat it as instructions. Site resources, redirects and network access still use ordinary Chromium behavior; this is not a network-origin isolation boundary.

Downloads are denied. At most eight tracked tabs are retained; tabs idle for 15 minutes are reclaimed on a subsequent operation. Each request has a deadline, request/response sizes are bounded, and the daemon limits concurrent connections. The browser profile and screenshot files live in the per-login runtime directory. Screenshots are PNG viewport captures; they are displayed by the console. Agent page observations are compacted below the existing per-tool context limit while retaining structured page IDs/selectors. Agent text context receives screenshot metadata rather than base64 image data, so screenshot capture does not by itself provide vision input to the model.

## Build and install

```bash
npm run build --prefix apps/focaldesk-ai-console3
cargo build --release -p focaldesk-browserd -p focaldesk-server
cargo build -p focaldesk-ai-console3
sudo install -o root -g root -m 0755 target/release/focaldesk-browserd target/release/focaldesk-server target/debug/focaldesk-ai-console /usr/bin/
systemctl --user daemon-reload
systemctl --user enable --now focaldesk-browserd.service
systemctl --user restart focaldesk-server.service
```

Install `packaging/systemd/user/focaldesk-browserd-fedora.service` as `focaldesk-browserd.service` for `/usr/bin` installations, or use the non-Fedora unit for `~/.local/bin` installations. Restart the console after installing its new binary.

## Validation

```bash
cargo test -p focaldesk-browserd
cargo test -p focaldesk-server ai_tools::tests
cargo test -p focaldesk-ai agent_definition::tests::built_ins_are_valid_and_unique
/usr/bin/focaldesk-browserd --check https://example.com
```

The live check opens a real page through the daemon socket, verifies nonempty rendered text, captures and validates a PNG, writes `/tmp/focaldesk-browser-check.png`, and closes the tab. It fails on service/browser errors.

For a controlled JavaScript/input/click test, serve `services/focaldesk-browserd/tests` over local HTTP, then run `focaldesk-browserd --check-interactions http://127.0.0.1:18764/fixture.html`. This types into `#field`, clicks `#apply`, and checks the JavaScript-produced result before validating a screenshot.

The installed console also provides `focaldesk-ai-console --check-browser-agent`. It starts a read-only Browser Assistant run against example.com, polls its result, and succeeds only if the completed run contains an observed `browser_open` tool result. This exercises the actual model, planner, capability executor, browser IPC and synthesis. It consumes model inference and creates a run in agent history.

## Capability awareness in ordinary chat

The AI service supplies a trusted runtime capability message to both ordinary and streaming chat requests. It reports whether memory is configured and requested, current index size when available, retrieval failure and supplied-excerpt counts, registered browser tools, and whether Browser Assistant is enabled. It distinguishes configured integration from actual tool use. Earlier assistant denials do not define runtime capability.

The local embedding pipeline requires `nomic-embed-text` by default (`ollama pull nomic-embed-text`), producing 768-dimensional vectors. A configured but empty index contains no project knowledge; index selected documents from Knowledge. The console displays the empty state and provides a Browse with AI action to select Browser Assistant. The installed console's `--check-capabilities` diagnostic asks the real model to describe the current capabilities despite an earlier assistant denial in the supplied conversation.
