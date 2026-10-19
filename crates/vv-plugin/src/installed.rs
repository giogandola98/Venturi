use std::sync::{Arc, Mutex};

use crate::{PluginInfo, default_dirs, scan};

static INSTALLED: Mutex<Option<Arc<[PluginInfo]>>> = Mutex::new(None);

/// The plugins found on this machine, scanned on first use so headless
/// renders work without the app.
pub fn installed() -> Arc<[PluginInfo]> {
    let mut slot = INSTALLED.lock().unwrap_or_else(|e| e.into_inner());
    slot.get_or_insert_with(|| scan(&default_dirs()).into())
        .clone()
}

/// Replaces the list: the app seeds it from its cache and after a rescan.
pub fn set_installed(list: Vec<PluginInfo>) {
    *INSTALLED.lock().unwrap_or_else(|e| e.into_inner()) = Some(list.into());
}

#[cfg(test)]
#[path = "tests/installed.rs"]
mod tests;
