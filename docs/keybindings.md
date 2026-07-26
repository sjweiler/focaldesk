# Default Keybindings

These bindings are defined by the compositor defaults. They are not currently
user-configurable, and they may change during the alpha period.

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
timestamp, and a sequence number in the filename.

## Reserved workspace-slot bindings

`Alt+1` through `Alt+9`, `Alt+Shift+1` through `Alt+Shift+9`, and `Alt+0` are
reserved for activating, assigning, and displaying workspace slots. The actions
are wired into the input map but are not implemented yet; they currently log a
warning instead of changing workspace state.

The Settings prototype may show shortcut hints that get ahead of compositor
behavior. This document follows the bindings in
`crates/focaldesk-flow/src/keybinds.rs`, which is the current source of truth.
