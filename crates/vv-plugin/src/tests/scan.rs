use super::*;
use std::path::{Path, PathBuf};

const FIXTURE: &str = include_str!("moduleinfo.json");

#[test]
fn parses_effect_classes_from_moduleinfo() {
    let path = Path::new("/plugins/Acme.vst3");
    let infos = parse_module_info(FIXTURE, path);
    assert_eq!(
        infos,
        vec![PluginInfo {
            class_id: "56565445535447414E00000000000001".into(),
            name: "Acme EQ".into(),
            vendor: "Acme Audio".into(),
            version: "1.2.3".into(),
            path: path.to_path_buf(),
        }]
    );
}

#[test]
fn tolerates_trailing_commas() {
    let json = r#"{"Classes": [{"CID": "AA", "Category": "Audio Module Class",
        "Name": "a,]", "Sub Categories": ["Fx",],},],}"#;
    let infos = parse_module_info(json, Path::new("x.vst3"));
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].name, "a,]");
}

#[test]
fn scan_skips_missing_dirs() {
    assert!(scan(&[PathBuf::from("/nonexistent/venturi/vst3")]).is_empty());
}

#[test]
#[ignore = "needs plugins installed on this machine"]
fn scans_installed_plugins() {
    let infos = scan(&default_dirs());
    if cfg!(target_os = "macos") {
        assert!(
            infos.iter().any(|i| i.name.starts_with("TDR Nova")),
            "{infos:?}"
        );
    }
}
