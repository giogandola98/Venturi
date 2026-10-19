mod module;
mod scan;
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;

pub use scan::{PluginInfo, default_dirs, scan};
