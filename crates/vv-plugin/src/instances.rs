use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use vv_audio::{PluginHandle, PluginRequest};
use vv_core::{PluginState, Vst3Effect};

use crate::{PluginInfo, ProcessMode, Vst3Instance};

pub type MakeInstance =
    Box<dyn FnMut(&Path, &Vst3Effect, u32) -> Option<Arc<dyn HostedPlugin>> + Send>;

/// What the pool needs beyond processing (Vst3Instance implements it).
pub trait HostedPlugin: vv_audio::PluginProcessor {
    fn set_state(&self, effect: &Vst3Effect);
    fn state_hash(&self) -> u64;
    fn mark_state(&self, hash: u64);
    /// The concrete instance, for the editor; `None` for a stand-in.
    fn into_vst3(self: Arc<Self>) -> Option<Arc<Vst3Instance>> {
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

struct Slot {
    used_this_pass: bool,
    state: SlotState,
}

enum SlotState {
    Ready {
        instance: Arc<dyn HostedPlugin>,
        /// The class and bundle it was made from.
        origin: (String, PathBuf),
        /// Reactivated by its first `get` of an offline render.
        stale: bool,
    },
    /// Creation failed: not retried until the effect's state changes.
    Failed { hash: u64 },
}

/// Instances are torn down on drop, which calls into the plugin: `sweep` and
/// the drop run on the owner's thread (the UI thread for live). An offline
/// pool may move between a worker's threads, one at a time, and is dropped
/// on whichever holds it.
pub struct Instances {
    slots: HashMap<u64, Slot>,
    /// Scanned plugins by class id: the path each is made from and its version.
    known: HashMap<String, PluginInfo>,
    make: MakeInstance,
    /// Instances an editor window drives, by the key they stand for.
    held: HashMap<u64, Arc<dyn HostedPlugin>>,
}

impl Instances {
    pub fn realtime(known: Arc<[PluginInfo]>) -> Self {
        Self::new(known, ProcessMode::Realtime)
    }

    /// Offline pools are used and dropped on worker threads (export, level
    /// and stretch), not the UI thread VST3 names for the lifecycle calls: a
    /// plugin that needs the main thread may fail and then plays dry.
    pub fn offline(known: Arc<[PluginInfo]>) -> Self {
        Self::new(known, ProcessMode::Offline)
    }

    fn new(known: Arc<[PluginInfo]>, mode: ProcessMode) -> Self {
        let make = Box::new(move |path: &Path, effect: &Vst3Effect, sample_rate| {
            let made = Vst3Instance::new(path, effect, sample_rate, mode);
            match made {
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

    pub fn with_maker(known: Arc<[PluginInfo]>, make: MakeInstance) -> Self {
        Self {
            slots: HashMap::new(),
            known: known
                .iter()
                .map(|i| (i.class_id.clone(), i.clone()))
                .collect(),
            make,
            held: HashMap::new(),
        }
    }

    pub fn get(&mut self, request: PluginRequest<'_>) -> Option<PluginHandle> {
        if !request.process {
            if let Some(slot) = self.slots.get_mut(&request.key) {
                slot.used_this_pass = true;
                // Its tails are from before the bypass.
                if let SlotState::Ready { stale, .. } = &mut slot.state {
                    *stale = true;
                }
            }
            return None;
        }
        let hash = request.effect.content_hash();
        if let Some(Slot {
            used_this_pass,
            state: SlotState::Ready {
                instance, stale, ..
            },
        }) = self.slots.get_mut(&request.key)
        {
            if *used_this_pass {
                // An analysis shares the pass with the live chain; they run
                // one after the other, never at once.
                return Some(instance.clone());
            }
            *used_this_pass = true;
            instance.service_restart();
            if std::mem::take(stale) {
                instance.reactivate();
            }
            if instance.state_hash() != hash {
                instance.set_state(request.effect);
            }
            return Some(instance.clone());
        }
        let path = self.known.get(&request.effect.class_id)?.path.clone();
        if let Some(slot) = self.slots.get_mut(&request.key) {
            slot.used_this_pass = true;
            if matches!(slot.state, SlotState::Failed { hash: failed } if failed == hash) {
                return None;
            }
        }
        let held = self.held.get(&request.key).cloned();
        let Some(instance) =
            held.or_else(|| (self.make)(&path, request.effect, request.sample_rate))
        else {
            self.slots.insert(
                request.key,
                Slot {
                    used_this_pass: true,
                    state: SlotState::Failed { hash },
                },
            );
            return None;
        };
        if self.held.contains_key(&request.key) && instance.state_hash() != hash {
            instance.set_state(request.effect);
        }
        let handle = instance.clone();
        self.slots.insert(
            request.key,
            Slot {
                used_this_pass: true,
                state: SlotState::Ready {
                    instance,
                    origin: (request.effect.class_id.clone(), path),
                    stale: false,
                },
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
        self.known.contains_key(&effect.class_id)
    }

    /// Fingerprint of the module this process runs for `effect`'s class;
    /// `None` if not installed. A new build is seen after a restart, the
    /// only time its code runs.
    pub fn build(&self, effect: &Vst3Effect) -> Option<u64> {
        let info = self.known.get(&effect.class_id)?;
        Some(crate::module::build(&info.path, &info.version))
    }

    /// Whether the last attempt to make the instance under `key` failed.
    pub fn failed(&self, key: u64) -> bool {
        matches!(
            self.slots.get(&key),
            Some(Slot {
                state: SlotState::Failed { .. },
                ..
            })
        )
    }

    /// An offline pool starts a new render: each instance is reactivated
    /// by its first `get`, as it carries the previous render's tail.
    pub fn begin_render(&mut self) {
        for slot in self.slots.values_mut() {
            if let SlotState::Ready { stale, .. } = &mut slot.state {
                *stale = true;
            }
        }
    }

    pub fn begin_pass(&mut self) {
        for slot in self.slots.values_mut() {
            slot.used_this_pass = false;
        }
    }

    pub fn sweep(&mut self) {
        self.slots.retain(|_, slot| slot.used_this_pass);
    }

    /// Adopts a new scan: instances of plugins still listed from the same
    /// bundle stay (the module is never reloaded, so they run the same code),
    /// the others are returned, and failures are forgotten so new plugins
    /// are tried.
    pub fn refresh(&mut self, known: Arc<[PluginInfo]>) -> Vec<Arc<dyn HostedPlugin>> {
        self.known = known
            .iter()
            .map(|i| (i.class_id.clone(), i.clone()))
            .collect();
        let known = &self.known;
        let mut dropped = Vec::new();
        self.slots.retain(|_, slot| match &slot.state {
            SlotState::Ready {
                instance,
                origin: (class_id, path),
                ..
            } => {
                let keep = known.get(class_id).is_some_and(|i| &i.path == path);
                if !keep {
                    dropped.push(instance.clone());
                }
                keep
            }
            SlotState::Failed { .. } => false,
        });
        dropped
    }

    /// Forgets every failure so the next `get` tries again.
    pub fn clear_failures(&mut self) {
        self.slots
            .retain(|_, slot| matches!(slot.state, SlotState::Ready { .. }));
    }

    pub fn instance(&self, key: u64) -> Option<Arc<dyn HostedPlugin>> {
        match self.slots.get(&key) {
            Some(Slot {
                state: SlotState::Ready { instance, .. },
                ..
            }) => Some(instance.clone()),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "tests/instances.rs"]
mod tests;
