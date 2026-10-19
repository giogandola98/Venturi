use super::*;

fn vst3(state: Vec<u8>) -> Vst3Effect {
    Vst3Effect {
        id: EffectId::fresh(),
        class_id: "0123456789ABCDEF0123456789ABCDEF".into(),
        name: "Test Plugin".into(),
        vendor: "Vendor".into(),
        version: "1.0".into(),
        component_state: PluginState(state),
        controller_state: PluginState(vec![7, 8, 9]),
    }
}

#[test]
fn vst3_effect_round_trips_through_ron() {
    let state: Vec<u8> = (0..70_000).map(|i| (i * 31 % 251) as u8).collect();
    let effect = AudioEffect::new(AudioEffectKind::Vst3(vst3(state.clone())));

    let mut track = Track::new(TrackKind::Audio);
    track.mix.effects.push(effect.clone());
    let mut project = Project::default();
    project.timelines.insert(Timeline {
        name: "Timeline 1".into(),
        fps: Rational::new(30, 1),
        resolution: (1920, 1080),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    });

    let text = ron::ser::to_string(&project).unwrap();
    let encoded = base64::engine::general_purpose::STANDARD.encode(&state);
    assert!(text.contains(&encoded));

    let back: Project = ron::from_str(&text).unwrap();
    let timeline = back.timelines.values().next().unwrap();
    assert_eq!(timeline.tracks[0].mix.effects, vec![effect]);
}

#[test]
fn plugin_state_debug_is_compact() {
    let state = PluginState(vec![0u8; 1 << 20]);
    assert!(format!("{state:?}").len() < 64);
}

#[test]
fn fresh_ids_differ() {
    let ids: Vec<EffectId> = (0..1000).map(|_| EffectId::fresh()).collect();
    assert!(ids.iter().all(|id| id.0 != 0));
    let unique: std::collections::HashSet<EffectId> = ids.into_iter().collect();
    assert_eq!(unique.len(), 1000);
}

#[test]
fn content_hash_depends_on_the_states() {
    let a = vst3(vec![1, 2, 3]);
    let mut b = a.clone();
    assert_eq!(a.content_hash(), b.content_hash());
    b.controller_state = PluginState(vec![0]);
    assert_ne!(a.content_hash(), b.content_hash());
}

#[test]
fn with_fresh_ids_changes_only_vst3() {
    let native = AudioEffect::new(AudioEffectKind::Mono);
    assert_eq!(native.clone().with_fresh_ids(), native);

    let plugin = AudioEffect::new(AudioEffectKind::Vst3(vst3(vec![1])));
    let AudioEffectKind::Vst3(before) = plugin.kind.clone() else {
        unreachable!()
    };
    let AudioEffectKind::Vst3(after) = plugin.with_fresh_ids().kind else {
        unreachable!()
    };
    assert_ne!(before.id, after.id);
    assert_eq!(before.class_id, after.class_id);
    assert_eq!(before.vendor, after.vendor);
    assert_eq!(before.version, after.version);
    assert_eq!(before.component_state, after.component_state);
}
