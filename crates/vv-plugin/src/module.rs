use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use vst3::ComPtr;
use vst3::Steinberg::{
    IPluginFactory, IPluginFactory2, IPluginFactory2Trait, IPluginFactoryTrait, PClassInfo,
    PClassInfo2, PFactoryInfo, kResultOk,
};

use crate::scan::{AUDIO_CLASS, PluginInfo, is_effect};

pub(crate) struct Module {
    // Never dropped: unloading plugin code is unsafe in practice.
    _library: libloading::Library,
    factory: ComPtr<IPluginFactory>,
}

// SAFETY: VST3 factories are required to be callable from any thread.
unsafe impl Send for Module {}
unsafe impl Sync for Module {}

static MODULES: Mutex<Option<HashMap<PathBuf, Arc<Module>>>> = Mutex::new(None);

/// Loaded once per path and kept.
pub(crate) fn module(path: &Path) -> Result<Arc<Module>, String> {
    let mut guard = MODULES.lock().unwrap_or_else(|e| e.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if let Some(m) = map.get(path) {
        return Ok(m.clone());
    }
    let m = Arc::new(load(path)?);
    map.insert(path.to_path_buf(), m.clone());
    Ok(m)
}

fn binary_path(bundle: &Path) -> Result<PathBuf, String> {
    let stem = bundle
        .file_stem()
        .ok_or_else(|| format!("bad bundle path {}", bundle.display()))?;
    let mut bin = bundle.join("Contents");
    if cfg!(target_os = "macos") {
        bin.push("MacOS");
        bin.push(stem);
    } else {
        bin.push(format!("{}-linux", std::env::consts::ARCH));
        bin.push(format!("{}.so", stem.to_string_lossy()));
    }
    Ok(bin)
}

fn load(bundle: &Path) -> Result<Module, String> {
    let bin = binary_path(bundle)?;
    let (library, handle) = open(&bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    enter(&library, handle, bundle)?;
    // SAFETY: GetPluginFactory is the VST3 module entry point with this signature.
    let raw = unsafe {
        let get = library
            .get::<unsafe extern "C" fn() -> *mut c_void>(b"GetPluginFactory\0")
            .map_err(|e| e.to_string())?;
        get()
    };
    // SAFETY: a non-null result is an owned reference to the factory.
    let factory = unsafe { ComPtr::from_raw(raw.cast::<IPluginFactory>()) }
        .ok_or("GetPluginFactory returned null")?;
    Ok(Module {
        _library: library,
        factory,
    })
}

// ModuleEntry wants the raw dlopen handle, so open through the unix layer.
fn open(bin: &Path) -> Result<(libloading::Library, *mut c_void), libloading::Error> {
    use libloading::os::unix::{Library, RTLD_LOCAL, RTLD_NOW};
    // SAFETY: loading a plugin runs its initialisers; that is what hosting means.
    let lib = unsafe { Library::open(Some(bin), RTLD_NOW | RTLD_LOCAL)? };
    let handle = lib.into_raw();
    // SAFETY: handle was just produced by into_raw.
    Ok((unsafe { Library::from_raw(handle) }.into(), handle))
}

#[cfg(target_os = "macos")]
fn enter(library: &libloading::Library, _handle: *mut c_void, bundle: &Path) -> Result<(), String> {
    use objc2_core_foundation::{CFBundle, CFURL};
    let url = CFURL::from_directory_path(bundle).ok_or("bad bundle url")?;
    let cf_bundle = CFBundle::new(None, Some(&url)).ok_or("CFBundleCreate failed")?;
    // SAFETY: bundleEntry takes the bundle ref; the bundle stays alive with the module.
    let ok = unsafe {
        let Ok(entry) =
            library.get::<unsafe extern "C" fn(*const CFBundle) -> bool>(b"bundleEntry\0")
        else {
            return Ok(());
        };
        entry(&*cf_bundle)
    };
    std::mem::forget(cf_bundle);
    ok.then_some(()).ok_or_else(|| "bundleEntry failed".into())
}

#[cfg(not(target_os = "macos"))]
fn enter(library: &libloading::Library, handle: *mut c_void, _bundle: &Path) -> Result<(), String> {
    // SAFETY: ModuleEntry is the VST3 Linux entry point with this signature.
    let ok = unsafe {
        let Ok(entry) = library.get::<unsafe extern "C" fn(*mut c_void) -> bool>(b"ModuleEntry\0")
        else {
            return Ok(());
        };
        entry(handle)
    };
    ok.then_some(()).ok_or_else(|| "ModuleEntry failed".into())
}

fn c_str(chars: &[c_char]) -> String {
    // SAFETY: the SDK fixed-size strings are NUL-terminated within their array.
    let bytes = unsafe { CStr::from_ptr(chars.as_ptr()) };
    bytes.to_string_lossy().into_owned()
}

fn hex(cid: &[c_char; 16]) -> String {
    cid.iter().map(|b| format!("{:02X}", *b as u8)).collect()
}

impl Module {
    pub(crate) fn effect_classes(&self, path: &Path) -> Vec<PluginInfo> {
        let factory = &self.factory;
        // SAFETY: plain out-parameter calls on a live factory with zeroed structs.
        unsafe {
            let mut finfo: PFactoryInfo = std::mem::zeroed();
            let factory_vendor = if factory.getFactoryInfo(&mut finfo) == kResultOk {
                c_str(&finfo.vendor)
            } else {
                String::new()
            };
            let factory2 = factory.cast::<IPluginFactory2>();
            let mut out = Vec::new();
            for i in 0..factory.countClasses() {
                if let Some(f2) = &factory2 {
                    let mut info: PClassInfo2 = std::mem::zeroed();
                    if f2.getClassInfo2(i, &mut info) != kResultOk {
                        continue;
                    }
                    let subs: Vec<String> = c_str(&info.subCategories)
                        .split('|')
                        .map(String::from)
                        .collect();
                    if !is_effect(&c_str(&info.category), &subs) {
                        continue;
                    }
                    let vendor = c_str(&info.vendor);
                    out.push(PluginInfo {
                        class_id: hex(&info.cid),
                        name: c_str(&info.name),
                        vendor: if vendor.is_empty() {
                            factory_vendor.clone()
                        } else {
                            vendor
                        },
                        version: c_str(&info.version),
                        path: path.to_path_buf(),
                    });
                } else {
                    let mut info: PClassInfo = std::mem::zeroed();
                    if factory.getClassInfo(i, &mut info) != kResultOk
                        || c_str(&info.category) != AUDIO_CLASS
                    {
                        continue;
                    }
                    out.push(PluginInfo {
                        class_id: hex(&info.cid),
                        name: c_str(&info.name),
                        vendor: factory_vendor.clone(),
                        version: String::new(),
                        path: path.to_path_buf(),
                    });
                }
            }
            out
        }
    }
}
