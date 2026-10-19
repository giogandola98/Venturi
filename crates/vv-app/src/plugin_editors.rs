//! Open plugin editor windows, and what a gesture in one does to the project.
use std::collections::HashMap;
use vv_core::{
    AudioEffect, AudioEffectKind, CommandLabel, EffectId, History, Id, MixerChannel, PluginState,
    Project, SetAudioEffect, TimelineId, Vst3Effect,
};
use vv_plugin::Editor;

use crate::mixer_panel::{channel_strip, plugin_key_of};
use crate::timeline_audio::TimelineAudio;

type Key = (TimelineId, MixerChannel, EffectId);

/// Keeps the editor's own instance clear of the live keys.
const ASIDE_SALT: u64 = 0x9E37_79B9_7F4A_7C15;

#[derive(Default)]
pub(crate) struct PluginEditors {
    open: HashMap<Key, Editor>,
    /// `History::generation` the instances were last compared with the project at.
    checked: u64,
}

fn find(project: &Project, (timeline, channel, id): Key) -> Option<(usize, &AudioEffect)> {
    let strip = channel_strip(project.timelines.get(timeline)?, channel)?;
    strip
        .effects
        .iter()
        .enumerate()
        .find(|(_, e)| matches!(&e.kind, AudioEffectKind::Vst3(v) if v.id == id))
}

fn plugin(effect: &AudioEffect) -> Option<&Vst3Effect> {
    match &effect.kind {
        AudioEffectKind::Vst3(v) => Some(v),
        _ => None,
    }
}

/// One undoable step for an edit gesture; the new content hash, or `None`
/// when the plugin's state did not change.
fn record_edit(
    project: &mut Project,
    history: &mut History,
    key: Key,
    component: PluginState,
    controller: PluginState,
) -> Option<u64> {
    let (index, effect) = find(project, key)?;
    let old = plugin(effect)?;
    if old.component_state == component && old.controller_state == controller {
        return None;
    }
    let new = Vst3Effect {
        component_state: component,
        controller_state: controller,
        ..old.clone()
    };
    let hash = new.content_hash();
    let edited = AudioEffect {
        kind: AudioEffectKind::Vst3(new),
        ..effect.clone()
    };
    let (timeline, channel, _) = key;
    let label = CommandLabel::EditAudioEffect;
    let command = SetAudioEffect::new(timeline, channel, index, edited, label);
    history.do_command(project, Box::new(command));
    Some(hash)
}

impl PluginEditors {
    /// `current` is the timeline the audio is playing.
    pub fn open(
        &mut self,
        ctx: &egui::Context,
        project: &Project,
        (timeline, channel, index): (TimelineId, MixerChannel, usize),
        current: Option<TimelineId>,
        audio: &mut TimelineAudio,
    ) {
        let Some(strip) = project
            .timelines
            .get(timeline)
            .and_then(|t| channel_strip(t, channel))
        else {
            return;
        };
        let Some(plugin) = strip.effects.get(index).and_then(plugin) else {
            return;
        };
        let key = (timeline, channel, plugin.id);
        if self.open.get(&key).is_some_and(Editor::is_open) {
            return;
        }
        self.open.remove(&key);
        let live_key = plugin_key_of(channel, &strip.effects, index);
        let live = (current == Some(timeline)).then_some(live_key);
        let aside = live_key ^ timeline.raw().wrapping_add(1).wrapping_mul(ASIDE_SALT);
        let Some(instance) = audio.plugin_instance(live, aside, plugin) else {
            eprintln!("vv-plugin: {}: no instance for the editor", plugin.name);
            return;
        };
        let ctx = ctx.clone();
        let waker = std::sync::Arc::new(move || ctx.request_repaint());
        match Editor::open(&instance, &plugin.name, waker) {
            Ok(editor) => {
                self.open.insert(key, editor);
            }
            Err(err) => eprintln!("vv-plugin: {}: {err}", plugin.name),
        }
    }

    /// Windows of effects that are gone (removed, undone, another timeline)
    /// close; the others are pumped and their gestures recorded.
    pub fn update(
        &mut self,
        ctx: &egui::Context,
        project: &mut Project,
        history: &mut History,
        current: Option<TimelineId>,
    ) {
        if self.open.is_empty() {
            return;
        }
        self.open.retain(|key, editor| {
            Some(key.0) == current && find(project, *key).is_some() && editor.is_open()
        });
        let generation = history.generation();
        let changed = generation != self.checked;
        let mut wake: Option<std::time::Duration> = None;
        for (key, editor) in &mut self.open {
            editor.idle();
            wake = match (wake, editor.next_wake()) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            let Some(instance) = editor.instance().as_vst3() else {
                continue;
            };
            if instance.take_dirty() {
                let (component, controller) = instance.get_state();
                if let Some(hash) = record_edit(project, history, *key, component, controller) {
                    // The pool must not read the state back into the instance.
                    instance.set_state_hash(hash);
                }
            } else if changed
                && let Some(plugin) = find(project, *key).and_then(|(_, e)| plugin(e))
                && plugin.content_hash() != instance.state_hash()
            {
                // An undo of an instance no mix is playing.
                instance.set_state(plugin);
                instance.set_state_hash(plugin.content_hash());
            }
        }
        // Edits, closing and resizing wake the app; only a plugin's own timers need this.
        self.checked = history.generation();
        if let Some(wake) = wake {
            ctx.request_repaint_after(wake);
        }
    }

    pub fn close_all(&mut self) {
        self.open.clear();
    }
}

#[cfg(test)]
#[path = "tests/plugin_editors.rs"]
mod tests;
