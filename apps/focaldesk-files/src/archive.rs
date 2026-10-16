use flate2::read::MultiGzDecoder;
use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::unix::ffi::OsStringExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const MAX_ARCHIVE_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_ENTRY_BYTES: u64 = 512 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;
const MAX_PATH_DEPTH: usize = 64;
const MAX_METADATA_BYTES: u64 = 1024 * 1024;
const MAX_COMPRESSION_RATIO: u64 = 1_000;
const RATIO_ALLOWANCE: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ArchiveKind {
    Zip,
    Tar,
    TarGz,
    TarXz,
}

#[derive(Debug)]
pub(crate) struct ExtractionReport {
    pub(crate) created_paths: Vec<PathBuf>,
    pub(crate) entries: usize,
    pub(crate) bytes: u64,
}

#[derive(Default)]
struct ExtractionState {
    created_paths: Vec<PathBuf>,
    entries: usize,
    bytes: u64,
}

pub(crate) fn archive_kind(path: &Path) -> Option<ArchiveKind> {
    let name = path.file_name()?.to_str()?.to_ascii_lowercase();
    if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        Some(ArchiveKind::TarGz)
    } else if name.ends_with(".tar.xz") || name.ends_with(".txz") {
        Some(ArchiveKind::TarXz)
    } else if name.ends_with(".tar") {
        Some(ArchiveKind::Tar)
    } else if name.ends_with(".zip") {
        Some(ArchiveKind::Zip)
    } else {
        None
    }
}

pub(crate) fn default_extract_name(path: &Path) -> Option<OsString> {
    let name = path.file_name()?.to_string_lossy();
    let lowercase = name.to_ascii_lowercase();
    for suffix in [".tar.gz", ".tar.xz", ".tgz", ".txz", ".tar", ".zip"] {
        if lowercase.ends_with(suffix) {
            let keep = name.len().saturating_sub(suffix.len());
            let stem = &name[..keep];
            return Some(OsString::from(if stem.is_empty() {
                "Archive"
            } else {
                stem
            }));
        }
    }
    None
}

pub(crate) fn extract_archive(
    source: &Path,
    destination: &Path,
    cancelled: Arc<AtomicBool>,
) -> io::Result<ExtractionReport> {
    let kind = archive_kind(source)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "unsupported archive format"))?;
    let source_metadata = std::fs::symlink_metadata(source)?;
    if !source_metadata.is_file() || source_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "the archive must be a local regular file, not a symbolic link",
        ));
    }
    let archive_size = source_metadata.len();
    if archive_size > MAX_ARCHIVE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "archive exceeds the 1 GiB input limit",
        ));
    }
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the extraction folder already exists",
        ));
    }
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "destination has no parent folder",
        )
    })?;
    if !parent.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the destination parent folder is unavailable",
        ));
    }

    std::fs::create_dir(destination)?;
    let mut state = ExtractionState {
        created_paths: vec![destination.to_path_buf()],
        ..ExtractionState::default()
    };
    let result = match kind {
        ArchiveKind::Zip => extract_zip(source, destination, archive_size, &cancelled, &mut state),
        ArchiveKind::Tar => File::open(source).and_then(|file| {
            extract_tar(
                file,
                destination,
                MAX_EXTRACTED_BYTES,
                &cancelled,
                &mut state,
            )
        }),
        ArchiveKind::TarGz => File::open(source).and_then(|file| {
            extract_tar(
                MultiGzDecoder::new(file),
                destination,
                compressed_output_limit(archive_size),
                &cancelled,
                &mut state,
            )
        }),
        ArchiveKind::TarXz => extract_xz_tar(
            source,
            destination,
            compressed_output_limit(archive_size),
            &cancelled,
            &mut state,
        ),
    };

    if let Err(error) = result {
        cleanup_created_paths(&state.created_paths);
        return Err(error);
    }
    Ok(ExtractionReport {
        created_paths: state.created_paths,
        entries: state.entries,
        bytes: state.bytes,
    })
}

