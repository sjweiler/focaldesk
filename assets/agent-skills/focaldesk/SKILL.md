---
name: focaldesk
description: Inspects, configures, troubleshoots, and extends the FocalDesk desktop safely. Use when a user asks an agent to change FocalDesk settings, arrange windows or workspaces, create themes, diagnose desktop failures, or explain FocalDesk state.
---

# FocalDesk

Use FocalDesk's typed interfaces before editing files or invoking generic shell commands.

## Workflow

1. Inspect the current state before proposing a change.
2. State the exact intended mutation and its scope.
3. Prefer FocalDesk MCP tools for desktop state and supported actions.
4. Prefer the Settings application for persistent configuration.
5. Verify the resulting state and report anything that remains uncertain.

## Desktop tools

When the `focaldesk` MCP server is available, use its bounded tools for outputs,
windows, workspaces, service health, rendering status, and recent logs. Treat
mutating tools as proposals: let the native confirmation flow obtain approval.
Never invent a `confirmed` argument or treat conversational assent as a tool
confirmation.

Without MCP, use read-only CLI commands first:

```sh
focaldesk-cli desktop-snapshot
focaldesk-cli focused-window-title
focaldesk-cli diagnostics --no-logs
focaldesk-cli ai providers
```

Use a log-bearing diagnostics archive only when the user understands that logs
may contain private application metadata.

## Configuration

The canonical configuration is `$XDG_CONFIG_HOME/focaldesk/settings.json`, or
`~/.config/focaldesk/settings.json` when `XDG_CONFIG_HOME` is unset.

- Do not edit generated `displays.json` while FocalDesk is running.
- Do not replace the whole settings document to change one field.
- Preserve unknown keys and create a recovery copy before a manual edit.
- Validate JSON before asking FocalDesk to reload settings.
- Use `focaldesk-cli reload-settings` only after a valid persistent change.

## Themes

Prefer the Theme Editor and portable `.fdtheme` packages. Preserve semantic
surface tokens, interaction states, contrast intent, wallpaper references, and
HDR or wide-gamut metadata. Do not flatten a theme to a small terminal palette.

## Safety boundaries

- Never retrieve or print plaintext secrets from `focald-secrets`.
- Do not enable automation, remote desktop, capture, or cloud providers without
  an explicit user request.
- Do not bypass native permission prompts or one-shot action confirmation.
- Keep recovery access to another desktop session when changing compositor,
  display-manager, DRM, or login configuration.
- Diagnose first; do not restart or terminate services unless the user asked
  for the operational change.

## Recovery

If a configuration change breaks the session, restore the recovery copy from a
known-working session, validate it, and restart only the affected user service.
For crashes or rendering failures, collect a diagnostics archive and include
the GPU, driver, backend, output topology, and reproduction steps.
