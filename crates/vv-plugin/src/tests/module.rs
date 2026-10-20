use super::{Registry, c_str, cached};
use std::ffi::c_char;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

fn registry() -> Registry<Arc<u32>> {
    Mutex::new(None)
}

#[test]
fn c_str_stops_at_nul_or_array_end() {
    let full = [b'a' as c_char; 4];
    assert_eq!(c_str(&full), "aaaa");
    let short = [b'h' as c_char, b'i' as c_char, 0, b'x' as c_char];
    assert_eq!(c_str(&short), "hi");
}

#[test]
fn a_cached_path_does_not_wait_for_another_path_loading() {
    let reg = registry();
    let a = cached(&reg, Path::new("a"), || Ok(Arc::new(1))).unwrap();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let reg = &reg;
    std::thread::scope(|s| {
        s.spawn(move || {
            cached(reg, Path::new("b"), || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Arc::new(2))
            })
            .unwrap();
        });
        started_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        s.spawn(move || {
            let again = cached(&reg, Path::new("a"), || panic!("a is cached")).unwrap();
            done_tx.send(again).unwrap();
        });
        let again = done_rx.recv_timeout(Duration::from_secs(5));
        release_tx.send(()).unwrap();
        assert!(Arc::ptr_eq(&again.expect("lookup waited on b's load"), &a));
    });
}

#[test]
fn another_path_loads_while_one_is_loading() {
    let reg = registry();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let reg = &reg;
    std::thread::scope(|s| {
        s.spawn(move || {
            cached(reg, Path::new("slow"), || {
                started_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok(Arc::new(1))
            })
            .unwrap();
        });
        started_rx.recv().unwrap();
        let (done_tx, done_rx) = mpsc::channel();
        s.spawn(move || {
            done_tx
                .send(cached(&reg, Path::new("fast"), || Ok(Arc::new(2))))
                .unwrap();
        });
        let fast = done_rx.recv_timeout(Duration::from_secs(5));
        release_tx.send(()).unwrap();
        assert_eq!(*fast.expect("fast load waited").unwrap(), 2);
    });
}

#[test]
fn concurrent_same_path_loads_once_and_shares_the_arc() {
    let reg = registry();
    let loads = AtomicUsize::new(0);
    let got: Vec<Arc<u32>> = std::thread::scope(|s| {
        let handles: Vec<_> = (0..8)
            .map(|_| {
                s.spawn(|| {
                    cached(&reg, Path::new("p"), || {
                        loads.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(50));
                        Ok(Arc::new(7))
                    })
                    .unwrap()
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(loads.load(Ordering::SeqCst), 1);
    assert!(got.iter().all(|m| Arc::ptr_eq(m, &got[0])));
}

#[test]
fn a_failed_load_is_retried() {
    let reg = registry();
    assert!(cached(&reg, Path::new("p"), || Err("no".into())).is_err());
    assert_eq!(
        *cached(&reg, Path::new("p"), || Ok(Arc::new(3))).unwrap(),
        3
    );
}

#[cfg(target_os = "macos")]
mod macos {
    use crate::instance::{ProcessMode, Vst3Instance};
    use crate::scan::scan;
    use crate::testing::fixture_bundle;
    use std::path::PathBuf;
    use vv_audio::PluginProcessor;
    use vv_core::{EffectId, PluginState, Vst3Effect};

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleExecutable</key><string>PluginHooksVST3</string>
<key>CFBundleIdentifier</key><string>com.venturi.renamed</string>
<key>CFBundlePackageType</key><string>BNDL</string>
</dict></plist>
"#;

    fn renamed_bundle() -> PathBuf {
        let src = fixture_bundle().join("Contents/MacOS");
        let binary = std::fs::read_dir(src)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let root = std::env::temp_dir().join(format!("venturi-renamed-{}", std::process::id()));
        let bundle = root.join("Renamed.vst3");
        let dir = bundle.join("Contents/MacOS");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::copy(binary, dir.join("PluginHooksVST3")).unwrap();
        std::fs::write(bundle.join("Contents/Info.plist"), PLIST).unwrap();
        bundle
    }

    #[test]
    fn bundle_executable_may_differ_from_the_folder_name() {
        let bundle = renamed_bundle();
        let found = scan(&[bundle.parent().unwrap().to_path_buf()]);
        assert_eq!(found.len(), 1, "{found:?}");
        let effect = Vst3Effect {
            id: EffectId(1),
            class_id: found[0].class_id.clone(),
            name: found[0].name.clone(),
            vendor: found[0].vendor.clone(),
            version: found[0].version.clone(),
            component_state: PluginState::new(0.25f32.to_le_bytes().to_vec()),
            controller_state: PluginState::new(Vec::new()),
        };
        let instance = Vst3Instance::new(&bundle, &effect, 48_000, ProcessMode::Offline).unwrap();
        let mut audio = vec![1.0f32; 64];
        instance.process(&mut audio, 2);
        assert!(audio.iter().all(|s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    #[ignore = "needs the installed RX 11 bundles"]
    fn installed_bundles_with_a_custom_executable_are_listed() {
        let found = scan(&crate::scan::default_dirs());
        let rx = found
            .iter()
            .filter(|p| p.path.to_string_lossy().contains("RX 11"))
            .count();
        println!("RX 11 plugins listed: {rx} of {} total", found.len());
        assert!(rx > 0);
    }
}