fn extract_zip(
    source: &Path,
    destination: &Path,
    archive_size: u64,
    cancelled: &AtomicBool,
    state: &mut ExtractionState,
) -> io::Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(source)?)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if archive.len() > MAX_ENTRIES {
        return Err(limit_error("archive contains more than 10,000 entries"));
    }
    let mut declared_total = 0_u64;
    for index in 0..archive.len() {
        check_cancelled(cancelled)?;
        let file = archive
            .by_index(index)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        if file.is_symlink() {
            return Err(unsafe_entry("symbolic links are not extracted"));
        }
        if file.size() > MAX_ENTRY_BYTES {
            return Err(limit_error("an archive entry exceeds 512 MiB"));
        }
        declared_total = declared_total.saturating_add(file.size());
        if declared_total > MAX_EXTRACTED_BYTES
            || declared_total
                > archive_size
                    .saturating_mul(MAX_COMPRESSION_RATIO)
                    .saturating_add(RATIO_ALLOWANCE)
        {
            return Err(limit_error("archive expands beyond the safe output limit"));
        }
        safe_relative_path(
            file.enclosed_name()
                .as_deref()
                .ok_or_else(|| unsafe_entry("an entry contains an unsafe path"))?,
        )?;
    }

    for index in 0..archive.len() {
        check_cancelled(cancelled)?;
        let mut entry = archive
            .by_index(index)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
        let relative = safe_relative_path(
            entry
                .enclosed_name()
                .as_deref()
                .ok_or_else(|| unsafe_entry("an entry contains an unsafe path"))?,
        )?;
        let output = destination.join(relative);
        if entry.is_dir() {
            ensure_directory(&output, destination, state)?;
        } else if entry.is_file() {
            ensure_parent_directories(&output, destination, state)?;
            let mut file = new_output_file(&output, state)?;
            let size = entry.size();
            copy_bounded(&mut entry, &mut file, size, cancelled, state)?;
        } else {
            return Err(unsafe_entry("special archive entries are not extracted"));
        }
        state.entries += 1;
    }
    Ok(())
}

fn extract_xz_tar(
    source: &Path,
    destination: &Path,
    output_limit: u64,
    cancelled: &AtomicBool,
    state: &mut ExtractionState,
) -> io::Result<()> {
    let archive = File::open(source)?;
    let mut child = Command::new("xz")
        .arg("--decompress")
        .arg("--stdout")
        .arg("--single-stream")
        .stdin(Stdio::from(archive))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| io::Error::new(error.kind(), format!("could not start xz: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("could not read xz output"))?;
    let result = extract_tar(stdout, destination, output_limit, cancelled, state);
    if result.is_err() || cancelled.load(Ordering::Acquire) {
        let _ = child.kill();
    }
    let status = child.wait()?;
    result?;
    if !status.success() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "xz could not decompress the archive",
        ));
    }
    Ok(())
}

