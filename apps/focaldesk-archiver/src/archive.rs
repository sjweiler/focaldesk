use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use std::{
    collections::HashSet,
    fs::{self, File},
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};
use zip::write::SimpleFileOptions;
type Result<T> = std::result::Result<T, String>;
fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

pub fn create(sources: &[String], destination: &str, format: &str) -> Result<String> {
    if sources.is_empty() {
        return Err("Add files or a folder first.".into());
    }
    let output = Path::new(destination);
    if output.exists() {
        return Err("Output already exists. Choose a new archive name.".into());
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = parent.canonicalize().map_err(err)?;
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    for source in sources {
        let source = Path::new(source).canonicalize().map_err(err)?;
        if source.is_dir() && parent.starts_with(&source) {
            return Err("Save the archive outside the source folder.".into());
        }
        let base = source.parent().ok_or("Cannot archive a filesystem root.")?;
        for entry in walkdir::WalkDir::new(&source).follow_links(false) {
            let entry = entry.map_err(err)?;
            if entry.file_type().is_symlink() {
                return Err(
                    "Symbolic links are not supported. Choose regular files and folders.".into(),
                );
            }
            if !entry.file_type().is_dir() && !entry.file_type().is_file() {
                return Err("Only regular files and folders are supported.".into());
            }
            let name = entry.path().strip_prefix(base).map_err(err)?.to_path_buf();
            if !names.insert(name.clone()) {
                return Err("Selected items have duplicate archive paths.".into());
            }
            entries.push((entry.path().to_path_buf(), name, entry.file_type().is_dir()));
        }
    }
    let mut temp = tempfile::NamedTempFile::new_in(&parent).map_err(err)?;
    match format {
        "zip" => {
            let mut zip = zip::ZipWriter::new(temp.as_file_mut());
            let options =
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
            for (path, name, directory) in &entries {
                let name = name
                    .to_str()
                    .ok_or("ZIP paths must be valid UTF-8.")?
                    .replace('\\', "/");
                if *directory {
                    zip.add_directory(name, options).map_err(err)?;
                } else {
                    zip.start_file(name, options).map_err(err)?;
                    io::copy(&mut File::open(path).map_err(err)?, &mut zip).map_err(err)?;
                }
            }
            zip.finish().map_err(err)?;
        }
        "tar" => write_tar(temp.as_file_mut(), &entries)?,
        "tar.gz" => {
            let mut encoder = GzEncoder::new(temp.as_file_mut(), Compression::default());
            write_tar(&mut encoder, &entries)?;
            encoder.finish().map_err(err)?;
        }
        "gz" => {
            if entries.len() != 1 || entries[0].2 {
                return Err(
                    "GZ compresses one file. Use TAR.GZ for folders or multiple files.".into(),
                );
            }
            let mut encoder = GzEncoder::new(temp.as_file_mut(), Compression::default());
            io::copy(&mut File::open(&entries[0].0).map_err(err)?, &mut encoder).map_err(err)?;
            encoder.finish().map_err(err)?;
        }
        _ => return Err("Unsupported archive format.".into()),
    }
    temp.as_file().sync_all().map_err(err)?;
    temp.persist_noclobber(output).map_err(err)?;
    Ok(destination.into())
}
fn write_tar(writer: impl Write, entries: &[(PathBuf, PathBuf, bool)]) -> Result<()> {
    let mut archive = tar::Builder::new(writer);
    for (path, name, directory) in entries {
        if *directory {
            archive.append_dir(name, path).map_err(err)?;
        } else {
            archive.append_path_with_name(path, name).map_err(err)?;
        }
    }
    archive.finish().map_err(err)
}
fn safe_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err("Archive contains an unsafe path.".into());
    }
    Ok(())
}
const LIMIT: u64 = 10 * 1024 * 1024 * 1024;
fn copy_entry(reader: impl Read, target: &Path, remaining: &mut u64) -> Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(err)?;
    }
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(target)
        .map_err(err)?;
    let count = io::copy(&mut reader.take(*remaining + 1), &mut file).map_err(err)?;
    if count > *remaining {
        return Err("Archive exceeds the 10 GB extraction limit.".into());
    }
    *remaining -= count;
    Ok(())
}
pub fn extract(source: &str, destination: &str) -> Result<String> {
    let source = Path::new(source);
    let filename = source
        .file_name()
        .and_then(|p| p.to_str())
        .ok_or("Invalid archive name.")?;
    let lower = filename.to_lowercase();
    let extension = [".tar.gz", ".tgz", ".zip", ".tar", ".gz"]
        .into_iter()
        .find(|ext| lower.ends_with(ext))
        .ok_or("Supported formats: ZIP, TAR, TAR.GZ, TGZ, GZ.")?;
    let stem = &filename[..filename.len() - extension.len()];
    if stem.is_empty() || stem == "." || stem == ".." {
        return Err("Invalid archive name.".into());
    }
    let parent = Path::new(destination).canonicalize().map_err(err)?;
    let target = parent.join(stem);
    if target.symlink_metadata().is_ok() {
        return Err(format!(
            "{} already exists. Choose another destination or rename the existing folder.",
            target.display()
        ));
    }
    let staging = tempfile::tempdir_in(&parent).map_err(err)?;
    let mut remaining = LIMIT;
    let file = File::open(source).map_err(err)?;
    match extension {
        ".zip" => {
            let mut archive = zip::ZipArchive::new(file).map_err(err)?;
            for i in 0..archive.len() {
                let mut entry = archive.by_index(i).map_err(err)?;
                let path = entry
                    .enclosed_name()
                    .ok_or("Archive contains an unsafe path.")?;
                safe_path(&path)?;
                if entry.unix_mode().is_some_and(|m| m & 0o170000 == 0o120000) {
                    return Err("Archives containing symbolic links are not supported.".into());
                }
                let out = staging.path().join(path);
                if entry.is_dir() {
                    fs::create_dir_all(out).map_err(err)?;
                } else {
                    copy_entry(&mut entry, &out, &mut remaining)?;
                }
            }
        }
        ".tar" => extract_tar(file, staging.path(), &mut remaining)?,
        ".tar.gz" | ".tgz" => extract_tar(GzDecoder::new(file), staging.path(), &mut remaining)?,
        ".gz" => copy_entry(
            GzDecoder::new(file),
            &staging.path().join(stem),
            &mut remaining,
        )?,
        _ => unreachable!(),
    }
    // Recheck before committing the fully extracted folder.
    if target.symlink_metadata().is_ok() {
        return Err("Destination folder already exists.".into());
    }
    fs::rename(staging.path(), &target).map_err(err)?;
    Ok(target.to_string_lossy().into_owned())
}
fn extract_tar(reader: impl Read, destination: &Path, remaining: &mut u64) -> Result<()> {
    let mut archive = tar::Archive::new(reader);
    for entry in archive.entries().map_err(err)? {
        let mut entry = entry.map_err(err)?;
        let path = entry.path().map_err(err)?.into_owned();
        safe_path(&path)?;
        let out = destination.join(path);
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            fs::create_dir_all(out).map_err(err)?;
        } else if kind.is_file() {
            copy_entry(&mut entry, &out, remaining)?;
        } else {
            return Err("Archive contains unsupported links or special files.".into());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip_all_formats() {
        for format in ["zip", "tar", "tar.gz", "gz"] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("hello.txt");
            fs::write(&source, "hello archive").unwrap();
            let output = temp.path().join(format!("bundle.{format}"));
            create(
                &[source.to_str().unwrap().into()],
                output.to_str().unwrap(),
                format,
            )
            .unwrap();
            let extracted =
                extract(output.to_str().unwrap(), temp.path().to_str().unwrap()).unwrap();
            let name = if format == "gz" {
                "bundle"
            } else {
                "hello.txt"
            };
            assert_eq!(
                fs::read_to_string(Path::new(&extracted).join(name)).unwrap(),
                "hello archive"
            );
            assert!(extract(output.to_str().unwrap(), temp.path().to_str().unwrap()).is_err());
            assert!(
                create(
                    &[source.to_str().unwrap().into()],
                    output.to_str().unwrap(),
                    format
                )
                .is_err()
            );
        }
    }
    #[test]
    fn rejects_traversal() {
        assert!(safe_path(Path::new("../escape")).is_err());
        assert!(safe_path(Path::new("/absolute")).is_err());
    }
    #[test]
    fn nested_folders_round_trip() {
        for format in ["zip", "tar", "tar.gz"] {
            let temp = tempfile::tempdir().unwrap();
            let folder = temp.path().join("project");
            fs::create_dir_all(folder.join("nested/empty")).unwrap();
            fs::write(folder.join("nested/file.txt"), "nested content").unwrap();
            let output = temp.path().join(format!("bundle.{format}"));
            create(
                &[folder.to_str().unwrap().into()],
                output.to_str().unwrap(),
                format,
            )
            .unwrap();
            let extracted =
                extract(output.to_str().unwrap(), temp.path().to_str().unwrap()).unwrap();
            let root = Path::new(&extracted).join("project/nested");
            assert!(root.join("empty").is_dir());
            assert_eq!(
                fs::read_to_string(root.join("file.txt")).unwrap(),
                "nested content"
            );
        }
    }
    #[test]
    fn malicious_zip_is_rejected_and_cleaned_up() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("unsafe.zip");
        let mut zip = zip::ZipWriter::new(File::create(&path).unwrap());
        zip.start_file("../escape.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"escape").unwrap();
        zip.finish().unwrap();
        assert!(extract(path.to_str().unwrap(), temp.path().to_str().unwrap()).is_err());
        assert!(!temp.path().join("unsafe").exists());
        assert!(!temp.path().join("escape.txt").exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }
}
