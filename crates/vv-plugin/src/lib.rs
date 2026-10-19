mod module;
mod scan;
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;

pub use scan::{PluginInfo, default_dirs, scan};
mod host;
mod instance;
mod instances;
pub use instance::{ProcessMode, Vst3Instance};
pub use instances::{HostedPlugin, Instances, MakeInstance};
