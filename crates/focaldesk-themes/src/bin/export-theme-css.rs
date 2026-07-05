use focaldesk_themes::write_builtin_theme_css;
use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let output_dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/themes"));

    let written = write_builtin_theme_css(&output_dir)?;
    for path in written {
        println!("{}", path.display());
    }

    Ok(())
}
