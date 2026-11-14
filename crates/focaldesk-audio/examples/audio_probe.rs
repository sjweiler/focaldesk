//! Read-only inventory by default; microphone tests require an explicit device ID.
use focaldesk_audio::{controls, meter::InputMeter};
use std::{io::Write, time::Duration};
fn main() -> Result<(), String> {
    let mut args = std::env::args().skip(1);
    let snapshot = controls::snapshot()?;
    match args.next().as_deref() {
        None | Some("snapshot") => println!(
            "{}",
            serde_json::to_string(&snapshot).map_err(|error| error.to_string())?
        ),
        Some("meter") => {
            let id = args
                .next()
                .ok_or("meter requires an explicit microphone device ID")?;
            let mut meter = InputMeter::default();
            let initial = meter.start(controls::resolve_device(&snapshot, &id)?)?;
            println!(
                "{}",
                serde_json::to_string(&initial).map_err(|error| error.to_string())?
            );
            std::io::stdout()
                .flush()
                .map_err(|error| error.to_string())?;
            loop {
                std::thread::sleep(Duration::from_millis(100));
                let status = meter.status(initial.id)?;
                if !status.active {
                    println!(
                        "{}",
                        serde_json::to_string(&status).map_err(|error| error.to_string())?
                    );
                    break;
                }
            }
        }
        Some(_) => return Err("Use snapshot or meter <device-id>.".into()),
    }
    Ok(())
}
