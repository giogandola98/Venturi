use std::sync::Arc;

use vst3::Steinberg::Vst::IComponentHandlerTrait;
use vv_audio::PluginProcessor;
use vv_core::{EffectId, PluginState, Vst3Effect};

use super::*;
use crate::testing::fixture_bundle;

fn ramp(frames: usize, channels: usize) -> Vec<f32> {
    (0..frames * channels)
        .map(|i| i as f32 * 0.001 + 0.1)
        .collect()
}

fn scratch() -> [Vec<f32>; 2] {
    [vec![9.0; 512], vec![9.0; 512]]
}

#[test]
fn deinterleave_round_trip_stereo() {
    let input = ramp(5, 2);
    let mut planar = scratch();
    deinterleave(&input, 2, &mut planar, 5);
    assert_eq!(planar[0][..5], [0.1, 0.102, 0.104, 0.106, 0.108]);
    assert_eq!(planar[1][0], 0.101);
    let mut out = vec![0.0; 10];
    interleave(&planar, 2, &mut out, 5);
    assert_eq!(out, input);
}

#[test]
fn interleave_maps_mono_and_surround() {
    let mono = ramp(4, 1);
    let mut planar = scratch();
    deinterleave(&mono, 1, &mut planar, 4);
    assert_eq!(planar[0][..4], planar[1][..4]);
    planar[0][..4].fill(0.5);
    planar[1][..4].fill(0.7);
    let mut out = mono.clone();
    interleave(&planar, 1, &mut out, 4);
    assert_eq!(out, [0.5; 4], "mono takes the left output");

    let surround = ramp(3, 6);
    deinterleave(&surround, 6, &mut planar, 3);
    assert_eq!(planar[0][1], surround[6]);
    assert_eq!(planar[1][1], surround[7]);
    planar[0][..3].fill(-1.0);
    planar[1][..3].fill(-2.0);
    let mut out = surround.clone();
    interleave(&planar, 6, &mut out, 3);
    for frame in out.chunks(6).zip(surround.chunks(6)) {
        assert_eq!(frame.0[..2], [-1.0, -2.0]);
        assert_eq!(frame.0[2..], frame.1[2..], "channels 2-5 are untouched");
    }
}

fn effect(state: Vec<u8>) -> Vst3Effect {
    Vst3Effect {
        id: EffectId(1),
        class_id: "56565445535447414E00000000000001".into(),
        name: "Venturi Test Gain".into(),
        vendor: "Venturi".into(),
        version: "1.0.0".into(),
        component_state: PluginState(state),
        controller_state: PluginState(Vec::new()),
    }
}

fn fixture(state: Vec<u8>) -> Arc<Vst3Instance> {
    Vst3Instance::new(
        &fixture_bundle(),
        &effect(state),
        48_000,
        ProcessMode::Realtime,
    )
    .expect("fixture instance")
}

fn run(instance: &Vst3Instance, input: &[f32]) -> Vec<f32> {
    let mut out = input.to_vec();
    instance.process(&mut out, 2);
    out
}

fn assert_scaled(out: &[f32], input: &[f32], gain: f32) {
    for (o, i) in out.iter().zip(input) {
        assert!((o - i * gain).abs() < 1e-6, "{o} vs {i} * {gain}");
    }
}

