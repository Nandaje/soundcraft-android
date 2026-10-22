# SoundCraft for Android

This directory contains the Android application project (`ai.storyteller.soundcraft`) that packages the Rust `soundcraft-android` crate (`libsoundcraft_android.so`) into an Android APK/AAB.

## Key Features

### 1. Hardware GPU Acceleration (`wgpu` Vulkan 1.1+ & OpenGL ES 3.0+)
- **Vulkan & OpenGL ES Backends**: `apps/soundcraft-android/src/gpu_startup.rs` configures `wgpu` with `wgpu::Backends::VULKAN | wgpu::Backends::GL` and `PowerPreference::HighPerformance`, preferring hardware Vulkan first (Adreno, Mali, Immortalis, Xclipse) with automatic fallback to OpenGL ES 3.0+.
- **Full Hardware Texture Limits**: Unlocks the device GPU's full `max_texture_dimension_2d` (8192–16384 px) for high-DPI multi-track waveforms, spectrogram/video textures, and mixer channel strips.
- **Crash-Safe Startup Sentinel**: Writes an initialization sentinel before GPU startup and automatically falls back to OpenGL ES (`Backends::GL`) if a driver crashes during Vulkan initialization.
- **Manifest Hardware Acceleration**: `AndroidManifest.xml` enables `android:hardwareAccelerated="true"`, `android:largeHeap="true"`, OpenGL ES 3.0+, Vulkan 1.1+, and low-latency / pro audio feature declarations.

### 2. File Permissions for Import & Export
- **Manifest & Runtime Permissions**:
  - `android.permission.READ_EXTERNAL_STORAGE` (Android 8–12L)
  - `android.permission.WRITE_EXTERNAL_STORAGE` (Android 8–10)
  - `android.permission.READ_MEDIA_AUDIO` & `android.permission.READ_MEDIA_VIDEO` (Android 13+)
  - `android.permission.MANAGE_EXTERNAL_STORAGE` (Android 11+ All Files Access for `.scraft`, `.scraftz`, `Audio Files`, `Bounced Files`, MIDI, and MusicXML project directories)
  - `android.permission.RECORD_AUDIO` (Low-latency Android AAudio input recording via `cpal`)
- **Dual File Picker (In-App Touch Browser + Android SAF)**:
  - **In-App Android File Browser (`apps/soundcraft-android/src/file_dialog.rs`)**: Quick bookmarks for `Downloads`, `Music`, `Documents`, `Shared Storage`, `App External`, `App Exports`, and `App Internal`, plus one-tap **Grant Storage Access**, **All Files Access**, and **System File Picker (SAF)** buttons.
  - **Storage Access Framework (`MainActivity.kt`)**: Bridges `ACTION_OPEN_DOCUMENT`, `ACTION_CREATE_DOCUMENT`, `ACTION_OPEN_DOCUMENT_TREE`, and incoming `ACTION_VIEW` / `ACTION_EDIT` / `ACTION_SEND` intents over JNI to Rust.
  - **Scoped-Storage Safe Exports & Media Indexing**: `services::write_with_android_permissions` writes atomically to the requested path (falling back automatically to writable `Download`, `Music`, or `App Exports` if scoped storage restricts a directory) and triggers `MediaScannerConnection.scanFile` so bounced WAV/AIFF/FLAC mixes and stems immediately appear in Android Music and Files apps.

## Building

```bash
rustup target add aarch64-linux-android x86_64-linux-android
cargo install cargo-ndk --locked
packaging/android/package.sh
```
