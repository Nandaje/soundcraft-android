//! SoundCraft for Android (`libsoundcraft_android.so`).
//!
//! Runs the full [`soundcraft_ui_egui::SoundApp`] digital audio workstation on Android with:
//! - **Hardware GPU Acceleration** ([`gpu_startup`]): `wgpu` Vulkan 1.1+ primary backend with
//!   OpenGL ES 3.0+ fallback, `PowerPreference::HighPerformance`, full hardware adapter texture
//!   limits (`max_texture_dimension_2d` up to 8192–16384 px) for Edit/Mix timelines, waveform
//!   caches, meters, video frames, and score rendering, plus crash-safe sentinel fallback.
//! - **File Permissions & Storage Access for Import/Export** ([`permissions`], [`file_dialog`],
//!   [`services`]): Runtime storage and audio permission management (`READ_EXTERNAL_STORAGE`,
//!   `WRITE_EXTERNAL_STORAGE`, `READ_MEDIA_AUDIO`, `READ_MEDIA_VIDEO`, `MANAGE_EXTERNAL_STORAGE`,
//!   `RECORD_AUDIO`), Android Storage Access Framework (SAF) system file/save/folder picker
//!   bridge, touch-friendly in-app file browser, automatic scoped-storage fallback to `Download` /
//!   `Music` / `Exports`, and `MediaScannerConnection` indexing.
//! - **Realtime Audio Playback & Recording & Touch/Stylus Input** ([`shell`]): Android AAudio via
//!   `cpal` (`soundcraft-playback`), touch-twin deduplication, and S-Pen / stylus pressure, tilt,
//!   and orientation axes.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

#[cfg(target_os = "android")]
mod android_entry;
pub mod file_dialog;
pub mod gpu_startup;
pub mod permissions;
pub mod services;
pub mod shell;

// On Windows GNU, a `cdylib` with zero exported symbols causes `ld.exe` to enable
// `--export-all-symbols`, exceeding the 65,535 PE ordinal limit. Exporting one symbol on host
// targets keeps the host `cdylib` export table minimal during `cargo test`.
#[cfg(not(target_os = "android"))]
#[allow(unsafe_code)]
// SAFETY: Unmangled C-ABI stub with a crate-prefixed name so host `cdylib` links cleanly.
#[unsafe(no_mangle)]
pub extern "C" fn soundcraft_android_abi_version() -> u32 {
    1
}