fn extract_tar(
    mut reader: impl Read,
    destination: &Path,
    output_limit: u64,
    cancelled: &AtomicBool,
    state: &mut ExtractionState,
) -> io::Result<()> {
    let mut pending_long_name: Option<PathBuf> = None;
    let mut pending_pax_path: Option<PathBuf> = None;
    let mut records = 0_usize;
    let mut stream_bytes = 0_u64;
    loop {
        check_cancelled(cancelled)?;
        let mut header = [0_u8; 512];
        read_exact_or_invalid(&mut reader, &mut header, "truncated tar header")?;
        if header.iter().all(|byte| *byte == 0) {
            return finish_tar_stream(&mut reader, cancelled);
        }
        validate_tar_checksum(&header)?;
        let size = parse_tar_number(&header[124..136])?;
        let type_flag = header[156];
        records += 1;
        if records > MAX_ENTRIES {
            return Err(limit_error("archive contains more than 10,000 records"));
        }
        let padded_size = size.saturating_add(511) / 512 * 512;
        stream_bytes = stream_bytes
            .checked_add(512_u64.saturating_add(padded_size))
            .ok_or_else(|| limit_error("archive expands beyond the safe stream limit"))?;
        if stream_bytes > output_limit.saturating_add(16 * 1024 * 1024) {
            return Err(limit_error("archive expands beyond the safe stream limit"));
        }
        if matches!(type_flag, b'L' | b'x' | b'g') {
            if size > MAX_METADATA_BYTES {
                return Err(limit_error("tar metadata exceeds 1 MiB"));
            }
            let data = read_tar_payload(&mut reader, size, cancelled)?;
            if type_flag == b'L' {
                let end = data
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(data.len());
                pending_long_name = Some(PathBuf::from(OsString::from_vec(data[..end].to_vec())));
            } else if type_flag == b'x' {
                pending_pax_path = parse_pax_path(&data)?;
            }
            continue;
        }

        let raw_name =
            if let Some(path) = pending_pax_path.take().or_else(|| pending_long_name.take()) {
                path
            } else {
                tar_header_path(&header)?
            };

        let relative = safe_relative_path(&raw_name)?;
        if size > MAX_ENTRY_BYTES {
            return Err(limit_error("an archive entry exceeds 512 MiB"));
        }
        let output = destination.join(relative);
        match type_flag {
            0 | b'0' => {
                state.bytes = state
                    .bytes
                    .checked_add(size)
                    .ok_or_else(|| limit_error("archive expands beyond the safe output limit"))?;
                if state.bytes > output_limit {
                    return Err(limit_error("archive expands beyond the safe output limit"));
                }
                ensure_parent_directories(&output, destination, state)?;
                let mut file = new_output_file(&output, state)?;
                copy_tar_file(&mut reader, &mut file, size, cancelled)?;
            }
            b'5' => {
                ensure_directory(&output, destination, state)?;
                skip_tar_payload(&mut reader, size, cancelled)?;
            }
            _ => {
                return Err(unsafe_entry(
                    "links, devices, and special tar entries are not extracted",
                ));
            }
        }
        state.entries += 1;
    }
}

fn finish_tar_stream(reader: &mut impl Read, cancelled: &AtomicBool) -> io::Result<()> {
    let mut second_end_block = [0_u8; 512];
    read_exact_or_invalid(
        reader,
        &mut second_end_block,
        "tar archive is missing its second end marker",
    )?;
    if second_end_block.iter().any(|byte| *byte != 0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "tar archive contains data after its end marker",
        ));
    }

    let mut trailing = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_cancelled(cancelled)?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Ok(());
        }
        trailing += read as u64;
        if trailing > MAX_METADATA_BYTES || buffer[..read].iter().any(|byte| *byte != 0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "tar archive has unexpected trailing data",
            ));
        }
    }
}

fn safe_relative_path(path: &Path) -> io::Result<PathBuf> {
    let mut safe = PathBuf::new();
    let mut depth = 0_usize;
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                depth += 1;
                if depth > MAX_PATH_DEPTH {
                    return Err(limit_error("an archive path is deeper than 64 levels"));
                }
                safe.push(part);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(unsafe_entry("an entry would escape the destination folder"));
            }
        }
    }
    if safe.as_os_str().is_empty() {
        return Err(unsafe_entry("an archive entry has an empty path"));
    }
    Ok(safe)
}

fn compressed_output_limit(archive_size: u64) -> u64 {
    MAX_EXTRACTED_BYTES.min(
        archive_size
            .saturating_mul(MAX_COMPRESSION_RATIO)
            .saturating_add(RATIO_ALLOWANCE),
    )
}

fn ensure_parent_directories(
    path: &Path,
    destination: &Path,
    state: &mut ExtractionState,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| unsafe_entry("entry has no parent folder"))?;
    ensure_directory(parent, destination, state)
}

