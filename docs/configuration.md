# Configuration

FocalDesk stores user configuration below the XDG configuration directory. On
a typical Linux installation this is `~/.config/focaldesk`; when
`XDG_CONFIG_HOME` is set, that directory is used instead.

Because the project is alpha, configuration formats may change. Keep a copy of
working configuration before testing a new revision.

## Files

| Path | Purpose | Editing guidance |
| --- | --- | --- |
| `$XDG_CONFIG_HOME/focaldesk/settings.json` | Main desktop, application, input, workspace, privacy, power, debug, and chrome settings | Prefer the Settings application |
| `$XDG_CONFIG_HOME/focaldesk/config.toml` | Compact compositor appearance and display configuration used by the typed config API | Developer-facing; may be consolidated with `settings.json` later |
| `$XDG_CONFIG_HOME/focaldesk/displays.json` | Detected output topology and runtime display choices | Generated; do not edit while FocalDesk is running |
| `$XDG_CONFIG_HOME/focaldesk/ai_permissions.toml` | Persisted AI permission decisions; stored mode `0600` | Manage through the permission UI when possible |
| `$XDG_CONFIG_HOME/focaldesk/automation/automations.toml` | Scheduled automation definitions | Experimental; service is opt-in |
| `$XDG_CONFIG_HOME/focaldesk/automation/scripts/` | Lua automation scripts referenced by `automations.toml` | Experimental and security-sensitive |
| `$XDG_CONFIG_HOME/focaldesk/secrets-acl.toml` | Per-systemd-unit access to native credential-broker keys | Security-sensitive; default deny |

FocalDesk falls back to built-in defaults when a settings file does not exist.
An invalid file may also cause the affected component to use defaults, so check
the logs after hand-editing configuration.

The GTK fallback shell can read alternate geometry and presentation from
`config.toml`. The primary GLES shell deliberately follows the compositor's
canonical top-bar and left-sidebar layout:

```toml
[shell]
style = "attached" # attached | floating

[panel]
position = "top" # top | bottom
corner_radius = 16

[dock]
position = "left" # left | right
corner_radius = 24
size = "normal" # compact | normal | expanded
```

These options configure the standalone GTK shell clients. Floating GTK docks do
not reserve a full work-area strip. Attached GTK panels and docks claim an
exclusive zone matching their configured edge and size. Restart the shell
clients after changing geometry.

## Main settings

The main settings model currently includes:

- Appearance: theme, accent, shell sizing, icons, and animations.
- Displays: connector, mode, position, scale, primary output, color profile,
  ICC profile, and requested HDR state.
- Input: pointer speed, natural scrolling, XKB layout options, and validated
  compositor shortcut overrides.
- Applications: preferred terminal, browser, browser backend, and file manager.
- Workspaces: session restore, launch maximization, and visible workspace slots.
- Privacy, power, diagnostic logging, and desktop chrome layout.

Use the Settings application for ordinary changes. If you edit JSON or TOML by
hand, stop the component that owns the file first and validate the syntax before
restarting it.

Common shortcuts are editable on the Keyboard page. Advanced overrides use the
`input.keybindings` object in `settings.json`; see
[Default Keybindings](keybindings.md#configuration-syntax) for action names and
shortcut syntax.

The `privacy.location_services` setting controls the standard XDG Location
portal. See [Location services](location-services.md) for the request flow,
runtime dependencies, and security boundary.

## Environment variables

The following variables are useful supported development controls. Variables
not listed here should be treated as internal and may change without notice.

| Variable | Purpose |
| --- | --- |
| `RUST_LOG` | Set tracing filters, for example `debug` or `focaldesk=trace` |
| `FOCALDESK_LOG_FILE` | Override the compositor log-file path |
| `FOCALDESK_AI_PERMISSION` | AI policy: `prompt`, `allow-session`, `allow-persistent`, or `deny` |
| `FOCALDESK_AI_PROVIDER` | Select the default configured AI provider; defaults to `ollama` |
| `FOCALDESK_OLLAMA_BASE_URL` | Override the Ollama service URL |
| `FOCALDESK_OLLAMA_MODEL` | Select the default Ollama model |
| `FOCALDESK_AI_SOCKET` | Override the AI service Unix-socket path for development |
| `FOCALDESK_SCREENCAST_OUTPUT` | Select the capture output when no portal chooser input is available |
| `FOCALDESK_VOSK_MODEL_DIR` | Point voice recognition at a Vosk model directory |
| `FOCALD_SPEECH_BACKEND` | Select `espeak-ng` or `piper` for speech synthesis |

IPC socket overrides such as `FOCALDESK_DESKTOP_SOCKET_PATH` and
`FOCALDESK_SETTINGS_SOCKET_PATH` exist for testing multiple local instances.
They are developer controls, not stable cross-version IPC contracts. Override
paths must live in a real directory owned by the current user; FocalDesk refuses
shared, symlinked, or foreign-owned socket directories. Normal services use
private sockets below `$XDG_RUNTIME_DIR/focaldesk`.

HDR and color-management overrides are intentionally omitted here. They bypass
hardware safeguards and should only be used while investigating the relevant
code and logs.

## Secrets

Cloud-backed AI providers first query `focald-secrets` for
`ai/openai-api-key`, `ai/anthropic-api-key`, or `ai/vllm-api-key`.
`OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, and `FOCALDESK_VLLM_API_KEY` remain
development and upgrade fallbacks. Do not commit credentials or put them in
screenshots, issue reports, logs, or shared configuration.

See [Credential broker](secrets.md) for installation, login-key provisioning,
ACLs, and commands for storing provider keys.

## Logs and state

See [Troubleshooting](troubleshooting.md#logs) for log locations. AI memory,
browser profiles, clipboard history, and other runtime state may also live below
the standard XDG config, state, or cache directories. Clipboard history, AI
Console state, AI permission records, and AI memory are created with private
per-user permissions. Clipboard capture is limited to one MiB and two seconds
per selection. Back up the entire `focaldesk` directory when preserving an
alpha installation across upgrades.
