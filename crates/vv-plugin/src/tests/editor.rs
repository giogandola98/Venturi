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

#[test]
fn a_refused_constrained_size_is_requested_once_then_the_window_size_is_accepted() {
    let mut refused = None;
    let first = resize_decision((1000, 700), (900, 700), &mut refused);
    assert_eq!(first.request, Some((900, 700)));
    assert_eq!(first.notify, (900, 700));
    let again = resize_decision((1000, 700), (900, 700), &mut refused);
    assert_eq!(again.request, None);
    assert_eq!(again.notify, (1000, 700));
}

#[test]
fn a_honoured_request_forgets_the_refusal() {
    let mut refused = None;
    resize_decision((1000, 700), (900, 700), &mut refused);
    let honoured = resize_decision((900, 700), (900, 700), &mut refused);
    assert_eq!(honoured.request, None);
    assert_eq!(refused, None);
    let later = resize_decision((1000, 700), (900, 700), &mut refused);
    assert_eq!(later.request, Some((900, 700)));
}

#[test]
fn a_different_window_size_after_a_refusal_is_constrained_again() {
    let mut refused = None;
    resize_decision((1000, 700), (900, 700), &mut refused);
    let other = resize_decision((1200, 700), (900, 700), &mut refused);
    assert_eq!(other.request, Some((900, 700)));
}
