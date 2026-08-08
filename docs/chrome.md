# Dynamic desktop chrome

Topbar indicators and sidebar buttons are configured in
`~/.config/focaldesk/settings.json` under the `chrome` object. The Settings
application exposes visibility and ordering under **Chrome** and reloads the
desktop immediately after each valid change.

## Built-in IDs

Topbar: network `100`, Bluetooth `101`, audio/microphone `102`, HDR `103`, and
power `104`.

Sidebar: launcher `1000`, settings `1001`, browser `1005`, terminal `1006`, and
files `1007`. Workspace buttons use IDs beginning at `1100` and remain generated
from the current workspace state.

Each region accepts:

- `order`: preferred IDs first; unlisted items are appended.
- `hidden`: IDs that should not consume a slot.
- `custom`: additional application launch items.

## Custom launch item

```json
{
  "chrome": {
    "sidebar": {
      "order": [1001, 1000, 1200, 1005, 1006, 1007],
      "hidden": [],
      "custom": [
        {
          "id": 1200,
          "icon": "browser",
          "tooltip": "Project dashboard",
          "command": "google-chrome https://example.com/dashboard",
          "enabled": true
        }
      ]
    },
    "topbar": {
      "order": [100, 101, 102, 103, 104],
      "hidden": [],
      "custom": []
    }
  }
}
```

Supported custom icon names are `launcher`, `ai_console`, `overflow`,
`settings`, `battery`, `ethernet`, `ethernet_off`, `wifi`, `wifi_off`,
`bluetooth`, `bluetooth_off`, `microphone`, `microphone_off`, `speaker`,
`speaker_off`, `power`, `hdr`, `browser`, `terminal`, `files`, `plus`, `minus`,
and `slot_1` through `slot_9`.

IDs must be unique within a region. Invalid icons, empty commands, and custom
items whose IDs collide with built-ins are ignored safely. When a region cannot
fit all visible items, its last available position becomes an overflow button
that opens Settings.

## Per-output ownership

Each connected display has a persistent `DesktopOutput` UI model owned by the
compositor's desktop state. The model owns that display's topbar, dock,
workarea, dialogs, overlays, and chrome rendering resources. Layout rebuilds
synchronize the compatibility `UiTree` projection into the model; input and
rendering then read the same output-owned component state instead of keeping a
second set of compositor-side chrome components.

The model is created when an output is registered and removed when a DRM
device disappears. This keeps hover state, layout metrics, scale, theme, and
rendering resources scoped to the correct display and prevents stale UI state
after hot-unplugging a monitor.
