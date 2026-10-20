use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use vv_audio::{AudioSource, ClipAudio, MixSnapshot, PluginHandle, PluginProcessor, PluginRequest};
use vv_core::{EffectId, PluginState, Vst3Effect};

use super::*;
use crate::scan;
use crate::testing::fixture_bundle;

const CLASS: &str = "56565445535447414E00000000000001";

#[derive(Default)]
struct Counts {
    made: AtomicUsize,
    loads: AtomicUsize,
    dropped: AtomicUsize,
    reactivated: AtomicUsize,
    processed: AtomicUsize,
}

struct Fake {
    hash: AtomicU64,
    counts: Arc<Counts>,
}

impl Drop for Fake {
    fn drop(&mut self) {
        self.counts.dropped.fetch_add(1, Ordering::SeqCst);
    }
}

impl PluginProcessor for Fake {
    fn process(&self, samples: &mut [f32], _: usize) {
        self.counts.processed.fetch_add(1, Ordering::SeqCst);
        samples.iter_mut().for_each(|s| *s *= 0.5);
    }
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl HostedPlugin for Fake {
    fn reactivate(&self) {
        self.counts.reactivated.fetch_add(1, Ordering::SeqCst);
    }
    fn set_state(&self, _: &Vst3Effect) {
        self.counts.loads.fetch_add(1, Ordering::SeqCst);
    }
    fn state_hash(&self) -> u64 {
        self.hash.load(Ordering::SeqCst)
    }
    fn set_state_hash(&self, hash: u64) {
        self.hash.store(hash, Ordering::SeqCst);
    }
}

fn info(class_id: &str) -> PluginInfo {
    PluginInfo {
        class_id: class_id.into(),
        name: "Fake".into(),
        vendor: "Venturi".into(),
        version: "1.0.0".into(),
        path: PathBuf::from("/fake.vst3"),
    }
}

fn pool(counts: &Arc<Counts>) -> Instances {
    let counts = counts.clone();
    Instances::with_maker(
        vec![info(CLASS)],
        Box::new(move |_, _| {
            counts.made.fetch_add(1, Ordering::SeqCst);
            Some(Arc::new(Fake {
                hash: AtomicU64::new(0),
                counts: counts.clone(),
            }))
        }),
    )
}

fn effect(class_id: &str, state: &[u8]) -> Vst3Effect {
    Vst3Effect {
        id: EffectId(1),
        class_id: class_id.into(),
        name: "Fake".into(),
        vendor: "Venturi".into(),
        version: "1.0.0".into(),
        component_state: PluginState::new(state.to_vec()),
        controller_state: PluginState::new(Vec::new()),
    }
}

fn request(key: u64, effect: &Vst3Effect) -> PluginRequest<'_> {
    PluginRequest {
        key,
        effect,
        sample_rate: 48_000,
    }
}

#[test]
fn same_key_reuses_instance() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    let first = pool.get(request(7, &e)).unwrap();
    pool.begin_pass();
    let second = pool.get(request(7, &e)).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    assert_eq!(counts.loads.load(Ordering::SeqCst), 0);
}

#[test]
fn changed_state_reloads_without_new_instance() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    pool.begin_pass();
    let first = pool.get(request(7, &effect(CLASS, &[1]))).unwrap();
    pool.begin_pass();
    let second = pool.get(request(7, &effect(CLASS, &[2]))).unwrap();
    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    assert_eq!(counts.loads.load(Ordering::SeqCst), 1);
    pool.begin_pass();
    pool.get(request(7, &effect(CLASS, &[2]))).unwrap();
    assert_eq!(
        counts.loads.load(Ordering::SeqCst),
        1,
        "same state is not reloaded"
    );
}

#[test]
fn unknown_class_is_none() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    pool.begin_pass();
    assert!(
        pool.get(request(7, &effect("BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB", &[])))
            .is_none()
    );
    assert_eq!(counts.made.load(Ordering::SeqCst), 0);
}

#[test]
fn sweep_drops_unused() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[]);
    pool.begin_pass();
    pool.get(request(1, &e)).unwrap();
    pool.get(request(2, &e)).unwrap();
    pool.begin_pass();
    pool.get(request(1, &e)).unwrap();
    pool.sweep();
    assert!(pool.instance(1).is_some());
    assert!(pool.instance(2).is_none());
    assert_eq!(counts.dropped.load(Ordering::SeqCst), 1);
}

