# VST3_HOST — VST3 audio plugins in the mixer chain

**Status:** implemented on branch `vst3-host`. Built on
[AUDIO_LIVE_MIX.md](AUDIO_LIVE_MIX.md) §3.6: the `Effect` trait is the
seam, the plugin is one more effect kind. Manual checks of the editor
window and the Linux build are still open (§5).

**Goal:** run VST3 effects on a track, on the master, or on a compound's
strip, live in the preview and in the export, with their editor window,
their state saved in the project, and every change undoable.

---

## 1. Scope

- VST3 effects only, on macOS (AppKit editors) and Linux (X11 editors).
  Instruments, MIDI and video plugins are out.
- A plugin is an effect in the chain of a strip, between the built-in
  effects: the same slot, the same order, the same bypass as the rest.
- A plugin's output is what the export writes. A plugin that fails to load
  is shown as missing and passes the audio through.

## 2. Design as built

### 2.1 Model (`vv-core`)

- `AudioEffectKind::Vst3(Vst3Effect)`: a stable `EffectId`, the class id,
  name, vendor and version, and the component and controller state.
- The state is `PluginState`: shared bytes (`Arc<[u8]>`) with the hash
  cached at construction. It is stored as base64 in the RON project file;
  older files load unchanged.
- A duplicated timeline gets fresh effect ids, so two copies never share a
  plugin placement.

### 2.2 Mixing (`vv-audio`)

- `PluginProcessor` is the trait the mixer drives: in place, interleaved,
  never blocking (it passes through when busy).
- `AudioSource::plugin()` is asked once per placement, and the mixer keeps
  the returned processor for the render.
- `plugin_key(scope, strip, id, occurrence)` identifies a placement. Each
  place a plugin plays has its own instance: a track, the master, each
  placement of a compound clip, and the pitch-corrected stretch of a
  compound. The panel computes the same key to find the live instance of
  an effect.

### 2.3 Plugin host (`vv-plugin`, new)

All `unsafe` and all COM code live here.

- **Scan:** `moduleinfo.json` inside the bundle first, so the module is not
  loaded; otherwise the module is loaded and its factory read. Vendor
  sub-folders are searched; a broken bundle does not hide the others.
  Default folders: `/Library/Audio/Plug-Ins/VST3` and the user one on
  macOS; `~/.vst3`, `/usr/lib/vst3`, `/usr/lib64/vst3`,
  `/usr/local/lib/vst3` on Linux.
- **Loading:** `bundleEntry` on macOS, `ModuleEntry` on Linux, through
  `libloading`.
- **`Vst3Instance`:** the realtime `process` does not allocate (checked by
  a test with a counting allocator). It supports a separate controller
  class and a single-component effect; parameter changes go through a
  queue; `IMessage` and `IAttributeList` are implemented for the plugins
  that need them.
- **`Instances`** pools: *live* instances persist for the preview, one per
  placement; *offline* instances are made per render, so an export starts
  from the saved state; a *negative* cache keeps a plugin that failed to
  load from being retried every pass.
- **`installed()` / `set_installed()`:** the scan runs on first use, so a
  headless render works without the app. The app seeds the list from its
  cache in the settings and replaces it after a rescan.

### 2.4 Editors (`vv-plugin`)

- macOS: an `NSWindow` hosting the plugin's view, on the main thread only.
- Linux: an X11 window with an `IRunLoop` for the plugin's timers and
  file descriptors. Venturi runs as a Wayland client and VST3 has no
  Wayland platform type, so the editor is an X11 window through XWayland
  (`kPlatformTypeX11EmbedWindowID`). On a Wayland session without XWayland,
  opening an editor fails with an error; audio is unaffected.
- Wake-ups are event-driven: a parameter edit, a `restartComponent`, a
  resize or a close wakes the app through a `Waker`. There is no periodic
  repaint on macOS; Linux wakes at the plugin's timer deadlines only.

### 2.5 Session and app

- `vv-session`: export and analyses create the offline instances lazily,
  once per render; a project without plugins makes none.
- `vv-app`: the "+" menu has a "VST3 plugins" submenu grouped by vendor,
  with Rescan. The scan runs in the background and its result is cached in
  the settings. A row shows a *missing* badge when the plugin is not
  installed, and a *latency* badge with its latency. Clicking a row opens
  the editor. Each gesture (add, remove, move, bypass, a parameter edit in
  the editor) is one undoable command.

### 2.6 Tests

- `vv-plugin-fixture` (new, `publish = false`): a stereo gain VST3 built as
  a cdylib. `vv_plugin::testing::fixture_bundle()` builds it into its own
  target directory and assembles the bundle, so the tests go through the
  real load, scan and process path.
- `vv-plugin` tests cover scan, module loading, instance processing, the
  pools and the editor's refusal paths. The editor is not opened in a test:
  it needs a display and the main thread.
- CI: the Linux job runs the fixture tests through `cargo test --workspace`;
  the `vv-plugin-macos` job runs `cargo test -p vv-plugin` on `macos-14`.

## 3. Decisions

- **One instance per placement, not per effect.** The same plugin on a
  track and in a compound clip keeps two states, one per place it plays.
- **Export starts from the saved state.** Offline instances are created
  per render, not reused from the live pool, so an export does not depend
  on what the preview did.
- **Realtime safety is checked, not assumed.** The no-allocation claim is a
  test; a `process` that finds the plugin busy passes the audio through.
- **Editors are event-driven.** A periodic repaint kept the app at a
  constant CPU cost while a plugin window was open.

## 4. Known limits

- Latency is shown as a badge only: tracks are not delay-compensated, so
  a plugin with latency shifts its track against the others.
- Export has no tail: audio a reverb produces after the timeline end is
  cut.
- Effects are per strip. A clip cannot have its own plugin (REAPER's take
  FX).
- Only VST3. CLAP, LV2 and AU are not loaded.
- Plugins run in the Venturi process: a crashing plugin takes the app down.

## 5. What's left

1. Latency compensation across tracks (the badge becomes a delay).
2. Tail at export end: render past the timeline end until the plugins
   are silent or a limit is hit.
3. Per-clip effects (take FX).
4. CLAP, then LV2 and AU, behind the same `PluginProcessor` seam.
5. Out-of-process hosting, so a crashing plugin does not take the app down.
6. Edits that do not go through a parameter gesture (a preset load) are
   recorded at the next gesture, not when they happen.
7. Linux editors: not tested by hand; a close or resize is noticed at the
   next repaint when the plugin has no timer. The run loop is pumped by
   waking a full egui frame at the next timer deadline (every 16 ms while
   fd handlers exist): a lighter frame that only pumps the run loop would
   cost less.
8. OpenFX video plugins, which need the video pipeline and are not on this
   plan.
9. Offline instances (export, level and stretch workers) are created,
   activated and dropped on worker threads, though VST3 names the UI thread
   for those calls. A plugin that needs the main thread (JUCE MessageManager,
   AppKit in initialize, dispatch_sync to main) may fail or hang there; a
   failed creation plays dry and is logged once per plugin class. Running
   them on the UI thread would make a worker wait for a UI frame, which does
   not run while the window is minimized, and the headless MCP has no UI
   thread.
