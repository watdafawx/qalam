//! Settings and replacement files, and DPAPI encryption for secrets.
//!
//! Data lives in %APPDATA%\qalam, or in a `qalam-data` folder next to the exe (portable mode).

use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub const DEFAULT_CONFIG: &str = r#"# qalam settings. Saved changes are picked up automatically within a couple of seconds.
# Replacements are edited in the Replacements window (tray icon > Replacements).

[general]
suggestions = true          # word completion popup
autocorrect = true          # fix typos when you finish a word (Backspace right after undoes it)
min_prefix = 2              # letters typed before suggestions appear
max_suggestions = 3
accept_with_tab = true      # Tab accepts the highlighted suggestion while the popup is open
space_after_accept = true
# How a line break inside a replacement is typed: "shift+enter" (safe in chat apps) or "enter".
newline = "shift+enter"
# Which apps qalam runs in (tray > Apps... edits these):
#   "blacklist": everywhere except disabled_apps;  "whitelist": only in enabled_apps.
app_mode = "blacklist"
disabled_apps = ["KeePassXC.exe", "1Password.exe"]
enabled_apps = []

# Languages: "en", "ar". All of them are used at once (by the script you type in),
# or set language_mode = "keyboard" to use only the language of the active keyboard layout.
languages = ["en"]
language_mode = "all"
autocorrect_languages = ["en"]   # Arabic is suggestions/underlines only unless you add "ar"
arabizi = false                  # suggest Arabic for Arabizi ("mar7aba" -> مرحبا)

disable_in_fullscreen = true     # off while a fullscreen app (games, video) is in front
auto_update = true               # install new releases from GitHub automatically (checked at start + every 12 h)
highlight_corrections = true     # flash the corrected word briefly
underline_misspelled = true      # squiggly lines under unknown words (apps with UI Automation text support)
underline_skip_apps = []         # e.g. apps that already have their own spell check
# Apps where only text replacement runs: no popup, autocorrect, underlines or learning (terminals, by default),
# so keys like Up/Tab/Enter always reach the app.
replacements_only_apps = ["WindowsTerminal.exe", "OpenConsole.exe", "conhost.exe", "cmd.exe", "powershell.exe", "pwsh.exe", "wezterm-gui.exe", "alacritty.exe", "mintty.exe", "Tabby.exe", "Hyper.exe", "putty.exe", "kitty.exe"]

# Your own autocorrect fixes (wrong = "right"), on top of the built-in list.
[autocorrect]
# "teh" = "the"
"#;

pub const PORTABLE_DIR: &str = "qalam-data";

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
pub struct Config {
    pub general: General,
    /// Old location for replacements; moved to replacements.toml on startup.
    pub replacements: BTreeMap<String, String>,
    pub autocorrect: BTreeMap<String, String>,
}

#[derive(Deserialize, Clone)]
#[serde(default)]
pub struct General {
    pub suggestions: bool,
    pub autocorrect: bool,
    pub min_prefix: usize,
    pub max_suggestions: usize,
    pub accept_with_tab: bool,
    pub space_after_accept: bool,
    pub newline: String,
    pub disabled_apps: Vec<String>,
    pub app_mode: String,
    pub enabled_apps: Vec<String>,
    pub languages: Vec<String>,
    pub language_mode: String,
    pub autocorrect_languages: Vec<String>,
    pub arabizi: bool,
    pub highlight_corrections: bool,
    pub auto_update: bool,
    pub disable_in_fullscreen: bool,
    pub underline_misspelled: bool,
    pub underline_skip_apps: Vec<String>,
    pub replacements_only_apps: Vec<String>,
}

impl Default for General {
    fn default() -> Self {
        General {
            suggestions: true,
            autocorrect: true,
            min_prefix: 2,
            max_suggestions: 3,
            accept_with_tab: true,
            space_after_accept: true,
            newline: "shift+enter".into(),
            disabled_apps: vec![],
            app_mode: "blacklist".into(),
            enabled_apps: vec![],
            languages: vec!["en".into()],
            language_mode: "all".into(),
            autocorrect_languages: vec!["en".into()],
            arabizi: false,
            highlight_corrections: true,
            auto_update: true,
            disable_in_fullscreen: true,
            underline_misspelled: true,
            underline_skip_apps: vec![],
            replacements_only_apps: vec!["WindowsTerminal.exe".into(), "OpenConsole.exe".into(), "conhost.exe".into(), "cmd.exe".into(), "powershell.exe".into(), "pwsh.exe".into(), "wezterm-gui.exe".into(), "alacritty.exe".into(), "mintty.exe".into(), "Tabby.exe".into(), "Hyper.exe".into(), "putty.exe".into(), "kitty.exe".into()],
        }
    }
}

