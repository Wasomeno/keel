//! Settings → Background and the custom alert sound: files you pick are
//! copied into the data dir, so moving or deleting the original breaks
//! nothing. Pictures are scaled down on import.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use keel_proto::{DEFAULT_SOUND, SoundChoice};

/// Longest edge kept for an imported background, in pixels.
pub const MAX_EDGE: u32 = 2560;
/// Picture types the chooser offers.
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "heic", "heif", "webp"];
/// Sound types the chooser offers (NSSound plays them all).
pub const SOUND_EXTENSIONS: &[&str] = &["aiff", "aif", "wav", "mp3", "m4a", "caf"];
const SYSTEM_SOUNDS: &str = "/System/Library/Sounds";

pub fn backgrounds_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("backgrounds")
}

pub fn sounds_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("sounds")
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

fn stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis())
}

/// Copies `src` into `backgrounds/`, scaled to at most [`MAX_EDGE`], and
/// returns the new file name (for `UiSettings::background`). Earlier
/// imports are removed. HEIC goes through macOS `sips`.
pub fn import_background(data_dir: &Path, src: &Path) -> io::Result<String> {
    let ext = extension(src);
    if !IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        return Err(invalid(format!("not a supported picture: .{ext}")));
    }
    let dir = backgrounds_dir(data_dir);
    fs::create_dir_all(&dir)?;
    let bytes = if matches!(ext.as_str(), "heic" | "heif") {
        heic_to_jpeg(src, &dir)?
    } else {
        fs::read(src)?
    };
    let image = image::load_from_memory(&bytes)
        .map_err(|e| invalid(format!("couldn't read the picture: {e}")))?;
    let image = if image.width().max(image.height()) > MAX_EDGE {
        image.resize(MAX_EDGE, MAX_EDGE, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let name = if image.color().has_alpha() {
        let name = format!("bg-{}.png", stamp());
        image
            .save_with_format(dir.join(&name), image::ImageFormat::Png)
            .map_err(|e| invalid(e.to_string()))?;
        name
    } else {
        let name = format!("bg-{}.jpg", stamp());
        let file = fs::File::create(dir.join(&name))?;
        let mut encoder =
            image::codecs::jpeg::JpegEncoder::new_with_quality(io::BufWriter::new(file), 90);
        encoder
            .encode_image(&image.to_rgb8())
            .map_err(|e| invalid(e.to_string()))?;
        name
    };
    remove_others(&dir, &name);
    Ok(name)
}

/// The imported background on disk, if it is still there.
pub fn background_path(data_dir: &Path, name: Option<&str>) -> Option<PathBuf> {
    let name = name?;
    // A bare file name only: never a path out of the folder.
    if name.contains('/') || name.contains("..") {
        return None;
    }
    Some(backgrounds_dir(data_dir).join(name)).filter(|p| p.is_file())
}

/// Back to the built-in picture: the imported files go.
pub fn reset_background(data_dir: &Path) {
    let _ = fs::remove_dir_all(backgrounds_dir(data_dir));
}

fn remove_others(dir: &Path, keep: &str) {
    for entry in fs::read_dir(dir).into_iter().flatten().flatten() {
        if entry.file_name().to_str() != Some(keep) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(target_os = "macos")]
fn heic_to_jpeg(src: &Path, dir: &Path) -> io::Result<Vec<u8>> {
    let out = dir.join(format!(".import-{}.jpg", stamp()));
    let status = std::process::Command::new("/usr/bin/sips")
        .args(["-s", "format", "jpeg", "-Z", &MAX_EDGE.to_string()])
        .arg(src)
        .arg("--out")
        .arg(&out)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()?;
    let bytes = if status.success() {
        fs::read(&out)
    } else {
        Err(invalid("macOS couldn't convert the HEIC picture"))
    };
    let _ = fs::remove_file(&out);
    bytes
}

#[cfg(not(target_os = "macos"))]
fn heic_to_jpeg(_src: &Path, _dir: &Path) -> io::Result<Vec<u8>> {
    Err(invalid("HEIC pictures need macOS"))
}

/// Copies a sound into `sounds/` and returns its file name (for
/// `SoundChoice::Custom`). Earlier custom sounds are removed.
pub fn import_sound(data_dir: &Path, src: &Path) -> io::Result<String> {
    let ext = extension(src);
    if !SOUND_EXTENSIONS.contains(&ext.as_str()) {
        return Err(invalid(format!("not a supported sound: .{ext}")));
    }
    let dir = sounds_dir(data_dir);
    fs::create_dir_all(&dir)?;
    let stem: String = src
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("sound")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let name = format!("{stem}-{}.{ext}", stamp());
    fs::copy(src, dir.join(&name))?;
    remove_others(&dir, &name);
    Ok(name)
}

/// The macOS alert sounds, by name ("Basso" … "Tink").
pub fn system_sounds() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(SYSTEM_SOUNDS)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            (extension(&path) == "aiff")
                .then(|| path.file_stem()?.to_str().map(str::to_string))
                .flatten()
        })
        .collect();
    names.sort();
    names
}

