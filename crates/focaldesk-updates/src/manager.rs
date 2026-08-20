use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::backend::{UpdateBackendKind, detect_backend, install_updates, list_updates};
use crate::model::UpdateSnapshot;

enum Job {
    Refresh { refresh_metadata: bool },
    Install { ids: Vec<String> },
}

/// In-process update cache plus a worker thread for PackageKit/DNF.
///
/// The mutex is only held while swapping snapshots. Listing and installing run
/// off-thread so IPC `GetState` stays cheap.
pub struct UpdateManager {
    state: Arc<Mutex<UpdateSnapshot>>,
    jobs: std::sync::mpsc::Sender<Job>,
}

impl UpdateManager {
    pub fn new() -> Self {
        let backend = detect_backend();
        let state = Arc::new(Mutex::new(UpdateSnapshot {
            backend: backend.map(UpdateBackendKind::as_str).map(str::to_string),
            last_error: backend
                .is_none()
                .then(|| "No package manager found (PackageKit or DNF)".to_string()),
            ..UpdateSnapshot::default()
        }));
        let (jobs, job_rx) = std::sync::mpsc::channel();
        let worker_state = Arc::clone(&state);
        thread::Builder::new()
            .name("focaldesk-updates-worker".into())
            .spawn(move || worker_loop(worker_state, job_rx))
            .expect("spawn update worker");
        Self { state, jobs }
    }

    pub fn snapshot(&self) -> UpdateSnapshot {
        self.state
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    pub fn request_refresh(&self, refresh_metadata: bool) -> Result<(), String> {
        self.jobs
            .send(Job::Refresh { refresh_metadata })
            .map_err(|_| "update worker is not running".to_string())
    }

    pub fn request_install(&self, ids: Vec<String>) -> Result<(), String> {
        if ids.is_empty() {
            return Err("no packages selected".into());
        }
        self.jobs
            .send(Job::Install { ids })
            .map_err(|_| "update worker is not running".to_string())
    }

    pub fn request_install_all(&self) -> Result<(), String> {
        let ids = self
            .snapshot()
            .packages
            .into_iter()
            .map(|package| package.id)
            .collect::<Vec<_>>();
        self.request_install(ids)
    }
}

fn worker_loop(state: Arc<Mutex<UpdateSnapshot>>, jobs: std::sync::mpsc::Receiver<Job>) {
    while let Ok(job) = jobs.recv() {
        match job {
            Job::Refresh { refresh_metadata } => run_refresh(&state, refresh_metadata),
            Job::Install { ids } => run_install(&state, ids),
        }
    }
}

fn run_refresh(state: &Arc<Mutex<UpdateSnapshot>>, refresh_metadata: bool) {
    let backend = {
        let mut snapshot = state.lock().unwrap();
        if snapshot.checking || snapshot.installing {
            return;
        }
        snapshot.checking = true;
        snapshot.progress = Some(if refresh_metadata {
            "Refreshing package metadata…".into()
        } else {
            "Checking for updates…".into()
        });
        snapshot.last_error = None;
        snapshot.backend.clone()
    };

    let Some(kind) = backend_kind(backend.as_deref()).or_else(detect_backend) else {
        let mut snapshot = state.lock().unwrap();
        snapshot.checking = false;
        snapshot.progress = None;
        snapshot.last_error = Some("No package manager found (PackageKit or DNF)".into());
        return;
    };

    let result = list_updates(kind, refresh_metadata);
    let mut snapshot = state.lock().unwrap();
    snapshot.checking = false;
    snapshot.progress = None;
    snapshot.backend = Some(kind.as_str().to_string());
    snapshot.last_check_unix = Some(unix_now());
    match result {
        Ok(packages) => {
            snapshot.packages = packages;
            snapshot.last_error = None;
        }
        Err(err) => snapshot.last_error = Some(err),
    }
}

fn run_install(state: &Arc<Mutex<UpdateSnapshot>>, ids: Vec<String>) {
    let backend = {
        let mut snapshot = state.lock().unwrap();
        if snapshot.checking || snapshot.installing {
            return;
        }
        snapshot.installing = true;
        snapshot.progress = Some(format!("Installing {} update(s)…", ids.len()));
        snapshot.last_error = None;
        snapshot.backend.clone()
    };

    let Some(kind) = backend_kind(backend.as_deref()).or_else(detect_backend) else {
        let mut snapshot = state.lock().unwrap();
        snapshot.installing = false;
        snapshot.progress = None;
        snapshot.last_error = Some("No package manager found (PackageKit or DNF)".into());
        return;
    };

    let result = install_updates(kind, &ids);
    {
        let mut snapshot = state.lock().unwrap();
        snapshot.installing = false;
        snapshot.progress = None;
        if let Err(err) = &result {
            snapshot.last_error = Some(err.clone());
        }
    }
    if result.is_ok() {
        run_refresh(state, false);
    }
}

fn backend_kind(name: Option<&str>) -> Option<UpdateBackendKind> {
    match name? {
        "packagekit" => Some(UpdateBackendKind::PackageKit),
        "dnf5" => Some(UpdateBackendKind::Dnf5),
        "dnf" => Some(UpdateBackendKind::Dnf),
        _ => None,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
