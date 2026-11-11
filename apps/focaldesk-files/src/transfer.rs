use std::collections::HashMap;
use std::fs::{File, FileTimes, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{symlink, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

const COPY_BUFFER_BYTES: usize = 1024 * 1024;
const MAX_TRANSFER_ENTRIES: usize = 100_000;
const MAX_TRANSFER_DEPTH: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TransferOperation {
    Copy,
    Move,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConflictPolicy {
    Replace,
    Skip,
    KeepBoth,
}

#[derive(Debug)]
pub(crate) struct TransferRequest {
    pub sources: Vec<PathBuf>,
    pub target_dir: PathBuf,
    pub operation: TransferOperation,
    pub conflict_policy: ConflictPolicy,
}

#[derive(Debug, Clone)]
pub(crate) enum TransferUndo {
    Created {
        path: PathBuf,
    },
    Moved {
        source: PathBuf,
        destination: PathBuf,
    },
    ReplacedCopy {
        destination: PathBuf,
        backup: PathBuf,
    },
    ReplacedMove {
        source: PathBuf,
        destination: PathBuf,
        backup: PathBuf,
    },
}

#[derive(Debug)]
pub(crate) struct TransferReport {
    pub completed: usize,
    pub skipped: usize,
    pub bytes: u64,
    pub cancelled: bool,
    pub errors: Vec<String>,
    pub undo: Vec<TransferUndo>,
}

#[derive(Debug)]
pub(crate) enum TransferEvent {
    Prepared {
        total_bytes: u64,
        total_items: usize,
    },
    Progress {
        completed_bytes: u64,
        current: String,
    },
    Finished(io::Result<TransferReport>),
}

#[derive(Debug)]
struct ScannedRoot {
    path: PathBuf,
    bytes: u64,
}

struct CopyContext<'a> {
    cancelled: &'a AtomicBool,
    sender: &'a mpsc::Sender<TransferEvent>,
    completed_bytes: u64,
    hard_links: HashMap<(u64, u64), PathBuf>,
}

impl CopyContext<'_> {
    fn check_cancelled(&self) -> io::Result<()> {
        if self.cancelled.load(Ordering::Acquire) {
            Err(io::Error::new(
                io::ErrorKind::Interrupted,
                "file transfer cancelled",
            ))
        } else {
            Ok(())
        }
    }

    fn advance(&mut self, bytes: u64, source: &Path) {
        self.completed_bytes = self.completed_bytes.saturating_add(bytes);
        let _ = self.sender.send(TransferEvent::Progress {
            completed_bytes: self.completed_bytes,
            current: source.to_string_lossy().into_owned(),
        });
    }
}

pub(crate) fn run_transfer(
    request: TransferRequest,
    cancelled: Arc<AtomicBool>,
    sender: mpsc::Sender<TransferEvent>,
) {
    let result = run_transfer_inner(request, &cancelled, &sender);
    let _ = sender.send(TransferEvent::Finished(result));
}

fn run_transfer_inner(
    request: TransferRequest,
    cancelled: &AtomicBool,
    sender: &mpsc::Sender<TransferEvent>,
) -> io::Result<TransferReport> {
    if request.sources.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "no files were selected",
        ));
    }
    if !request.target_dir.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "the transfer destination is unavailable",
        ));
    }

    let target_metadata = std::fs::metadata(&request.target_dir)?;
    let target_device = target_metadata.dev();
    let target_canonical = request.target_dir.canonicalize()?;
    let mut entry_count = 0;
    let mut roots = Vec::with_capacity(request.sources.len());
    let mut total_bytes = 0_u64;
    for source in &request.sources {
        check_cancelled(cancelled)?;
        let metadata = std::fs::symlink_metadata(source)?;
        if metadata.is_dir() && target_canonical.starts_with(source.canonicalize()?) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("cannot copy {} into itself", source.display()),
            ));
        }
        let bytes = scan_entry(source, 0, &mut entry_count, cancelled)?;
        total_bytes = total_bytes
            .checked_add(bytes)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "transfer size overflow"))?;
        roots.push(ScannedRoot {
            path: source.clone(),
            bytes,
        });
    }
    let _ = sender.send(TransferEvent::Prepared {
        total_bytes,
        total_items: entry_count,
    });

    let mut report = TransferReport {
        completed: 0,
        skipped: 0,
        bytes: 0,
        cancelled: false,
        errors: Vec::new(),
        undo: Vec::new(),
    };
    let mut context = CopyContext {
        cancelled,
        sender,
        completed_bytes: 0,
        hard_links: HashMap::new(),
    };

    for root in roots {
        context.hard_links.clear();
        if cancelled.load(Ordering::Acquire) {
            report.cancelled = true;
            break;
        }
        let Some(name) = root.path.file_name() else {
            report.errors.push(format!(
                "{} has no transferable file name",
                root.path.display()
            ));
            continue;
        };
        let destination = request.target_dir.join(name);
        if destination == root.path {
            report.skipped += 1;
            context.advance(root.bytes, &root.path);
            continue;
        }
        if request.conflict_policy == ConflictPolicy::Skip && path_exists(&destination) {
            report.skipped += 1;
            context.advance(root.bytes, &root.path);
            continue;
        }

        let stage = unique_sibling(&destination, "partial")?;
        let source_metadata = match std::fs::symlink_metadata(&root.path) {
            Ok(metadata) => metadata,
            Err(error) => {
                report
                    .errors
                    .push(format!("{}: {error}", root.path.display()));
                continue;
            }
        };
        let same_device_move =
            request.operation == TransferOperation::Move && source_metadata.dev() == target_device;
        let staged = if same_device_move {
            match std::fs::rename(&root.path, &stage) {
                Ok(()) => {
                    context.advance(root.bytes, &root.path);
                    Ok(())
                }
                Err(error) => Err(error),
            }
        } else {
            copy_entry(&root.path, &stage, &mut context)
        };
        if let Err(error) = staged {
            let _ = remove_path(&stage);
            if error.kind() == io::ErrorKind::Interrupted {
                report.cancelled = true;
                break;
            }
            report
                .errors
                .push(format!("{}: {error}", root.path.display()));
            continue;
        }
        if cancelled.load(Ordering::Acquire) {
            if same_device_move {
                let _ = std::fs::rename(&stage, &root.path);
            } else {
                let _ = remove_path(&stage);
            }
            report.cancelled = true;
            break;
        }

        let commit = commit_stage(&stage, &destination, request.conflict_policy);
        let (final_destination, backup) = match commit {
            Ok(CommitResult::Committed {
                destination,
                backup,
            }) => (destination, backup),
            Ok(CommitResult::Skipped) => {
                if same_device_move {
                    let _ = std::fs::rename(&stage, &root.path);
                } else {
                    let _ = remove_path(&stage);
                }
                report.skipped += 1;
                continue;
            }
            Err(error) => {
                if same_device_move {
                    let _ = std::fs::rename(&stage, &root.path);
                } else {
                    let _ = remove_path(&stage);
                }
                report
                    .errors
                    .push(format!("{}: {error}", root.path.display()));
                continue;
            }
        };

        if request.operation == TransferOperation::Move && !same_device_move {
            if let Err(error) = remove_path(&root.path) {
                report.errors.push(format!(
                    "copied {} but could not remove the source: {error}",
                    root.path.display()
                ));
                report.completed += 1;
                report.bytes = report.bytes.saturating_add(root.bytes);
                if let Some(backup) = backup {
                    report.undo.push(TransferUndo::ReplacedCopy {
                        destination: final_destination,
                        backup,
                    });
                } else {
                    report.undo.push(TransferUndo::Created {
                        path: final_destination,
                    });
                }
                continue;
            }
        }

        let undo = match (request.operation, backup) {
            (TransferOperation::Copy, Some(backup)) => TransferUndo::ReplacedCopy {
                destination: final_destination,
                backup,
            },
            (TransferOperation::Copy, None) => TransferUndo::Created {
                path: final_destination,
            },
            (TransferOperation::Move, Some(backup)) => TransferUndo::ReplacedMove {
                source: root.path,
                destination: final_destination,
                backup,
            },
            (TransferOperation::Move, None) => TransferUndo::Moved {
                source: root.path,
                destination: final_destination,
            },
        };
        report.undo.push(undo);
        report.completed += 1;
        report.bytes = report.bytes.saturating_add(root.bytes);
    }
    Ok(report)
}

