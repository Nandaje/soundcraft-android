//! Android file, media & audio storage permissions and directory resolution for import and export.
//!
//! Android's storage & audio permission model spans three generations:
//! - **Android 8–10 (API 26–29)**: `READ_EXTERNAL_STORAGE`, `WRITE_EXTERNAL_STORAGE`, and
//!   `RECORD_AUDIO` runtime permissions grant direct access to
//!   `/storage/emulated/0/{Download,Music,Movies,Documents}` and audio capture.
//! - **Android 11–12L (API 30–32)**: Scoped storage + `READ_EXTERNAL_STORAGE` + `RECORD_AUDIO`,
//!   with optional `MANAGE_EXTERNAL_STORAGE` (`ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION`) for
//!   full session folder access (`.scraft`, `.scraftz`, `Audio Files/`, `Video Files/`, `Backups/`,
//!   stem bounces, and clip exports).
//! - **Android 13+ (API 33+)**: Granular media permissions (`READ_MEDIA_AUDIO`,
//!   `READ_MEDIA_VIDEO`) + `RECORD_AUDIO` + Storage Access Framework (`ACTION_OPEN_DOCUMENT` /
//!   `ACTION_CREATE_DOCUMENT` / `ACTION_OPEN_DOCUMENT_TREE`) + optional
//!   `MANAGE_EXTERNAL_STORAGE`.
//!
//! App-specific directories (`internal_data_dir` and `external_data_dir`) are always readable and
//! writable without runtime permissions on all Android versions.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock, PoisonError};

/// Android manifest permission identifiers required for full DAW import/export and recording.
pub const PERM_READ_EXTERNAL_STORAGE: &str = "android.permission.READ_EXTERNAL_STORAGE";
pub const PERM_WRITE_EXTERNAL_STORAGE: &str = "android.permission.WRITE_EXTERNAL_STORAGE";
pub const PERM_READ_MEDIA_AUDIO: &str = "android.permission.READ_MEDIA_AUDIO";
pub const PERM_READ_MEDIA_VIDEO: &str = "android.permission.READ_MEDIA_VIDEO";
pub const PERM_MANAGE_EXTERNAL_STORAGE: &str = "android.permission.MANAGE_EXTERNAL_STORAGE";
pub const PERM_RECORD_AUDIO: &str = "android.permission.RECORD_AUDIO";

/// Runtime permissions to request for a given Android SDK version (`Build.VERSION.SDK_INT`).
pub fn required_runtime_permissions(sdk_int: i32) -> &'static [&'static str] {
    if sdk_int >= 33 {
        &[PERM_READ_MEDIA_AUDIO, PERM_READ_MEDIA_VIDEO, PERM_RECORD_AUDIO]
    } else if sdk_int >= 30 {
        &[PERM_READ_EXTERNAL_STORAGE, PERM_RECORD_AUDIO]
    } else {
        &[PERM_READ_EXTERNAL_STORAGE, PERM_WRITE_EXTERNAL_STORAGE, PERM_RECORD_AUDIO]
    }
}

/// Current state of Android file/storage permissions for import and export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoragePermissionStatus {
    /// Whether read access to shared audio/video/storage (`READ_MEDIA_AUDIO` or
    /// `READ_EXTERNAL_STORAGE`) is granted.
    pub read_granted: bool,
    /// Whether direct write access to shared external storage (`WRITE_EXTERNAL_STORAGE` or
    /// `MANAGE_EXTERNAL_STORAGE` or writable `/storage/emulated/0/Download`) is granted.
    pub write_granted: bool,
    /// Whether full `MANAGE_EXTERNAL_STORAGE` (All Files Access on API 30+) is granted.
    pub all_files_access: bool,
    /// Device Android SDK version (`ro.build.version.sdk`), e.g. 34 for Android 14.
    pub sdk_int: i32,
}

