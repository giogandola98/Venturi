use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use vv_audio::{PluginProcessor, PluginRequest};
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
    fn process(&self, _: &mut [f32], _: usize) {}
    fn reset(&self) {}
    fn latency(&self) -> u32 {
        0
    }
}

impl HostedPlugin for Fake {
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
        component_state: PluginState(state.to_vec()),
        controller_state: PluginState(Vec::new()),
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
fn second_get_of_a_key_in_one_pass_is_none() {
    let counts = Arc::new(Counts::default());
    let mut pool = pool(&counts);
    let e = effect(CLASS, &[]);
    pool.begin_pass();
    assert!(pool.get(request(1, &e)).is_some());
    assert!(pool.get(request(1, &e)).is_none());
    assert_eq!(counts.made.load(Ordering::SeqCst), 1);
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
