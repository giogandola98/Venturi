use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

const BUNDLE: &str = "VenturiTestGain";

/// The fixture gain plugin as a `.vst3` bundle, built once per process.
pub fn fixture_bundle() -> PathBuf {
    static BUNDLE_PATH: OnceLock<PathBuf> = OnceLock::new();
    BUNDLE_PATH.get_or_init(build_bundle).clone()
}

fn build_bundle() -> PathBuf {
    let workspace_target = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or(workspace_target);
    // Own target dir: the outer cargo holds the lock on the shared one while tests run.
    let output = Command::new(env!("CARGO"))
        .args(["build", "-p", "vv-plugin-fixture", "--message-format=json"])
        .arg("--target-dir")
        .arg(target.join("fixture"))
        .output()
        .expect("failed to run cargo");
    assert!(
        output.status.success(),
        "building vv-plugin-fixture failed:\n{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let cdylib = cdylib_path(&String::from_utf8_lossy(&output.stdout)).unwrap_or_else(|| {
        panic!(
            "no cdylib in cargo output:\n{}",
            String::from_utf8_lossy(&output.stdout)
        )
    });

    // One bundle per target dir (removed by `cargo clean`). Every process writes the same
    // bytes to its own temp file and renames it over the binary, so readers never see a
    // partial file and a dylib already loaded keeps its old inode.
    let root = target.join("fixture").join("bundle");
    let bundle = root.join(format!("{BUNDLE}.vst3"));
    let (dir, file) = if cfg!(target_os = "macos") {
        ("MacOS".to_string(), BUNDLE.to_string())
    } else {
        (
            format!("{}-linux", std::env::consts::ARCH),
            format!("{BUNDLE}.so"),
        )
    };
    let bin_dir = bundle.join("Contents").join(dir);
    std::fs::create_dir_all(&bin_dir).expect("create bundle dir");
    static CALLS: AtomicUsize = AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, Ordering::Relaxed);
    let tmp = root.join(format!(".{BUNDLE}-{}-{call}.tmp", std::process::id()));
    std::fs::copy(&cdylib, &tmp).expect("copy fixture binary");
    std::fs::rename(&tmp, bin_dir.join(file)).expect("install fixture binary");
    bundle
}

fn cdylib_path(cargo_json: &str) -> Option<PathBuf> {
    cargo_json
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|m| {
            m["reason"] == "compiler-artifact" && m["target"]["name"] == "vv_plugin_fixture"
        })
        .flat_map(|m| m["filenames"].as_array().cloned().unwrap_or_default())
        .filter_map(|f| f.as_str().map(PathBuf::from))
        .find(|p| p.extension().is_some_and(|e| e == "dylib" || e == "so"))
}

#[cfg(test)]
#[path = "tests/fixture.rs"]
mod tests;
