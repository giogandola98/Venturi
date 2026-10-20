use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use vv_audio::{PluginHandle, PluginRequest};
use vv_core::{PluginState, Vst3Effect};

use crate::{PluginInfo, ProcessMode, Vst3Instance};

pub type MakeInstance = Box<dyn FnMut(&Vst3Effect, u32) -> Option<Arc<dyn HostedPlugin>> + Send>;

/// What the pool needs beyond processing (Vst3Instance implements it).
pub trait HostedPlugin: vv_audio::PluginProcessor {
    fn set_state(&self, effect: &Vst3Effect);
    fn state_hash(&self) -> u64;
    fn set_state_hash(&self, hash: u64);
    fn as_vst3(&self) -> Option<&Vst3Instance> {
        None
    }
    /// Acts on a restart the plugin asked for; called every pass on the UI thread.
    fn service_restart(&self) {}
    /// The full deactivate/activate cycle: clears what `reset` leaves, such
    /// as a reverb's tail.
    fn reactivate(&self) {}
    /// The state to record when an edit gesture ended since the last call.
    fn take_gesture(&self) -> Option<(PluginState, PluginState)> {
        None
    }
}

/// Whether `class_id` has not failed yet in this process: pools are per
/// worker, so the same plugin would otherwise log once per pool.
fn first_failure(class_id: &str) -> bool {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut seen = SEEN.get_or_init(Default::default).lock().unwrap();
    seen.insert(class_id.to_owned())
}

struct Entry {
    instance: Arc<dyn HostedPlugin>,
    used_this_pass: bool,
    /// Reactivated by its first `get` of an offline render.
    stale: bool,
}

/// A key whose creation failed: not retried until its state changes.
struct Failure {
    hash: u64,
    used_this_pass: bool,
}

/// Instances are torn down on drop, which calls into the plugin: `sweep` and
/// the drop run on the owner's thread (the UI thread for live). An offline
/// pool may move between a worker's threads, one at a time, and is dropped
/// on whichever holds it.
pub struct Instances {
    entries: HashMap<u64, Entry>,
    failed: HashMap<u64, Failure>,
    known: HashSet<String>,
    make: MakeInstance,
    /// Instances an editor window drives, by the key they stand for.
    held: HashMap<u64, Arc<dyn HostedPlugin>>,
}

impl Instances {
    pub fn realtime(known: Vec<PluginInfo>) -> Self {
        Self::new(known, ProcessMode::Realtime)
    }

    /// Offline pools are used and dropped on worker threads (export, level
    /// and stretch), not the UI thread VST3 names for the lifecycle calls: a
    /// plugin that needs the main thread may fail and then plays dry.
    pub fn offline(known: Vec<PluginInfo>) -> Self {
        Self::new(known, ProcessMode::Offline)
    }

    fn new(known: Vec<PluginInfo>, mode: ProcessMode) -> Self {
        let paths: HashMap<String, PathBuf> = known
            .iter()
            .map(|i| (i.class_id.clone(), i.path.clone()))
            .collect();
        let make = Box::new(move |effect: &Vst3Effect, sample_rate| {
            let path = paths.get(&effect.class_id)?;
            match Vst3Instance::new(path, effect, sample_rate, mode) {
                Ok(instance) => Some(instance as Arc<dyn HostedPlugin>),
                Err(err) => {
                    if first_failure(&effect.class_id) {
                        eprintln!("vv-plugin: {}: {err}", effect.name);
                    }
                    None
                }
            }
        });
        Self::with_maker(known, make)
    }

    pub fn with_maker(known: Vec<PluginInfo>, make: MakeInstance) -> Self {
        Self {
            entries: HashMap::new(),
            failed: HashMap::new(),
            known: known.into_iter().map(|i| i.class_id).collect(),
            make,
            held: HashMap::new(),
        }
    }

