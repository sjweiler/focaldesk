use std::process::Command;
use url::Url;

#[tauri::command]
fn open_provider(url: &str) -> Result<(), String> {
    let url = Url::parse(url).map_err(|error| error.to_string())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("Only HTTPS provider links are supported.".into());
    }
    Command::new("xdg-open")
        .arg(url.as_str())
        .spawn()
        .map(|mut child| {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
        })
        .map_err(|error| error.to_string())
}

fn main() {
    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![open_provider])
        .run(tauri::generate_context!())
        .expect("failed to run FocalDesk Entertainment");
}
