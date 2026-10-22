//! Android shell wrapping [`soundcraft_ui_egui::SoundApp`] with touch-twin filtering, GPU
//! acceleration startup verification, SAF/Intent file import/export routing, and the Android
//! file/permission modal overlay.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};

use serde_json::json;
use soundcraft_engine::{Engine, io};
use soundcraft_ui_egui::dialogs::{self, Dialog};
use soundcraft_ui_egui::{SoundApp, UiState};

use crate::file_dialog::{self, FileDialogOutcome};
use crate::gpu_startup::{Plan, SharedSentinel};
use crate::permissions;
use crate::services;

/// The `eframe::App` wrapper for SoundCraft on Android.
pub struct AndroidShell {
    pub app: SoundApp,
    started_sentinel: SharedSentinel,
    gpu_note: Arc<Mutex<Option<String>>>,
    was_bouncing: Option<String>,
}

impl AndroidShell {
    /// Construct the Android shell inside `eframe::run_native`'s creation callback, logging the
    /// active `wgpu` hardware GPU adapter and initializing realtime `cpal` audio playback.
    pub fn create(cc: &eframe::CreationContext<'_>, plan: &Plan, started_sentinel: SharedSentinel, gpu_note: Arc<Mutex<Option<String>>>) -> Self {
        let prefs_path = services::prefs_file();
        let prefs: UiState = services::load_ui_prefs(&prefs_path).ok().flatten().unwrap_or_default();
        let empty = std::env::var_os("SOUNDCRAFT_EMPTY").is_some();
        let no_audio = std::env::var_os("SOUNDCRAFT_NO_AUDIO").is_some();
        let engine = if empty { Engine::default() } else { soundcraft_engine::demo::demo_engine() };
        let player =
            if no_audio { None } else { Some(soundcraft_playback::Player::with_config(Arc::new(engine.session().clone()), &prefs.audio)) };

        let mut app = SoundApp::new(engine, player, services::android_services());
        let autosave = services::autosave_dir();
        app.leftovers = soundcraft_engine::autosave::leftovers(&autosave);
        app.autosave_dir = Some(autosave);
        app.preset_dir = Some(services::presets_dir());
        app.extra.ui_scale = prefs.ui_scale;
        app.ui = prefs;

        if let Some(rs) = &cc.wgpu_render_state {
            let adapter_info = rs.adapter.get_info();
            log::info!(
                "soundcraft-android: wgpu adapter {:?} ({:?}, backend {:?}, max_tex_2d={})",
                adapter_info.name,
                adapter_info.device_type,
                adapter_info.backend,
                rs.device.limits().max_texture_dimension_2d
            );
            if let Some(reason) = plan.reason.as_deref() {
                app.ui.status = format!("GPU: {} ({reason})", adapter_info.name);
            }
        }
        if let Some(note) = gpu_note.lock().unwrap_or_else(PoisonError::into_inner).clone()
            && app.ui.status.is_empty()
        {
            app.ui.status = note;
        }

        Self { app, started_sentinel, gpu_note, was_bouncing: None }
    }

    /// Normalize relative `.` paths in newly opened `Dialog::Bounce` or `Dialog::PathPrompt` to
    /// Android's writable `default_save_dir()` or `default_open_dir()`.
    pub fn normalize_open_dialog_paths(&mut self) {
        let dirs = permissions::current_dirs();
        let is_rel = |p: &str| p == "./" || p == "." || p.starts_with("./") || p.starts_with(".\\") || (!Path::new(p).is_absolute() && !p.starts_with('/'));
        match &mut self.app.dialogs.open {
            Some(Dialog::Bounce { path, .. }) => {
                if is_rel(path.as_str()) {
                    let file_name = Path::new(path.as_str()).file_name().and_then(|n| n.to_str()).unwrap_or("Bounce.wav");
                    let target = dirs.default_save_dir().join(file_name);
                    *path = target.to_string_lossy().into_owned();
                } else {
                    let (resolved, _) = permissions::resolve_export_target(Path::new(path.as_str()), &dirs);
                    *path = resolved.to_string_lossy().into_owned();
                }
            }
            Some(Dialog::PathPrompt { cmd, path, key, .. }) => {
                let importing = cmd.contains("import") || cmd.contains("open");
                if is_rel(path.as_str()) {
                    if importing || key == "dir" {
                        let base = if importing { dirs.default_open_dir() } else { dirs.default_save_dir() };
                        *path = base.to_string_lossy().into_owned();
                    } else {
                        let rel = path.trim_start_matches("./").trim_start_matches(".\\");
                        let target = dirs.default_save_dir().join(if rel.is_empty() { "Untitled.scraft" } else { rel });
                        *path = target.to_string_lossy().into_owned();
                    }
                }
            }
            _ => {}
        }
    }

