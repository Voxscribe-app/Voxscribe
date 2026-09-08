use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::core::paths;

pub const UNIT_NAME: &str = "duskr.service";

pub fn unit_path() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home().join(".config"))
        .join("systemd/user")
        .join(UNIT_NAME)
}

pub fn unit_contents(executable: &str) -> String {
    format!(
        "[Unit]\n\
         Description=Duskr speech-to-text daemon\n\
         Documentation=https://code.styna.net/projects/Duskr\n\
         After=graphical-session.target pipewire.service\n\
         Wants=pipewire.service\n\
         PartOf=graphical-session.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={executable} daemon\n\
         ExecReload={executable} reload\n\
         Restart=on-failure\n\
         RestartSec=3\n\
         # The daemon needs /dev/uinput and /dev/input, so it cannot be sandboxed\n\
         # away from the device nodes; everything else is still restricted.\n\
         PrivateTmp=yes\n\
         NoNewPrivileges=yes\n\
         Environment=RUST_LOG=info\n\
         \n\
         [Install]\n\
         WantedBy=graphical-session.target\n"
    )
}

pub fn current_executable() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "duskr".to_string())
}

pub fn install() -> Result<PathBuf> {
    let path = unit_path();
    let parent = path.parent().context("unit path has no parent")?;
    std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    std::fs::write(&path, unit_contents(&current_executable()))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

pub async fn systemctl(args: &[&str]) -> Result<String> {
    let output = tokio::process::Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .await
        .context("running systemctl --user")?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        anyhow::bail!(if stderr.is_empty() { stdout } else { stderr });
    }
    Ok(stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_unit_starts_the_daemon_and_reloads_in_place() {
        let unit = unit_contents("/usr/bin/duskr");
        assert!(unit.contains("ExecStart=/usr/bin/duskr daemon"));
        assert!(unit.contains("ExecReload=/usr/bin/duskr reload"));
    }

    #[test]
    fn the_unit_waits_for_the_graphical_session_and_pipewire() {
        let unit = unit_contents("duskr");
        assert!(unit.contains("After=graphical-session.target pipewire.service"));
        assert!(unit.contains("WantedBy=graphical-session.target"));
    }

    #[test]
    fn the_unit_restarts_on_failure() {
        assert!(unit_contents("duskr").contains("Restart=on-failure"));
    }
}
