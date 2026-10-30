use std::borrow::Cow;
use std::path::{Path, PathBuf};

use chrono::Local;

/// `~/…` with the home directory filled in.
pub fn expand_home(path: &str) -> Cow<'_, Path> {
    match (path.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => Cow::Owned(home.join(rest)),
        _ => Cow::Borrowed(Path::new(path)),
    }
}

/// `directory/<prefix> <date> at <time>.<extension>`, unique within `directory`.
pub fn timestamped_file(directory: &Path, prefix: &str, extension: &str) -> PathBuf {
    let stamp = Local::now().format("%Y-%m-%d at %H.%M.%S");
    let base = format!("{prefix} {stamp}");
    let mut path = directory.join(format!("{base}.{extension}"));
    let mut copy = 2;
    while path.exists() {
        path = directory.join(format!("{base} ({copy}).{extension}"));
        copy += 1;
    }
    path
}

pub fn default_screenshot_directory() -> PathBuf {
    dirs::picture_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_default()
        .join("Screenshots")
}

pub fn default_recording_directory() -> PathBuf {
    dirs::video_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_default()
        .join("Recordings")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands_home() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_home("~/Pictures"), home.join("Pictures"));
        assert_eq!(expand_home("/tmp/shots"), Path::new("/tmp/shots"));
    }

    #[test]
    fn avoids_existing_files() {
        let directory =
            std::env::temp_dir().join(format!("crownos-screenshot-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let first = timestamped_file(&directory, "Screenshot", "png");
        std::fs::write(&first, b"").unwrap();
        let second = timestamped_file(&directory, "Screenshot", "png");
        assert_ne!(first, second);
        assert!(second.to_string_lossy().ends_with(").png"));
        std::fs::remove_dir_all(directory).unwrap();
    }
}