    /// Apply completed outcomes from the Android File Dialog or SAF system picker.
    pub fn process_file_dialog_outcomes(&mut self) {
        for outcome in file_dialog::take_outcomes() {
            match outcome {
                FileDialogOutcome::OpenPaths { purpose, paths } => {
                    let Some(first) = paths.first() else { continue };
                    if let Some(Dialog::PathPrompt { path, .. }) = &mut self.app.dialogs.open {
                        path.clone_from(first);
                        continue;
                    }
                    for p in &paths {
                        self.open_or_import_path(&purpose, p);
                    }
                }
                FileDialogOutcome::OpenContents { purpose, files } => {
                    for (name, bytes) in files {
                        if let Some(Dialog::PathPrompt { path, .. }) = &mut self.app.dialogs.open
                            && let Ok(staged) = services::stage_imported_bytes(&name, &bytes)
                        {
                            *path = staged.to_string_lossy().into_owned();
                            continue;
                        }
                        self.open_or_import_bytes(&purpose, &name, bytes);
                    }
                }
                FileDialogOutcome::SaveTo { purpose, path: chosen } => {
                    let dirs = permissions::current_dirs();
                    let (resolved, note) = permissions::resolve_export_target(Path::new(&chosen), &dirs);
                    let resolved_str = resolved.to_string_lossy().into_owned();
                    if let Some(note) = note {
                        self.app.ui.status = note;
                    }
                    match &mut self.app.dialogs.open {
                        Some(Dialog::Bounce { path, format, .. }) if purpose == "file.bounce_mix" || purpose.is_empty() => {
                            *path = resolved_str;
                            match Path::new(path.as_str()).extension().and_then(|x| x.to_str()).map(str::to_ascii_lowercase).as_deref() {
                                Some("wav") => *format = "wav".into(),
                                Some("aif" | "aiff") => *format = "aiff".into(),
                                Some("flac") => *format = "flac".into(),
                                _ => {}
                            }
                        }
                        Some(Dialog::PathPrompt { path, .. }) => {
                            *path = resolved_str;
                        }
                        _ => {}
                    }
                }
                FileDialogOutcome::FolderTo { path: chosen, .. } => {
                    if let Some(Dialog::PathPrompt { path, .. }) = &mut self.app.dialogs.open {
                        *path = chosen;
                    }
                }
            }
        }
    }

    /// Open or import a local file path based on `purpose` or its file extension.
    pub fn open_or_import_path(&mut self, purpose: &str, p: &str) {
        if purpose == "mix.sampler_load" {
            if let Some(track) = self.sampler_target_track() {
                let _ = self.app.run("mix.sampler_load", json!({"track": track.0, "path": p}));
                return;
            }
        }
        let lower = p.to_ascii_lowercase();
        let r = if purpose == "session.open" || lower.ends_with(".scraft") || lower.ends_with(".scraftz") {
            if lower.ends_with(".scraftz")
                && let Ok(bytes) = std::fs::read(p)
            {
                let name = Path::new(p).file_name().and_then(|n| n.to_str()).unwrap_or("Session.scraftz").to_string();
                dialogs::open_session_bytes(&mut self.app, name, bytes);
                Ok(serde_json::Value::Null)
            } else {
                dialogs::run_or_ask(&mut self.app, "session.open", json!({"path": p}))
            }
        } else if purpose == "file.import_midi" || lower.ends_with(".mid") || lower.ends_with(".midi") || lower.ends_with(".smf") {
            self.app.run("file.import_midi", json!({"path": p}))
        } else if purpose == "file.import_video"
            || lower.ends_with(".mov")
            || lower.ends_with(".m4v")
            || lower.ends_with(".avi")
        {
            let at = self.app.engine.session().edit.selection.start;
            self.app.run("file.import_video", json!({"path": p, "at": at}))
        } else if purpose == "file.import_clip_groups" || lower.ends_with(".scgrp") {
            let at = self.app.engine.session().edit.selection.start;
            self.app.run("file.import_clip_groups", json!({"path": p, "at": at}))
        } else {
            let at = self.app.engine.session().edit.selection.start;
            self.app.run("file.import_audio", json!({"path": p, "at": at}))
        };
        if let Err(e) = r {
            log::warn!("{p}: {e}");
        }
    }

