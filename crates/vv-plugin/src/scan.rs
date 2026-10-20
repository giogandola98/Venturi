use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginInfo {
    pub class_id: String,
    pub name: String,
    pub vendor: String,
    pub version: String,
    pub path: PathBuf,
}

pub fn default_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
        dirs.extend(home.map(|h| h.join("Library/Audio/Plug-Ins/VST3")));
    } else {
        dirs.extend(home.map(|h| h.join(".vst3")));
        dirs.extend(["/usr/lib/vst3", "/usr/lib64/vst3", "/usr/local/lib/vst3"].map(PathBuf::from));
    }
    dirs
}

/// Effects only (category "Audio Module Class", subcategories without "Instrument").
pub fn scan(dirs: &[PathBuf]) -> Vec<PluginInfo> {
    bundles_in(dirs)
        .iter()
        .flat_map(|b| scan_bundle(b, None))
        .collect()
}

/// Like `scan`, but a bundle that has to be loaded is first named in `marker`,
/// so a crash inside it is blamed on the next run: that bundle is returned in
/// the second list, to be persisted by the caller and passed back as `blocklist`.
pub fn scan_with_guard(
    dirs: &[PathBuf],
    marker: &Path,
    blocklist: &[PathBuf],
) -> (Vec<PluginInfo>, Vec<PathBuf>) {
    let mut newly_blocked = Vec::new();
    if let Ok(crashed) = std::fs::read_to_string(marker) {
        let crashed = PathBuf::from(crashed.trim());
        eprintln!("VST3 scan crashed in {}, skipping it", crashed.display());
        if !blocklist.contains(&crashed) {
            newly_blocked.push(crashed);
        }
    }
    let blocked =
        |b: &Path| blocklist.contains(&b.to_path_buf()) || newly_blocked.iter().any(|n| n == b);
    let infos = bundles_in(dirs)
        .iter()
        .filter(|b| !blocked(b))
        .flat_map(|b| scan_bundle(b, Some(marker)))
        .collect();
    let _ = std::fs::remove_file(marker);
    (infos, newly_blocked)
}

fn bundles_in(dirs: &[PathBuf]) -> Vec<PathBuf> {
    let mut bundles = Vec::new();
    for dir in dirs {
        collect_bundles(dir, &mut bundles);
    }
    bundles
}

// Vendor sub-folders are common; bundles are never descended into.
fn collect_bundles(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if path.extension().is_some_and(|e| e == "vst3") {
            out.push(path);
        } else if path.is_dir() {
            collect_bundles(&path, out);
        }
    }
}

fn scan_bundle(bundle: &Path, marker: Option<&Path>) -> Vec<PluginInfo> {
    let info_path = bundle.join("Contents/Resources/moduleinfo.json");
    if let Some(infos) = std::fs::read_to_string(info_path)
        .ok()
        .and_then(|json| parse_module_info(&json, bundle))
    {
        return infos;
    }
    if let Some(marker) = marker {
        let _ = std::fs::write(marker, bundle.to_string_lossy().as_bytes());
    }
    // A broken plugin must not hide the others.
    let infos = crate::module::module(bundle)
        .map(|m| m.effect_classes(bundle))
        .unwrap_or_default();
    if let Some(marker) = marker {
        let _ = std::fs::remove_file(marker);
    }
    infos
}

#[derive(Deserialize)]
struct ModuleInfo {
    #[serde(rename = "Version", default)]
    version: String,
    #[serde(rename = "Factory Info", default)]
    factory: FactoryInfo,
    #[serde(rename = "Classes", default)]
    classes: Vec<ClassInfo>,
}

#[derive(Deserialize, Default)]
struct FactoryInfo {
    #[serde(rename = "Vendor", default)]
    vendor: String,
}

#[derive(Deserialize)]
struct ClassInfo {
    #[serde(rename = "CID")]
    cid: String,
    #[serde(rename = "Category", default)]
    category: String,
    #[serde(rename = "Name", default)]
    name: String,
    #[serde(rename = "Vendor")]
    vendor: Option<String>,
    #[serde(rename = "Version")]
    version: Option<String>,
    #[serde(rename = "Sub Categories", default)]
    sub_categories: Vec<String>,
}

pub(crate) const AUDIO_CLASS: &str = "Audio Module Class";

pub(crate) fn is_effect(category: &str, sub_categories: &[String]) -> bool {
    category == AUDIO_CLASS && !sub_categories.iter().any(|s| s.contains("Instrument"))
}

pub(crate) fn parse_module_info(json: &str, path: &Path) -> Option<Vec<PluginInfo>> {
    let info = serde_json::from_str::<ModuleInfo>(&strip_trailing_commas(json)).ok()?;
    let infos = info
        .classes
        .into_iter()
        .filter(|c| is_effect(&c.category, &c.sub_categories))
        .map(|c| PluginInfo {
            class_id: c.cid,
            name: c.name,
            vendor: c.vendor.unwrap_or_else(|| info.factory.vendor.clone()),
            version: c.version.unwrap_or_else(|| info.version.clone()),
            path: path.to_path_buf(),
        })
        .collect();
    Some(infos)
}

fn strip_trailing_commas(json: &str) -> String {
    let chars: Vec<char> = json.chars().collect();
    let mut out = String::with_capacity(json.len());
    let (mut in_str, mut escaped) = (false, false);
    for (i, &c) in chars.iter().enumerate() {
        if in_str {
            in_str = !(c == '"' && !escaped);
            escaped = c == '\\' && !escaped;
        } else if c == '"' {
            in_str = true;
        } else if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some(']' | '}')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
#[path = "tests/scan.rs"]
mod tests;
