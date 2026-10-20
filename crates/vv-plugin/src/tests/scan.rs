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

fn guard_dirs(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("vv-guard-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let plugins = root.join("plugins");
    std::fs::create_dir_all(&plugins).unwrap();
    (root.clone(), plugins, root.join("scan.marker"))
}

#[test]
fn a_clean_guarded_scan_finds_the_plugin_and_leaves_no_marker() {
    let (root, plugins, marker) = guard_dirs("clean");
    let bundle = crate::testing::fixture_bundle();
    let copy = plugins.join(bundle.file_name().unwrap());
    copy_dir(&bundle, &copy);
    let (infos, blocked) = scan_with_guard(&[plugins], &marker, &[]);
    let marker_left = marker.exists();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(infos.len(), 1, "{infos:?}");
    assert!(blocked.is_empty());
    assert!(!marker_left);
}

#[test]
fn a_leftover_marker_blocks_that_bundle_and_it_is_not_loaded() {
    let (root, plugins, marker) = guard_dirs("crashed");
    let bundle = crate::testing::fixture_bundle();
    let bad = plugins.join("Bad.vst3");
    copy_dir(&bundle, &bad);
    std::fs::write(&marker, bad.to_string_lossy().as_bytes()).unwrap();
    let (infos, blocked) = scan_with_guard(std::slice::from_ref(&plugins), &marker, &[]);
    let marker_left = marker.exists();
    let _ = std::fs::remove_dir_all(&root);
    assert!(infos.is_empty(), "{infos:?}");
    assert_eq!(blocked, vec![bad]);
    assert!(!marker_left);
}

#[test]
fn an_already_blocked_bundle_is_skipped_without_being_reported_again() {
    let (root, plugins, marker) = guard_dirs("blocked");
    let bad = plugins.join("Bad.vst3");
    copy_dir(&crate::testing::fixture_bundle(), &bad);
    let (infos, blocked) = scan_with_guard(&[plugins], &marker, std::slice::from_ref(&bad));
    let _ = std::fs::remove_dir_all(&root);
    assert!(infos.is_empty());
    assert!(blocked.is_empty());
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let dest = to.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), dest).unwrap();
        }
    }
}

#[cfg(unix)]
#[test]
fn a_non_utf8_path_survives_the_marker_exactly() {
    use std::os::unix::ffi::OsStringExt;
    let (root, _plugins, marker) = guard_dirs("bytes");
    let weird = PathBuf::from(std::ffi::OsString::from_vec(
        b" /p/\xff caf\xe9 .vst3 ".to_vec(),
    ));
    std::fs::write(&marker, super::path_to_bytes(&weird)).unwrap();
    let crashed = take_crashed(&marker);
    let marker_left = marker.exists();
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(crashed, Some(weird));
    assert!(!marker_left);
}

#[test]
fn a_path_with_surrounding_spaces_is_not_trimmed() {
    let (root, _plugins, marker) = guard_dirs("spaces");
    let odd = PathBuf::from(" /p/a b .vst3 ");
    std::fs::write(&marker, super::path_to_bytes(&odd)).unwrap();
    let crashed = take_crashed(&marker);
    let _ = std::fs::remove_dir_all(&root);
    assert_eq!(crashed, Some(odd));
}
