use base64::Engine as _;
use focaldesk_ipc::{
    send_microphone_request, send_speech_request, MicrophoneIpcRequest, MicrophoneIpcResponse,
    SpeechIpcRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    path::PathBuf,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

fn config_dir() -> Result<PathBuf, String> {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .map(|p| p.join("focaldesk"))
        .ok_or("No configuration directory".into())
}
fn state_path() -> Result<PathBuf, String> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/share")))
        .map(|p| p.join("focaldesk/ai-console/state.json"))
        .ok_or("No application data directory".into())
}
fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err("Refusing to replace a symbolic link".into());
    }
    std::fs::create_dir_all(path.parent().ok_or("Invalid destination")?)
        .map_err(|e| e.to_string())?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_nanos();
    let temp = path.with_extension(format!("{}-{stamp}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    std::fs::rename(temp, path).map_err(|e| e.to_string())
}
#[derive(Default)]
pub struct DesktopState {
    pub state_lock: Mutex<()>,
    pub voices: Mutex<HashSet<String>>,
    pub backed_up: AtomicBool,
}
#[tauri::command]
pub async fn load_console_state() -> Result<Option<Value>, String> {
    tauri::async_runtime::spawn_blocking(|| match std::fs::read(state_path()?) {
        Ok(data) => serde_json::from_slice(&data)
            .map(Some)
            .map_err(|e| format!("Saved state cannot be read; it has been preserved: {e}")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn save_console_state(
    value: Value,
    state: tauri::State<'_, DesktopState>,
) -> Result<(), String> {
    if !value.get("chats").is_some_and(Value::is_array)
        || !value.get("projects").is_some_and(Value::is_array)
    {
        return Err("Invalid console state".into());
    }
    let bytes = serde_json::to_vec(&value).map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 * 1024 {
        return Err("History exceeds 64 MB; export and remove older image attachments".into());
    }
    let _lock = state.state_lock.lock().map_err(|e| e.to_string())?;
    let path = state_path()?;
    if !state.backed_up.load(Ordering::SeqCst) {
        if let Ok(previous) = std::fs::read(&path) {
            write_private(&path.with_extension("previous.json"), &previous)?;
        }
        state.backed_up.store(true, Ordering::SeqCst);
    }
    write_private(&path, &bytes)
}
#[tauri::command]
pub async fn legacy_history() -> Result<Value, String> {
    let path = config_dir()?.join("ai_console.json");
    match std::fs::read(&path) {
        Ok(data) => serde_json::from_slice(&data).map_err(|e| e.to_string()),
        Err(e) => Err(format!("Read legacy history {}: {e}", path.display())),
    }
}
#[tauri::command]
pub async fn pick_source(kind: String, multiple: bool) -> Result<Vec<String>, String> {
    let dialog = rfd::AsyncFileDialog::new();
    if kind == "folder" {
        Ok(dialog
            .pick_folder()
            .await
            .into_iter()
            .map(|f| f.path().to_string_lossy().into_owned())
            .collect())
    } else if multiple {
        Ok(dialog
            .pick_files()
            .await
            .unwrap_or_default()
            .iter()
            .map(|f| f.path().to_string_lossy().into_owned())
            .collect())
    } else {
        Ok(dialog
            .pick_file()
            .await
            .into_iter()
            .map(|f| f.path().to_string_lossy().into_owned())
            .collect())
    }
}
#[tauri::command]
pub async fn read_attachment(path: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move||{
        let path=PathBuf::from(path);let name=path.file_name().ok_or("Invalid filename")?.to_string_lossy().into_owned();
        let size=std::fs::metadata(&path).map_err(|e|e.to_string())?.len();if size>8*1024*1024{return Err("Attachments must be at most 8 MB".into());}
        let bytes=std::fs::read(&path).map_err(|e|e.to_string())?;
        let mime=if bytes.starts_with(b"\x89PNG\r\n\x1a\n"){Some("image/png")}else if bytes.starts_with(b"\xff\xd8\xff"){Some("image/jpeg")}else if bytes.starts_with(b"RIFF")&&bytes.get(8..12)==Some(b"WEBP"){Some("image/webp")}else{None};
        if let Some(mime)=mime{return Ok(json!({"name":name,"content":"","media_type":mime,"data_url":format!("data:{mime};base64,{}",base64::engine::general_purpose::STANDARD.encode(bytes))}));}
        let content=if bytes.starts_with(b"%PDF-"){pdf_extract::extract_text_from_mem(&bytes).map_err(|e|format!("Extract PDF text: {e}"))?}else{String::from_utf8(bytes).map_err(|_|"Unsupported binary attachment. Choose text, PDF, PNG, JPEG or WebP.")?};
        if content.contains('\0'){return Err("Binary attachments are not supported".into());}
        let truncated=content.chars().count()>128_000;
        Ok(json!({"name":name,"content":content.chars().take(128_000).collect::<String>(),"truncated":truncated}))
    }).await.map_err(|e|e.to_string())?
}
#[tauri::command]
pub async fn voice_start(
    wake_phrase: Option<String>,
    blocked_applications: Vec<String>,
    state: tauri::State<'_, DesktopState>,
) -> Result<MicrophoneIpcResponse, String> {
    let request = if let Some(wake_phrase) = wake_phrase.filter(|s| !s.trim().is_empty()) {
        MicrophoneIpcRequest::StartAmbient {
            requester: "focaldesk-ai-console".into(),
            wake_phrase,
            blocked_applications,
        }
    } else {
        MicrophoneIpcRequest::Start {
            requester: "focaldesk-ai-console".into(),
        }
    };
    let response = tauri::async_runtime::spawn_blocking(move || send_microphone_request(&request))
        .await
        .map_err(|e| e.to_string())??;
    if response.status == "error" {
        return Err(response.message.unwrap_or("Microphone start failed".into()));
    }
    if let Some(id) = &response.lease_id {
        state
            .voices
            .lock()
            .map_err(|e| e.to_string())?
            .insert(id.clone());
    }
    Ok(response)
}
#[tauri::command]
pub async fn voice_poll(
    lease_id: String,
    after_sequence: u64,
    state: tauri::State<'_, DesktopState>,
) -> Result<MicrophoneIpcResponse, String> {
    if !state
        .voices
        .lock()
        .map_err(|e| e.to_string())?
        .contains(&lease_id)
    {
        return Err("This console does not own that microphone lease".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        send_microphone_request(&MicrophoneIpcRequest::Poll {
            lease_id,
            after_sequence,
        })
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn voice_stop(
    lease_id: String,
    state: tauri::State<'_, DesktopState>,
) -> Result<(), String> {
    if !state
        .voices
        .lock()
        .map_err(|e| e.to_string())?
        .remove(&lease_id)
    {
        return Ok(());
    }
    tauri::async_runtime::spawn_blocking(move || {
        send_microphone_request(&MicrophoneIpcRequest::Stop {
            lease_id: Some(lease_id),
        })
        .map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn speak(text: Option<String>) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let request = text
            .map(SpeechIpcRequest::interrupt)
            .unwrap_or_else(SpeechIpcRequest::stop);
        send_speech_request(&request).and_then(|r| {
            if r.status == "error" {
                return Err(r.message.unwrap_or("Speech request failed".into()));
            }
            serde_json::to_value(r).map_err(|e| e.to_string())
        })
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn gpu_status() -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(||{
        let mut child=match Command::new("nvidia-smi").args(["--query-gpu=name,utilization.gpu,memory.used,memory.total","--format=csv,noheader,nounits"]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn(){Ok(c)=>c,Err(_)=>return Ok(json!({"gpus":[],"available":false,"message":"NVIDIA telemetry is unavailable on this system"}))};
        let start=Instant::now();loop{if child.try_wait().map_err(|e|e.to_string())?.is_some(){break;}
                if start.elapsed()>Duration::from_secs(3){let _=child.kill();let _=child.wait();return Err("GPU telemetry timed out".into());}std::thread::sleep(Duration::from_millis(30));}
        let output=child.wait_with_output().map_err(|e|e.to_string())?;
        let gpus=String::from_utf8_lossy(&output.stdout).lines().filter_map(|line|{let p=line.split(',').map(str::trim).collect::<Vec<_>>();if p.len()!=4{return None;}Some(json!({"name":p[0],"utilization_percent":p[1].parse::<f64>().ok(),"memory_used_mib":p[2].parse::<f64>().ok(),"memory_total_mib":p[3].parse::<f64>().ok()}))}).collect::<Vec<_>>();
        Ok(json!({"available":!gpus.is_empty(),"gpus":gpus}))
    }).await.map_err(|e|e.to_string())?
}
const SETTINGS_KEYS: &[&str] = &[
    "FOCALDESK_AI_PROVIDER",
    "FOCALDESK_OLLAMA_BASE_URL",
    "FOCALDESK_OLLAMA_MODEL",
    "FOCALDESK_VLLM_BASE_URL",
    "FOCALDESK_VLLM_MODEL",
    "FOCALDESK_OPENAI_MODEL",
    "FOCALDESK_ANTHROPIC_MODEL",
];
#[derive(Serialize, Deserialize)]
pub struct ProviderSettings {
    values: BTreeMap<String, String>,
    #[serde(default)]
    secrets: BTreeMap<String, String>,
    #[serde(default)]
    broker_available: bool,
}
#[tauri::command]
pub async fn provider_settings() -> Result<ProviderSettings, String> {
    let text =
        std::fs::read_to_string(config_dir()?.join("focaldesk-server.env")).unwrap_or_default();
    let mut values = BTreeMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if SETTINGS_KEYS.contains(&key) {
                values.insert(key.into(), value.trim().trim_matches('"').into());
            }
        }
    }
    Ok(ProviderSettings {
        values,
        secrets: BTreeMap::new(),
        broker_available: focaldesk_secrets_client::socket_path().is_ok_and(|path| path.exists()),
    })
}
#[tauri::command]
pub async fn save_provider_settings(settings: ProviderSettings) -> Result<(), String> {
    if settings
        .values
        .get("FOCALDESK_AI_PROVIDER")
        .is_some_and(|value| {
            !value.is_empty()
                && !matches!(value.as_str(), "ollama" | "vllm" | "openai" | "anthropic")
        })
    {
        return Err("Choose ollama, vllm, openai or anthropic as default provider".into());
    }
    for (key, value) in &settings.values {
        if !SETTINGS_KEYS.contains(&key.as_str()) || value.chars().any(char::is_control) {
            return Err("Unsupported provider setting".into());
        }
        if key.ends_with("BASE_URL") && !value.is_empty() {
            let url = url::Url::parse(value).map_err(|e| e.to_string())?;
            if !matches!(url.scheme(), "http" | "https")
                || !url.username().is_empty()
                || url.password().is_some()
            {
                return Err("Use an HTTP(S) endpoint without embedded credentials".into());
            }
        }
    }
    tauri::async_runtime::spawn_blocking(move || {
        for (provider, secret) in settings.secrets {
            if secret.is_empty() {
                continue;
            }
            let key = match provider.as_str() {
                "openai" => "ai/openai-api-key",
                "anthropic" => "ai/anthropic-api-key",
                "vllm" => "ai/vllm-api-key",
                _ => return Err("Unknown credential provider".into()),
            };
            focaldesk_secrets_client::set(key, &secret, "FocalDesk AI provider")
                .map_err(|e| format!("Credential broker: {e:#}"))?;
        }
        let path = config_dir()?.join("focaldesk-server.env");
        let old = std::fs::read_to_string(&path).unwrap_or_default();
        let mut lines = old
            .lines()
            .filter(|line| !SETTINGS_KEYS.contains(&line.split('=').next().unwrap_or("")))
            .map(str::to_string)
            .collect::<Vec<_>>();
        for (key, value) in settings.values {
            if !value.is_empty() {
                lines.push(format!(
                    "{key}=\"{}\"",
                    value.replace('\\', "\\\\").replace('"', "\\\"")
                ));
            }
        }
        write_private(&path, (lines.join("\n") + "\n").as_bytes())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn restart_ai_backend() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| {
        let output = Command::new("systemctl")
            .args(["--user", "restart", "focaldesk-server.service"])
            .output()
            .map_err(|e| e.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).into_owned());
        }
        Ok(())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub async fn read_json_file(path: String) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut file = std::fs::File::open(path).map_err(|e| e.to_string())?;
        use std::io::Read;
        let mut bytes = Vec::new();
        (&mut file)
            .take(64 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err("JSON file exceeds 64 MB".into());
        }
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}
#[tauri::command]
pub async fn export_json_file(value: Value, name: String) -> Result<bool, String> {
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_file_name(&name)
        .add_filter("JSON", &["json"])
        .save_file()
        .await
    else {
        return Ok(false);
    };
    let bytes = serde_json::to_vec_pretty(&value).map_err(|e| e.to_string())?;
    tauri::async_runtime::spawn_blocking(move || write_private(file.path(), &bytes))
        .await
        .map_err(|e| e.to_string())??;
    Ok(true)
}