impl Default for StoragePermissionStatus {
    fn default() -> Self {
        Self { read_granted: false, write_granted: false, all_files_access: false, sdk_int: 34 }
    }
}

impl StoragePermissionStatus {
    /// Human-readable summary shown in the Android File Browser header.
    pub fn summary(&self) -> &'static str {
        if self.all_files_access || (self.read_granted && self.write_granted) {
            "Storage Permission: Full Import/Export Access Granted"
        } else if self.read_granted {
            "Storage Permission: Media Read Granted (Exports use SAF / App Storage / Downloads)"
        } else {
            "Storage Permission: App Storage & System File Picker (Tap 'Grant Storage Access' for shared folders)"
        }
    }
}

/// Standard directories used by SoundCraft on Android.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AndroidDirs {
    /// App-private internal storage (`/data/user/0/ai.storyteller.soundcraft/files`).
    /// Always readable/writable without runtime permissions; holds `ui.json`, `Autosave/`,
    /// `Presets/`, and `logs/`.
    pub internal_data: PathBuf,
    /// App-specific external storage (`/storage/emulated/0/Android/data/ai.storyteller.soundcraft/files`).
    /// Visible to the user via USB / Android Files app and always writable without permissions.
    pub external_data: Option<PathBuf>,
    /// Shared user storage root (`/storage/emulated/0`).
    pub shared_root: PathBuf,
    /// Shared `Download` directory (`/storage/emulated/0/Download`).
    pub downloads: PathBuf,
    /// Shared `Music` directory (`/storage/emulated/0/Music`).
    pub music: PathBuf,
    /// Shared `Movies` directory (`/storage/emulated/0/Movies`).
    pub movies: PathBuf,
    /// Shared `Documents` directory (`/storage/emulated/0/Documents`).
    pub documents: PathBuf,
}

impl AndroidDirs {
    /// Resolve Android storage directories from the activity paths and environment.
    pub fn resolve(internal: Option<PathBuf>, external: Option<PathBuf>, env_config: Option<PathBuf>, env_external: Option<PathBuf>) -> Self {
        let internal_data = env_config.or(internal).unwrap_or_else(|| PathBuf::from("/data/user/0/ai.storyteller.soundcraft/files"));
        let shared_root = env_external.filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| PathBuf::from("/storage/emulated/0"));
        let downloads = shared_root.join("Download");
        let music = shared_root.join("Music");
        let movies = shared_root.join("Movies");
        let documents = shared_root.join("Documents");
        Self { internal_data, external_data: external, shared_root, downloads, music, movies, documents }
    }

    /// Directory where exported files and sessions can always be written even if shared storage
    /// permission is denied (`<external_data>/Exports`, else `<internal_data>/Exports`).
    pub fn fallback_exports_dir(&self) -> PathBuf {
        self.external_data.as_ref().unwrap_or(&self.internal_data).join("Exports")
    }

    /// Default directory to open when importing audio, MIDI, video, or sessions.
    pub fn default_open_dir(&self) -> PathBuf {
        for candidate in [&self.downloads, &self.music, &self.documents, &self.movies] {
            if candidate.is_dir() && std::fs::read_dir(candidate).is_ok() {
                return candidate.clone();
            }
        }
        self.external_data.clone().unwrap_or_else(|| self.internal_data.clone())
    }

    /// Default directory to suggest when exporting/bouncing/saving files.
    pub fn default_save_dir(&self) -> PathBuf {
        for candidate in [&self.downloads, &self.music, &self.documents] {
            if is_dir_writable(candidate) {
                return candidate.clone();
            }
        }
        let exports = self.fallback_exports_dir();
        let _ = std::fs::create_dir_all(&exports);
        exports
    }

    /// Named bookmark locations shown in the Android File Browser sidebar.
    pub fn bookmarks(&self) -> Vec<(&'static str, PathBuf)> {
        let mut list = vec![
            ("Downloads", self.downloads.clone()),
            ("Music", self.music.clone()),
            ("Documents", self.documents.clone()),
            ("Movies", self.movies.clone()),
            ("Shared Storage", self.shared_root.clone()),
        ];
        if let Some(ext) = &self.external_data {
            list.push(("App External", ext.clone()));
            list.push(("App Exports", ext.join("Exports")));
        }
        list.push(("App Internal", self.internal_data.clone()));
        list
    }
}