#[test]
fn same_key_twice_in_one_pass_gets_two_instances() {
    // Keys are unique per placement, so two keys for one effect are two instances.
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[]);
    pool.begin_pass();
    let a = pool.get(request(1, &e)).unwrap();
    let b = pool.get(request(2, &e)).unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
    assert_eq!(counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn second_get_of_a_key_in_one_pass_is_the_same_instance() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    let first = pool.get(request(1, &e)).unwrap();
    let again = pool.get(request(1, &e)).unwrap();
    assert!(Arc::ptr_eq(&first, &again));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    assert_eq!(counts.loads.load(Ordering::SeqCst), 0);
    assert_eq!(counts.reactivated.load(Ordering::SeqCst), 0);
    pool.sweep();
    assert!(pool.instance(1).is_some());
}

/// A source over the pool that keeps the default `level`, which measures
/// through `self` before the live chain asks for the same keys.
struct PoolSource(Instances);

impl AudioSource for PoolSource {
    fn file(&mut self, _: &std::path::Path, _: usize) -> ClipAudio {
        ClipAudio::Ready(vv_audio::SampleBlocks::contiguous(
            Arc::new(vec![0.5; 10_000]),
            0,
            1,
        ))
    }

    fn plugin(&mut self, request: PluginRequest<'_>) -> Option<PluginHandle> {
        self.0.get(request)
    }
}

#[test]
fn default_level_does_not_leave_the_live_chain_dry() {
    use vv_core::*;
    let mut project = Project::default();
    let media = project.media_pool.insert(MediaItem {
        path: PathBuf::from("a.wav"),
        meta: MediaMeta {
            duration_frames: 1000,
            fps: Rational::new(10, 1),
            width: 0,
            height: 0,
            has_video: false,
            has_audio: true,
            sample_rate: 100,
            channels: 1,
            audio_streams: 1,
            file: Default::default(),
        },
        content_hash: 7,
        compound: None,
        folder: None,
    });
    let clip = Clip::from_source_range(
        ClipId(1),
        ClipSource::Media(media),
        0,
        5,
        0,
        Rational::one(),
    );
    let mut track = Track {
        clips: vec![clip],
        ..Track::new(TrackKind::Audio)
    };
    let plugin = effect(CLASS, &[]);
    track.mix.effects = vec![
        AudioEffect::new(AudioEffectKind::Vst3(plugin)),
        AudioEffect::new(AudioEffectKind::Normalize {
            target_db: 0.0,
            mode: NormalizeMode::SamplePeak,
            set_level: SetLevel::Relative,
        }),
    ];
    let timeline = Timeline {
        name: "t".into(),
        fps: Rational::new(10, 1),
        resolution: (4, 2),
        tracks: vec![track],
        markers: Vec::new(),
        master: Default::default(),
    };
    let counts = Arc::new(Counts::default());
    let mut source = PoolSource(pool(&counts));
    source.0.begin_pass();
    let snapshot = MixSnapshot::from_timeline(&project, &timeline, 100, 1, &mut source);
    counts.processed.store(0, Ordering::SeqCst);
    let mut out = vec![0.0; 40];
    vv_audio::mixer::mix_range(&snapshot, 0, &mut out);
    assert!(
        counts.processed.load(Ordering::SeqCst) > 0,
        "the live chain played dry"
    );
    // The gain is measured after the plugin: 0.5 * 0.5 normalized to 1.0.
    assert!(out.iter().all(|s| (s - 1.0).abs() < 1e-4), "{out:?}");
}

#[test]
fn fixture_instance_reused_across_passes() {
    let infos = scan(&[fixture_bundle().parent().unwrap().to_path_buf()]);
    let class_id = infos.first().expect("fixture is scanned").class_id.clone();
    let e = effect(&class_id, &0.25f32.to_le_bytes());
    let mut pool = Instances::realtime(infos);
    let mut handles = Vec::new();
    for _ in 0..2 {
        pool.begin_pass();
        handles.push(pool.get(request(1, &e)).expect("fixture instance"));
        pool.sweep();
    }
    assert!(Arc::ptr_eq(&handles[0], &handles[1]));
}

fn failing_pool(counts: &Arc<Counts>) -> Instances {
    let counts = counts.clone();
    Instances::with_maker(
        vec![info(CLASS)],
        Box::new(move |_, _| {
            counts.made.fetch_add(1, Ordering::SeqCst);
            None
        }),
    )
}

#[test]
fn failed_instance_is_not_recreated_every_pass() {
    let counts = Arc::new(Counts::default());
    let mut pool = failing_pool(&counts);
    let e = effect(CLASS, &[1]);
    for _ in 0..3 {
        pool.begin_pass();
        assert!(pool.get(request(7, &e)).is_none());
        pool.sweep();
    }
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
    pool.begin_pass();
    assert!(pool.get(request(7, &effect(CLASS, &[2]))).is_none());
    assert_eq!(
        counts.made.load(Ordering::SeqCst),
        2,
        "a new state is tried again"
    );
}

