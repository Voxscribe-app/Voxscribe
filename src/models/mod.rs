
pub mod download;

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::Serialize;

use crate::core::config::Config;

const GGML_MAGIC: &[u8; 4] = b"lmgg";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelEntry {
    pub name: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub valid: bool,
}

impl ModelEntry {
    pub fn size_human(&self) -> String {
        human_size(self.size_bytes)
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KiB", "MiB", "GiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

pub const CATALOG: &[(&str, &str)] = &[
    ("tiny", "75 MiB, fastest, lowest quality"),
    ("tiny.en", "75 MiB, English-only"),
    ("base", "142 MiB, good default"),
    ("base.en", "142 MiB, English-only"),
    ("small", "466 MiB"),
    ("small.en", "466 MiB, English-only"),
    ("medium", "1.5 GiB"),
    ("medium.en", "1.5 GiB, English-only"),
    ("large-v3", "3.1 GiB, best quality"),
    (
        "large-v3-turbo",
        "1.6 GiB, near-large quality at small-model speed",
    ),
];

pub fn is_catalog_model(name: &str) -> bool {
    CATALOG.iter().any(|(model, _)| *model == name)
}

pub fn looks_like_model(path: &Path) -> bool {
    let Ok(mut file) = fs::File::open(path) else {
        return false;
    };
    let mut magic = [0u8; 4];
    use std::io::Read;
    matches!(file.read_exact(&mut magic), Ok(())) && &magic == GGML_MAGIC
}

pub fn list(models_dir: &Path) -> Vec<ModelEntry> {
    let mut entries = Vec::new();
    let Ok(dir) = fs::read_dir(models_dir) else {
        return entries;
    };
    for entry in dir.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let file_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if !file_name.ends_with(".bin") {
            continue;
        }
        let size_bytes = entry.metadata().map(|m| m.len()).unwrap_or(0);
        entries.push(ModelEntry {
            name: crate::asr::whisper::model_name_from_path(&path),
            valid: looks_like_model(&path),
            path,
            size_bytes,
        });
    }
    entries.sort_by(|a, b| a.name.cmp(&b.name));
    entries
}

pub fn find(models_dir: &Path, name: &str) -> Option<ModelEntry> {
    list(models_dir)
        .into_iter()
        .find(|entry| entry.name == name)
}

pub fn add(models_dir: &Path, source: &Path, move_file: bool) -> Result<ModelEntry> {
    if !source.is_file() {
        bail!("{} is not a file", source.display());
    }
    if !looks_like_model(source) {
        bail!(
            "{} does not look like a ggml model (expected the whisper.cpp magic 0x67676d6c)",
            source.display()
        );
    }

    fs::create_dir_all(models_dir).with_context(|| format!("creating {}", models_dir.display()))?;

    let file_name = source
        .file_name()
        .and_then(|n| n.to_str())
        .context("source has no file name")?;
    let target_name = if file_name.starts_with("ggml-") {
        file_name.to_string()
    } else {
        format!("ggml-{file_name}")
    };
    let destination = models_dir.join(&target_name);

    if destination.exists() {
        bail!("{} already exists", destination.display());
    }

    if move_file {
        if fs::rename(source, &destination).is_err() {
            fs::copy(source, &destination)
                .with_context(|| format!("copying to {}", destination.display()))?;
            fs::remove_file(source).with_context(|| format!("removing {}", source.display()))?;
        }
    } else {
        fs::copy(source, &destination)
            .with_context(|| format!("copying to {}", destination.display()))?;
    }

    Ok(ModelEntry {
        name: crate::asr::whisper::model_name_from_path(&destination),
        size_bytes: fs::metadata(&destination).map(|m| m.len()).unwrap_or(0),
        valid: true,
        path: destination,
    })
}

pub fn remove(models_dir: &Path, name: &str) -> Result<PathBuf> {
    let entry = find(models_dir, name)
        .with_context(|| format!("no model named '{name}' in {}", models_dir.display()))?;
    fs::remove_file(&entry.path).with_context(|| format!("removing {}", entry.path.display()))?;
    Ok(entry.path)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RelocateReport {
    pub from: PathBuf,
    pub to: PathBuf,
    pub moved: Vec<String>,
}

pub fn relocate(config: &mut Config, new_dir: &Path, move_models: bool) -> Result<RelocateReport> {
    let old_dir = config.models_dir_configured();
    let new_dir = if new_dir.is_absolute() {
        new_dir.to_path_buf()
    } else {
        std::env::current_dir()?.join(new_dir)
    };

    if new_dir == old_dir {
        return Ok(RelocateReport {
            from: old_dir,
            to: new_dir,
            moved: Vec::new(),
        });
    }

    fs::create_dir_all(&new_dir).with_context(|| format!("creating {}", new_dir.display()))?;

    let mut moved = Vec::new();
    if move_models {
        for entry in list(&old_dir) {
            let destination =
                new_dir.join(entry.path.file_name().context("model has no file name")?);
            if destination.exists() {
                tracing::warn!(
                    "skipping {}: already present in the new directory",
                    entry.name
                );
                continue;
            }
            if fs::rename(&entry.path, &destination).is_err() {
                fs::copy(&entry.path, &destination)
                    .with_context(|| format!("copying {}", entry.name))?;
                fs::remove_file(&entry.path)
                    .with_context(|| format!("removing {}", entry.path.display()))?;
            }
            moved.push(entry.name);
        }
    }

    config.models.dir = Some(new_dir.clone());
    Ok(RelocateReport {
        from: old_dir,
        to: new_dir,
        moved,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_model(dir: &Path, file_name: &str, extra: usize) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = dir.join(file_name);
        let mut bytes = GGML_MAGIC.to_vec();
        bytes.extend(std::iter::repeat_n(0u8, extra));
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn the_magic_matches_what_whisper_cpp_actually_writes_to_disk() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("ggml-medium.en.bin");
        fs::write(&real, 0x6767_6d6cu32.to_le_bytes()).unwrap();
        assert!(looks_like_model(&real));

        let reversed = dir.path().join("ggml-reversed.bin");
        fs::write(&reversed, b"ggml").unwrap();
        assert!(!looks_like_model(&reversed));
    }

    #[test]
    fn only_ggml_files_are_treated_as_models() {
        let dir = tempfile::tempdir().unwrap();
        let good = write_model(dir.path(), "ggml-base.en.bin", 10);
        let bad = dir.path().join("notes.bin");
        fs::write(&bad, b"nope").unwrap();

        assert!(looks_like_model(&good));
        assert!(!looks_like_model(&bad));

        let entries = list(dir.path());
        assert_eq!(entries.len(), 2);
        assert!(entries.iter().any(|e| e.name == "base.en" && e.valid));
        assert!(entries.iter().any(|e| e.name == "notes" && !e.valid));
    }

    #[test]
    fn adding_a_model_normalizes_its_file_name() {
        let source_dir = tempfile::tempdir().unwrap();
        let models = tempfile::tempdir().unwrap();
        let source = write_model(source_dir.path(), "medium.en.bin", 32);

        let entry = add(models.path(), &source, false).unwrap();
        assert_eq!(entry.name, "medium.en");
        assert_eq!(
            entry.path.file_name().unwrap().to_str().unwrap(),
            "ggml-medium.en.bin"
        );
        assert!(source.exists());
    }

    #[test]
    fn adding_a_model_by_move_removes_the_source() {
        let source_dir = tempfile::tempdir().unwrap();
        let models = tempfile::tempdir().unwrap();
        let source = write_model(source_dir.path(), "ggml-tiny.bin", 8);
        add(models.path(), &source, true).unwrap();
        assert!(!source.exists());
        assert!(models.path().join("ggml-tiny.bin").exists());
    }

    #[test]
    fn adding_a_non_model_file_is_refused() {
        let source_dir = tempfile::tempdir().unwrap();
        let models = tempfile::tempdir().unwrap();
        let source = source_dir.path().join("readme.bin");
        fs::write(&source, b"not a model").unwrap();
        let err = add(models.path(), &source, false).unwrap_err().to_string();
        assert!(err.contains("ggml"), "{err}");
    }

    #[test]
    fn adding_over_an_existing_model_is_refused_rather_than_overwriting() {
        let source_dir = tempfile::tempdir().unwrap();
        let models = tempfile::tempdir().unwrap();
        let source = write_model(source_dir.path(), "ggml-tiny.bin", 8);
        add(models.path(), &source, false).unwrap();
        assert!(add(models.path(), &source, false).is_err());
    }

    #[test]
    fn relocating_without_moving_leaves_the_models_where_they_are() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        write_model(old.path(), "ggml-base.bin", 4);

        let mut config = Config::default();
        config.models.dir = Some(old.path().to_path_buf());
        let report = relocate(&mut config, new.path(), false).unwrap();

        assert!(report.moved.is_empty());
        assert_eq!(config.models.dir.as_deref(), Some(new.path()));
        assert!(old.path().join("ggml-base.bin").exists());
    }

    #[test]
    fn relocating_with_move_brings_the_models_along() {
        let old = tempfile::tempdir().unwrap();
        let new = tempfile::tempdir().unwrap();
        write_model(old.path(), "ggml-base.bin", 4);
        write_model(old.path(), "ggml-tiny.en.bin", 4);

        let mut config = Config::default();
        config.models.dir = Some(old.path().to_path_buf());
        let report = relocate(&mut config, new.path(), true).unwrap();

        assert_eq!(report.moved.len(), 2);
        assert!(new.path().join("ggml-base.bin").exists());
        assert!(!old.path().join("ggml-base.bin").exists());
    }

    #[test]
    fn relocating_to_the_current_directory_is_a_no_op() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = Config::default();
        config.models.dir = Some(dir.path().to_path_buf());
        let report = relocate(&mut config, dir.path(), true).unwrap();
        assert!(report.moved.is_empty());
    }

    #[test]
    fn sizes_are_rendered_for_humans() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KiB");
        assert_eq!(human_size(1_610_612_736), "1.5 GiB");
    }

    #[test]
    fn the_catalog_covers_the_models_hyprwhspr_users_are_likely_to_have() {
        for name in ["base.en", "medium.en", "large-v3-turbo"] {
            assert!(is_catalog_model(name), "{name} missing from the catalog");
        }
        assert!(!is_catalog_model("not-a-model"));
    }
}
