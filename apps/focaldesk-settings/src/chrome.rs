//! Desktop chrome preferences. Custom launch items are preserved, never executed.
use crate::preferences::{read_document, reload_message, write_document};
use anyhow::{bail, Context, Result};
use focaldesk_settings_core::ChromeSettings;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeSet, path::Path};

#[derive(Serialize)]
pub struct ChromeSnapshot {
    pub chrome: ChromeSettings,
    pub path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegionChange {
    pub order: String,
    pub hidden: Vec<u32>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChromeChange {
    pub expected: ChromeSettings,
    pub topbar: RegionChange,
    pub sidebar: RegionChange,
}
#[derive(Serialize)]
pub struct ChromeResult {
    pub snapshot: ChromeSnapshot,
    pub message: String,
}
fn read(document: &Value) -> Result<ChromeSettings> {
    document
        .get("chrome")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("Chrome preferences are malformed; nothing was changed.")
        .map(Option::unwrap_or_default)
}
fn parse_order(text: &str, previous: &[u32]) -> Result<Vec<u32>> {
    if text.len() > 65536 {
        bail!("The item order is too long.");
    }
    let order = if text.trim().is_empty() {
        vec![]
    } else {
        text.split(',')
            .map(|part| {
                let part = part.trim();
                if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
                    bail!("Enter comma-separated numeric item IDs, or leave the order blank.");
                }
                part.parse::<u32>()
                    .context("Item IDs must fit in an unsigned 32-bit integer.")
            })
            .collect::<Result<Vec<_>>>()?
    };
    if order != previous
        && (order.len() > 4096 || order.iter().collect::<BTreeSet<_>>().len() != order.len())
    {
        bail!("Use each item ID once, with at most 4096 items.");
    }
    Ok(order)
}
fn save_at(path: &Path, change: ChromeChange) -> Result<ChromeSettings> {
    let mut document = read_document(path)?;
    let current = read(&document)?;
    if current != change.expected {
        bail!("Chrome preferences changed in another client. Reload before applying.");
    }
    let top_order = parse_order(&change.topbar.order, &current.topbar.order)?;
    let side_order = parse_order(&change.sidebar.order, &current.sidebar.order)?;
    // Only known switches may change hidden membership. Keep future/custom IDs.
    for (requested, previous, known) in [
        (
            &change.topbar.hidden,
            &current.topbar.hidden,
            &[100, 101, 102, 103, 104, 105, 106, 107, 108, 109][..],
        ),
        (
            &change.sidebar.hidden,
            &current.sidebar.hidden,
            &[1000, 1001, 1005, 1006, 1007][..],
        ),
    ] {
        let unknown = |items: &[u32]| {
            items
                .iter()
                .filter(|id| !known.contains(id))
                .copied()
                .collect::<Vec<_>>()
        };
        if unknown(requested) != unknown(previous)
            || requested.len() > previous.len().saturating_add(known.len())
        {
            bail!("Only the listed built-in visibility controls can be changed here.");
        }
    }
    let target = document
        .as_object_mut()
        .unwrap()
        .entry("chrome")
        .or_insert_with(|| serde_json::json!({}));
    for (name, order, hidden) in [
        ("topbar", top_order, change.topbar.hidden),
        ("sidebar", side_order, change.sidebar.hidden),
    ] {
        let region = target
            .as_object_mut()
            .context("Chrome preferences are malformed")?
            .entry(name)
            .or_insert_with(|| serde_json::json!({}));
        let region = region
            .as_object_mut()
            .context("Chrome region is malformed")?;
        region.insert("order".into(), serde_json::to_value(order)?);
        region.insert("hidden".into(), serde_json::to_value(hidden)?);
    }
    write_document(path, &document)?;
    read(&document)
}
fn snapshot_with(chrome: ChromeSettings) -> ChromeSnapshot {
    ChromeSnapshot {
        chrome,
        path: focaldesk_settings_core::settings_path()
            .display()
            .to_string(),
    }
}
pub fn snapshot() -> Result<ChromeSnapshot> {
    Ok(snapshot_with(read(&read_document(
        &focaldesk_settings_core::settings_path(),
    )?)?))
}
pub fn apply(change: ChromeChange) -> Result<ChromeResult> {
    Ok(ChromeResult {
        snapshot: snapshot_with(save_at(&focaldesk_settings_core::settings_path(), change)?),
        message: reload_message(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_invalid_orders_and_preserves_existing_duplicate_orders() {
        for text in ["1,,2", "1,1", "-1", "4294967296", "one", "1,2,"] {
            assert!(parse_order(text, &[]).is_err(), "{text}");
        }
        assert_eq!(parse_order(" 108, 100 ", &[]).unwrap(), vec![108, 100]);
        assert_eq!(parse_order(" ", &[100]).unwrap(), Vec::<u32>::new());
        assert_eq!(parse_order("1,1", &[1, 1]).unwrap(), vec![1, 1]);
    }
    #[test]
    fn saves_only_controls_and_rejects_stale_or_invalid_changes() {
        let path =
            std::env::temp_dir().join(format!("chrome-settings-{}.json", std::process::id()));
        let initial = serde_json::json!({"other":{"keep":true},"chrome":{"future":42,"sidebar":{"custom":[{"id":9000,"icon":"terminal","tooltip":"Custom","command":"do-not-run","future":"keep"}],"hidden":[9000],"future":true}}});
        std::fs::write(&path, initial.to_string()).unwrap();
        let change = || ChromeChange {
            expected: read(&initial).unwrap(),
            topbar: RegionChange {
                order: "108,100".into(),
                hidden: vec![103],
            },
            sidebar: RegionChange {
                order: "".into(),
                hidden: vec![9000, 1005],
            },
        };
        save_at(&path, change()).unwrap();
        let saved = read_document(&path).unwrap();
        assert_eq!(saved["other"], initial["other"]);
        assert_eq!(
            saved["chrome"]["sidebar"]["custom"],
            initial["chrome"]["sidebar"]["custom"]
        );
        assert_eq!(saved["chrome"]["future"], 42);
        assert_eq!(saved["chrome"]["sidebar"]["future"], true);
        assert_eq!(
            saved["chrome"]["topbar"]["order"],
            serde_json::json!([108, 100])
        );
        let bytes = std::fs::read(&path).unwrap();
        assert!(save_at(&path, change()).is_err());
        let mut invalid = change();
        invalid.expected = read(&saved).unwrap();
        invalid.sidebar.hidden.clear();
        assert!(save_at(&path, invalid).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_file(path).unwrap();
    }
}
