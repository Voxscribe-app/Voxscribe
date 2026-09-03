//! Detecting and adapting an existing Quickshell hyprwhspr integration.
//!
//! The lowest-risk migration replaces only the service file: the indicator,
//! waveform, island and morph overlay all talk to it through the same five
//! properties, so leaving them untouched keeps the shell working exactly as
//! before while the data behind it comes from Duskr.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::core::paths;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detection {
    pub root: PathBuf,
    /// `services/HyprwhsprService.qml`, if present.
    pub service: Option<PathBuf>,
    /// Other files referring to the service, which the adapter keeps working.
    pub dependents: Vec<PathBuf>,
}

impl Detection {
    pub fn found(&self) -> bool {
        self.service.is_some()
    }
}

const DEPENDENT_CANDIDATES: &[&str] = &[
    "components/HyprwhsprIndicator.qml",
    "components/HyprwhsprWaveform.qml",
    "components/DictationIslandContent.qml",
    "components/CompactIslandContent.qml",
    "panels/MorphOverlay.qml",
    "shell.qml",
    "ShellController.qml",
];

pub fn detect() -> Detection {
    detect_in(&paths::quickshell_config_dir())
}

pub fn detect_in(root: &Path) -> Detection {
    let service = root.join("services/HyprwhsprService.qml");
    let dependents = DEPENDENT_CANDIDATES
        .iter()
        .map(|relative| root.join(relative))
        .filter(|path| path.exists())
        .collect();

    Detection {
        root: root.to_path_buf(),
        service: service.exists().then_some(service),
        dependents,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallReport {
    pub service_written: PathBuf,
    pub backup: Option<PathBuf>,
    /// Written alongside as the canonical native service.
    pub native_written: Option<PathBuf>,
}

/// Replace the hyprwhspr service with the Duskr-backed adapter.
///
/// The original is backed up first, and the native `DuskrService.qml` is
/// written next to it so the shell can be moved over at leisure.
pub fn install(detection: &Detection) -> Result<InstallReport> {
    let services_dir = detection.root.join("services");
    std::fs::create_dir_all(&services_dir)
        .with_context(|| format!("creating {}", services_dir.display()))?;

    let service_path = services_dir.join("HyprwhsprService.qml");
    let backup = super::backup(&service_path)?;

    std::fs::write(
        &service_path,
        include_str!("../../assets/quickshell/HyprwhsprService.qml"),
    )
    .with_context(|| format!("writing {}", service_path.display()))?;

    let native_path = services_dir.join("DuskrService.qml");
    std::fs::write(
        &native_path,
        include_str!("../../assets/quickshell/DuskrService.qml"),
    )
    .with_context(|| format!("writing {}", native_path.display()))?;

    Ok(InstallReport {
        service_written: service_path,
        backup,
        native_written: Some(native_path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaffold() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("services")).unwrap();
        std::fs::create_dir_all(dir.path().join("components")).unwrap();
        std::fs::create_dir_all(dir.path().join("panels")).unwrap();
        dir
    }

    #[test]
    fn a_hyprwhspr_integration_is_detected_with_its_dependents() {
        let dir = scaffold();
        std::fs::write(dir.path().join("services/HyprwhsprService.qml"), "// old").unwrap();
        std::fs::write(dir.path().join("components/HyprwhsprIndicator.qml"), "").unwrap();
        std::fs::write(dir.path().join("panels/MorphOverlay.qml"), "").unwrap();

        let detection = detect_in(dir.path());
        assert!(detection.found());
        assert_eq!(detection.dependents.len(), 2);
    }

    #[test]
    fn a_config_without_the_service_is_reported_as_absent() {
        let dir = scaffold();
        let detection = detect_in(dir.path());
        assert!(!detection.found());
        assert!(detection.dependents.is_empty());
    }

    #[test]
    fn installing_backs_up_the_original_and_writes_both_services() {
        let dir = scaffold();
        let original = dir.path().join("services/HyprwhsprService.qml");
        std::fs::write(&original, "// original service").unwrap();

        let report = install(&detect_in(dir.path())).unwrap();
        let backup = report.backup.expect("the original must be backed up");
        assert_eq!(
            std::fs::read_to_string(&backup).unwrap(),
            "// original service"
        );

        let installed = std::fs::read_to_string(&report.service_written).unwrap();
        assert!(installed.contains("duskr"));
        // The property API the rest of the shell binds to must survive.
        for property in ["available", "state", "tooltip", "level", "levelActive"] {
            assert!(installed.contains(property), "{property} missing");
        }
        assert!(installed.contains("controller.hyprwhsprService = service"));
        assert!(report.native_written.unwrap().exists());
    }

    #[test]
    fn partial_watch_lines_are_merged_rather_than_overwriting_the_state() {
        let dir = scaffold();
        let report = install(&detect_in(dir.path())).unwrap();

        // `quickshell watch` interleaves {"level":...} lines with full status
        // lines. Assigning every field on every line dropped the state to idle
        // between level updates, which restarted the island morph ~8 times a
        // second while recording.
        for path in [
            report.service_written.clone(),
            report.native_written.clone().unwrap(),
        ] {
            let installed = std::fs::read_to_string(&path).unwrap();
            for field in ["class", "tooltip", "level", "ready"] {
                assert!(
                    installed.contains(&format!("if (data.{field} !== undefined)")),
                    "{} does not merge {field} conditionally",
                    path.display()
                );
            }
        }
    }

    #[test]
    fn installing_into_a_config_without_the_service_still_works() {
        let dir = scaffold();
        let report = install(&detect_in(dir.path())).unwrap();
        assert!(report.backup.is_none());
        assert!(report.service_written.exists());
    }
}