fn check_cancelled(cancelled: &AtomicBool) -> io::Result<()> {
    if cancelled.load(Ordering::Acquire) {
        Err(io::Error::new(
            io::ErrorKind::Interrupted,
            "file transfer cancelled",
        ))
    } else {
        Ok(())
    }
}

fn scan_entry(
    path: &Path,
    depth: usize,
    entry_count: &mut usize,
    cancelled: &AtomicBool,
) -> io::Result<u64> {
    check_cancelled(cancelled)?;
    if depth > MAX_TRANSFER_DEPTH {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("transfer exceeds {MAX_TRANSFER_DEPTH} folder levels"),
        ));
    }
    *entry_count += 1;
    if *entry_count > MAX_TRANSFER_ENTRIES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("transfer exceeds {MAX_TRANSFER_ENTRIES} filesystem entries"),
        ));
    }
    let metadata = std::fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return Ok(0);
    }
    if file_type.is_file() {
        return Ok(metadata.len());
    }
    if !file_type.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("special files cannot be transferred: {}", path.display()),
        ));
    }
    let mut total = 0_u64;
    let mut entries = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        total = total
            .checked_add(scan_entry(
                &entry.path(),
                depth + 1,
                entry_count,
                cancelled,
            )?)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "transfer size overflow"))?;
    }
    Ok(total)
}

