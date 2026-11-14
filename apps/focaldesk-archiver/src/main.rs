#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
mod archive;

#[tauri::command]
async fn choose(kind: String) -> Result<Vec<String>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let dialog = rfd::FileDialog::new();
        let paths = match kind.as_str() {
            "files" => dialog.pick_files().unwrap_or_default(),
            "folder" | "destination" => dialog.pick_folder().into_iter().collect(),
            "archive" => dialog
                .add_filter("Archives", &["zip", "tar", "gz", "tgz"])
                .pick_file()
                .into_iter()
                .collect(),
            "save" => dialog
                .set_file_name("Archive.zip")
                .save_file()
                .into_iter()
                .collect(),
            _ => return Err("Unknown picker".into()),
        };
        Ok(paths
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn process(
    mode: String,
    sources: Vec<String>,
    destination: String,
    format: String,
) -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if mode == "create" {
            archive::create(&sources, &destination, &format)
        } else if mode == "extract" && sources.len() == 1 {
            archive::extract(&sources[0], &destination)
        } else {
            Err("Choose one archive to extract.".into())
        }
    })
    .await
    .map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![choose, process])
        .run(tauri::generate_context!())
        .expect("Could not launch Focaldesk Archiver");
}
