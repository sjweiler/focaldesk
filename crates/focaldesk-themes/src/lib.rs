pub mod builtins;
pub mod document;
pub mod export;
pub mod gtk;
pub mod manager;
pub mod package;
pub mod paint;
pub mod semantic;
pub mod theme;

pub const SYSTEM_DEFAULT_THEME_PATH: &str = "/usr/share/focaldesk/default.toml";

pub use theme::{
    BackgroundTheme, ChromeTheme, DialogTheme, FlowTheme, FlowThemeId, IconTheme, TextTheme,
    WallpaperTheme,
};

pub use document::{ThemeDocument, ThemeWallpaper, ThemeWallpaperFit, THEME_DOCUMENT_VERSION};

pub use export::{builtin_theme_css, write_builtin_theme_css};
pub use gtk::{gtk_app_css, gtk_app_prefers_dark, GtkAppThemeOptions};
pub use manager::{ActiveTheme, ThemeManager};
pub use package::{
    theme_slug, InstalledTheme, ThemePackage, ThemePackageAsset, MAX_THEME_ASSET_BYTES,
    THEME_PACKAGE_VERSION,
};
pub use paint::{
    GradientInterpolation, GradientStop, ThemeColor, ThemeColorSpace, ThemeDynamicRange,
    ThemePaint, ThemePaintIntent,
};
pub use semantic::*;
pub use theme::theme_by_name;