#[test]
fn fixture_gain_applies() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    let input = ramp(512, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
    // Longer than one plugin block: processed in slices.
    let input = ramp(1300, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn fixture_default_state_is_unity() {
    let instance = fixture(Vec::new());
    let input = ramp(100, 2);
    assert_scaled(&run(&instance, &input), &input, 1.0);
}

#[test]
fn fixture_state_round_trips() {
    let bytes = 0.25f32.to_le_bytes().to_vec();
    let instance = fixture(Vec::new());
    instance.set_state(&effect(bytes.clone()));
    let (component, _) = instance.get_state();
    assert_eq!(component.0, bytes);
    let input = ramp(64, 2);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn fixture_param_change_reaches_processor() {
    let instance = fixture(Vec::new());
    let input = ramp(256, 2);
    assert_scaled(&run(&instance, &input), &input, 1.0);
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance.handler.beginEdit(0);
        instance.handler.performEdit(0, 0.25);
        instance.handler.endEdit(0);
    }
    assert!(instance.take_dirty());
    assert!(!instance.take_dirty());
    assert_scaled(&run(&instance, &input), &input, 0.5);
    assert_scaled(&run(&instance, &input), &input, 0.5);
}

#[test]
fn busy_instance_passes_audio_through() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    instance
        .busy
        .store(true, std::sync::atomic::Ordering::Release);
    let input = ramp(64, 2);
    assert_eq!(run(&instance, &input), input);
    instance
        .busy
        .store(false, std::sync::atomic::Ordering::Release);
}

#[test]
fn mono_and_surround_run_through_the_plugin() {
    let instance = fixture(0.25f32.to_le_bytes().to_vec());
    let mut mono = ramp(32, 1);
    let want: Vec<f32> = mono.iter().map(|s| s * 0.5).collect();
    instance.process(&mut mono, 1);
    assert_eq!(mono.len(), want.len());
    for (a, b) in mono.iter().zip(&want) {
        assert!((a - b).abs() < 1e-6);
    }
    let surround = ramp(32, 6);
    let mut out = surround.clone();
    instance.process(&mut out, 6);
    for (o, i) in out.chunks(6).zip(surround.chunks(6)) {
        assert!((o[0] - i[0] * 0.5).abs() < 1e-6);
        assert_eq!(o[2..], i[2..]);
    }
}

#[test]
fn unknown_class_is_an_error() {
    let mut bad = effect(Vec::new());
    bad.class_id = "00000000000000000000000000000000".into();
    let r = Vst3Instance::new(&fixture_bundle(), &bad, 48_000, ProcessMode::Offline);
    assert!(r.is_err());
    bad.class_id = "zz".into();
    assert!(Vst3Instance::new(&fixture_bundle(), &bad, 48_000, ProcessMode::Offline).is_err());
}

#[cfg(target_os = "macos")]
mod real_plugins {
    use super::*;
    use crate::scan;
    use std::path::PathBuf;

    fn installed(name: &str) -> Option<(PathBuf, Vst3Effect)> {
        let dir = PathBuf::from("/Library/Audio/Plug-Ins/VST3");
        let info = scan(&[dir]).into_iter().find(|p| p.name.contains(name))?;
        let mut e = effect(Vec::new());
        e.class_id = info.class_id;
        e.name = info.name;
        Some((info.path, e))
    }

    fn noise(frames: usize) -> Vec<f32> {
        let mut x = 0x1234_5678u32;
        (0..frames * 2)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                (x as f32 / u32::MAX as f32 - 0.5) * 0.5
            })
            .collect()
    }

    #[test]
    #[ignore = "needs Wider installed"]
    fn wider_processes_without_new_allocs_after_setup() {
        let (path, e) = installed("Wider").expect("Wider installed");
        let inst = Vst3Instance::new(&path, &e, 48_000, ProcessMode::Realtime).unwrap();
        let mut audio = noise(48_000);
        for block in audio.chunks_mut(512 * 2) {
            inst.process(block, 2);
        }
        assert!(audio.iter().all(|s| s.is_finite()));
    }

    // TDR writes a per-process instance counter into its state.
    fn mask_instance(mut state: Vec<u8>) -> Vec<u8> {
        let key = b"instance=\"";
        if let Some(at) = state.windows(key.len()).position(|w| w == key) {
            let end = (at + key.len() + 16).min(state.len());
            state[at + key.len()..end].fill(0);
        }
        state
    }

    #[test]
    #[ignore = "needs TDR Nova installed"]
    fn state_round_trips() {
        let (path, e) = installed("TDR Nova").expect("TDR Nova installed");
        let a = Vst3Instance::new(&path, &e, 48_000, ProcessMode::Realtime).unwrap();
        let (c1, k1) = a.get_state();
        assert!(!c1.0.is_empty());
        let mut saved = e.clone();
        saved.component_state = c1.clone();
        saved.controller_state = k1.clone();
        let b = Vst3Instance::new(&path, &saved, 48_000, ProcessMode::Realtime).unwrap();
        let (c2, k2) = b.get_state();
        assert_eq!(mask_instance(c1.0), mask_instance(c2.0));
        assert_eq!(k1, k2);
    }

    #[test]
    #[ignore = "needs TDR Nova installed"]
    fn nova_default_is_near_transparent() {
        let (path, e) = installed("TDR Nova").expect("TDR Nova installed");
        let inst = Vst3Instance::new(&path, &e, 48_000, ProcessMode::Realtime).unwrap();
        let latency = inst.latency() as usize;
        let input = noise(48_000);
        let mut out = input.clone();
        for block in out.chunks_mut(512 * 2) {
            inst.process(block, 2);
        }
        // Compare after the plugin's own delay, skipping the settle time.
        let skip = 8192 * 2;
        let (mut err, mut sig) = (0.0f64, 0.0f64);
        for i in skip..input.len() - latency * 2 {
            let d = f64::from(out[i + latency * 2] - input[i]);
            err += d * d;
            sig += f64::from(input[i]).powi(2);
        }
        let db = 10.0 * (err / sig).log10();
        assert!(db < -90.0, "residual {db} dB");
    }
}
