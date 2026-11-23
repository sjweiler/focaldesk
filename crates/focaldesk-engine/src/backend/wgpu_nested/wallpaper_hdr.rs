use std::collections::{HashMap, VecDeque};
use std::io::{Cursor, Write};
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, SystemTime};

use anyhow::{ensure, Context, Result};
use base64::Engine;
use image::{DynamicImage, ImageFormat};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::mpsc;

const PROMPT: &str = r#"Analyze this wallpaper to identify regions suitable for
HDR highlight enhancement.

Identify visible light sources, illuminated atmospheric
edges, emissive objects, and specular reflections.

Divide the image into a uniform 4×4 grid.

Assign one integer score to each cell:

0 = No qualifying HDR highlight. Ordinary surfaces,
    diffuse bright objects, and shadows.

1 = Subtle specular reflection or weak illumination.

2 = Moderate reflection or localized illumination.

3 = Strong visible light source, atmospheric glow,
    or intense specular reflection.

4 = Strongest visible light source or highlight
    in the entire image.

Rules:
- Score the strongest qualifying highlight in each cell.
- Do not classify ordinary white or bright surfaces
  as light sources solely because of their brightness.
- Preserve the distinction between emissive light
  and diffuse reflected light.
- Do not boost shadows or ordinary surfaces.
- Ignore text, labels, and interface elements.
- If no qualifying highlights exist, return all zeros.
- Return exactly 16 integers in row-major order.
- Every integer must be between 0 and 4.
- If no qualifying highlights exist, return all zeros.
- Return only valid JSON, with no Markdown or explanation.

Output format:
Return a JSON object containing exactly one field named
"strength", holding 16 integers between 0 and 4
in row-major order."#;

const PREVIEW_EDGE: u32 = 768;
// Bump when preview encoding or the inference request changes.
const PREPROCESSING_VERSION: &str = "jpeg-rgb8-v1";
const QUEUE_CAPACITY: usize = 8;
const CACHE_CAPACITY: usize = 32;
type Treatment = [u8; 16];
pub(super) type Analysis = Arc<OnceLock<Option<Treatment>>>;

#[derive(Clone, Eq, Hash, PartialEq)]
struct Source {
    path: String,
    size: (u32, u32),
    modified: Option<SystemTime>,
    file_len: u64,
    model: String,
}

struct Job {
    source: Source,
    result: Weak<OnceLock<Option<Treatment>>>,
}

struct Worker {
    jobs: mpsc::Sender<Job>,
    active: HashMap<Source, Weak<OnceLock<Option<Treatment>>>>,
}

impl Worker {
    fn start() -> Option<Mutex<Self>> {
        let (jobs, receiver) = mpsc::channel(QUEUE_CAPACITY);
        std::thread::Builder::new()
            .name("wallpaper-hdr".into())
            .spawn(move || run(receiver))
            .ok()?;
        Some(Mutex::new(Self {
            jobs,
            active: HashMap::new(),
        }))
    }

    fn enqueue(&mut self, source: Source) -> Option<Analysis> {
        self.active
            .retain(|_, analysis| analysis.strong_count() != 0);
        if let Some(existing) = self.active.get(&source).and_then(Weak::upgrade) {
            return Some(existing);
        }
        let result = Arc::new(OnceLock::new());
        self.jobs
            .try_send(Job {
                source: source.clone(),
                result: Arc::downgrade(&result),
            })
            .ok()?;
        self.active.insert(source, Arc::downgrade(&result));
        Some(result)
    }
}

pub(super) fn strength() -> f32 {
    static STRENGTH: OnceLock<f32> = OnceLock::new();
    *STRENGTH.get_or_init(|| {
        std::env::var("FOCALDESK_WALLPAPER_HDR_STRENGTH")
            .ok()
            .and_then(|value| value.parse::<f32>().ok())
            .filter(|value| value.is_finite())
            .map_or(1.0, |value| value.clamp(0.0, 1.0))
    })
}

pub(super) fn start(path: &str, size: (u32, u32)) -> Option<Analysis> {
    static WORKER: OnceLock<Option<Mutex<Worker>>> = OnceLock::new();
    let metadata = std::fs::metadata(path).ok()?;
    let source = Source {
        path: path.into(),
        size,
        modified: metadata.modified().ok(),
        file_len: metadata.len(),
        model: std::env::var("FOCALDESK_WALLPAPER_HDR_MODEL")
            .unwrap_or_else(|_| "gemma4:12b".into()),
    };
    let mut worker = WORKER.get_or_init(Worker::start).as_ref()?.lock().ok()?;
    worker.enqueue(source)
}

fn run(receiver: mpsc::Receiver<Job>) {
    if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        runtime.block_on(process_jobs(receiver));
    }
}