fn ensure_directory(
    path: &Path,
    destination: &Path,
    state: &mut ExtractionState,
) -> io::Result<()> {
    if path == destination {
        return match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
            Ok(_) => Err(unsafe_entry(
                "the extraction folder is no longer a safe directory",
            )),
            Err(error) => Err(error),
        };
    }
    let relative = path
        .strip_prefix(destination)
        .map_err(|_| unsafe_entry("entry would escape the destination folder"))?;
    let mut current = destination.to_path_buf();
    for component in relative.components() {
        let Component::Normal(part) = component else {
            return Err(unsafe_entry("entry contains an unsafe path"));
        };
        current.push(part);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!(
                        "{} already exists and is not a safe folder",
                        current.display()
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                std::fs::create_dir(&current)?;
                state.created_paths.push(current.clone());
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn new_output_file(path: &Path, state: &mut ExtractionState) -> io::Result<File> {
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    state.created_paths.push(path.to_path_buf());
    Ok(file)
}

fn copy_bounded(
    reader: &mut impl Read,
    writer: &mut impl io::Write,
    expected: u64,
    cancelled: &AtomicBool,
    state: &mut ExtractionState,
) -> io::Result<()> {
    let mut copied = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_cancelled(cancelled)?;
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        copied += read as u64;
        if copied > expected || copied > MAX_ENTRY_BYTES {
            return Err(limit_error("an entry expanded beyond its declared size"));
        }
        writer.write_all(&buffer[..read])?;
    }
    if copied != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "an entry did not match its declared size",
        ));
    }
    state.bytes = state
        .bytes
        .checked_add(copied)
        .ok_or_else(|| limit_error("archive expands beyond the safe output limit"))?;
    if state.bytes > MAX_EXTRACTED_BYTES {
        return Err(limit_error("archive expands beyond the 2 GiB output limit"));
    }
    Ok(())
}

fn copy_tar_file(
    reader: &mut impl Read,
    writer: &mut impl io::Write,
    size: u64,
    cancelled: &AtomicBool,
) -> io::Result<()> {
    let mut remaining = size;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        check_cancelled(cancelled)?;
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = reader.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated tar entry",
            ));
        }
        writer.write_all(&buffer[..read])?;
        remaining -= read as u64;
    }
    skip_padding(reader, size, cancelled)
}

fn read_tar_payload(
    reader: &mut impl Read,
    size: u64,
    cancelled: &AtomicBool,
) -> io::Result<Vec<u8>> {
    let length = usize::try_from(size)
        .map_err(|_| limit_error("tar metadata is too large for this system"))?;
    let mut data = vec![0_u8; length];
    read_exact_or_invalid(reader, &mut data, "truncated tar metadata")?;
    skip_padding(reader, size, cancelled)?;
    Ok(data)
}

fn skip_tar_payload(reader: &mut impl Read, size: u64, cancelled: &AtomicBool) -> io::Result<()> {
    let mut remaining = size;
    let mut buffer = [0_u8; 64 * 1024];
    while remaining > 0 {
        check_cancelled(cancelled)?;
        let requested = usize::try_from(remaining.min(buffer.len() as u64)).unwrap_or(buffer.len());
        let read = reader.read(&mut buffer[..requested])?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated tar entry",
            ));
        }
        remaining -= read as u64;
    }
    skip_padding(reader, size, cancelled)
}

fn skip_padding(reader: &mut impl Read, size: u64, cancelled: &AtomicBool) -> io::Result<()> {
    check_cancelled(cancelled)?;
    let padding = (512 - (size % 512)) % 512;
    let mut buffer = [0_u8; 512];
    read_exact_or_invalid(
        reader,
        &mut buffer[..padding as usize],
        "truncated tar padding",
    )
}

fn tar_header_path(header: &[u8; 512]) -> io::Result<PathBuf> {
    let name = nul_terminated(&header[0..100]);
    let prefix = nul_terminated(&header[345..500]);
    let mut bytes = Vec::new();
    if !prefix.is_empty() {
        bytes.extend_from_slice(prefix);
        bytes.push(b'/');
    }
    bytes.extend_from_slice(name);
    if bytes.is_empty() {
        return Err(unsafe_entry("tar entry has no path"));
    }
    Ok(PathBuf::from(OsString::from_vec(bytes)))
}

fn parse_pax_path(data: &[u8]) -> io::Result<Option<PathBuf>> {
    let mut offset = 0_usize;
    let mut path = None;
    while offset < data.len() {
        let space = data[offset..]
            .iter()
            .position(|byte| *byte == b' ')
            .map(|index| offset + index)
            .ok_or_else(|| unsafe_entry("invalid PAX metadata"))?;
        let length = std::str::from_utf8(&data[offset..space])
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .ok_or_else(|| unsafe_entry("invalid PAX record length"))?;
        if length == 0 || offset + length > data.len() || space + 1 >= offset + length {
            return Err(unsafe_entry("invalid PAX record"));
        }
        let record = &data[space + 1..offset + length];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        if let Some(value) = record.strip_prefix(b"path=") {
            path = Some(PathBuf::from(OsString::from_vec(value.to_vec())));
        }
        offset += length;
    }
    Ok(path)
}

