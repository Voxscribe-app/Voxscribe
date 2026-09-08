//! Focused-window identification for per-application rules. Queried over each
//! compositor's IPC socket, never by shelling out. No interface yields `None`,
//! and the global rules apply.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;

const IO_TIMEOUT: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowInfo {
    pub class: String,
    pub title: String,
    pub source: &'static str,
}

impl WindowInfo {
    /// Keys a config rule may use, most specific first.
    pub fn identifiers(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |value: &str| {
            let ident = normalize(value);
            if !ident.is_empty() && !out.contains(&ident) {
                out.push(ident);
            }
        };

        push(&self.class);
        // `org.kde.konsole` should also match a rule written `konsole`.
        if let Some(tail) = self.class.rsplit('.').next() {
            if tail != self.class {
                push(tail);
            }
        }
        push(&self.title);
        for part in self.title.split(" - ") {
            push(part);
        }
        out
    }
}

/// Lower-case, punctuation-collapsed form used for rule matching.
pub fn normalize(value: &str) -> String {
    let trimmed = value.trim().to_ascii_lowercase();
    let trimmed = trimmed.strip_suffix(".desktop").unwrap_or(&trimmed);
    let mut out = String::with_capacity(trimmed.len());
    let mut last_dash = true;
    for c in trimmed.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

pub fn focused() -> Option<WindowInfo> {
    hyprland().or_else(niri).or_else(sway)
}

fn connect(path: PathBuf) -> Option<UnixStream> {
    let stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).ok()?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).ok()?;
    Some(stream)
}

fn runtime_dir() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

#[derive(Deserialize)]
struct HyprWindow {
    #[serde(default)]
    class: String,
    #[serde(default)]
    title: String,
}

fn hyprland() -> Option<WindowInfo> {
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;
    let base = runtime_dir().join("hypr").join(&signature);
    // Hyprland moved the socket to $XDG_RUNTIME_DIR/hypr; older builds /tmp.
    let mut stream = connect(base.join(".socket.sock"))
        .or_else(|| connect(PathBuf::from(format!("/tmp/hypr/{signature}/.socket.sock"))))?;

    stream.write_all(b"j/activewindow").ok()?;
    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;

    let window: HyprWindow = serde_json::from_str(&body).ok()?;
    if window.class.is_empty() && window.title.is_empty() {
        return None;
    }
    Some(WindowInfo {
        class: window.class,
        title: window.title,
        source: "hyprland",
    })
}

#[derive(Deserialize)]
struct NiriResponse {
    #[serde(rename = "Ok")]
    ok: Option<NiriOk>,
}

#[derive(Deserialize)]
struct NiriOk {
    #[serde(rename = "FocusedWindow")]
    focused_window: Option<Option<NiriWindow>>,
}

#[derive(Deserialize)]
struct NiriWindow {
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    title: Option<String>,
}

fn niri() -> Option<WindowInfo> {
    let socket = std::env::var_os("NIRI_SOCKET").map(PathBuf::from)?;
    let mut stream = connect(socket)?;
    stream.write_all(b"\"FocusedWindow\"\n").ok()?;

    let mut body = String::new();
    stream.read_to_string(&mut body).ok()?;
    let line = body.lines().next()?;

    let response: NiriResponse = serde_json::from_str(line).ok()?;
    let window = response.ok?.focused_window??;
    Some(WindowInfo {
        class: window.app_id.unwrap_or_default(),
        title: window.title.unwrap_or_default(),
        source: "niri",
    })
}

#[derive(Deserialize)]
struct SwayNode {
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    app_id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    window_properties: Option<SwayWindowProperties>,
    #[serde(default)]
    nodes: Vec<SwayNode>,
    #[serde(default)]
    floating_nodes: Vec<SwayNode>,
}

#[derive(Deserialize)]
struct SwayWindowProperties {
    #[serde(default)]
    class: Option<String>,
}

impl SwayNode {
    fn find_focused(&self) -> Option<&SwayNode> {
        if self.focused {
            return Some(self);
        }
        self.nodes
            .iter()
            .chain(self.floating_nodes.iter())
            .find_map(|node| node.find_focused())
    }
}

fn sway() -> Option<WindowInfo> {
    let socket = std::env::var_os("SWAYSOCK").map(PathBuf::from)?;
    let mut stream = connect(socket)?;

    // i3 IPC: "i3-ipc" + payload length + message type, all native-endian.
    let mut request = Vec::with_capacity(14);
    request.extend_from_slice(b"i3-ipc");
    request.extend_from_slice(&0u32.to_ne_bytes());
    request.extend_from_slice(&4u32.to_ne_bytes()); // GET_TREE
    stream.write_all(&request).ok()?;

    let mut header = [0u8; 14];
    stream.read_exact(&mut header).ok()?;
    if &header[..6] != b"i3-ipc" {
        return None;
    }
    let len = u32::from_ne_bytes(header[6..10].try_into().ok()?) as usize;
    let mut body = vec![0u8; len];
    stream.read_exact(&mut body).ok()?;

    let tree: SwayNode = serde_json::from_slice(&body).ok()?;
    let focused = tree.find_focused()?;
    let class = focused
        .app_id
        .clone()
        .or_else(|| {
            focused
                .window_properties
                .as_ref()
                .and_then(|p| p.class.clone())
        })
        .unwrap_or_default();
    Some(WindowInfo {
        class,
        title: focused.name.clone().unwrap_or_default(),
        source: "sway",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identifiers_normalize_punctuation_and_case() {
        assert_eq!(normalize("org.kde.Konsole"), "org-kde-konsole");
        assert_eq!(
            normalize("Visual Studio Code.desktop"),
            "visual-studio-code"
        );
        assert_eq!(normalize("  --Firefox--  "), "firefox");
        assert_eq!(normalize("!!!"), "");
    }

    #[test]
    fn a_reverse_dns_class_also_matches_its_last_segment() {
        let window = WindowInfo {
            class: "org.kde.konsole".into(),
            title: String::new(),
            source: "test",
        };
        let ids = window.identifiers();
        assert_eq!(ids[0], "org-kde-konsole");
        assert!(ids.contains(&"konsole".to_string()));
    }

    #[test]
    fn title_segments_become_identifiers_for_browser_style_titles() {
        let window = WindowInfo {
            class: "firefox".into(),
            title: "Inbox - Mozilla Firefox".into(),
            source: "test",
        };
        let ids = window.identifiers();
        assert!(ids.contains(&"firefox".to_string()));
        assert!(ids.contains(&"inbox".to_string()));
        assert!(ids.contains(&"inbox-mozilla-firefox".to_string()));
    }

    #[test]
    fn sway_tree_walk_finds_the_focused_leaf() {
        let json = r#"{
            "focused": false, "nodes": [
                {"focused": false, "nodes": [], "floating_nodes": [
                    {"focused": true, "app_id": "kitty", "name": "zsh", "nodes": [], "floating_nodes": []}
                ]}
            ], "floating_nodes": []
        }"#;
        let tree: SwayNode = serde_json::from_str(json).unwrap();
        let focused = tree.find_focused().unwrap();
        assert_eq!(focused.app_id.as_deref(), Some("kitty"));
    }
}