/// The file a sound choice plays, if any.
pub fn sound_path(data_dir: &Path, choice: &SoundChoice) -> Option<PathBuf> {
    let path = match choice {
        SoundChoice::None => return None,
        SoundChoice::Default => Path::new(SYSTEM_SOUNDS).join(format!("{DEFAULT_SOUND}.aiff")),
        SoundChoice::System(name) if !name.contains('/') => {
            Path::new(SYSTEM_SOUNDS).join(format!("{name}.aiff"))
        }
        SoundChoice::Custom(name) if !name.contains('/') && !name.contains("..") => {
            sounds_dir(data_dir).join(name)
        }
        _ => return None,
    };
    path.is_file().then_some(path)
}

/// What the sound picker shows for a choice.
pub fn sound_label(choice: &SoundChoice) -> String {
    match choice {
        SoundChoice::None => "None".into(),
        SoundChoice::Default => format!("{DEFAULT_SOUND} (default)"),
        SoundChoice::System(name) => name.clone(),
        SoundChoice::Custom(name) => {
            // "ding-1728450000000.wav" → "ding.wav"
            let (stem, ext) = name.rsplit_once('.').unwrap_or((name, ""));
            let stem = stem.rsplit_once('-').map_or(stem, |(s, _)| s);
            if ext.is_empty() {
                stem.to_string()
            } else {
                format!("{stem}.{ext}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "keel-bg-{label}-{}-{}",
            std::process::id(),
            stamp()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn import_scales_down_copies_and_replaces_the_previous_one() {
        let dir = tmp("import");
        let src = dir.join("big.png");
        image::RgbImage::from_pixel(4000, 2000, image::Rgb([10, 120, 110]))
            .save(&src)
            .unwrap();
        let first = import_background(&dir, &src).unwrap();
        assert!(first.ends_with(".jpg"), "{first}");
        let path = background_path(&dir, Some(&first)).expect("imported");
        let (w, h) = image::image_dimensions(&path).unwrap();
        assert_eq!((w, h), (MAX_EDGE, MAX_EDGE / 2));
        // The original can go; the copy stays.
        fs::remove_file(&src).unwrap();
        assert!(background_path(&dir, Some(&first)).is_some());
        // A second import replaces the first; a small one keeps its size and alpha.
        std::thread::sleep(std::time::Duration::from_millis(3));
        let small = dir.join("small.png");
        image::RgbaImage::from_pixel(64, 32, image::Rgba([1, 2, 3, 128]))
            .save(&small)
            .unwrap();
        let second = import_background(&dir, &small).unwrap();
        assert!(second.ends_with(".png"));
        assert_eq!(
            image::image_dimensions(background_path(&dir, Some(&second)).unwrap()).unwrap(),
            (64, 32)
        );
        assert!(background_path(&dir, Some(&first)).is_none());
        // Reset: back to the built-in picture.
        reset_background(&dir);
        assert!(background_path(&dir, Some(&second)).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_rejects_other_files_and_paths_stay_inside() {
        let dir = tmp("reject");
        let text = dir.join("notes.txt");
        fs::write(&text, "hi").unwrap();
        assert!(import_background(&dir, &text).is_err());
        let fake = dir.join("fake.png");
        fs::write(&fake, "not a png").unwrap();
        assert!(import_background(&dir, &fake).is_err());
        assert!(background_path(&dir, Some("../ui-settings.json")).is_none());
        assert!(background_path(&dir, None).is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sounds_import_and_resolve() {
        let dir = tmp("sound");
        let src = dir.join("My Ding.wav");
        fs::write(&src, b"RIFF....WAVE").unwrap();
        let name = import_sound(&dir, &src).unwrap();
        assert!(
            name.starts_with("My-Ding-") && name.ends_with(".wav"),
            "{name}"
        );
        assert_eq!(
            sound_label(&SoundChoice::Custom(name.clone())),
            "My-Ding.wav"
        );
        assert!(sound_path(&dir, &SoundChoice::Custom(name)).is_some());
        assert!(sound_path(&dir, &SoundChoice::None).is_none());
        assert!(sound_path(&dir, &SoundChoice::Custom("../x.wav".into())).is_none());
        assert!(import_sound(&dir, &dir.join("x.txt")).is_err());
        assert_eq!(sound_label(&SoundChoice::Default), "Glass (default)");
        if cfg!(target_os = "macos") {
            assert!(system_sounds().iter().any(|s| s == "Glass"));
            assert!(sound_path(&dir, &SoundChoice::Default).is_some());
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