    /// Open or import raw file bytes received from Android SAF or Intent (`ACTION_VIEW` /
    /// `ACTION_SEND`).
    pub fn open_or_import_bytes(&mut self, purpose: &str, name: &str, bytes: Vec<u8>) {
        if purpose == "mix.sampler_load"
            && let Some(track) = self.sampler_target_track()
        {
            match io::load_sampler_bytes(&mut self.app.engine, track, name, &bytes) {
                Ok(_) => self.app.ui.status = format!("{name} loaded into the sampler"),
                Err(e) => self.app.ui.status = format!("{name}: {e}"),
            }
            return;
        }
        let lower = name.to_ascii_lowercase();
        if purpose == "session.open" || lower.ends_with(".scraft") || lower.ends_with(".scraftz") {
            dialogs::open_session_bytes(&mut self.app, name.to_string(), bytes);
            return;
        }
        let at = self.app.engine.session().edit.selection.start.max(0);
        if purpose == "file.import_midi" || lower.ends_with(".mid") || lower.ends_with(".midi") || lower.ends_with(".smf") {
            let before = self.app.engine.session_arc();
            match io::import_midi_bytes(&mut self.app.engine, &bytes, at, true) {
                Ok(_) => {
                    self.app.engine.push_undo("Import MIDI", before);
                    self.app.ui.status = format!("Imported {name}");
                }
                Err(e) => self.app.ui.status = e.to_string(),
            }
            return;
        }
        if purpose == "file.import_video" || lower.ends_with(".mov") || lower.ends_with(".m4v") || lower.ends_with(".avi") {
            let staged_path = services::stage_imported_bytes(name, &bytes)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_else(|_| name.to_string());
            let before = self.app.engine.session_arc();
            match io::import_video_bytes(&mut self.app.engine, &staged_path, bytes, at) {
                Ok(_) => {
                    self.app.engine.push_undo("Import Video", before);
                    self.app.ui.status = format!("Imported {name}");
                }
                Err(e) => self.app.ui.status = e.to_string(),
            }
            return;
        }
        let staged_path = services::stage_imported_bytes(name, &bytes).ok().and_then(|p| p.to_str().map(str::to_string));
        let before = self.app.engine.session_arc();
        match io::import_audio_bytes(&mut self.app.engine, name, &bytes, staged_path.as_deref(), None, at) {
            Ok(_) => {
                self.app.engine.push_undo("Import Audio", before);
                self.app.ui.status = format!("Imported {name}");
            }
            Err(e) => self.app.ui.status = e.to_string(),
        }
    }

    fn sampler_target_track(&self) -> Option<soundcraft_model::TrackId> {
        let s = self.app.engine.session();
        self.app
            .ui
            .plugin_windows
            .iter()
            .find_map(|(t, slot)| {
                (*slot == soundcraft_mix::INSTRUMENT_SLOT
                    && s.track(*t).and_then(|x| x.instrument.as_ref()).is_some_and(|i| i.plugin == soundcraft_ui_egui::sampler_window::SAMPLER))
                .then_some(*t)
            })
            .or_else(|| s.edit.selected_tracks.first().copied())
    }
}

