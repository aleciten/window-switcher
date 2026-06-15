use std::{collections::HashSet, fs, path::PathBuf, process::Command};

use anyhow::{anyhow, Result};
use indexmap::IndexMap;
use ini::{Ini, ParseOption};
use log::LevelFilter;
use windows::core::w;

use crate::utils::{get_exe_folder, RegKey};

pub const SWITCH_WINDOWS_HOTKEY_ID: u32 = 1;
pub const SWITCH_APPS_HOTKEY_ID: u32 = 2;
pub const SWITCH_APP_WINDOWS_HOTKEY_ID: u32 = 3;

const DEFAULT_CONFIG: &str = include_str!("../window-switcher.ini");

// Default overlay colors (ARGB), matching the renderer's built-in look.
pub const DEFAULT_BACKGROUND_COLOR: u32 = 0xe0f3f3f3;
pub const DEFAULT_BORDER_LIGHT_COLOR: u32 = 0x5cffffff;
pub const DEFAULT_BORDER_DARK_COLOR: u32 = 0x26000000;
pub const DEFAULT_SELECTION_COLOR: u32 = 0x590067c0;

/// Configurable colors for the switch-apps overlay (ARGB).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SwitchAppsColors {
    /// Frosted background tint of the pane.
    pub background: u32,
    /// Lit bevel edge (top + left).
    pub border_light: u32,
    /// Shadowed bevel edge (bottom + right).
    pub border_dark: u32,
    /// Highlight behind the selected app (the cursor).
    pub selection: u32,
}

impl Default for SwitchAppsColors {
    fn default() -> Self {
        Self {
            background: DEFAULT_BACKGROUND_COLOR,
            border_light: DEFAULT_BORDER_LIGHT_COLOR,
            border_dark: DEFAULT_BORDER_DARK_COLOR,
            selection: DEFAULT_SELECTION_COLOR,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub trayicon: bool,
    pub log_level: LevelFilter,
    pub log_file: Option<PathBuf>,
    pub switch_windows_hotkey: Vec<Hotkey>,
    pub switch_windows_blacklist: HashSet<String>,
    pub switch_windows_ignore_minimal: bool,
    switch_windows_only_current_desktop: Option<bool>,
    pub switch_apps_enable: bool,
    pub switch_apps_hotkey: Vec<Hotkey>,
    pub switch_apps_ignore_minimal: bool,
    pub switch_apps_override_icons: IndexMap<String, String>,
    switch_apps_only_current_desktop: Option<bool>,
    pub switch_apps_colors: SwitchAppsColors,
    pub switch_app_windows_enable: bool,
    pub switch_app_windows_hotkey: Vec<Hotkey>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            trayicon: true,
            log_level: LevelFilter::Info,
            log_file: None,
            switch_windows_hotkey: vec![Hotkey::create(
                SWITCH_WINDOWS_HOTKEY_ID,
                "switch windows",
                "alt + `",
            )
            .unwrap()],
            switch_windows_blacklist: Default::default(),
            switch_windows_ignore_minimal: false,
            switch_windows_only_current_desktop: None,
            switch_apps_enable: true,
            switch_apps_hotkey: vec![Hotkey::create(
                SWITCH_APPS_HOTKEY_ID,
                "switch apps",
                "alt + tab",
            )
            .unwrap()],
            switch_apps_ignore_minimal: false,
            switch_apps_override_icons: Default::default(),
            switch_apps_only_current_desktop: None,
            switch_apps_colors: SwitchAppsColors::default(),
            switch_app_windows_enable: true,
            switch_app_windows_hotkey: vec![Hotkey::create(
                SWITCH_APP_WINDOWS_HOTKEY_ID,
                "switch app windows",
                "alt + ctrl + tab",
            )
            .unwrap()],
        }
    }
}

