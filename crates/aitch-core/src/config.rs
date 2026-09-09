//! `aitchrc.toml`: the settings, and where they come from.
//!
//! One rule shapes this whole module: **a broken config never stops the
//! editor from opening.** A config that refuses to load leaves you unable to
//! open the editor to fix the config, which is a trap an editor of all things
//! must not set. So loading always yields a usable [`Config`]; a problem comes
//! back beside it as a message for the status line, and the defaults stand in.
//!
//! The file is nanorc in spirit: a short list of the things people actually
//! change, not a settings GUI in TOML.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// Which of the built-in themes to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    #[default]
    Dark,
    Light,
}

impl ThemeChoice {
    pub fn name(self) -> &'static str {
        match self {
            ThemeChoice::Dark => "dark",
            ThemeChoice::Light => "light",
        }
    }
}

/// The font to draw with.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct FontConfig {
    /// A family name, or `None` for whatever the system calls monospace.
    pub family: Option<String>,
    /// Size in logical pixels. Read through `size()`, which keeps it sane.
    pub size: f32,
}

impl FontConfig {
    /// The size to actually draw at.
    ///
    /// Clamped, because the window is built from this: a size of zero gives a
    /// line height of zero, which the text shaper asserts on, and the editor
    /// would abort before there was a status line to complain on — the one
    /// thing this module exists to prevent. NaN falls back to the default for
    /// the same reason.
    pub fn size(&self) -> f32 {
        if self.size.is_nan() {
            return DEFAULT_FONT_SIZE;
        }
        self.size.clamp(MIN_FONT_SIZE, MAX_FONT_SIZE)
    }
}

/// What a font size falls back to, and the range it is held within.
const DEFAULT_FONT_SIZE: f32 = 14.0;
/// Below about four pixels a glyph has no pixels left to be a glyph with.
const MIN_FONT_SIZE: f32 = 4.0;
/// Above this a single line does not fit on a screen.
const MAX_FONT_SIZE: f32 = 200.0;

impl Default for FontConfig {
    fn default() -> FontConfig {
        FontConfig {
            family: None,
            size: DEFAULT_FONT_SIZE,
        }
    }
}

/// Everything `aitchrc.toml` can say.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Config {
    pub theme: ThemeChoice,
    /// `nano`, `modern`, or a path to a keymap file in the same format as the
    /// shipped ones. Writing your own is how custom binds work: the format is
    /// documented, and one mechanism beats two.
    pub keymap: String,
    /// How wide a tab looks, and what the Tab key inserts when expanding.
    pub tab_width: usize,
    /// Insert spaces rather than a tab character.
    pub expand_tabs: bool,
    /// Whether the gutter and whitespace marks start switched on.
    pub line_numbers: bool,
    pub whitespace: bool,
    /// Extra ignore globs, on top of `.gitignore`, for the tree, quick open
    /// and project search.
    pub ignore: Vec<String>,
    pub font: FontConfig,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            theme: ThemeChoice::Dark,
            keymap: "nano".to_string(),
            tab_width: 4,
            expand_tabs: false,
            line_numbers: false,
            whitespace: false,
            ignore: Vec::new(),
            font: FontConfig::default(),
        }
    }
}

impl Config {
    /// What the Tab key inserts.
    pub fn tab_text(&self) -> String {
        if self.expand_tabs {
            " ".repeat(self.tab_width.clamp(1, 16))
        } else {
            "\t".to_string()
        }
    }

    /// Where `aitchrc.toml` lives on this machine.
    ///
    /// Computed from the environment rather than pulled in as a dependency:
    /// it is two rules and they are stable.
    pub fn default_path() -> Option<PathBuf> {
        if cfg!(windows) {
            std::env::var_os("APPDATA")
                .map(|base| PathBuf::from(base).join("aitch").join("aitchrc.toml"))
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config"))
                })
                .map(|base| base.join("aitch").join("aitchrc.toml"))
        }
    }

    /// Load the config, always returning one.
    ///
    /// The second half of the pair is what went wrong, for the status line. A
    /// missing file is not a problem — it is the normal case.
    pub fn load() -> (Config, Option<ConfigError>) {
        match Config::default_path() {
            Some(path) => Config::load_from(&path),
            None => (Config::default(), None),
        }
    }

    /// Load from a particular file, falling back to defaults on any problem.
    pub fn load_from(path: &Path) -> (Config, Option<ConfigError>) {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            // No config file is the ordinary state of a fresh install.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Config::default(), None),
            Err(e) => {
                return (
                    Config::default(),
                    Some(ConfigError {
                        path: path.to_path_buf(),
                        message: e.to_string(),
                    }),
                )
            }
        };

        match toml::from_str::<Config>(&text) {
            Ok(config) => (config, None),
            Err(e) => (
                Config::default(),
                Some(ConfigError {
                    path: path.to_path_buf(),
                    // The first line carries the useful part; the rest is a
                    // span diagram that a one-line status bar cannot show.
                    message: e
                        .message()
                        .lines()
                        .next()
                        .unwrap_or("could not be read")
                        .to_string(),
                }),
            ),
        }
    }
}

