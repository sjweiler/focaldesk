use image::ImageFormat;
use image::Limits;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

pub const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_PDF_BYTES: u64 = 128 * 1024 * 1024;
pub const MAX_TEXT_BYTES: u64 = 1024 * 1024;
const MAX_TEXT_PREVIEW_BYTES: u64 = 64 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 16_384;
const MAX_IMAGE_ALLOCATION: u64 = 128 * 1024 * 1024;
const THUMBNAIL_EDGE: u32 = 256;
const MAX_CACHE_ENTRIES: usize = 512;
const MAX_CACHE_BYTES: u64 = 128 * 1024 * 1024;
const PDF_TIMEOUT: Duration = Duration::from_secs(5);
static TEMPORARY_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbnailKind {
    Image,
    Pdf,
}

#[derive(Clone, Debug)]
pub struct ThumbnailRequest {
    pub uri: String,
    pub path: PathBuf,
    pub kind: ThumbnailKind,
}

pub fn thumbnail_request(
    uri: &str,
    path: Option<&Path>,
    content_type: &str,
    size: u64,
) -> Option<ThumbnailRequest> {
    let path = path?.to_path_buf();
    if !path.is_file() {
        return None;
    }
    let kind = if matches!(
        content_type,
        "image/jpeg" | "image/jpg" | "image/png" | "image/x-png"
    ) && size <= MAX_IMAGE_BYTES
    {
        ThumbnailKind::Image
    } else if content_type == "application/pdf" && size <= MAX_PDF_BYTES {
        ThumbnailKind::Pdf
    } else {
        return None;
    };
    Some(ThumbnailRequest {
        uri: uri.to_string(),
        path,
        kind,
    })
}

pub fn is_text_preview(content_type: &str, size: u64) -> bool {
    size <= MAX_TEXT_BYTES
        && (content_type.starts_with("text/")
            || matches!(
                content_type,
                "application/json"
                    | "application/toml"
                    | "application/xml"
                    | "application/x-yaml"
                    | "application/yaml"
            ))
}

pub fn read_text_preview(path: &Path) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_TEXT_PREVIEW_BYTES)
        .read_to_end(&mut bytes)?;
    if bytes.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "binary content is not previewed as text",
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let mut preview = text.lines().take(80).collect::<Vec<_>>().join("\n");
    if bytes.len() as u64 == MAX_TEXT_PREVIEW_BYTES || text.lines().count() > 80 {
        preview.push_str("\n…");
    }
    Ok(preview)
}

pub fn generate_thumbnail(request: &ThumbnailRequest) -> io::Result<PathBuf> {
    let destination = thumbnail_path(&request.uri)?;
    if cache_is_current(&request.path, &destination) {
        return Ok(destination);
    }
    let parent = destination.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "thumbnail path has no parent")
    })?;
    prepare_cache_directory(parent)?;
    let temporary = temporary_path(parent, &destination);
    let result = match request.kind {
        ThumbnailKind::Image => generate_image_thumbnail(&request.path, &temporary),
        ThumbnailKind::Pdf => generate_pdf_thumbnail(&request.path, &temporary),
    };
    if let Err(error) = result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    fs::rename(&temporary, &destination)?;
    Ok(destination)
}

