# Theme Editor

FocalDesk Settings includes an experimental Theme Editor for authoring a
versioned theme document, previewing it in the running compositor, and
packaging it with a wallpaper. Open **Settings → Theme Editor** to use it.

Theme documents and the editor IPC are versioned, but remain alpha interfaces.
Keep source copies of themes you want to preserve across FocalDesk upgrades.

## Editing a theme

The editor is divided into General, Surfaces, Layout & Color, Wallpaper, and
Paint pages. It can edit:

- solid, linear-gradient, and radial-gradient paint, with movable color stops;
- sRGB and Display P3 colors, including an sRGB gamut-boundary warning;
- SDR or HDR intent and HDR luminance metadata;
- semantic colors for the bar, dock, buttons, popups, and window frames;
- inherited hover, pressed, selected, focused, urgent, and disabled states;
- borders, highlights, shadows, glow, corner radius, and typography;
- bar, dock, spacing, padding, and icon metrics; and
- wallpaper fit, dimming, tint, blur, saturation, and automatic accent
  extraction.

The state-matrix preview and compositor status report contrast issues for
primary text. Treat those reports as authoring guidance: the editor does not
silently rewrite a theme's colors.

The compositor currently reports its supported editor capabilities. Semantic
colors, wallpaper processing, layout metrics, and typography metrics are
rendered by the current implementation. Gradient documents are preserved and
previewed in the editor, while the compositor currently samples them to a
single representative color; its status therefore reports gradient rendering
as unavailable.

## Save, preview, and apply

**Save** writes the source `ThemeDocument` as TOML. Saving a document is
separate from changing the running desktop.

With **Live preview** enabled, valid edits are sent to the compositor after a
short debounce. A preview is temporary. **Apply** makes the current document
the applied theme for the compositor process, **Revert Preview** returns to the
last applied runtime theme, and leaving the editor also removes an outstanding
preview. Runtime Apply does not replace Save and is not persistent across a
compositor restart.

The editor refuses invalid or unsupported document versions, non-finite or
out-of-range values, malformed gradients, and unsafe wallpaper references
before sending a theme to the compositor.

## Theme documents and packages

The current TOML document format is version 1. A document stores paint and
color-space intent, semantic tokens, and a wallpaper reference. A standalone
TOML file references the wallpaper by path, so moving either file can break
that association.

Use **Export** to create a portable `.fdtheme` package. A package embeds the
validated document and at most one PNG, JPEG, or WebP wallpaper. Wallpapers are
limited to 64 MiB, 32,768 pixels on either axis, and 100 million pixels total.
The package records the asset size and SHA-256 digest and validates both before
installation.

**Import** validates and installs a package below
`$XDG_DATA_HOME/focaldesk/themes/<theme-slug>/` (normally
`~/.local/share/focaldesk/themes/`). It writes `theme.toml` and the packaged
wallpaper without following an existing symlink at either destination.
**Uninstall** removes the installed directory for the imported theme. Save or
revert unsaved editor changes before importing another package.

## System default

When `appearance.theme` is absent or is `"Default"`, the compositor loads
`/usr/share/focaldesk/default.toml`. `just install-desktop` installs that
document and its wallpaper from `assets/themes/default.toml` and
`assets/wallpaper/focaldesk_wallpaper.png`. If the document cannot be loaded or
validated, FocalDesk falls back to the built-in Eagle theme.

Selecting Eagle, Moonbase, or Classic explicitly in Settings continues to use
the corresponding built-in theme.
