mod module;
mod scan;
#[cfg(any(test, feature = "testing"))]
#[doc(hidden)]
pub mod testing;

pub use scan::{PluginInfo, default_dirs, scan};
mod editor;
mod host;
mod installed;
mod instance;
mod instances;
pub use editor::Editor;
pub use host::Waker;
pub use installed::{installed, set_installed};
pub use instance::{ProcessMode, Vst3Instance};
pub use instances::{HostedPlugin, Instances, MakeInstance};
