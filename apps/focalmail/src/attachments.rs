use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};

pub const MAX_BYTES: usize = 20 * 1024 * 1024;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Attachment {
    pub name: String,
    pub size: u64,
    pub id: String,
    pub data: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct DraftAttachment {
    pub name: String,
    pub data: Vec<u8>,
}

pub fn filename(name: &str) -> String {
    let name = name.rsplit(['/', '\\']).next().unwrap_or("");
    let clean: String = name.chars().filter(|c| !c.is_control()).take(160).collect();
    if clean.trim_matches('.').trim().is_empty() {
        "attachment.bin".into()
    } else {
        clean
    }
}

pub fn import(path: &Path, remaining: usize) -> Result<DraftAttachment> {
    ensure!(std::fs::metadata(path)?.is_file(), "Choose a regular file");
    let file = std::fs::File::open(path).context("Cannot open attachment")?;
    ensure!(file.metadata()?.is_file(), "Choose a regular file");
    let mut data = Vec::new();
    file.take(remaining.min(MAX_BYTES) as u64 + 1)
        .read_to_end(&mut data)?;
    ensure!(
        data.len() <= remaining.min(MAX_BYTES),
        "Attachments may total at most 20 MiB"
    );
    Ok(DraftAttachment {
        name: filename(
            &path
                .file_name()
                .context("Missing filename")?
                .to_string_lossy(),
        ),
        data,
    })
}

pub fn save_download(directory: &Path, name: &str, data: &[u8]) -> Result<PathBuf> {
    ensure!(
        data.len() <= MAX_BYTES,
        "Attachment exceeds the 20 MiB download limit"
    );
    std::fs::create_dir_all(directory)?;
    let name = filename(name);
    for index in 0..1000 {
        let candidate = directory.join(if index == 0 {
            name.clone()
        } else {
            format!("{index}-{name}")
        });
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&candidate) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(data).and_then(|()| file.sync_all()) {
                    let _ = std::fs::remove_file(&candidate);
                    return Err(error.into());
                }
                return Ok(candidate);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error.into()),
        }
    }
    anyhow::bail!("Too many files with that name in Downloads")
}

#[derive(Default)]
pub struct Picker {
    pub open: bool,
    directory: String,
    entries: Vec<PathBuf>,
    notice: String,
}
impl Picker {
    pub fn start(&mut self) {
        self.open = true;
        self.directory = dirs::home_dir().unwrap_or_default().display().to_string();
        self.refresh();
    }
    fn refresh(&mut self) {
        match std::fs::read_dir(&self.directory) {
            Ok(entries) => {
                self.entries = entries
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .take(1000)
                    .collect();
                self.entries.sort();
                self.notice = "Showing up to 1,000 entries. Select a file to attach.".into();
            }
            Err(error) => {
                self.entries.clear();
                self.notice = error.to_string();
            }
        }
    }
    pub fn show(&mut self, ctx: &eframe::egui::Context) -> Option<PathBuf> {
        let mut chosen = None;
        let mut open = self.open;
        eframe::egui::Window::new("Choose attachment")
            .open(&mut open)
            .default_width(550.0)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.text_edit_singleline(&mut self.directory);
                    if ui.button("Go").clicked() {
                        self.refresh();
                    }
                    if ui.button("Up").clicked() {
                        if let Some(parent) = Path::new(&self.directory).parent() {
                            self.directory = parent.display().to_string();
                            self.refresh();
                        }
                    }
                });
                ui.weak(&self.notice);
                let mut enter = None;
                eframe::egui::ScrollArea::vertical()
                    .max_height(350.0)
                    .show(ui, |ui| {
                        for path in &self.entries {
                            let directory = path.is_dir();
                            let label = format!(
                                "{} {}",
                                if directory { "▸" } else { "+" },
                                path.file_name().unwrap_or_default().to_string_lossy()
                            );
                            if ui.button(label).clicked() {
                                if directory {
                                    enter = Some(path.clone());
                                } else {
                                    chosen = Some(path.clone());
                                }
                            }
                        }
                    });
                if let Some(path) = enter {
                    self.directory = path.display().to_string();
                    self.refresh();
                }
            });
        self.open = open && chosen.is_none();
        chosen
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downloads_stay_in_directory_and_never_replace_existing_files() {
        let directory =
            std::env::temp_dir().join(format!("focalmail-download-{}", rand::random::<u64>()));
        let first = save_download(&directory, "../../report.txt", b"first").unwrap();
        let second = save_download(&directory, "..\\report.txt", b"second").unwrap();
        assert_eq!(first.parent().unwrap(), directory);
        assert_ne!(first, second);
        assert_eq!(std::fs::read(&first).unwrap(), b"first");
        assert_eq!(std::fs::read(&second).unwrap(), b"second");
        assert_eq!(filename(".."), "attachment.bin");
        assert_eq!(filename("bad\r\nname.txt"), "badname.txt");
        std::fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn importing_copies_bytes_and_enforces_remaining_budget() {
        let path =
            std::env::temp_dir().join(format!("focalmail-import-{}.txt", rand::random::<u64>()));
        std::fs::write(&path, b"original").unwrap();
        assert!(import(&path, 7).is_err());
        let attachment = import(&path, 8).unwrap();
        std::fs::write(&path, b"changed").unwrap();
        assert_eq!(attachment.data, b"original");
        assert!(import(path.parent().unwrap(), MAX_BYTES).is_err());
        std::fs::remove_file(path).unwrap();
    }
}