pub fn prune_cache() {
    let Ok(directory) = thumbnail_cache_directory() else {
        return;
    };
    let Ok(entries) = fs::read_dir(&directory) else {
        return;
    };
    let mut files = entries
        .filter_map(Result::ok)
        .filter(|entry| !entry.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|entry| {
            let metadata = entry.metadata().ok()?;
            metadata.is_file().then_some((
                entry.path(),
                metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                metadata.len(),
            ))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(_, modified, _)| *modified);
    let mut total = files.iter().map(|(_, _, size)| *size).sum::<u64>();
    let mut count = files.len();
    for (path, _, size) in files {
        if count <= MAX_CACHE_ENTRIES && total <= MAX_CACHE_BYTES {
            break;
        }
        if fs::remove_file(path).is_ok() {
            count = count.saturating_sub(1);
            total = total.saturating_sub(size);
        }
    }
}

fn thumbnail_cache_directory() -> io::Result<PathBuf> {
    dirs::cache_dir()
        .map(|directory| directory.join("focaldesk/thumbnails/normal"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "cache directory unavailable"))
}

fn thumbnail_path(uri: &str) -> io::Result<PathBuf> {
    Ok(thumbnail_cache_directory()?.join(format!("{:x}.png", md5::compute(uri.as_bytes()))))
}

fn prepare_cache_directory(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn cache_is_current(source: &Path, cache: &Path) -> bool {
    let Ok(source_modified) = source.metadata().and_then(|metadata| metadata.modified()) else {
        return false;
    };
    let Ok(metadata) = cache.metadata() else {
        return false;
    };
    metadata.len() > 0
        && metadata
            .modified()
            .is_ok_and(|cached_modified| cached_modified >= source_modified)
}

fn temporary_path(parent: &Path, destination: &Path) -> PathBuf {
    let id = TEMPORARY_ID.fetch_add(1, Ordering::Relaxed);
    let name = destination
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("thumbnail");
    parent.join(format!(".{name}.{}.{}.tmp.png", std::process::id(), id))
}

fn generate_image_thumbnail(source: &Path, destination: &Path) -> io::Result<()> {
    let mut reader = image::ImageReader::open(source)
        .map_err(io::Error::other)?
        .with_guessed_format()
        .map_err(io::Error::other)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
    limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
    limits.max_alloc = Some(MAX_IMAGE_ALLOCATION);
    reader.limits(limits);
    let image = reader.decode().map_err(io::Error::other)?;
    image
        .thumbnail(THUMBNAIL_EDGE, THUMBNAIL_EDGE)
        .save_with_format(destination, ImageFormat::Png)
        .map_err(io::Error::other)
}

fn generate_pdf_thumbnail(source: &Path, destination: &Path) -> io::Result<()> {
    let prefix = destination.with_extension("");
    let produced = PathBuf::from(format!("{}.png", prefix.display()));
    let mut child = Command::new("pdftoppm")
        .args([
            "-f",
            "1",
            "-l",
            "1",
            "-singlefile",
            "-png",
            "-scale-to",
            "256",
        ])
        .arg(source)
        .arg(&prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let started = std::time::Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let _ = fs::remove_file(&produced);
                return Err(io::Error::other("PDF thumbnailer failed"));
            }
            break;
        }
        if started.elapsed() >= PDF_TIMEOUT {
            let _ = child.kill();
            let _ = child.wait();
            let _ = fs::remove_file(&produced);
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "PDF thumbnailer timed out",
            ));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    if produced.metadata()?.len() > 16 * 1024 * 1024 {
        let _ = fs::remove_file(&produced);
        return Err(io::Error::other("PDF thumbnail exceeded output limit"));
    }
    if produced != destination {
        fs::rename(produced, destination)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_pdf() -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            "<< /Length 36 >>\nstream\nBT /F1 12 Tf 10 50 Td (Hi) Tj ET\nendstream".to_string(),
        ];
        let mut pdf = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", index + 1).as_bytes());
        }
        let xref = pdf.len();
        pdf.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        pdf
    }

    #[test]
    fn only_local_bounded_images_and_pdfs_are_thumbnail_eligible() {
        let root =
            std::env::temp_dir().join(format!("focaldesk-thumbnail-test-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let image = root.join("image.png");
        fs::write(&image, b"not decoded by this test").unwrap();

        assert!(
            thumbnail_request("file:///tmp/image.png", Some(&image), "image/png", 32).is_some()
        );
        assert!(thumbnail_request(
            "file:///tmp/large.png",
            Some(&image),
            "image/png",
            MAX_IMAGE_BYTES + 1
        )
        .is_none());
        assert!(thumbnail_request("smb://server/image.png", None, "image/png", 32).is_none());
        assert!(thumbnail_request(
            "file:///tmp/document.pdf",
            Some(&image),
            "application/pdf",
            32
        )
        .is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn text_preview_rejects_binary_content_and_is_bounded() {
        let path =
            std::env::temp_dir().join(format!("focaldesk-text-preview-{}", std::process::id()));
        fs::write(&path, b"hello\nworld").unwrap();
        assert_eq!(read_text_preview(&path).unwrap(), "hello\nworld");
        fs::write(&path, b"hello\0world").unwrap();
        assert_eq!(
            read_text_preview(&path).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        let _ = fs::remove_file(path);
    }

    #[test]
    fn image_thumbnail_is_scaled_within_the_preview_edge() {
        let root =
            std::env::temp_dir().join(format!("focaldesk-image-thumbnail-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.png");
        let thumbnail = root.join("thumbnail.png");
        image::DynamicImage::new_rgb8(640, 320)
            .save_with_format(&source, ImageFormat::Png)
            .unwrap();

        generate_image_thumbnail(&source, &thumbnail).unwrap();
        let result = image::open(&thumbnail).unwrap();
        assert_eq!((result.width(), result.height()), (256, 128));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn pdf_thumbnail_is_first_page_png_when_poppler_is_available() {
        if Command::new("pdftoppm")
            .arg("-v")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let root =
            std::env::temp_dir().join(format!("focaldesk-pdf-thumbnail-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.pdf");
        let thumbnail = root.join("thumbnail.tmp.png");
        fs::write(&source, minimal_pdf()).unwrap();

        generate_pdf_thumbnail(&source, &thumbnail).unwrap();
        assert!(thumbnail.metadata().unwrap().len() > 0);
        let _ = fs::remove_dir_all(root);
    }
}