fn parse_tar_number(field: &[u8]) -> io::Result<u64> {
    if field.first().is_some_and(|byte| byte & 0x80 != 0) {
        return Err(unsafe_entry("base-256 tar sizes are not supported"));
    }
    let text = std::str::from_utf8(field)
        .map_err(|_| unsafe_entry("invalid tar size"))?
        .trim_matches(['\0', ' ']);
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|_| unsafe_entry("invalid tar size"))
}

fn validate_tar_checksum(header: &[u8; 512]) -> io::Result<()> {
    let expected = parse_tar_number(&header[148..156])?;
    let actual = header
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if (148..156).contains(&index) {
                b' '
            } else {
                *byte
            }
        })
        .map(u64::from)
        .sum::<u64>();
    if actual != expected {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "tar header checksum is invalid",
        ));
    }
    Ok(())
}

fn nul_terminated(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len())]
}

fn read_exact_or_invalid(
    reader: &mut impl Read,
    buffer: &mut [u8],
    message: &'static str,
) -> io::Result<()> {
    reader.read_exact(buffer).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            io::Error::new(io::ErrorKind::InvalidData, message)
        } else {
            error
        }
    })
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "extraction cancelled",
        ))
    } else {
        Ok(())
    }
}

fn cleanup_created_paths(paths: &[PathBuf]) {
    for path in paths.iter().rev() {
        let result = match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                std::fs::remove_dir(path)
            }
            Ok(_) => std::fs::remove_file(path),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        };
        let _ = result;
    }
}

