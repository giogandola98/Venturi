use std::path::PathBuf;

use super::*;

/// One test: the list is process-global, so the unseeded state is only
/// observable before seeding.
#[test]
fn installed_is_empty_until_seeded_and_never_scans() {
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
}