/// Folder holding the running exe.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok()?.parent().map(PathBuf::from)
}

/// Portable mode: a `qalam-data` folder sits next to the exe.
pub fn portable() -> bool {
    exe_dir().is_some_and(|d| d.join(PORTABLE_DIR).is_dir())
}

pub fn dir() -> PathBuf {
    if let Some(d) = exe_dir().map(|d| d.join(PORTABLE_DIR)).filter(|d| d.is_dir()) {
        return d;
    }
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    base.join("qalam")
}
/// Earlier builds were called "typeahead": move their data folder over once.
pub fn migrate_old_name() {
    let Some(base) = std::env::var_os("APPDATA").map(PathBuf::from) else { return };
    let (old, new) = (base.join("typeahead"), base.join("qalam"));
    if old.is_dir() && !new.exists() {
        let _ = std::fs::rename(&old, &new);
    }
}

pub fn config_path() -> PathBuf {
    dir().join("config.toml")
}
pub fn replacements_path() -> PathBuf {
    dir().join("replacements.toml")
}
pub fn secrets_path() -> PathBuf {
    dir().join("secrets.toml")
}
pub fn learned_path() -> PathBuf {
    dir().join("learned.txt")
}

/// Load config.toml (writing the default one first if missing). Returns an error string to show the user.
pub fn load() -> Result<Config, String> {
    let p = config_path();
    if !p.exists() {
        let _ = std::fs::create_dir_all(dir());
        std::fs::write(&p, DEFAULT_CONFIG).map_err(|e| format!("can't write {}: {e}", p.display()))?;
    }
    let text = std::fs::read_to_string(&p).map_err(|e| format!("can't read {}: {e}", p.display()))?;
    toml::from_str(&text).map_err(|e| format!("config.toml: {e}"))
}

fn load_map(p: PathBuf) -> BTreeMap<String, String> {
    std::fs::read_to_string(p).ok().and_then(|t| toml::from_str(&t).ok()).unwrap_or_default()
}

fn save_map(p: PathBuf, header: &str, map: &BTreeMap<String, String>) -> std::io::Result<()> {
    std::fs::create_dir_all(dir())?;
    let body = toml::to_string(map).map_err(std::io::Error::other)?;
    std::fs::write(p, format!("{header}\n{body}"))
}

/// Plain replacements: trigger -> text.
pub fn load_replacements() -> BTreeMap<String, String> {
    load_map(replacements_path())
}

pub fn save_replacements(map: &BTreeMap<String, String>) -> std::io::Result<()> {
    save_map(replacements_path(), "# Managed by the qalam Replacements window.", map)
}

/// Secret replacements: trigger -> base64 DPAPI blob.
pub fn load_secrets_raw() -> BTreeMap<String, String> {
    load_map(secrets_path())
}

pub fn save_secrets_raw(map: &BTreeMap<String, String>) -> std::io::Result<()> {
    save_map(
        secrets_path(),
        "# Managed by qalam. Values are encrypted with your Windows login (DPAPI) and only work for this user.",
        map,
    )
}

/// Decrypted secrets: trigger -> plaintext. Undecryptable entries are skipped.
pub fn load_secrets() -> BTreeMap<String, String> {
    load_secrets_raw()
        .into_iter()
        .filter_map(|(k, v)| dpapi::decrypt_b64(&v).map(|p| (k, p)))
        .collect()
}

/// Everything the engine should expand beyond config.toml: replacements.toml + decrypted secrets.
pub fn extra_replacements() -> BTreeMap<String, String> {
    let mut m = load_replacements();
    m.extend(load_secrets());
    m
}

/// Move a `[replacements]` table from config.toml into replacements.toml (older versions kept them there).
pub fn migrate_replacements() {
    let Ok(text) = std::fs::read_to_string(config_path()) else { return };
    let Ok(cfg) = toml::from_str::<Config>(&text) else { return };
    if cfg.replacements.is_empty() {
        return;
    }
    let mut map = load_replacements();
    for (k, v) in cfg.replacements {
        map.entry(k).or_insert(v);
    }
    if save_replacements(&map).is_ok() {
        let _ = std::fs::write(config_path(), strip_table(&text, "replacements"));
    }
}

