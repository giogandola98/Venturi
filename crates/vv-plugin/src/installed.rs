use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Whether two scans found the same plugins (order aside): a rescan then
/// has nothing to rebuild.
pub fn same_plugins(a: &[PluginInfo], b: &[PluginInfo]) -> bool {
    fn keys(list: &[PluginInfo]) -> Vec<(&String, &PathBuf, &String)> {
        let mut keys: Vec<_> = list
            .iter()
            .map(|i| (&i.class_id, &i.path, &i.version))
            .collect();
        keys.sort();
        keys
    }
    a.len() == b.len() && keys(a) == keys(b)
}

static SCANNING: AtomicBool = AtomicBool::new(false);

/// Whether the app's background scan is still running: `installed()` may be
/// incomplete until it ends.
pub fn scanning() -> bool {
    SCANNING.load(Ordering::Acquire)
}

pub fn set_scanning(on: bool) {
    SCANNING.store(on, Ordering::Release);
}

#[cfg(test)]
#[path = "tests/installed.rs"]
mod tests;