fn copy_entry(source: &Path, destination: &Path, context: &mut CopyContext<'_>) -> io::Result<()> {
    context.check_cancelled()?;
    let metadata = std::fs::symlink_metadata(source)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        symlink(std::fs::read_link(source)?, destination)?;
        return Ok(());
    }
    if file_type.is_file() {
        let identity = (metadata.dev(), metadata.ino());
        if metadata.nlink() > 1 {
            if let Some(existing) = context.hard_links.get(&identity) {
                std::fs::hard_link(existing, destination)?;
                context.advance(metadata.len(), source);
                return Ok(());
            }
        }
        let mut input = File::open(source)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(destination)?;
        let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
        loop {
            context.check_cancelled()?;
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output.write_all(&buffer[..read])?;
            context.advance(read as u64, source);
        }
        output.sync_all()?;
        apply_metadata(&output, &metadata)?;
        if metadata.nlink() > 1 {
            context
                .hard_links
                .insert(identity, destination.to_path_buf());
        }
        return Ok(());
    }
    if file_type.is_dir() {
        std::fs::create_dir(destination)?;
        let mut entries = std::fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            copy_entry(&entry.path(), &destination.join(entry.file_name()), context)?;
        }
        let directory = File::open(destination)?;
        apply_metadata(&directory, &metadata)?;
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("special files cannot be transferred: {}", source.display()),
    ))
}

fn apply_metadata(file: &File, metadata: &std::fs::Metadata) -> io::Result<()> {
    let times = FileTimes::new()
        .set_accessed(metadata.accessed()?)
        .set_modified(metadata.modified()?);
    file.set_times(times)?;
    file.set_permissions(std::fs::Permissions::from_mode(metadata.mode() & 0o7777))
}

enum CommitResult {
    Committed {
        destination: PathBuf,
        backup: Option<PathBuf>,
    },
    Skipped,
}