/// Change one `[general]` setting in config.toml in place, keeping comments. `value` is TOML (e.g. `true`, `["en", "ar"]`).
pub fn set_general(key: &str, value: &str) -> std::io::Result<()> {
    let _ = load(); // make sure the file exists
    let text = std::fs::read_to_string(config_path())?;
    std::fs::write(config_path(), set_key_in(&text, "general", key, value))
}

fn set_key_in(text: &str, table: &str, key: &str, value: &str) -> String {
    let header = format!("[{table}]");
    let mut out: Vec<String> = Vec::new();
    let mut in_table = false;
    let mut done = false;
    let mut header_at = None;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_table = t.split('#').next().unwrap_or("").trim() == header;
            if in_table {
                header_at = Some(out.len());
            }
        } else if in_table && !done {
            let k = t.split('=').next().unwrap_or("").trim();
            if t.contains('=') && !t.starts_with('#') && k == key {
                // Keep a trailing comment in the same column.
                let start = format!("{key} = ");
                let mut new = format!("{start}{value}");
                if let Some(h) = line.find('#') {
                    let col = h.max(new.len() + 1);
                    new = format!("{new:<col$}{}", &line[h..]);
                }
                out.push(new);
                done = true;
                continue;
            }
        }
        out.push(line.to_string());
    }
    if !done {
        match header_at {
            Some(i) => out.insert(i + 1, format!("{key} = {value}")),
            None => {
                out.insert(0, String::new());
                out.insert(0, format!("{key} = {value}"));
                out.insert(0, header);
            }
        }
    }
    out.join("\n") + "\n"
}

/// Remove a top-level `[name]` table (header up to the next table header) from TOML text.
fn strip_table(text: &str, name: &str) -> String {
    let header = format!("[{name}]");
    let mut out = String::new();
    let mut skipping = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            skipping = t.split('#').next().unwrap_or("").trim() == header;
        }
        if !skipping {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

pub mod dpapi {
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    };
    use windows::core::PCWSTR;

    pub fn encrypt_b64(plain: &str) -> Option<String> {
        let data = plain.as_bytes();
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptProtectData(&input, PCWSTR::null(), None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).ok()?;
            let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(out.pbData as _)));
            Some(STANDARD.encode(bytes))
        }
    }

    pub fn decrypt_b64(b64: &str) -> Option<String> {
        let data = STANDARD.decode(b64.trim()).ok()?;
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out = CRYPT_INTEGER_BLOB::default();
        unsafe {
            CryptUnprotectData(&input, None, None, None, None, CRYPTPROTECT_UI_FORBIDDEN, &mut out).ok()?;
            let bytes = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
            let _ = LocalFree(Some(HLOCAL(out.pbData as _)));
            String::from_utf8(bytes).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn default_config_parses() {
        let c: super::Config = toml::from_str(super::DEFAULT_CONFIG).unwrap();
        assert!(c.replacements.is_empty());
        assert_eq!(c.general.newline, "shift+enter");
    }

    #[test]
    fn strip_replacements_table() {
        let t = "[general]\na = 1\n\n[replacements]\n\"x\" = \"y\"\n\n[autocorrect] # mine\n\"teh\" = \"the\"\n";
        let s = super::strip_table(t, "replacements");
        assert!(!s.contains("\"x\""));
        assert!(s.contains("[autocorrect] # mine") && s.contains("a = 1"));
        let c: super::Config = toml::from_str(&s).unwrap();
        assert_eq!(c.autocorrect["teh"], "the");
    }

    #[test]
    fn set_general_keeps_comments() {
        let t = "[general]\nsuggestions = true          # popup\nmin_prefix = 2\n\n[autocorrect]\n";
        let s = super::set_key_in(t, "general", "suggestions", "false");
        assert!(s.contains("suggestions = false         # popup"), "{s}"); // "#" stays in its column
        let s = super::set_key_in(&s, "general", "languages", "[\"en\", \"ar\"]");
        let c: super::Config = toml::from_str(&s).unwrap();
        assert_eq!(c.general.languages, vec!["en", "ar"]);
        assert!(!c.general.suggestions);
    }

    #[test]
    fn map_with_newlines_roundtrips() {
        let mut m = std::collections::BTreeMap::new();
        m.insert("@@addr".to_string(), "1 Street\n\"Town\"".to_string());
        let s = toml::to_string(&m).unwrap();
        let back: std::collections::BTreeMap<String, String> = toml::from_str(&s).unwrap();
        assert_eq!(back, m);
    }

    #[test]
    fn dpapi_roundtrip() {
        let e = super::dpapi::encrypt_b64("hunter2 £").unwrap();
        assert_eq!(super::dpapi::decrypt_b64(&e).unwrap(), "hunter2 £");
    }
}
