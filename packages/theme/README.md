# @focaldesk/theme

Framework-neutral CSS variables for FocalDesk desktop application frontends.
Settings, Mail, and AI Console import `@focaldesk/theme/tokens.css`; each application
can override the `--fd-*` variables to suit its surfaces. The package includes
colors, typography, spacing, radii, focus, and reduced-motion behavior. It has no
JavaScript runtime and does not change the compositor's theme configuration.

Do not treat these SDR UI colors as HDR calibration values. System theme
validation and persistence remain in Rust.