fn commit_stage(
    stage: &Path,
    requested_destination: &Path,
    conflict_policy: ConflictPolicy,
) -> io::Result<CommitResult> {
    if !path_exists(requested_destination) {
        std::fs::rename(stage, requested_destination)?;
        return Ok(CommitResult::Committed {
            destination: requested_destination.to_path_buf(),
            backup: None,
        });
    }
    match conflict_policy {
        ConflictPolicy::Skip => Ok(CommitResult::Skipped),
        ConflictPolicy::KeepBoth => {
            let destination = available_destination(requested_destination)?;
            std::fs::rename(stage, &destination)?;
            Ok(CommitResult::Committed {
                destination,
                backup: None,
            })
        }
        ConflictPolicy::Replace => {
            let backup = unique_sibling(requested_destination, "replaced")?;
            std::fs::rename(requested_destination, &backup)?;
            if let Err(error) = std::fs::rename(stage, requested_destination) {
                let rollback = std::fs::rename(&backup, requested_destination);
                return Err(match rollback {
                    Ok(()) => error,
                    Err(rollback) => io::Error::other(format!(
                        "{error}; restoring the replaced item also failed: {rollback}"
                    )),
                });
            }
            Ok(CommitResult::Committed {
                destination: requested_destination.to_path_buf(),
                backup: Some(backup),
            })
        }
    }
}

fn available_destination(path: &Path) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    let name = path.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
    })?;
    let stem = Path::new(name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Item");
    let extension = Path::new(name).extension().and_then(|value| value.to_str());
    for index in 1..=10_000 {
        let candidate_name = match extension {
            Some(extension) if !extension.is_empty() => {
                format!("{stem} copy {index}.{extension}")
            }
            _ => format!("{stem} copy {index}"),
        };
        let candidate = parent.join(candidate_name);
        if !path_exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not find an available destination name",
    ))
}

fn unique_sibling(path: &Path, suffix: &str) -> io::Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..1000 {
        let candidate = parent.join(format!(
            ".focaldesk-transfer-{}-{stamp}-{attempt}-{suffix}",
            std::process::id()
        ));
        if !path_exists(&candidate) {
            return Ok(candidate);
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not allocate a private transfer path",
    ))
}

fn path_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

