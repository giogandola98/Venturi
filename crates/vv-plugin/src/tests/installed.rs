use std::path::PathBuf;

use super::*;

#[test]
fn set_installed_replaces_the_list_without_scanning() {
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
