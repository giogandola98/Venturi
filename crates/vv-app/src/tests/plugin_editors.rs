use vv_core::{AudioEffect, Rational, Timeline, Track, TrackKind};

use super::*;

fn plugin_effect(state: &[u8]) -> Vst3Effect {
    Vst3Effect {
        id: EffectId(7),
        class_id: "ab".into(),
        name: "Comp".into(),
        vendor: String::new(),
        version: String::new(),
        component_state: PluginState::new(state.to_vec()),
        controller_state: PluginState::new(Vec::new()),
    }
}

fn setup() -> (Project, TimelineId, Key) {
    let mut project = Project::default();
    let mut timeline = Timeline {
        name: "T".into(),
        fps: Rational::new(25, 1),
        resolution: (64, 36),
        tracks: vec![Track::new(TrackKind::Audio)],
        markers: Vec::new(),
        master: Default::default(),
    };
    let mut off = AudioEffect::new(AudioEffectKind::Vst3(plugin_effect(&[1])));
    off.enabled = false;
    timeline.master.effects = vec![
        AudioEffect::new(AudioEffectKind::Mono),
        AudioEffect::new(AudioEffectKind::Vst3(plugin_effect(&[1]))),
        off,
    ];
    let id = project.timelines.insert(timeline);
    (project, id, (id, MixerChannel::Master, EffectId(7)))
}

fn state_of(project: &Project, id: TimelineId, index: usize) -> Vec<u8> {
    match &project.timelines[id].master.effects[index].kind {
        AudioEffectKind::Vst3(v) => v.component_state.bytes().to_vec(),
        _ => panic!("not a plugin"),
    }
}

#[test]
fn plugin_gesture_becomes_one_undoable_command() {
    let (mut project, id, key) = setup();
    let mut history = History::default();
    let hash = record_edit(
        &mut project,
        &mut history,
        key,
        PluginState::new(vec![2, 2]),
        PluginState::new(vec![3]),
    );
    // Found by effect id even though it is not the first plugin of the chain.
    assert_eq!(state_of(&project, id, 1), [2, 2]);
    let AudioEffectKind::Vst3(new) = &project.timelines[id].master.effects[1].kind else {
        panic!("not a plugin");
    };
    assert_eq!(hash, Some(new.content_hash()));
    assert_eq!(history.labels().count(), 1);
    assert!(
        project.timelines[id].master.effects[1].enabled,
        "the edit keeps the effect's other fields"
    );

    history.undo(&mut project);
    assert_eq!(state_of(&project, id, 1), [1]);
    history.redo(&mut project);
    assert_eq!(state_of(&project, id, 1), [2, 2]);
}

#[test]
fn a_gesture_that_changed_nothing_leaves_no_undo_step() {
    let (mut project, _, key) = setup();
    let mut history = History::default();
    let same = record_edit(
        &mut project,
        &mut history,
        key,
        PluginState::new(vec![1]),
        PluginState::new(Vec::new()),
    );
    assert_eq!(same, None);
    assert_eq!(history.labels().count(), 0);
}

#[test]
fn a_gesture_of_a_removed_effect_is_dropped() {
    let (mut project, id, _) = setup();
    let mut history = History::default();
    let gone = (id, MixerChannel::Master, EffectId(99));
    let state = PluginState::new(vec![5]);
    assert_eq!(
        record_edit(
            &mut project,
            &mut history,
            gone,
            state,
            PluginState::default()
        ),
        None
    );
    assert_eq!(history.labels().count(), 0);
}

#[derive(Default)]
struct Fake {
    gesture: std::sync::Mutex<Option<(PluginState, PluginState)>>,
    hash: std::sync::atomic::AtomicU64,
}

impl Fake {
    fn with_gesture(component: Vec<u8>) -> Self {
        let fake = Fake::default();
        *fake.gesture.lock().unwrap() =
            Some((PluginState::new(component), PluginState::new(Vec::new())));
        fake
    }
}

