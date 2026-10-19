use super::fixture_bundle;
use crate::scan;

#[test]
fn fixture_scans_as_effect() {
    let bundle = fixture_bundle();
    let found = scan(&[bundle.parent().unwrap().to_path_buf()]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].name, "Venturi Test Gain");
    assert_eq!(found[0].vendor, "Venturi");
    assert_eq!(found[0].class_id, "56565445535447414E00000000000001");
    assert_eq!(found[0].path, bundle);
}