async fn process_jobs(mut receiver: mpsc::Receiver<Job>) {
    let timeout_secs = std::env::var("FOCALDESK_WALLPAPER_HDR_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| (1..=90).contains(seconds))
        .unwrap_or(45);
    let Ok(client) = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(2))
        .timeout(Duration::from_secs(timeout_secs))
        .build()
    else {
        return;
    };
    let cache_dir = dirs::cache_dir().map(|dir| dir.join("focaldesk/wallpaper-hdr"));
    let mut cache = VecDeque::new();
    while let Some(Job { source, result }) = receiver.recv().await {
        if result.strong_count() == 0 {
            continue;
        }
        let treatment = analyze(&source, &result, &mut cache, cache_dir.as_deref(), &client).await;
        if let Some(result) = result.upgrade() {
            if let Err(error) = &treatment {
                tracing::info!(
                    target: "focaldesk",
                    path = %source.path,
                    %error,
                    "wallpaper HDR: using shader fallback"
                );
            }
            let _ = result.set(treatment.ok());
        }
    }
}

fn cache_key(file: &[u8], model: &str, prompt: &str, size: (u32, u32)) -> String {
    let mut hasher = Sha256::new();
    hasher.update(Sha256::digest(file));
    for part in [model, prompt, PREPROCESSING_VERSION] {
        hasher.update((part.len() as u64).to_le_bytes());
        hasher.update(part.as_bytes());
    }
    for value in [size.0, size.1, PREVIEW_EDGE] {
        hasher.update(value.to_le_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn save_treatment(path: &Path, treatment: Treatment) -> Result<()> {
    let directory = path.parent().context("missing treatment cache directory")?;
    std::fs::create_dir_all(directory)?;
    let mut temp = tempfile::NamedTempFile::new_in(directory)?;
    serde_json::to_writer(&mut temp, &json!({ "strength": treatment }))?;
    temp.flush()?;
    temp.persist(path)?;
    Ok(())
}

async fn analyze(
    source: &Source,
    result: &Weak<OnceLock<Option<Treatment>>>,
    cache: &mut VecDeque<(String, Treatment)>,
    cache_dir: Option<&Path>,
    client: &reqwest::Client,
) -> Result<Treatment> {
    let file = std::fs::read(&source.path)?;
    let key = cache_key(&file, &source.model, PROMPT, source.size);
    if let Some(index) = cache.iter().position(|(cached, _)| *cached == key) {
        let entry = cache.remove(index).unwrap();
        let treatment = entry.1;
        cache.push_back(entry);
        return Ok(treatment);
    }
    let cache_path = cache_dir.map(|dir| dir.join(format!("{key}.json")));
    let treatment = match cache_path
        .as_ref()
        .and_then(|path| std::fs::read_to_string(path).ok())
        .and_then(|data| parse_treatment(&data).ok())
    {
        Some(treatment) => treatment,
        None => {
            let treatment = infer(source, result, client, &file).await?;
            if let Some(path) = cache_path {
                if let Err(error) = save_treatment(&path, treatment) {
                    tracing::debug!(target: "focaldesk", path = %path.display(), %error,
                        "wallpaper HDR: could not cache treatment");
                }
            }
            treatment
        }
    };
    if cache.len() == CACHE_CAPACITY {
        cache.pop_front();
    }
    cache.push_back((key, treatment));
    Ok(treatment)
}

async fn infer(
    source: &Source,
    result: &Weak<OnceLock<Option<Treatment>>>,
    client: &reqwest::Client,
    file: &[u8],
) -> Result<Treatment> {
    let image = image::load_from_memory(file)?;
    ensure!(
        (image.width(), image.height()) == source.size,
        "wallpaper dimensions changed"
    );
    let preview = if image.width().max(image.height()) > PREVIEW_EDGE {
        image.thumbnail(PREVIEW_EDGE, PREVIEW_EDGE)
    } else {
        image
    };
    let mut encoded = Cursor::new(Vec::new());
    DynamicImage::ImageRgb8(preview.to_rgb8()).write_to(&mut encoded, ImageFormat::Jpeg)?;
    let request = client
        .post("http://127.0.0.1:11434/api/generate")
        .json(&json!({
            "model": source.model,
            "stream": false,
            "think": false,
            "keep_alive": 0,
            "format": {
                "type": "object",
                "properties": {
                    "strength": {
                        "type": "array",
                        "items": { "type": "integer", "minimum": 0, "maximum": 4 },
                        "minItems": 16,
                        "maxItems": 16
                    }
                },
                "required": ["strength"],
                "additionalProperties": false
            },
            "options": { "temperature": 0, "num_predict": 128 },
            "prompt": PROMPT,
            "images": [base64::engine::general_purpose::STANDARD.encode(encoded.into_inner())]
        }));
    let response: Value = until_obsolete(result, async {
        Ok(request.send().await?.error_for_status()?.json().await?)
    })
    .await?;
    parse_treatment(
        response["response"]
            .as_str()
            .context("missing model response")?,
    )
}

async fn until_obsolete<T>(
    result: &Weak<OnceLock<Option<Treatment>>>,
    request: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let mut request = std::pin::pin!(request);
    loop {
        ensure!(result.strong_count() != 0, "wallpaper analysis is obsolete");
        // Poll one request until either the response arrives or all outputs
        // release the analysis. Returning drops the pending HTTP future.
        if let Ok(response) =
            tokio::time::timeout(Duration::from_millis(50), request.as_mut()).await
        {
            return response;
        }
    }
}

fn parse_treatment(response: &str) -> Result<Treatment> {
    let treatment: Value = serde_json::from_str(response)?;
    ensure!(
        treatment
            .as_object()
            .is_some_and(|object| object.len() == 1),
        "invalid treatment object"
    );
    let cells = treatment["strength"]
        .as_array()
        .context("missing treatment grid")?;
    let mut strengths = [0; 16];
    ensure!(
        cells.len() == strengths.len(),
        "invalid treatment grid size"
    );
    for (strength, cell) in strengths.iter_mut().zip(cells) {
        *strength = cell
            .as_u64()
            .filter(|value| *value <= 4)
            .context("invalid treatment strength")? as u8;
    }
    Ok(strengths)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_treatment_scores() {
        let scores = [0; 16];
        assert_eq!(
            parse_treatment(&json!({ "strength": scores }).to_string()).unwrap(),
            [0; 16]
        );
        let mixed = [0, 1, 2, 3, 4, 3, 2, 1, 0, 0, 0, 0, 4, 0, 2, 0];
        assert_eq!(
            parse_treatment(&json!({ "strength": mixed }).to_string()).unwrap(),
            mixed
        );
        for bad in [
            json!(5),
            json!(-1),
            json!(1.5),
            json!(1.0),
            json!("1"),
            Value::Null,
        ] {
            let mut scores = json!(scores);
            scores[15] = bad;
            assert!(parse_treatment(&json!({ "strength": scores }).to_string()).is_err());
        }
        assert!(parse_treatment(r#"{"strength":[0]}"#).is_err());
        assert!(parse_treatment("{}").is_err());
        assert!(parse_treatment(&json!({"strength": mixed, "other": 0}).to_string()).is_err());
    }

    #[test]
    fn cache_key_changes_with_image_model_prompt_and_dimensions() {
        let key = cache_key(b"image", "model", "prompt", (2, 3));
        for other in [
            cache_key(b"IMAGE", "model", "prompt", (2, 3)),
            cache_key(b"image", "model2", "prompt", (2, 3)),
            cache_key(b"image", "model", "prompt2", (2, 3)),
            cache_key(b"image", "model", "prompt", (3, 2)),
        ] {
            assert_ne!(key, other);
        }
    }

    #[test]
    fn loads_persisted_treatment_with_empty_memory_cache() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source {
            path: dir.path().join("wallpaper.png").to_str().unwrap().into(),
            size: (1, 1),
            modified: None,
            file_len: 0,
            model: "model".into(),
        };
        image::RgbImage::new(1, 1).save(&source.path).unwrap();
        let file = std::fs::read(&source.path).unwrap();
        let key = cache_key(&file, &source.model, PROMPT, source.size);
        let path = dir.path().join(format!("{key}.json"));
        save_treatment(&path, [1; 16]).unwrap();
        save_treatment(&path, [0; 16]).unwrap();
        let result = Arc::new(OnceLock::new());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let treatment = runtime.block_on(analyze(
            &source,
            &Arc::downgrade(&result),
            &mut VecDeque::new(),
            Some(dir.path()),
            &reqwest::Client::new(),
        ));
        assert_eq!(treatment.unwrap(), [0; 16]);
    }

    #[test]
    fn full_queue_shares_existing_jobs_and_allows_retry() {
        let (jobs, mut receiver) = mpsc::channel(QUEUE_CAPACITY);
        let mut worker = Worker {
            jobs,
            active: HashMap::new(),
        };
        let mut source = Source {
            path: "wallpaper".into(),
            size: (1, 1),
            modified: None,
            file_len: 0,
            model: "model".into(),
        };
        let analyses: Vec<_> = (0..QUEUE_CAPACITY)
            .map(|index| {
                source.path = index.to_string();
                worker.enqueue(source.clone()).unwrap()
            })
            .collect();
        assert!(Arc::ptr_eq(
            analyses.last().unwrap(),
            &worker.enqueue(source.clone()).unwrap()
        ));
        source.path = "next".into();
        assert!(worker.enqueue(source.clone()).is_none());
        receiver.try_recv().unwrap();
        assert!(worker.enqueue(source).is_some());
    }

    #[test]
    fn cancels_an_in_flight_request_when_the_last_output_releases_it() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        runtime.block_on(async {
            let result = Arc::new(OnceLock::new());
            let weak = Arc::downgrade(&result);
            let request = async move {
                drop(result);
                std::future::pending::<Result<()>>().await
            };
            let error =
                tokio::time::timeout(Duration::from_secs(1), until_obsolete(&weak, request))
                    .await
                    .unwrap()
                    .unwrap_err();
            assert_eq!(error.to_string(), "wallpaper analysis is obsolete");
        });
    }
}