/// Check if a directory can be listed (read permission check).
pub fn is_dir_readable(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok()
}

/// Check if a directory allows creating files (write permission check).
pub fn is_dir_writable(dir: &Path) -> bool {
    if !dir.is_dir() {
        let parent_exists = dir.parent().is_some_and(Path::is_dir);
        if !parent_exists || std::fs::create_dir_all(dir).is_err() {
            return false;
        }
    }
    let probe = dir.join(format!(".soundcraft_perm_probe_{}", std::process::id()));
    match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&probe) {
        Ok(f) => {
            drop(f);
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// Resolve the actual path to write an exported file or session to.
///
/// If `requested_path`'s parent directory is writable (e.g. user granted storage permission or
/// chose a writable `Download` / `Music` / SAF staging path), returns `(requested_path, None)`.
/// If Android scoped storage blocks writing directly to `requested_path`, falls back to
/// `<downloads>/<file_name>` or `<fallback_exports_dir>/<file_name>` and returns
/// `(fallback_path, Some(notice))` so the export or bounce succeeds without data loss.
pub fn resolve_export_target(requested_path: &Path, dirs: &AndroidDirs) -> (PathBuf, Option<String>) {
    let parent = requested_path.parent().filter(|p| !p.as_os_str().is_empty() && *p != Path::new("."));
    if let Some(p) = parent
        && is_dir_writable(p)
    {
        return (requested_path.to_path_buf(), None);
    }
    let file_name = requested_path.file_name().map(|n| n.to_os_string()).unwrap_or_else(|| std::ffi::OsString::from("Untitled.wav"));
    // Try shared Downloads or Music first before falling back to app-scoped Exports.
    for shared in [&dirs.downloads, &dirs.music] {
        if is_dir_writable(shared) {
            let target = shared.join(&file_name);
            let note =
                format!("Saved to {} (direct write to {} was restricted by Android scoped storage)", target.display(), requested_path.display());
            return (target, Some(note));
        }
    }
    let fallback_dir = dirs.fallback_exports_dir();
    let _ = std::fs::create_dir_all(&fallback_dir);
    let target = fallback_dir.join(&file_name);
    let note = format!("Saved to {} (grant storage permission or use System Picker to write directly to {})", target.display(), requested_path.display());
    (target, Some(note))
}

static DIRS: OnceLock<Mutex<AndroidDirs>> = OnceLock::new();
static PERM_STATUS: OnceLock<Mutex<StoragePermissionStatus>> = OnceLock::new();

fn dirs_lock() -> &'static Mutex<AndroidDirs> {
    DIRS.get_or_init(|| {
        let env_cfg = std::env::var_os("SOUNDCRAFT_CONFIG_DIR").map(PathBuf::from);
        let env_ext = std::env::var_os("EXTERNAL_STORAGE").map(PathBuf::from);
        Mutex::new(AndroidDirs::resolve(None, None, env_cfg, env_ext))
    })
}

fn perm_lock() -> &'static Mutex<StoragePermissionStatus> {
    PERM_STATUS.get_or_init(|| Mutex::new(StoragePermissionStatus::default()))
}

