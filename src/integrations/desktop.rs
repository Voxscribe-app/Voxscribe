//! Snippets and assets for the desktop shells Duskr integrates with.
//!
//! Everything here is generated rather than shipped as a blob, so a change to
//! the CLI surface cannot leave a stale example behind.

use crate::core::config::Config;

/// Waybar module definition. `duskr waybar` supplies the JSON on stdout.
pub fn waybar_module() -> String {
    r#"// Add to ~/.config/waybar/config.jsonc and include "custom/duskr" in a module list.
"custom/duskr": {
    "format": "{}",
    "return-type": "json",
    "exec": "duskr waybar --watch",
    "on-click": "duskr toggle",
    "on-click-right": "duskr cancel",
    "tooltip": true
}
"#
    .to_string()
}

pub fn waybar_style() -> String {
    r#"#custom-duskr { padding: 0 8px; }
#custom-duskr.stopped    { color: @text; }
#custom-duskr.starting   { color: @overlay1; }
#custom-duskr.recording  { color: @red; }
#custom-duskr.processing { color: @yellow; }
#custom-duskr.paused     { color: @peach; }
#custom-duskr.error      { color: @maroon; }
"#
    .to_string()
}

/// Hyprland bindings, for users who prefer compositor shortcuts to evdev.
///
/// Duskr's own evdev listener works on every compositor and keeps working when
/// the shortcut is held; these are offered as an alternative, not a requirement.
pub fn hyprland_config(config: &Config) -> String {
    let primary = to_hypr_bind(&config.shortcuts.primary);
    let mut out = format!(
        "# Duskr - compositor bindings.\n\
         # Set shortcuts.primary = \"\" in Duskr's config if you use these instead\n\
         # of its evdev listener, so a key press is not handled twice.\n\
         bind = {primary}, exec, duskr toggle\n"
    );
    if let Some(cancel) = &config.shortcuts.cancel {
        out.push_str(&format!(
            "bind = {}, exec, duskr cancel\n",
            to_hypr_bind(cancel)
        ));
    }
    if let Some(secondary) = &config.shortcuts.secondary {
        let language = config
            .shortcuts
            .secondary_language
            .as_deref()
            .unwrap_or("en");
        out.push_str(&format!(
            "bind = {}, exec, duskr toggle --language {language}\n",
            to_hypr_bind(secondary)
        ));
    }
    out
}

/// `SUPER+ALT+D` becomes Hyprland's `SUPER ALT, D`.
fn to_hypr_bind(chord: &str) -> String {
    let parts: Vec<&str> = chord
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    let Some((key, modifiers)) = parts.split_last() else {
        return String::new();
    };
    format!(
        "{}, {}",
        modifiers.join(" ").to_uppercase(),
        key.to_uppercase()
    )
}

/// KDE global shortcuts are declared through a desktop entry, which
/// `kglobalaccel` picks up.
pub fn kde_desktop_entry() -> String {
    r#"[Desktop Entry]
Name=Duskr
Comment=Toggle Duskr dictation
Exec=duskr toggle
Icon=audio-input-microphone
Type=Application
NoDisplay=true
X-KDE-GlobalAccel-CommandShortcut=true
"#
    .to_string()
}

/// Quickshell service exposing the API the existing hyprwhspr widgets consume:
/// `available`, `state`, `tooltip`, `level`, `levelActive`.
pub fn quickshell_service() -> String {
    include_str!("../../assets/quickshell/DuskrService.qml").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chords_translate_into_hyprland_bind_syntax() {
        assert_eq!(to_hypr_bind("SUPER+ALT+D"), "SUPER ALT, D");
        assert_eq!(to_hypr_bind("ctrl+shift+space"), "CTRL SHIFT, SPACE");
        assert_eq!(to_hypr_bind("F12"), ", F12");
        assert_eq!(to_hypr_bind(""), "");
    }

    #[test]
    fn hyprland_config_covers_every_configured_shortcut() {
        let mut config = Config::default();
        config.shortcuts.cancel = Some("SUPER+ESCAPE".into());
        config.shortcuts.secondary = Some("SUPER+ALT+I".into());
        config.shortcuts.secondary_language = Some("it".into());

        let rendered = hyprland_config(&config);
        assert!(rendered.contains("bind = SUPER ALT, D, exec, duskr toggle"));
        assert!(rendered.contains("bind = SUPER, ESCAPE, exec, duskr cancel"));
        assert!(rendered.contains("duskr toggle --language it"));
    }

    #[test]
    fn the_waybar_module_drives_itself_from_the_event_stream() {
        let module = waybar_module();
        assert!(module.contains("duskr waybar --watch"));
        assert!(module.contains("\"return-type\": \"json\""));
    }

    #[test]
    fn the_waybar_stylesheet_covers_every_state_class() {
        let style = waybar_style();
        for class in ["stopped", "recording", "processing", "paused", "error"] {
            assert!(style.contains(class), "{class} has no style rule");
        }
    }

    #[test]
    fn the_quickshell_service_exposes_the_legacy_property_api() {
        let qml = quickshell_service();
        for property in ["available", "state", "tooltip", "level", "levelActive"] {
            assert!(
                qml.contains(property),
                "{property} missing from the service"
            );
        }
    }
}