impl Config {
    pub fn load(ini_conf: &Ini) -> Result<Self> {
        let mut conf = Config::default();
        if let Some(section) = ini_conf.section(None::<String>) {
            if let Some(v) = section.get("trayicon").and_then(Config::to_bool) {
                conf.trayicon = v;
            }
        }

        if let Some(section) = ini_conf.section(Some("log")) {
            if let Some(level) = section.get("level").and_then(|v| v.parse().ok()) {
                conf.log_level = level;
            }
            if let Some(path) = section.get("path").map(normalize_path_value) {
                if !path.trim().is_empty() {
                    let mut path = PathBuf::from(path);
                    if !path.is_absolute() {
                        let parent = get_exe_folder()?;
                        path = parent.join(path);
                    }
                    conf.log_file = Some(path);
                }
            }
        }

        if let Some(section) = ini_conf.section(Some("switch-windows")) {
            if let Some(v) = section.get("hotkey") {
                if !v.trim().is_empty() {
                    conf.switch_windows_hotkey =
                        parse_hotkeys(SWITCH_WINDOWS_HOTKEY_ID, "switch windows", v)?;
                }
            }

            if let Some(v) = section
                .get("blacklist")
                .map(normalize_path_value)
                .map(|v| v.split(',').map(|v| v.trim().to_string()).collect())
            {
                conf.switch_windows_blacklist = v;
            }
            if let Some(v) = section.get("ignore_minimal").and_then(Config::to_bool) {
                conf.switch_windows_ignore_minimal = v;
            }
            if let Some(v) = section
                .get("only_current_desktop")
                .and_then(Config::to_bool)
            {
                conf.switch_windows_only_current_desktop = Some(v);
            }
        }
        if let Some(section) = ini_conf.section(Some("switch-apps")) {
            if let Some(v) = section.get("enable").and_then(Config::to_bool) {
                conf.switch_apps_enable = v;
            }
            if let Some(v) = section.get("hotkey") {
                if !v.trim().is_empty() {
                    conf.switch_apps_hotkey =
                        parse_hotkeys(SWITCH_APPS_HOTKEY_ID, "switch apps", v)?;
                }
            }
            if let Some(v) = section.get("ignore_minimal").and_then(Config::to_bool) {
                conf.switch_apps_ignore_minimal = v;
            }
            if let Some(v) = section.get("override_icons").map(normalize_path_value) {
                conf.switch_apps_override_icons = v
                    .split([',', ';'])
                    .filter_map(|v| {
                        v.trim()
                            .split_once("=")
                            .map(|(k, v)| (k.to_lowercase(), v.to_string()))
                    })
                    .collect();
            }

            if let Some(v) = section
                .get("only_current_desktop")
                .and_then(Config::to_bool)
            {
                conf.switch_apps_only_current_desktop = Some(v);
            }

            if let Some(v) = section.get("background_color").and_then(parse_color) {
                conf.switch_apps_colors.background = v;
            }
            if let Some(v) = section.get("border_light_color").and_then(parse_color) {
                conf.switch_apps_colors.border_light = v;
            }
            if let Some(v) = section.get("border_dark_color").and_then(parse_color) {
                conf.switch_apps_colors.border_dark = v;
            }
            if let Some(v) = section.get("selection_color").and_then(parse_color) {
                conf.switch_apps_colors.selection = v;
            }
        }
        if let Some(section) = ini_conf.section(Some("switch-app-windows")) {
            if let Some(v) = section.get("enable").and_then(Config::to_bool) {
                conf.switch_app_windows_enable = v;
            }
            if let Some(v) = section.get("hotkey") {
                if !v.trim().is_empty() {
                    conf.switch_app_windows_hotkey =
                        parse_hotkeys(SWITCH_APP_WINDOWS_HOTKEY_ID, "switch app windows", v)?;
                }
            }
        }
        Ok(conf)
    }

    pub fn to_hotkeys(&self) -> Vec<&Hotkey> {
        let mut hotkeys: Vec<&Hotkey> = self.switch_windows_hotkey.iter().collect();
        // Register the two-modifier app-windows hotkey BEFORE switch-apps: the keyboard
        // hook matches first-and-break, so `alt+ctrl+tab` resolves to app-windows rather
        // than the `alt+tab` switch-apps hotkey (alt is held in both).
        if self.switch_app_windows_enable {
            hotkeys.extend(self.switch_app_windows_hotkey.iter());
        }
        if self.switch_apps_enable {
            hotkeys.extend(self.switch_apps_hotkey.iter());
        }
        hotkeys
    }

    pub fn to_bool(v: &str) -> Option<bool> {
        match v {
            "yes" | "true" | "on" | "1" => Some(true),
            "no" | "false" | "off" | "0" => Some(false),
            _ => None,
        }
    }

    /// Whether the user has configured app switching to include other desktops.
    /// If the configured value is not a valid bool, the Windows registry will be
    /// used as a fallback.
    pub fn switch_apps_only_current_desktop(&self) -> bool {
        self.switch_apps_only_current_desktop
            .unwrap_or_else(Self::system_switcher_only_current_desktop)
    }

    /// Whether the user has configured window switching to include other desktops.
    /// If the configured value is not a valid bool, the Windows registry will be
    /// used as a fallback.
    pub fn switch_windows_only_current_desktop(&self) -> bool {
        self.switch_windows_only_current_desktop
            .unwrap_or_else(Self::system_switcher_only_current_desktop)
    }