/// A config that could not be read. The editor opens anyway.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigError {
    pub path: PathBuf,
    pub message: String,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = self
            .path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| self.path.display().to_string());
        write!(f, "{name}: {} — using defaults", self.message)
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_font_size_that_would_not_open_a_window_is_clamped() {
        // Zero gives a line height of zero, which the shaper asserts on: the
        // editor would abort before there was a status line to complain on,
        // and the only way out would be editing the config in another editor.
        let zero = FontConfig {
            family: None,
            size: 0.0,
        };
        assert!(zero.size() >= MIN_FONT_SIZE);

        let negative = FontConfig {
            family: None,
            size: -12.0,
        };
        assert!(negative.size() >= MIN_FONT_SIZE);

        let enormous = FontConfig {
            family: None,
            size: 100_000.0,
        };
        assert!(enormous.size() <= MAX_FONT_SIZE);

        let nonsense = FontConfig {
            family: None,
            size: f32::NAN,
        };
        assert_eq!(nonsense.size(), DEFAULT_FONT_SIZE);
    }

    #[test]
    fn a_sensible_font_size_is_left_alone() {
        let config = FontConfig {
            family: None,
            size: 13.5,
        };
        assert_eq!(config.size(), 13.5);
    }
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    fn write_config(body: &str) -> (PathBuf, PathBuf) {
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("aitch-config-{}-{unique}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("aitchrc.toml");
        std::fs::write(&path, body).unwrap();
        (dir, path)
    }

    #[test]
    fn a_missing_config_is_not_a_problem() {
        let path = std::env::temp_dir().join("aitch-no-such-config-anywhere.toml");
        let _ = std::fs::remove_file(&path);

        let (config, error) = Config::load_from(&path);
        assert_eq!(config, Config::default());
        assert!(error.is_none(), "a fresh install has no config file");
    }

    #[test]
    fn settings_are_read_from_the_file() {
        let (dir, path) = write_config(
            r#"
            theme = "light"
            keymap = "modern"
            tab_width = 2
            expand_tabs = true
            line_numbers = true
            ignore = ["*.min.js", "vendor/"]

            [font]
            family = "Cascadia Code"
            size = 15.5
            "#,
        );

        let (config, error) = Config::load_from(&path);
        assert!(error.is_none(), "{error:?}");
        assert_eq!(config.theme, ThemeChoice::Light);
        assert_eq!(config.keymap, "modern");
        assert_eq!(config.tab_width, 2);
        assert!(config.expand_tabs);
        assert!(config.line_numbers);
        assert!(!config.whitespace, "unset keys keep their defaults");
        assert_eq!(config.ignore, ["*.min.js", "vendor/"]);
        assert_eq!(config.font.family.as_deref(), Some("Cascadia Code"));
        assert_eq!(config.font.size, 15.5);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_partial_config_leaves_the_rest_alone() {
        let (dir, path) = write_config("tab_width = 8\n");
        let (config, error) = Config::load_from(&path);

        assert!(error.is_none());
        assert_eq!(config.tab_width, 8);
        assert_eq!(config.keymap, "nano", "the default profile stands");
        assert_eq!(config.theme, ThemeChoice::Dark);

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_broken_config_still_opens_the_editor() {
        // The trap this module exists to avoid: a config you cannot open the
        // editor to fix.
        let (dir, path) = write_config("theme = \"light\"\ntab_width = = 4\n");
        let (config, error) = Config::load_from(&path);

        assert_eq!(config, Config::default(), "defaults stood in");
        let error = error.expect("the problem should be reported");
        assert!(error.to_string().contains("using defaults"), "{error}");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unknown_setting_is_reported_rather_than_ignored() {
        // A typo that silently does nothing is worse than one that says so.
        let (dir, path) = write_config("tabwidth = 4\n");
        let (config, error) = Config::load_from(&path);

        assert_eq!(config, Config::default());
        assert!(error.is_some(), "an unknown key should be reported");

        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn an_unknown_theme_is_reported_rather_than_guessed() {
        let (dir, path) = write_config("theme = \"solarized\"\n");
        let (_, error) = Config::load_from(&path);
        assert!(error.is_some());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_tab_key_inserts_what_the_config_says() {
        let mut config = Config::default();
        assert_eq!(config.tab_text(), "\t");

        config.expand_tabs = true;
        config.tab_width = 2;
        assert_eq!(config.tab_text(), "  ");

        // A width nobody meant should not produce a line of a thousand spaces.
        config.tab_width = 9999;
        assert_eq!(config.tab_text().len(), 16);
        config.tab_width = 0;
        assert_eq!(config.tab_text(), " ");
    }

    #[test]
    fn the_default_path_is_under_the_usual_config_directory() {
        let Some(path) = Config::default_path() else {
            // A machine with neither APPDATA nor HOME. Nothing to check.
            return;
        };
        assert!(path.ends_with("aitch/aitchrc.toml") || path.ends_with("aitch\\aitchrc.toml"));
    }
}