#[test]
fn failure_is_forgotten_once_its_key_is_not_asked() {
    let counts = Arc::new(Counts::default());
    let mut pool = failing_pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    assert!(pool.get(request(7, &e)).is_none());
    pool.sweep();
    pool.begin_pass();
    pool.sweep();
    pool.begin_pass();
    assert!(pool.get(request(7, &e)).is_none());
    assert_eq!(counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn a_failed_key_is_reported_until_it_stops_being_asked() {
    let counts = Arc::new(Counts::default());
    let mut pool = failing_pool(&counts);
    let e = effect(CLASS, &[1]);
    assert!(!pool.failed(7));
    pool.begin_pass();
    assert!(pool.get(request(7, &e)).is_none());
    assert!(pool.failed(7));
    assert!(!pool.failed(8), "another key");
    pool.sweep();
    assert!(pool.failed(7), "still asked");
    pool.begin_pass();
    pool.sweep();
    assert!(!pool.failed(7));
}

fn same<A: ?Sized, B: ?Sized>(a: &Arc<A>, b: &Arc<B>) -> bool {
    Arc::as_ptr(a).cast::<()>() == Arc::as_ptr(b).cast::<()>()
}

#[test]
fn a_held_instance_survives_a_disable_and_is_the_one_played_again() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    pool.get(request(1, &e)).unwrap();
    let editor = pool.instance(1).unwrap();
    pool.set_held(vec![(1, editor.clone())]);
    pool.sweep();
    pool.begin_pass();
    pool.sweep();
    assert!(pool.instance(1).is_none(), "disabled: not in the mix");
    pool.begin_pass();
    let played = pool.get(request(1, &e)).unwrap();
    assert!(same(&played, &editor));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
}

#[test]
fn a_held_instance_is_adopted_by_a_key_made_after_it() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    pool.get(request(99, &e)).unwrap();
    let aside = pool.instance(99).unwrap();
    pool.set_held(vec![(1, aside.clone())]);
    pool.sweep();
    pool.begin_pass();
    let played = pool.get(request(1, &e)).unwrap();
    assert!(same(&played, &aside));
    assert!(Arc::ptr_eq(&pool.instance(1).unwrap(), &aside));
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
}

#[test]
fn a_held_instance_with_stale_state_is_reloaded_when_adopted() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    pool.begin_pass();
    pool.get(request(99, &effect(CLASS, &[1]))).unwrap();
    let aside = pool.instance(99).unwrap();
    pool.set_held(vec![(1, aside)]);
    pool.begin_pass();
    pool.get(request(1, &effect(CLASS, &[2]))).unwrap();
    assert_eq!(counts.loads.load(Ordering::SeqCst), 1);
}

#[test]
fn released_instances_are_no_longer_adopted() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    pool.begin_pass();
    pool.get(request(99, &e)).unwrap();
    let aside = pool.instance(99).unwrap();
    pool.set_held(vec![(1, aside)]);
    pool.set_held(Vec::new());
    pool.begin_pass();
    pool.get(request(1, &e)).unwrap();
    assert_eq!(counts.made.load(Ordering::SeqCst), 2);
}

#[test]
fn a_restart_request_is_serviced_by_the_pool_without_an_editor() {
    use vst3::Steinberg::Vst::{IComponentHandlerTrait, RestartFlags_};
    let class_id = "56565445535447414E00000000000008";
    let e = effect(class_id, &[]);
    let instance = Vst3Instance::new(&fixture_bundle(), &e, 48_000, ProcessMode::Realtime)
        .expect("latency fixture instance");
    let shared = instance.clone();
    let mut pool = Instances::with_maker(
        vec![info(class_id)],
        Box::new(move |_, _| Some(shared.clone() as Arc<dyn HostedPlugin>)),
    );
    pool.begin_pass();
    pool.get(request(1, &e)).expect("instance");
    pool.sweep();
    assert_eq!(instance.latency(), 0);
    // SAFETY: the handler is a live COM object owned by the instance.
    unsafe {
        instance
            .handler
            .restartComponent(RestartFlags_::kLatencyChanged)
    };
    pool.begin_pass();
    pool.get(request(1, &e)).expect("instance");
    assert_eq!(instance.latency(), 64);
}

#[test]
fn a_new_render_reactivates_each_instance_once_not_per_pass() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[1]);
    for render in 0..3 {
        pool.begin_render();
        for _ in 0..4 {
            pool.begin_pass();
            pool.get(request(7, &e)).unwrap();
            pool.sweep();
        }
        // A new instance starts clean: the first render reactivates nothing.
        assert_eq!(counts.reactivated.load(Ordering::SeqCst), render);
    }
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
}

#[test]
fn a_failing_class_is_reported_once_per_process() {
    assert!(first_failure("test-class-a"));
    assert!(!first_failure("test-class-a"));
    assert!(first_failure("test-class-b"));
}