    fn system_switcher_only_current_desktop() -> bool {
        let alt_tab_filter = RegKey::new_hkcu(
            w!(r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced"),
            w!("VirtualDesktopAltTabFilter"),
        )
        .and_then(|k| k.get_int())
        .unwrap_or(1);

        alt_tab_filter != 0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hotkey {
    pub id: u32,
    pub name: String,
    pub modifier: [u32; 2],
    /// Optional second modifier that must also be held (e.g. ctrl in `alt + ctrl + tab`).
    pub modifier2: Option<[u32; 2]>,
    pub code: u32,
}

impl Hotkey {
    pub fn create(id: u32, name: &str, value: &str) -> Result<Self> {
        let (modifier, modifier2, code) =
            Self::parse(value).ok_or_else(|| anyhow!("Invalid {name} hotkey"))?;
        Ok(Self {
            id,
            name: name.to_string(),
            modifier,
            modifier2,
            code,
        })
    }

    pub fn get_modifier(&self) -> u32 {
        self.modifier[0]
    }

    pub fn parse(value: &str) -> Option<([u32; 2], Option<[u32; 2]>, u32)> {
        let value = value
            .to_ascii_lowercase()
            .replace(' ', "")
            .replace("vk_", "");
        let keys: Vec<&str> = value.split('+').collect();
        let (modifier, modifier2, key) = match keys.as_slice() {
            [m, k] => (parse_modifier(m)?, None, *k),
            [m, m2, k] => {
                let modifier = parse_modifier(m)?;
                let modifier2 = parse_modifier(m2)?;
                if modifier == modifier2 {
                    return None;
                }
                (modifier, Some(modifier2), *k)
            }
            _ => return None,
        };
        let code = parse_key(key)?;
        Some((modifier, modifier2, code))
    }
}

fn parse_modifier(key: &str) -> Option<[u32; 2]> {
    match key {
        "win" => Some([0x5b, 0x5c]),
        "alt" => Some([0x38, 0x38]),
        "ctrl" => Some([0x1d, 0x1d]),
        _ => None,
    }
}

fn parse_key(key: &str) -> Option<u32> {
    // see <https://kbdlayout.info/kbdus/overview+scancodes>
    let code = match key {
            "esc" | "escape" => 0x01,
            "1" | "!" => 0x02,
            "2" | "@" => 0x03,
            "3" | "#" => 0x04,
            "4" | "$" => 0x05,
            "5" | "%" => 0x06,
            "6" | "^" => 0x07,
            "7" | "&" => 0x08,
            "8" | "*" => 0x09,
            "9" | "(" => 0x0a,
            "0" | ")" => 0x0b,
            "-" | "_" | "oem_minus" => 0x0c,
            "+" | "=" | "oem_plus" => 0x0d,
            "bs" | "backspace" => 0x0e,
            "tab" => 0x0f,
            "q" => 0x10,
            "w" => 0x11,
            "e" => 0x12,
            "r" => 0x13,
            "t" => 0x14,
            "y" => 0x15,
            "u" => 0x16,
            "i" => 0x17,
            "o" => 0x18,
            "p" => 0x19,
            "{" | "[" | "oem_4" => 0x1a,
            "}" | "]" | "oem_6" => 0x1b,
            "enter" | "return" => 0x1c,
            "a" => 0x1e,
            "s" => 0x1f,
            "d" => 0x20,
            "f" => 0x21,
            "g" => 0x22,
            "h" => 0x23,
            "j" => 0x24,
            "k" => 0x25,
            "l" => 0x26,
            ":" | ";" | "oem_1" => 0x27,
            "\"" | "'" | "oem_7" => 0x28,
            "~" | "`" | "oem_3" => 0x29,
            "|" | "\\" | "oem_5" => 0x2b,
            "z" => 0x2c,
            "x" => 0x2d,
            "c" => 0x2e,
            "v" => 0x2f,
            "b" => 0x30,
            "n" => 0x31,
            "m" => 0x32,
            "<" | "," | "oem_comma" => 0x33,
            ">" | "." | "oem_period" => 0x34,
            "?" | "/" | "oem_2" => 0x35,
            "space" => 0x39,
            "capslock" => 0x3a,
            "f1" => 0x3b,
            "f2" => 0x3c,
            "f3" => 0x3d,
            "f4" => 0x3e,
            "f5" => 0x3f,
            "f6" => 0x40,
            "f7" => 0x41,
            "f8" => 0x42,
            "f9" => 0x43,
            "f10" => 0x44,
            "numlock" => 0x45,
            "scrolllock" => 0x46,
            "home" => 0x47,
            "up" => 0x48,
            "pageup" => 0x49,
            "left" => 0x4b,
            "right" => 0x4d,
            "end" => 0x4f,
            "down" => 0x50,
            "pagedown" => 0x51,
            "insert" => 0x52,
            "delete" => 0x53,
            "prtsc" | "printscreen" => 0x54,
            "oem_102" => 0x56,
            "f11" => 0x57,
            "f12" => 0x58,
            "menu" => 0x5d,
            _ => return None,
        };
        Some(code)
    }

pub fn load_config() -> Result<Config> {
    let filepath = get_config_path()?;
    let opt = ParseOption {
        enabled_escape: false,
        ..Default::default()
    };
    let conf = Ini::load_from_file_opt(&filepath, opt)
        .map_err(|err| anyhow!("Failed to load config file '{}', {err}", filepath.display()))?;
    Config::load(&conf)
}

pub(crate) fn edit_config_file() -> Result<bool> {
    let filepath = get_config_path()?;
    debug!("open config file '{}'", filepath.display());
    if !filepath.exists() {
        fs::write(&filepath, DEFAULT_CONFIG).map_err(|err| {
            anyhow!(
                "Failed to write config file '{}', {err}",
                filepath.display()
            )
        })?;
    }
    let exit = Command::new("notepad.exe")
        .arg(&filepath)
        .spawn()
        .map_err(|err| anyhow!("Failed to open config file '{}', {err}", filepath.display()))?
        .wait()
        .map_err(|err| {
            anyhow!(
                "Failed to close config file '{}', {err}",
                filepath.display()
            )
        })?;

    Ok(exit.success())
}

fn get_config_path() -> Result<PathBuf> {
    let folder = get_exe_folder()?;
    let config_path = folder.join("window-switcher.ini");
    Ok(config_path)
}

fn normalize_path_value(value: &str) -> String {
    value.replace("\\\\", "\\")
}

/// Parses a hex color into an ARGB `u32`. Accepts `RRGGBB` (opaque) or `AARRGGBB`,
/// with an optional leading `#`.
fn parse_color(value: &str) -> Option<u32> {
    let s = value.trim().trim_start_matches('#');
    let v = u32::from_str_radix(s, 16).ok()?;
    match s.len() {
        6 => Some(0xff00_0000 | v),
        8 => Some(v),
        _ => None,
    }
}

fn parse_hotkeys(id: u32, name: &str, value: &str) -> Result<Vec<Hotkey>> {
    let parts: Vec<&str> = value.split("||").collect();
    let mut hotkeys = vec![];
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        hotkeys.push(Hotkey::create(id, name, part)?);
    }
    if hotkeys.is_empty() {
        return Err(anyhow!("Invalid {name} hotkey"));
    }
    Ok(hotkeys)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hotkey() {
        assert_eq!(Hotkey::parse("alt + `"), Some(([0x38, 0x38], None, 0x29)));
        assert_eq!(Hotkey::parse("alt + tab"), Some(([0x38, 0x38], None, 0x0f)));
        assert_eq!(
            Hotkey::parse("alt + ctrl + tab"),
            Some(([0x38, 0x38], Some([0x1d, 0x1d]), 0x0f))
        );
        // Two identical modifiers are rejected.
        assert_eq!(Hotkey::parse("alt + alt + tab"), None);
        // More than two modifiers is rejected.
        assert_eq!(Hotkey::parse("alt + ctrl + win + tab"), None);
    }

    #[test]
    fn test_parse_hotkeys() {
        let hotkeys = parse_hotkeys(1, "test", "alt+` || alt+tab").unwrap();
        assert_eq!(hotkeys.len(), 2);
        assert_eq!(hotkeys[0].modifier, [0x38, 0x38]);
        assert_eq!(hotkeys[0].code, 0x29);
        assert_eq!(hotkeys[1].modifier, [0x38, 0x38]);
        assert_eq!(hotkeys[1].code, 0x0f);

        let hotkeys = parse_hotkeys(1, "test", "alt+`").unwrap();
        assert_eq!(hotkeys.len(), 1);
        assert_eq!(hotkeys[0].modifier, [0x38, 0x38]);
        assert_eq!(hotkeys[0].code, 0x29);

        let hotkeys = parse_hotkeys(3, "test", "alt + ctrl + tab").unwrap();
        assert_eq!(hotkeys.len(), 1);
        assert_eq!(hotkeys[0].modifier, [0x38, 0x38]);
        assert_eq!(hotkeys[0].modifier2, Some([0x1d, 0x1d]));
        assert_eq!(hotkeys[0].code, 0x0f);
    }

    #[test]
    fn test_parse_color() {
        assert_eq!(parse_color("e0f3f3f3"), Some(0xe0f3f3f3));
        assert_eq!(parse_color("#590067c0"), Some(0x590067c0));
        assert_eq!(parse_color("0067c0"), Some(0xff0067c0)); // RGB => opaque
        assert_eq!(parse_color("  #ff0000ff  "), Some(0xff0000ff));
        assert_eq!(parse_color("xyz"), None);
        assert_eq!(parse_color("12345"), None); // wrong length
    }
}