impl vv_audio::PluginProcessor for Fake {
    fn process(&self, _: &mut [f32], _: usize) {}
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl HostedPlugin for Fake {
    fn set_state(&self, _: &Vst3Effect) {}
    fn state_hash(&self) -> u64 {
        self.hash.load(std::sync::atomic::Ordering::Relaxed)
    }
    fn set_state_hash(&self, hash: u64) {
        self.hash.store(hash, std::sync::atomic::Ordering::Relaxed);
    }
    fn take_gesture(&self) -> Option<(PluginState, PluginState)> {
        self.gesture.lock().unwrap().take()
    }
}

#[test]
fn an_editor_instance_is_held_under_the_key_the_mix_asks() {
    let (mut project, id, key) = setup();
    let instance: Arc<dyn HostedPlugin> = Arc::new(Fake::default());
    let held = held_instances(&project, [(key, instance.clone())].into_iter());
    assert_eq!(held.len(), 1);
    let expected = plugin_key_of(&project.timelines[id], MixerChannel::Master, 1);
    assert_eq!(held[0].0, expected);
    assert!(Arc::ptr_eq(&held[0].1, &instance));

    // Disabled, the key is the same one the enabled effect plays under.
    project.timelines[id].master.effects[1].enabled = false;
    let again = held_instances(&project, [(key, instance)].into_iter());
    assert_eq!(again[0].0, held[0].0);

    let gone = (id, MixerChannel::Master, EffectId(99));
    assert!(
        held_instances(
            &project,
            [(gone, Arc::new(Fake::default()) as _)].into_iter()
        )
        .is_empty()
    );
}

#[test]
fn a_gesture_pending_at_close_is_recorded_once() {
    let (mut project, id, key) = setup();
    let mut history = History::default();
    let fake = Fake::with_gesture(vec![9]);
    record_gesture(&mut project, &mut history, key, &fake, false);
    assert_eq!(state_of(&project, id, 1), [9]);
    assert_eq!(history.labels().count(), 1);
    assert_ne!(fake.state_hash(), 0, "the pool must not reload the state");

    record_gesture(&mut project, &mut history, key, &fake, false);
    assert_eq!(history.labels().count(), 1);
}

#[test]
fn the_gesture_of_a_removed_effect_is_consumed_not_recorded() {
    let (mut project, id, _) = setup();
    let mut history = History::default();
    let gone = (id, MixerChannel::Master, EffectId(99));
    let fake = Fake::with_gesture(vec![9]);
    record_gesture(&mut project, &mut history, gone, &fake, false);
    assert_eq!(history.labels().count(), 0);
    assert!(fake.take_gesture().is_none(), "nothing left for a reopen");
}

#[test]
fn a_gesture_during_an_undo_group_is_its_own_step_after_it() {
    let (mut project, id, key) = setup();
    let mut history = History::default();
    let fake = Fake::with_gesture(vec![9]);

    let mark = history.begin_group();
    assert!(!record_gesture(
        &mut project,
        &mut history,
        key,
        &fake,
        true
    ));
    assert_eq!(history.labels().count(), 0);
    assert_eq!(state_of(&project, id, 1), [1]);
    history.end_group(mark);

    assert!(record_gesture(
        &mut project,
        &mut history,
        key,
        &fake,
        false
    ));
    assert_eq!(state_of(&project, id, 1), [9]);
    assert_eq!(history.labels().count(), 1);
    history.undo(&mut project);
    assert_eq!(state_of(&project, id, 1), [1]);
}

#[test]
fn a_closed_editors_gesture_waits_for_the_group_then_is_its_own_step() {
    let (mut project, id, key) = setup();
    let mut history = History::default();
    let fake: Arc<dyn HostedPlugin> = Arc::new(Fake::with_gesture(vec![9]));
    let mut pending = vec![(key, fake)];

    let mark = history.begin_group();
    drain_pending(&mut pending, &mut project, &mut history, true);
    assert_eq!(pending.len(), 1);
    assert_eq!(history.labels().count(), 0);
    history.end_group(mark);

    drain_pending(&mut pending, &mut project, &mut history, false);
    assert!(pending.is_empty());
    assert_eq!(state_of(&project, id, 1), [9]);
    assert_eq!(history.labels().count(), 1);
    history.undo(&mut project);
    assert_eq!(state_of(&project, id, 1), [1]);
}