/// Initialize the global Android directories from the `AndroidApp` instance at startup.
pub fn init_dirs(internal: Option<PathBuf>, external: Option<PathBuf>) {
    let env_cfg = std::env::var_os("SOUNDCRAFT_CONFIG_DIR").map(PathBuf::from);
    let env_ext = std::env::var_os("EXTERNAL_STORAGE").map(PathBuf::from);
    let resolved = AndroidDirs::resolve(internal, external, env_cfg, env_ext);
    let _ = std::fs::create_dir_all(&resolved.internal_data);
    if let Some(ext) = &resolved.external_data {
        let _ = std::fs::create_dir_all(ext);
        let _ = std::fs::create_dir_all(ext.join("Exports"));
        let _ = std::fs::create_dir_all(ext.join("Imports"));
    }
    *dirs_lock().lock().unwrap_or_else(PoisonError::into_inner) = resolved;
    refresh_permission_probe();
}

/// Current Android storage directories.
pub fn current_dirs() -> AndroidDirs {
    dirs_lock().lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Current Android storage permission status.
pub fn current_status() -> StoragePermissionStatus {
    perm_lock().lock().unwrap_or_else(PoisonError::into_inner).clone()
}

/// Update permission state from JNI callback (`MainActivity.onRequestPermissionsResult`) or probe.
pub fn set_permission_status(read_granted: bool, write_granted: bool, all_files_access: bool, sdk_int: i32) {
    let mut guard = perm_lock().lock().unwrap_or_else(PoisonError::into_inner);
    guard.read_granted = read_granted;
    guard.write_granted = write_granted;
    guard.all_files_access = all_files_access;
    if sdk_int > 0 {
        guard.sdk_int = sdk_int;
    }
}

/// Probe the filesystem directly to detect whether shared storage is currently readable/writable.
pub fn refresh_permission_probe() -> StoragePermissionStatus {
    let dirs = current_dirs();
    let readable = is_dir_readable(&dirs.downloads) || is_dir_readable(&dirs.music) || is_dir_readable(&dirs.shared_root);
    let writable = is_dir_writable(&dirs.downloads) || is_dir_writable(&dirs.music);
    let mut guard = perm_lock().lock().unwrap_or_else(PoisonError::into_inner);
    if readable {
        guard.read_granted = true;
    }
    if writable {
        guard.write_granted = true;
    }
    guard.clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn required_permissions_match_android_sdk_generations() {
        assert_eq!(required_runtime_permissions(28), &[PERM_READ_EXTERNAL_STORAGE, PERM_WRITE_EXTERNAL_STORAGE, PERM_RECORD_AUDIO]);
        assert_eq!(required_runtime_permissions(29), &[PERM_READ_EXTERNAL_STORAGE, PERM_WRITE_EXTERNAL_STORAGE, PERM_RECORD_AUDIO]);
        assert_eq!(required_runtime_permissions(30), &[PERM_READ_EXTERNAL_STORAGE, PERM_RECORD_AUDIO]);
        assert_eq!(required_runtime_permissions(33), &[PERM_READ_MEDIA_AUDIO, PERM_READ_MEDIA_VIDEO, PERM_RECORD_AUDIO]);
        assert_eq!(required_runtime_permissions(35), &[PERM_READ_MEDIA_AUDIO, PERM_READ_MEDIA_VIDEO, PERM_RECORD_AUDIO]);
    }

    #[test]
    fn export_target_uses_requested_dir_when_writable_and_falls_back_otherwise() {
        let base = std::env::temp_dir().join(format!("soundcraft-android-perm-test-{}", std::process::id()));
        let internal = base.join("internal");
        let external = base.join("external");
        let shared = base.join("shared");
        let _ = std::fs::create_dir_all(&internal);
        let _ = std::fs::create_dir_all(&external);
        let _ = std::fs::create_dir_all(shared.join("Download"));
        let dirs = AndroidDirs::resolve(Some(internal), Some(external), None, Some(shared.clone()));

        let direct = shared.join("Download").join("mix_bounce.wav");
        let (resolved, note) = resolve_export_target(&direct, &dirs);
        assert_eq!(resolved, direct);
        assert!(note.is_none());

        let _ = std::fs::remove_dir_all(&base);
    }
}
