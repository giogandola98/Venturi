use super::*;
use std::path::{Path, PathBuf};

const FIXTURE: &str = include_str!("moduleinfo.json");

#[test]
fn parses_effect_classes_from_moduleinfo() {
    let path = Path::new("/plugins/Acme.vst3");
    let infos = parse_module_info(FIXTURE, path);
    assert_eq!(
        infos,
        Some(vec![PluginInfo {
            class_id: "56565445535447414E00000000000001".into(),
            name: "Acme EQ".into(),
            vendor: "Acme Audio".into(),
            version: "1.2.3".into(),
            path: path.to_path_buf(),
        }])
    );
}

#[test]
fn tolerates_trailing_commas() {
    let json = r#"{"Classes": [{"CID": "AA", "Category": "Audio Module Class",
        "Name": "a,]", "Sub Categories": ["Fx",],},],}"#;
    let infos = parse_module_info(json, Path::new("x.vst3"));
    let infos = infos.unwrap();
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].name, "a,]");
}

#[test]
fn scan_skips_missing_dirs() {
    assert!(scan(&[PathBuf::from("/nonexistent/venturi/vst3")]).is_empty());
}

#[test]
fn instrument_only_moduleinfo_is_trusted_without_loading() {
    let json = r#"{"Classes": [{"CID": "AA", "Category": "Audio Module Class",
        "Name": "s", "Sub Categories": ["Instrument"]}]}"#;
    assert_eq!(parse_module_info(json, Path::new("x.vst3")), Some(vec![]));
    assert_eq!(parse_module_info("not json", Path::new("x.vst3")), None);
}

#[test]
fn scan_descends_into_vendor_folders_but_not_bundles() {
    let root = std::env::temp_dir().join(format!("vv-plugin-scan-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let nested = root.join("Vendor/Acme.vst3/Contents");
    std::fs::create_dir_all(nested.join("Resources")).unwrap();
    std::fs::write(nested.join("Resources/moduleinfo.json"), FIXTURE).unwrap();
    let hidden = root.join("Vendor/Acme.vst3/Contents/Inner.vst3/Contents/Resources");
    std::fs::create_dir_all(&hidden).unwrap();
    std::fs::write(hidden.join("moduleinfo.json"), FIXTURE).unwrap();
    let infos = scan(std::slice::from_ref(&root));
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(infos.len(), 1);
    assert_eq!(infos[0].path, root.join("Vendor/Acme.vst3"));
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
