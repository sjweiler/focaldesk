#[cfg(feature = "desktop")]
mod desktop {
    use focaldesk_automation::{
        config,
        studio::{self, Workflow},
    };
    use serde_json::{Value, json};
    use std::sync::Mutex;

    #[derive(Default)]
    pub struct State(Mutex<()>);

    #[tauri::command]
    pub async fn studio_request(
        state: tauri::State<'_, State>,
        op: String,
        flow: Option<Workflow>,
        id: Option<String>,
    ) -> Result<Value, String> {
        // Serialize writes from windows and perform filesystem work off the UI thread.
        let state = state.inner().0.lock().map_err(|e| e.to_string())?;
        let root = config::config_path()
            .parent()
            .ok_or("Missing automation directory")?
            .join("studio");
        let result = match op.as_str() {
            "list" => {
                let mut drafts = Vec::new();
                match std::fs::read_dir(root.join("drafts")) {
                    Ok(entries) => {
                        for entry in entries {
                            let path = entry.map_err(|e| e.to_string())?.path();
                            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                                let text =
                                    std::fs::read_to_string(path).map_err(|e| e.to_string())?;
                                let draft: Workflow = serde_json::from_str(&text)
                                    .map_err(|e| format!("Cannot read draft: {e}"))?;
                                drafts.push(json!({"id":draft.id,"name":draft.name}));
                            }
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(e.to_string()),
                }
                drafts.sort_by_key(|v| v["name"].as_str().unwrap_or_default().to_owned());
                Ok(json!(drafts))
            }
            "load" => {
                let id = id.ok_or("Missing workflow ID")?;
                if !studio::valid_id(&id) {
                    return Err("Invalid workflow ID".into());
                }
                let text = std::fs::read_to_string(root.join("drafts").join(format!("{id}.json")))
                    .map_err(|e| e.to_string())?;
                let draft: Workflow = serde_json::from_str(&text).map_err(|e| e.to_string())?;
                serde_json::to_value(draft).map_err(|e| e.to_string())
            }
            "save" => {
                studio::save_draft(&root, &flow.ok_or("Missing workflow")?)?;
                Ok(json!(null))
            }
            "validate" => {
                let (schedule, lua) = studio::compile(&flow.ok_or("Missing workflow")?)?;
                Ok(json!({"schedule":schedule,"lua":lua}))
            }
            "publish" => {
                studio::publish(&root, &flow.ok_or("Missing workflow")?)?;
                Ok(json!(null))
            }
            _ => Err("Unknown Studio operation".into()),
        };
        drop(state);
        result
    }

    pub fn run() {
        tauri::Builder::default()
            .manage(State::default())
            .invoke_handler(tauri::generate_handler![studio_request])
            .run(tauri::generate_context!())
            .expect("Unable to start Automation Studio");
    }
}

fn main() {
    #[cfg(feature = "desktop")]
    desktop::run();
    #[cfg(not(feature = "desktop"))]
    eprintln!("Build with the desktop feature to open Automation Studio");
}