/// Deduplicate simultaneous Android touch + pointer twin events within the same frame so tapping a
/// menu item does not immediately close the menu.
pub fn drop_touch_twins(raw: &mut egui::RawInput) {
    let has_touch = raw.events.iter().any(|e| matches!(e, egui::Event::Touch { .. }));
    if !has_touch {
        return;
    }
    let mut seen_press = false;
    let mut seen_release = false;
    raw.events.retain(|ev| match ev {
        egui::Event::PointerButton { pressed: true, .. } => {
            if seen_press {
                return false;
            }
            seen_press = true;
            true
        }
        egui::Event::PointerButton { pressed: false, .. } => {
            if seen_release {
                return false;
            }
            seen_release = true;
            true
        }
        _ => true,
    });
}

impl eframe::App for AndroidShell {
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw: &mut egui::RawInput) {
        drop_touch_twins(raw);
        self.app.raw_input_hook(raw);
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Some(s) = self.started_sentinel.lock().unwrap_or_else(PoisonError::into_inner).take() {
            s.finish();
        }
        let _ = &self.gpu_note;

        // Handle incoming files from Android ACTION_VIEW / ACTION_EDIT / ACTION_SEND intents
        for (name, bytes) in services::take_incoming_files() {
            self.open_or_import_bytes("", &name, bytes);
        }

        // Handle completed Android File Dialog / SAF outcomes
        self.process_file_dialog_outcomes();
        self.normalize_open_dialog_paths();

        // Track active bounce path so we can notify MediaScannerConnection when it finishes
        if let Some(b) = &self.app.bounce {
            self.was_bouncing = Some(b.path().to_string_lossy().into_owned());
        }

        self.app.guarded(|app| app.logic(ctx));

        if self.app.bounce.is_none()
            && let Some(finished_bounce) = self.was_bouncing.take()
            && Path::new(&finished_bounce).is_file()
        {
            file_dialog::notify_media_scanner(&finished_bounce);
        }

        // Normalize any dialog opened during this frame's logic pass
        self.normalize_open_dialog_paths();

        // Handle files dropped onto the window
        let dropped: Vec<String> =
            self.app.take_dropped_files(ctx).iter().map(|f| f.path().to_string_lossy().into_owned()).filter(|s| !s.is_empty()).collect();
        for p in dropped {
            self.open_or_import_path("", &p);
        }

        soundcraft_ui_egui::fit_window_to_screen(ctx);
        self.app.ui.ui_scale = self.app.extra.ui_scale;
        if self.app.take_recent_changed() {
            let _ = services::save_ui_prefs(&services::prefs_file(), &self.app.ui);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.app.guarded(|app| app.ui(ui));
        file_dialog::render_modal(ui.ctx());
    }

    fn on_exit(&mut self) {
        self.app.close_autosave();
        let _ = services::save_ui_prefs(&services::prefs_file(), &self.app.ui);
        self.app.player = None;
        soundcraft_engine::shutdown_plugin_hosts();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_open_dialog_paths_rewrites_relative_bounce_and_prompt_paths_to_android_dirs() {
        let base = std::env::temp_dir().join(format!("soundcraft-android-shell-test-{}", std::process::id()));
        let internal = base.join("internal");
        let external = base.join("external");
        let _ = std::fs::create_dir_all(&internal);
        let _ = std::fs::create_dir_all(&external);
        permissions::init_dirs(Some(internal), Some(external));

        let app = SoundApp::new(Engine::default(), None, services::android_services());
        let mut shell = AndroidShell {
            app,
            started_sentinel: Arc::new(Mutex::new(None)),
            gpu_note: Arc::default(),
            was_bouncing: None,
        };

        shell.app.dialogs.open = Some(Dialog::Bounce {
            path: "./Untitled Bounce.wav".into(),
            format: "wav".into(),
            bit_depth: "24".into(),
            normalize: false,
        });
        shell.normalize_open_dialog_paths();
        match &shell.app.dialogs.open {
            Some(Dialog::Bounce { path, .. }) => {
                assert!(Path::new(path).is_absolute(), "bounce path must be absolute: {path}");
                assert!(path.ends_with("Untitled Bounce.wav"), "{path}");
            }
            other => panic!("unexpected dialog: {other:?}"),
        }

        let _ = std::fs::remove_dir_all(&base);
    }
}
