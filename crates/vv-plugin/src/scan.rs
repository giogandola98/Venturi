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
    let mut out = Vec::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        let mut bundles: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "vst3"))
            .collect();
        bundles.sort();
        for bundle in bundles {
            out.extend(scan_bundle(&bundle));
        }
    }
    out
}

fn scan_bundle(bundle: &Path) -> Vec<PluginInfo> {
    let info_path = bundle.join("Contents/Resources/moduleinfo.json");
    if let Ok(json) = std::fs::read_to_string(info_path) {
        let infos = parse_module_info(&json, bundle);
        if !infos.is_empty() {
            return infos;
        }
    }
    // A broken plugin must not hide the others.
    crate::module::module(bundle)
        .map(|m| m.effect_classes(bundle))
        .unwrap_or_default()
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

pub(crate) fn parse_module_info(json: &str, path: &Path) -> Vec<PluginInfo> {
    let Ok(info) = serde_json::from_str::<ModuleInfo>(&strip_trailing_commas(json)) else {
        return Vec::new();
    };
    info.classes
        .into_iter()
        .filter(|c| is_effect(&c.category, &c.sub_categories))
        .map(|c| PluginInfo {
            class_id: c.cid,
            name: c.name,
            vendor: c.vendor.unwrap_or_else(|| info.factory.vendor.clone()),
            version: c.version.unwrap_or_else(|| info.version.clone()),
            path: path.to_path_buf(),
        })
        .collect()
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
