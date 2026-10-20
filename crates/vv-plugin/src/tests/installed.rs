use std::path::PathBuf;

use super::*;
use crate::{default_dirs, scan};

/// One test: the list is process-global, so the unseeded state is only
/// observable before seeding.
#[test]
fn installed_never_scans_and_returns_what_was_seeded() {
    let home = std::env::temp_dir().join(format!("vv-installed-home-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    // SAFETY: no other test in this binary reads HOME.
    unsafe { std::env::set_var("HOME", &home) };
    let plugin_dir = default_dirs()
        .into_iter()
        .find(|d| d.starts_with(&home))
        .unwrap();
    std::fs::create_dir_all(&plugin_dir).unwrap();
    let bundle = crate::testing::fixture_bundle();
    std::os::unix::fs::symlink(&bundle, plugin_dir.join(bundle.file_name().unwrap())).unwrap();
    assert!(
        !scan(&default_dirs()).is_empty(),
        "setup: a scan would find the fixture"
    );

    assert!(installed().is_empty());

    let info = PluginInfo {
        class_id: "AB".into(),
        name: "X".into(),
        vendor: "V".into(),
        version: "1".into(),
        path: PathBuf::from("/x.vst3"),
    };
    set_installed(vec![info.clone()]);
    assert_eq!(&*installed(), &[info]);
    let _ = std::fs::remove_dir_all(&home);
}
