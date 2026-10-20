use std::sync::Arc;

use super::*;
use crate::HostedPlugin;
use crate::testing::{fixture_effect, fixture_instance};

#[test]
fn editor_open_fails_cleanly_without_view() {
    let instance: Arc<dyn HostedPlugin> = fixture_instance(&fixture_effect(&[]));
    // The fixture's createView returns null: nothing to attach, no window made.
    let err = Editor::open(&instance, "Venturi Test Gain", Arc::new(|| {})).err();
    // On macOS the test thread is refused first: only the main thread may open windows.
    let expected = if cfg!(target_os = "macos") {
        "plugin editors open on the main thread"
    } else {
        "the plugin has no editor"
    };
    assert_eq!(err.as_deref(), Some(expected));
}