pub(crate) fn remove_path(path: &Path) -> io::Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn test_dir(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "focaldesk-transfer-{label}-{}-{unique}",
            std::process::id()
        ))
    }

    fn run(request: TransferRequest, cancelled: Arc<AtomicBool>) -> TransferReport {
        let (sender, receiver) = mpsc::channel();
        run_transfer(request, cancelled, sender);
        receiver
            .into_iter()
            .find_map(|event| match event {
                TransferEvent::Finished(result) => Some(result.expect("transfer result")),
                _ => None,
            })
            .expect("finished event")
    }

    #[test]
    fn copies_symlinks_hardlinks_modes_and_timestamps_without_following_links() {
        let root = test_dir("metadata");
        let source_parent = root.join("source");
        let target = root.join("target");
        let folder = source_parent.join("folder");
        std::fs::create_dir_all(&folder).expect("source folder");
        std::fs::create_dir_all(&target).expect("target folder");
        let file = folder.join("data.bin");
        std::fs::write(&file, vec![7_u8; 32 * 1024]).expect("source file");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640))
            .expect("source mode");
        std::fs::hard_link(&file, folder.join("data-link.bin")).expect("hard link");
        symlink("data.bin", folder.join("symbolic")).expect("symbolic link");

        let report = run(
            TransferRequest {
                sources: vec![folder],
                target_dir: target.clone(),
                operation: TransferOperation::Copy,
                conflict_policy: ConflictPolicy::KeepBoth,
            },
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(report.completed, 1);
        let copied = target.join("folder");
        assert_eq!(
            std::fs::metadata(copied.join("data.bin"))
                .expect("copied metadata")
                .permissions()
                .mode()
                & 0o7777,
            0o640
        );
        assert_eq!(
            std::fs::metadata(copied.join("data.bin"))
                .expect("first hardlink")
                .ino(),
            std::fs::metadata(copied.join("data-link.bin"))
                .expect("second hardlink")
                .ino()
        );
        assert!(std::fs::symlink_metadata(copied.join("symbolic"))
            .expect("copied symlink")
            .file_type()
            .is_symlink());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn cancellation_removes_partial_output_and_preserves_source() {
        let root = test_dir("cancel");
        let source = root.join("large.bin");
        let target = root.join("target");
        std::fs::create_dir_all(&target).expect("target folder");
        std::fs::write(&source, vec![3_u8; 64 * COPY_BUFFER_BYTES]).expect("large source");
        let cancelled = Arc::new(AtomicBool::new(false));
        let (sender, receiver) = mpsc::channel();
        let cancelled_for_thread = cancelled.clone();
        let request = TransferRequest {
            sources: vec![source.clone()],
            target_dir: target.clone(),
            operation: TransferOperation::Copy,
            conflict_policy: ConflictPolicy::KeepBoth,
        };
        let worker = std::thread::spawn(move || {
            run_transfer(request, cancelled_for_thread, sender);
        });
        while let Ok(event) = receiver.recv() {
            match event {
                TransferEvent::Progress { .. } => cancelled.store(true, Ordering::Release),
                TransferEvent::Finished(result) => {
                    assert!(result.expect("cancel report").cancelled);
                    break;
                }
                TransferEvent::Prepared { .. } => {}
            }
        }
        worker.join().expect("worker join");
        assert!(source.exists());
        assert!(!target.join("large.bin").exists());
        assert!(std::fs::read_dir(&target)
            .expect("target entries")
            .next()
            .is_none());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn preflight_rejects_special_files_before_creating_output() {
        let root = test_dir("special-file");
        let source = root.join("ordinary.txt");
        let target = root.join("target");
        std::fs::create_dir_all(&target).expect("target folder");
        std::fs::write(&source, "ordinary").expect("ordinary file");
        let (sender, receiver) = mpsc::channel();
        run_transfer(
            TransferRequest {
                sources: vec![source, PathBuf::from("/dev/null")],
                target_dir: target.clone(),
                operation: TransferOperation::Copy,
                conflict_policy: ConflictPolicy::KeepBoth,
            },
            Arc::new(AtomicBool::new(false)),
            sender,
        );
        let result = receiver
            .into_iter()
            .find_map(|event| match event {
                TransferEvent::Finished(result) => Some(result),
                _ => None,
            })
            .expect("finished event");
        assert!(result.is_err());
        assert!(std::fs::read_dir(&target)
            .expect("target entries")
            .next()
            .is_none());
        std::fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn conflict_policies_replace_skip_and_keep_both() {
        let root = test_dir("conflicts");
        let source_dir = root.join("source");
        let target = root.join("target");
        std::fs::create_dir_all(&source_dir).expect("source folder");
        std::fs::create_dir_all(&target).expect("target folder");
        let source = source_dir.join("note.txt");
        std::fs::write(&source, "new").expect("source");
        std::fs::write(target.join("note.txt"), "old").expect("existing");

        let skip = run(
            TransferRequest {
                sources: vec![source.clone()],
                target_dir: target.clone(),
                operation: TransferOperation::Copy,
                conflict_policy: ConflictPolicy::Skip,
            },
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(skip.skipped, 1);
        assert_eq!(
            std::fs::read_to_string(target.join("note.txt")).expect("old"),
            "old"
        );

        let keep = run(
            TransferRequest {
                sources: vec![source.clone()],
                target_dir: target.clone(),
                operation: TransferOperation::Copy,
                conflict_policy: ConflictPolicy::KeepBoth,
            },
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(keep.completed, 1);
        assert_eq!(
            std::fs::read_to_string(target.join("note copy 1.txt")).expect("kept copy"),
            "new"
        );

        std::fs::write(&source, "replacement").expect("new replacement");
        let replace = run(
            TransferRequest {
                sources: vec![source],
                target_dir: target.clone(),
                operation: TransferOperation::Copy,
                conflict_policy: ConflictPolicy::Replace,
            },
            Arc::new(AtomicBool::new(false)),
        );
        assert_eq!(replace.completed, 1);
        assert_eq!(
            std::fs::read_to_string(target.join("note.txt")).expect("replacement"),
            "replacement"
        );
        assert!(matches!(replace.undo[0], TransferUndo::ReplacedCopy { .. }));
        for undo in replace.undo {
            if let TransferUndo::ReplacedCopy { backup, .. } = undo {
                remove_path(&backup).expect("remove backup");
            }
        }
        std::fs::remove_dir_all(root).expect("cleanup");
    }
}
