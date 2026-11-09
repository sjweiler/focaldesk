#[cfg(test)]
use anyhow::Context;
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
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

#[cfg(test)]
pub fn import(path: &Path, remaining: usize) -> Result<DraftAttachment> {
    ensure!(std::fs::metadata(path)?.is_file(), "Choose a regular file");
    use std::io::Read;
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
