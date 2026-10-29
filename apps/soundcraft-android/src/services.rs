//! Android platform services: file import/export with permission fallback, SAF dialog hooks,
//! crash-recovery autosave directory, presets directory, and preferences (`ui.json`).

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

use soundcraft_ui_egui::{Services, UiState};

use crate::file_dialog::{self, FileDialogRequest};
use crate::permissions;

/// Global inbox for files opened via Android `Intent.ACTION_VIEW` / `Intent.ACTION_EDIT` /
/// `Intent.ACTION_SEND` (`(file_name, bytes)`).
static INCOMING_FILES: OnceLock<Mutex<Vec<(String, Vec<u8>)>>> = OnceLock::new();

fn incoming_lock() -> &'static Mutex<Vec<(String, Vec<u8>)>> {
    INCOMING_FILES.get_or_init(|| Mutex::new(Vec::new()))
}

/// Enqueue a file (`name`, `bytes`) received from an Android Intent (`ACTION_VIEW`, `ACTION_EDIT`,
/// `ACTION_SEND`) so `AndroidShell` imports or opens it on the next frame.
pub fn enqueue_incoming_file(name: String, bytes: Vec<u8>) {
    incoming_lock().lock().unwrap_or_else(PoisonError::into_inner).push((name, bytes));
}

/// Drain all queued incoming intent files.
pub fn take_incoming_files() -> Vec<(String, Vec<u8>)> {
    std::mem::take(&mut *incoming_lock().lock().unwrap_or_else(PoisonError::into_inner))
}

/// Per-user settings directory on Android (`internal_data` from `AndroidApp`, or
/// `SOUNDCRAFT_CONFIG_DIR`).
pub fn config_dir() -> PathBuf {
    permissions::current_dirs().internal_data
}

/// Path to the UI & audio preferences file (`ui.json`).
pub fn prefs_file() -> PathBuf {
    config_dir().join("ui.json")
}

/// Path to the crash-recovery autosave folder (`Autosave/`).
pub fn autosave_dir() -> PathBuf {
    config_dir().join("Autosave")
}

/// Path to the user plugin presets folder (`Presets/`).
pub fn presets_dir() -> PathBuf {
    config_dir().join("Presets")
}

const MAX_UI_BYTES: u64 = 4 * 1024 * 1024;

/// Load bounded [`UiState`] preferences from `path`, recovering unreadable docking layouts
/// independently of other preferences.
pub fn load_ui_prefs(path: &Path) -> Result<Option<UiState>, String> {
    use std::io::Read as _;
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Settings could not be read: {error}")),
    };
    let mut bytes = Vec::new();
    file.take(MAX_UI_BYTES + 1).read_to_end(&mut bytes).map_err(|error| format!("Settings could not be read: {error}"))?;
    if bytes.len() as u64 > MAX_UI_BYTES {
        return Err("Settings file exceeds the 4 MiB limit".into());
    }
    let mut saved: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| format!("Settings could not be restored: {error}"))?;
    soundcraft_ui_egui::docking::reset_old_layouts(&mut saved);
    if let Some(fields) = saved.as_object_mut() {
        for (field, mix) in [("docking", false), ("mix_docking", true)] {
            let unreadable = fields.get(field).is_some_and(|value| {
                match serde_json::from_value::<soundcraft_ui_egui::docking::Workspace>(value.clone()) {
                    Ok(workspace) => {
                        if mix { soundcraft_ui_egui::docking::validate_mix(&workspace) } else { soundcraft_ui_egui::docking::validate(&workspace) }.is_err()
                    }
                    Err(_) => true,
                }
            });
            if unreadable {
                fields.remove(field);
            }
        }
    }
    let state: UiState = serde_json::from_value(saved).map_err(|error| format!("Settings could not be restored: {error}"))?;
    soundcraft_ui_egui::docking::validate(&state.docking)?;
    soundcraft_ui_egui::docking::validate_mix(&state.mix_docking)?;
    Ok(Some(state))
}

