# Default Keybindings

These are the compositor defaults. Common shortcuts can be changed on the
Keyboard page in Settings, and all supported actions can be overridden in
`settings.json`. Changes are applied to the running compositor after Settings
saves them. Invalid or conflicting overrides are ignored individually.

`Super` means the logo or Windows key.

## Common bindings

| Shortcut | Action | Status |
| --- | --- | --- |
| `Super+Enter` | Launch the configured terminal | Working |
| `Super+B` | Launch the configured browser | Working |
| `Super+Q` | Close the focused window | Working |
| `Super+L` | Lock the session | Working |
| `Super+V` | Open clipboard history | Working |
| `Super+Shift+V` | Toggle voice capture | Experimental |
| `Super+F7` | Focus the previous window | Working |
| `Super+F8` | Focus the next window | Working |
| `Ctrl+Alt+Tab` | Enter or advance shell accessibility focus | Working |
| `Ctrl+Alt+Shift+Tab` | Enter or reverse shell accessibility focus | Working |
| `Ctrl+Alt+D` | Toggle the application launcher | Working |
| `Super+Shift+Q` | Exit the compositor | Working; use with care |

After entering shell accessibility focus, use `Tab` or `Shift+Tab` to move,
`Enter` or `Space` to activate the focused control, and `Escape` to return
keyboard input fully to the active application. Pointer clicks also leave shell
accessibility focus.

## Direct DRM/KMS session bindings

These bindings are not registered by the nested winit backend.

| Shortcut | Action |
| --- | --- |
| `Super+Space` | Toggle the application launcher |
| `Print` | Capture the focused output |
| `Shift+Print` | Capture all outputs |

DRM screenshots are written below `~/Pictures/Screenshots` with the output name,
timestamp, and a sequence number in the filename. They are 16-bit Display P3
PNGs with an embedded matching ICC profile.

## Workspace-slot bindings

`Alt+1` through `Alt+9`, `Alt+Shift+1` through `Alt+Shift+9`, and `Alt+0` are
used for activating a workspace, moving the focused window to a workspace, and
opening the complete workspace list. A numbered binding only acts when that
workspace exists.

| Shortcut | Action |
| --- | --- |
| `Alt+1` through `Alt+9` | Switch the focused display to that workspace |
| `Alt+Shift+1` through `Alt+Shift+9` | Move the focused window to that workspace and follow it |
| `Alt+0` | Open the complete workspace list |

This document follows the bindings in
`crates/focaldesk-flow/src/keybinds.rs`, which is the current source of truth.

## Configuration syntax

Shortcut overrides live below `input.keybindings`:

```json
{
  "input": {
    "keybindings": {
      "launch_terminal": "Ctrl+Alt+T",
      "toggle_launcher": "Super+Space",
      "activate_workspace_4": "Super+4",
      "move_to_workspace_4": "Super+Shift+4"
    }
  }
}
```

Separate modifiers and the key with `+`. Supported modifier names are `Shift`,
`Ctrl`, `Alt`, and `Super`; `Control`, `Logo`, and `Meta` are accepted aliases.
Key names use XKB names such as `Return`, `Escape`, `Tab`, `Print`, `Space`,
`F1` through `F12`, or a single character.

Supported action names are `launch_terminal`, `launch_browser`, `launch_files`,
`toggle_launcher`, `close_focused`, `lock_screen`, `focus_next`,
`focus_previous`, `focus_shell_next`, `focus_shell_previous`,
`toggle_clipboard_history`, `toggle_voice_capture`, `show_workspaces`,
`take_screenshot`, `take_screenshot_all`, and `quit_compositor`. Workspace
actions use `activate_workspace_1` through `activate_workspace_9` and
`move_to_workspace_1` through `move_to_workspace_9`.
