pub mod builtins;
pub mod document;
pub mod gtk;
pub mod manager;
pub mod package;
pub mod paint;
pub mod semantic;
pub mod theme;

pub const SYSTEM_DEFAULT_THEME_PATH: &str = "/usr/share/focaldesk/default.toml";

pub use theme::{
    BackgroundTheme, BackingTheme, ChromeTheme, DialogTheme, FlowTheme, FlowThemeId, IconTheme,
    TextTheme, WallpaperTheme,
};

pub use document::{
    MAX_GRADIENT_STOPS, THEME_DOCUMENT_VERSION, ThemeBacking, ThemeDocument, ThemeWallpaper,
    ThemeWallpaperFit,
};

pub use gtk::{GtkAppThemeOptions, gtk_app_css, gtk_app_prefers_dark};
pub use manager::{ActiveTheme, ThemeManager};
pub use package::{
    InstalledTheme, MAX_THEME_ASSET_BYTES, THEME_PACKAGE_VERSION, ThemePackage, ThemePackageAsset,
    theme_slug,
};
pub use paint::{
    GradientInterpolation, GradientStop, ThemeColor, ThemeColorSpace, ThemeDynamicRange,
    ThemePaint, ThemePaintIntent,
};
pub use semantic::*;
pub use theme::theme_by_name;