    pub fn get(&mut self, request: PluginRequest<'_>) -> Option<PluginHandle> {
        let hash = request.effect.content_hash();
        if let Some(entry) = self.entries.get_mut(&request.key) {
            if entry.used_this_pass {
                // An analysis shares the pass with the live chain; they run
                // one after the other, never at once.
                return Some(entry.instance.clone());
            }
            entry.used_this_pass = true;
            entry.instance.service_restart();
            if std::mem::take(&mut entry.stale) {
                entry.instance.reactivate();
            }
            if entry.instance.state_hash() != hash {
                entry.instance.set_state(request.effect);
                entry.instance.set_state_hash(hash);
            }
            return Some(entry.instance.clone());
        }
        if !self.known.contains(&request.effect.class_id) {
            return None;
        }
        if let Some(failure) = self.failed.get_mut(&request.key) {
            failure.used_this_pass = true;
            if failure.hash == hash {
                return None;
            }
        }
        let held = self.held.get(&request.key).cloned();
        let Some(instance) = held.or_else(|| (self.make)(request.effect, request.sample_rate))
        else {
            self.failed.insert(
                request.key,
                Failure {
                    hash,
                    used_this_pass: true,
                },
            );
            return None;
        };
        self.failed.remove(&request.key);
        if self.held.contains_key(&request.key) && instance.state_hash() != hash {
            instance.set_state(request.effect);
        }
        instance.set_state_hash(hash);
        let handle = instance.clone();
        self.entries.insert(
            request.key,
            Entry {
                instance,
                used_this_pass: true,
                stale: false,
            },
        );
        Some(handle)
    }

    /// Replaces the held instances: when a key has none, `get` takes the held
    /// one instead of making another, so a window keeps driving what plays
    /// after the effect is disabled and enabled again.
    pub fn set_held(&mut self, held: Vec<(u64, Arc<dyn HostedPlugin>)>) {
        self.held = held.into_iter().collect();
    }

    /// Whether `effect` is a scanned plugin: what `get` needs to try it.
    pub fn knows(&self, effect: &Vst3Effect) -> bool {
        self.known.contains(&effect.class_id)
    }

    /// Whether the last attempt to make the instance under `key` failed.
    pub fn failed(&self, key: u64) -> bool {
        self.failed.contains_key(&key)
    }

    /// An offline pool starts a new render: each instance is reactivated
    /// by its first `get`, as it carries the previous render's tail.
    pub fn begin_render(&mut self) {
        for entry in self.entries.values_mut() {
            entry.stale = true;
        }
    }

    pub fn begin_pass(&mut self) {
        for entry in self.entries.values_mut() {
            entry.used_this_pass = false;
        }
        for failure in self.failed.values_mut() {
            failure.used_this_pass = false;
        }
    }

    /// Drops what the last pass did not ask for.
    pub fn sweep(&mut self) {
        self.entries.retain(|_, e| e.used_this_pass);
        self.failed.retain(|_, f| f.used_this_pass);
    }

    /// Forgets every failure so the next `get` tries again.
    pub fn clear_failures(&mut self) {
        self.failed.clear();
    }

    pub fn instance(&self, key: u64) -> Option<Arc<dyn HostedPlugin>> {
        self.entries.get(&key).map(|e| e.instance.clone())
    }
}

impl HostedPlugin for Vst3Instance {
    fn set_state(&self, effect: &Vst3Effect) {
        Vst3Instance::set_state(self, effect)
    }

    fn state_hash(&self) -> u64 {
        Vst3Instance::state_hash(self)
    }

    fn set_state_hash(&self, hash: u64) {
        Vst3Instance::set_state_hash(self, hash)
    }

    fn as_vst3(&self) -> Option<&Vst3Instance> {
        Some(self)
    }

    fn service_restart(&self) {
        Vst3Instance::service_restart(self)
    }

    fn reactivate(&self) {
        Vst3Instance::reactivate(self)
    }

    fn take_gesture(&self) -> Option<(PluginState, PluginState)> {
        self.take_dirty().then(|| self.get_state())
    }
}

#[cfg(test)]
#[path = "tests/instances.rs"]
mod tests;