/// Save [`UiState`] preferences atomically to `path`.
pub fn save_ui_prefs(path: &Path, ui: &UiState) -> Result<(), String> {
    use std::io::Write as _;
    soundcraft_ui_egui::docking::validate(&ui.docking)?;
    soundcraft_ui_egui::docking::validate_mix(&ui.mix_docking)?;
    let mut saved = ui.clone();
    saved.plugin_windows.clear();
    saved.status.clear();
    saved.docking_version = soundcraft_ui_egui::docking::LAYOUT_VERSION;
    let bytes = serde_json::to_vec_pretty(&saved).map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_UI_BYTES {
        return Err("Settings exceed the 4 MiB limit".into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error| error.to_string())?.as_nanos();
    let temporary = path.with_extension(format!("json.{}.{nonce}.tmp", std::process::id()));
    let mut file =
        std::fs::OpenOptions::new().write(true).create_new(true).open(&temporary).map_err(|error| format!("Settings could not be saved: {error}"))?;
    let written = (|| -> std::io::Result<()> {
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    written.map_err(|error| format!("Settings could not be saved: {error}"))
}

/// Write `bytes` crash-safely to `path`, automatically falling back to the writable Android
/// `Download`, `Music`, or `Exports` directory if Android scoped storage restricts direct writes
/// to `path`, and notifying the Android `MediaScannerConnection` so exported audio/MIDI/sessions
/// appear in system media/file apps.
pub fn write_with_android_permissions(path: &Path, bytes: &[u8]) -> Result<(PathBuf, Option<String>), String> {
    let dirs = permissions::current_dirs();
    let (target, fallback_note) = permissions::resolve_export_target(path, &dirs);
    if let Some(parent) = target.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = target.with_extension(format!("part.{}", std::process::id()));
    soundcraft_engine::io::replace_file(&tmp, &target, bytes).map_err(|e| e.to_string())?;
    if let Some(target_str) = target.to_str() {
        file_dialog::notify_media_scanner(target_str);
    }
    Ok((target, fallback_note))
}

/// Stage incoming SAF or Intent bytes into `<external_data>/Imports/<file_name>` (or
/// `<internal_data>/Imports/<file_name>`) so path-based engine commands can open them directly.
pub fn stage_imported_bytes(file_name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let dirs = permissions::current_dirs();
    let base = dirs.external_data.as_ref().unwrap_or(&dirs.internal_data).join("Imports");
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let safe_name = soundcraft_engine::io::sanitize_name(file_name);
    let dest = base.join(if safe_name.is_empty() { "imported.wav".into() } else { safe_name });
    std::fs::write(&dest, bytes).map_err(|e| e.to_string())?;
    Ok(dest)
}

/// Construct the Android [`Services`] table for [`soundcraft_ui_egui::SoundApp`].
///
/// Clicking `Browse…` in `Dialog::PathPrompt` or `Dialog::Bounce`, or `Load Sample…` in the
/// Sampler window, opens the non-blocking touch-friendly Android File Dialog modal; when the user
/// picks a file, save path, or folder, `AndroidShell` applies the outcome on the UI thread.
pub fn android_services() -> Services {
    Services {
        pick_open: Some(Box::new(|purpose: &str, exts: &[&str]| {
            let extensions = file_dialog::extensions_for_purpose(purpose, exts);
            file_dialog::show_file_dialog(FileDialogRequest::Open {
                purpose: purpose.to_string(),
                initial_dir: None,
                extensions: Some(extensions),
                multiple: false,
            });
            None
        })),
        pick_save: Some(Box::new(|purpose: &str, default: &Path| {
            file_dialog::show_file_dialog(FileDialogRequest::Save {
                purpose: purpose.to_string(),
                suggested: default.to_string_lossy().into_owned(),
            });
            None
        })),
        pick_folder: Some(Box::new(|start: &Path| {
            let start_str = start.to_string_lossy().into_owned();
            file_dialog::show_file_dialog(FileDialogRequest::Folder {
                purpose: "folder".into(),
                initial_dir: (!start_str.is_empty()).then_some(start_str),
            });
            None
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soundcraft_audio_io::{AudioBuffer, EncodeOptions};

    #[test]
    fn android_services_roundtrip_audio_export_and_import_with_permissions() {
        let dir = std::env::temp_dir().join(format!("soundcraft-android-svc-test-{}", std::process::id()));
        let internal = dir.join("internal");
        let external = dir.join("external");
        let shared = dir.join("shared");
        let _ = std::fs::create_dir_all(shared.join("Download"));
        permissions::init_dirs(Some(internal), Some(external));

        let buf = AudioBuffer { sample_rate: 48_000, channels: vec![vec![0.25f32; 480], vec![-0.25f32; 480]] };
        let wav_bytes = soundcraft_audio_io::encode(&buf, &EncodeOptions::default()).unwrap();

        let out_path = shared.join("Download").join("android_bounce.wav");
        let (written_path, note) = write_with_android_permissions(&out_path, &wav_bytes).unwrap();
        assert_eq!(written_path, out_path);
        assert!(note.is_none());
        assert!(out_path.is_file(), "exported WAV should be written to Download directory");

        let mut engine = soundcraft_engine::Engine::default();
        let read_back = std::fs::read(&out_path).unwrap();
        soundcraft_engine::io::import_audio_bytes(&mut engine, "android_bounce.wav", &read_back, out_path.to_str(), None, 0).unwrap();
        assert_eq!(engine.session().tracks.len(), 1);
        assert_eq!(engine.session().sources.len(), 1);
        assert_eq!(engine.session().sources[0].frames, 480);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ui_prefs_roundtrip_cleanly() {
        let dir = std::env::temp_dir().join(format!("soundcraft-android-prefs-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("ui.json");
        let mut ui = UiState::default();
        ui.narrow_mix = true;
        ui.ui_scale = 1.25;
        save_ui_prefs(&path, &ui).unwrap();
        let loaded = load_ui_prefs(&path).unwrap().unwrap();
        assert!(loaded.narrow_mix);
        assert_eq!(loaded.ui_scale, 1.25);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
