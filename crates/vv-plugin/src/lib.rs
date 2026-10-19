mod module;
mod scan;
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;

pub use scan::{PluginInfo, default_dirs, scan};
mod host;
mod instance;
pub use instance::{ProcessMode, Vst3Instance};
