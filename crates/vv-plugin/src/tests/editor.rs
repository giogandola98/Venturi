use std::sync::Arc;

use vv_core::{EffectId, PluginState, Vst3Effect};

use super::*;
use crate::testing::fixture_bundle;
use crate::{HostedPlugin, ProcessMode, Vst3Instance};

fn effect(class_id: &str, name: &str) -> Vst3Effect {
    Vst3Effect {
        id: EffectId(1),
        class_id: class_id.into(),
        name: name.into(),
        vendor: String::new(),
        version: String::new(),
        component_state: PluginState::new(Vec::new()),
        controller_state: PluginState::new(Vec::new()),
    }
}

#[test]
fn editor_open_fails_cleanly_without_view() {
    let e = effect("56565445535447414E00000000000001", "Venturi Test Gain");
    let instance = Vst3Instance::new(&fixture_bundle(), &e, 48_000, ProcessMode::Realtime)
        .expect("fixture instance");
    let instance: Arc<dyn HostedPlugin> = instance;
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
