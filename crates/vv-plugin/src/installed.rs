use std::sync::{Arc, Mutex};

use crate::PluginInfo;

static INSTALLED: Mutex<Option<Arc<[PluginInfo]>>> = Mutex::new(None);

/// The seeded plugin list, empty until `set_installed`. Never scans: a lazy
/// scan here would stall a render under the lock and bypass the crash guard.
pub fn installed() -> Arc<[PluginInfo]> {
    let slot = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    slot.clone().unwrap_or_else(|| Arc::from([]))
}

/// Replaces the list: the app seeds it from its cache and after a rescan.
pub fn set_installed(list: Vec<PluginInfo>) {
    *INSTALLED.lock().unwrap_or_else(|e| e.into_inner()) = Some(list.into());
}

#[cfg(test)]
#[path = "tests/installed.rs"]
mod tests;
