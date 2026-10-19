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
