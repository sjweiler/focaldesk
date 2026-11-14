//! Fixed destinations for the GTK companion. Never pass arbitrary panels through IPC.
pub fn existing_panel(panel: &str) -> Result<&'static str, String> {
    match panel {
        "appearance" => Ok("appearance"),
        "accessibility" => Ok("accessibility"),
        "theme-editor" => Ok("theme editor"),
        "network" => Ok("network"),
        "calendar" => Ok("calendar"),
        "bluetooth" => Ok("bluetooth"),
        "printers" => Ok("printers"),
        "displays" => Ok("displays"),
        "audio" | "sound" => Ok("sound"),
        "applications" => Ok("applications"),
        "chrome" => Ok("chrome"),
        "workspaces" => Ok("workspaces"),
        "mouse-touchpad" => Ok("mouse & touchpad"),
        "keyboard" => Ok("keyboard"),
        "privacy" => Ok("privacy"),
        "power" => Ok("power"),
        "debug" => Ok("debug"),
        "about" => Ok("about"),
        _ => Err("This Settings section is unavailable.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_companion_destination_matches_a_gtk_panel_and_unknown_panels_are_rejected() {
        let panels = [
            "appearance",
            "accessibility",
            "theme editor",
            "network",
            "calendar",
            "bluetooth",
            "printers",
            "displays",
            "sound",
            "applications",
            "chrome",
            "workspaces",
            "mouse & touchpad",
            "keyboard",
            "privacy",
            "power",
            "debug",
            "about",
        ];
        let ids = [
            "appearance",
            "accessibility",
            "theme-editor",
            "network",
            "calendar",
            "bluetooth",
            "printers",
            "displays",
            "audio",
            "applications",
            "chrome",
            "workspaces",
            "mouse-touchpad",
            "keyboard",
            "privacy",
            "power",
            "debug",
            "about",
        ];
        for (id, panel) in ids.into_iter().zip(panels) {
            assert_eq!(existing_panel(id).unwrap(), panel);
        }
        for invalid in ["", "--help", "../../bin/sh", "privacy; reboot"] {
            assert!(existing_panel(invalid).is_err());
        }
    }
}