fn unsafe_entry(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn limit_error(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time")
            .as_nanos();
        std::env::temp_dir().join(format!("focaldesk-archive-{label}-{unique}"))
    }

    #[test]
    fn recognizes_supported_archive_names() {
        assert_eq!(
            archive_kind(Path::new("bundle.zip")),
            Some(ArchiveKind::Zip)
        );
        assert_eq!(
            archive_kind(Path::new("bundle.tar")),
            Some(ArchiveKind::Tar)
        );
        assert_eq!(
            archive_kind(Path::new("bundle.TAR.GZ")),
            Some(ArchiveKind::TarGz)
        );
        assert_eq!(
            archive_kind(Path::new("bundle.txz")),
            Some(ArchiveKind::TarXz)
        );
        assert_eq!(archive_kind(Path::new("bundle.7z")), None);
        assert_eq!(
            default_extract_name(Path::new("bundle.tar.gz")).as_deref(),
            Some(std::ffi::OsStr::new("bundle"))
        );
    }

    #[test]
    fn rejects_paths_that_escape_or_are_too_deep() {
        assert!(safe_relative_path(Path::new("../escape")).is_err());
        assert!(safe_relative_path(Path::new("/absolute")).is_err());
        let deep = std::iter::repeat_n("folder", MAX_PATH_DEPTH + 1).collect::<PathBuf>();
        assert!(safe_relative_path(&deep).is_err());
    }

    #[test]
    fn safely_extracts_zip_and_cleans_cancelled_work() {
        let root = unique_dir("zip");
        std::fs::create_dir_all(&root).expect("create root");
        let archive_path = root.join("bundle.zip");
        let file = File::create(&archive_path).expect("create zip");
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("folder/note.txt", zip::write::SimpleFileOptions::default())
            .expect("start zip file");
        writer.write_all(b"hello").expect("write zip file");
        writer.finish().expect("finish zip");

        let destination = root.join("bundle");
        let report = extract_archive(
            &archive_path,
            &destination,
            Arc::new(AtomicBool::new(false)),
        )
        .expect("extract zip");
        assert_eq!(report.entries, 1);
        assert_eq!(report.bytes, 5);
        assert_eq!(
            std::fs::read_to_string(destination.join("folder/note.txt")).expect("read output"),
            "hello"
        );

        let cancelled_destination = root.join("cancelled");
        let error = extract_archive(
            &archive_path,
            &cancelled_destination,
            Arc::new(AtomicBool::new(true)),
        )
        .expect_err("cancel extraction");
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert!(!cancelled_destination.exists());

        let unsafe_archive = root.join("unsafe.zip");
        let file = File::create(&unsafe_archive).expect("create unsafe zip");
        let mut writer = zip::ZipWriter::new(file);
        writer
            .start_file("../escape.txt", zip::write::SimpleFileOptions::default())
            .expect("start unsafe zip file");
        writer.write_all(b"escape").expect("write unsafe zip file");
        writer.finish().expect("finish unsafe zip");
        let unsafe_destination = root.join("unsafe");
        assert!(extract_archive(
            &unsafe_archive,
            &unsafe_destination,
            Arc::new(AtomicBool::new(false)),
        )
        .is_err());
        assert!(!unsafe_destination.exists());
        assert!(!root.join("escape.txt").exists());
        std::fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn extracts_tar_gzip_and_xz_without_links() {
        use std::os::unix::fs::symlink;

        let root = unique_dir("tar");
        let input = root.join("input");
        std::fs::create_dir_all(input.join("folder")).expect("create input");
        std::fs::write(input.join("folder/note.txt"), "hello tar").expect("write input");

        for (name, compression) in [("bundle.tar.gz", "--gzip"), ("bundle.tar.xz", "--xz")] {
            let archive = root.join(name);
            let status = Command::new("tar")
                .current_dir(&input)
                .arg("--create")
                .arg(compression)
                .arg("--file")
                .arg(&archive)
                .arg("--")
                .arg("folder")
                .status()
                .expect("run tar");
            assert!(status.success());

            let destination = root.join(format!("out-{name}"));
            let report = extract_archive(&archive, &destination, Arc::new(AtomicBool::new(false)))
                .expect("extract tar archive");
            assert!(report.entries >= 2);
            assert_eq!(
                std::fs::read_to_string(destination.join("folder/note.txt"))
                    .expect("read extracted file"),
                "hello tar"
            );
        }

        symlink("note.txt", input.join("folder/note-link")).expect("create archive symlink");
        let linked_archive = root.join("linked.tar");
        let status = Command::new("tar")
            .current_dir(&input)
            .arg("--create")
            .arg("--file")
            .arg(&linked_archive)
            .arg("--")
            .arg("folder")
            .status()
            .expect("create linked tar");
        assert!(status.success());
        let linked_destination = root.join("linked");
        assert!(extract_archive(
            &linked_archive,
            &linked_destination,
            Arc::new(AtomicBool::new(false)),
        )
        .is_err());
        assert!(!linked_destination.exists());
        std::fs::remove_dir_all(root).expect("remove root");
    }

    #[test]
    fn rejects_tar_traversal_and_removes_partial_output() {
        let root = unique_dir("unsafe-tar");
        std::fs::create_dir_all(&root).expect("create root");
        let archive = root.join("unsafe.tar");
        let mut file = File::create(&archive).expect("create tar");
        let mut header = [0_u8; 512];
        header[..13].copy_from_slice(b"../escape.txt");
        write_tar_octal(&mut header[100..108], 0o644);
        write_tar_octal(&mut header[108..116], 0);
        write_tar_octal(&mut header[116..124], 0);
        write_tar_octal(&mut header[124..136], 0);
        write_tar_octal(&mut header[136..148], 0);
        header[148..156].fill(b' ');
        header[156] = b'0';
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        let checksum = header.iter().map(|byte| u64::from(*byte)).sum();
        write_tar_octal(&mut header[148..156], checksum);
        file.write_all(&header).expect("write tar header");
        file.write_all(&[0_u8; 1024]).expect("write tar ending");
        drop(file);

        let destination = root.join("unsafe");
        assert!(
            extract_archive(&archive, &destination, Arc::new(AtomicBool::new(false)),).is_err()
        );
        assert!(!destination.exists());
        assert!(!root.join("escape.txt").exists());
        std::fs::remove_dir_all(root).expect("remove root");
    }

    fn write_tar_octal(field: &mut [u8], value: u64) {
        field.fill(b'0');
        let value = format!("{value:o}");
        let start = field.len().saturating_sub(value.len() + 1);
        field[start..start + value.len()].copy_from_slice(value.as_bytes());
        field[field.len() - 1] = 0;
    }
}
