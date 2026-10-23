use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use etcetera::BaseStrategy;
use serde::Deserialize;

use crate::codeforces::Handle;

#[derive(Deserialize)]
pub struct Config {
    pub handle: Handle,
    #[serde(default = "editor")]
    pub editor: String,
    pub default_lang: String,
    pub langs: HashMap<String, Lang>,
    #[serde(default)]
    pub lists: HashMap<String, String>,
    // `keys` replaces the default keys, `keys+` adds to them or rebinds some.
    keys: Option<HashMap<String, Command>>,
    #[serde(rename = "keys+", default)]
    more_keys: HashMap<String, Command>,
}

// What a key does in the problem view.
#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Command {
    Next,
    Prev,
    Up,
    Down,
    First,
    Last,
    NextFailed,
    PrevFailed,
    NextSolved,
    PrevSolved,
    NextUntouched,
    PrevUntouched,
    NextLocalError,
    PrevLocalError,
    Copy,
    Submit,
    Web,
    Start,
    Swap,
    Rerun,
    Quit,
}

const KEYS: [(&str, Command); 21] = [
    ("l", Command::Next),
    ("h", Command::Prev),
    ("k", Command::Up),
    ("j", Command::Down),
    ("gg", Command::First),
    ("ge", Command::Last),
    ("]f", Command::NextFailed),
    ("[f", Command::PrevFailed),
    ("]s", Command::NextSolved),
    ("[s", Command::PrevSolved),
    ("]t", Command::NextUntouched),
    ("[t", Command::PrevUntouched),
    ("]e", Command::NextLocalError),
    ("[e", Command::PrevLocalError),
    ("y", Command::Copy),
    ("o", Command::Submit),
    ("w", Command::Web),
    ("e", Command::Start),
    ("s", Command::Swap),
    ("r", Command::Rerun),
    ("q", Command::Quit),
];

#[derive(Deserialize, Clone)]
pub struct Lang {
    pub ext: String,
    // A nerd font glyph, in a color of the terminal's palette like "blue".
    pub icon: String,
    #[serde(default = "color")]
    pub color: String,
    pub comment: String,
    pub build: String,
    pub run: String,
    pub submit: String,
}

fn color() -> String {
    "reset".to_string()
}

fn editor() -> String {
    "$EDITOR {source}".to_string()
}

const DEFAULTS: [(&str, &str); 2] = [
    ("config.json", include_str!("../defaults/config.json")),
    (
        "templates/main.cpp",
        include_str!("../defaults/templates/main.cpp"),
    ),
];

pub struct Dirs {
    pub config: PathBuf,
    pub cache: PathBuf,
}

impl Dirs {
    pub fn new() -> Result<Dirs> {
        let strategy = etcetera::choose_base_strategy().context("no home directory")?;
        Ok(Dirs {
            config: strategy.config_dir().join("cf"),
            cache: strategy.cache_dir().join("cf"),
        })
    }

    pub fn templates(&self) -> PathBuf {
        self.config.join("templates")
    }

    pub fn samples(&self) -> PathBuf {
        self.cache.join("samples")
    }

    pub fn builds(&self, folder: &Path) -> PathBuf {
        self.cache.join("build").join(flat(folder))
    }

    pub fn results(&self, folder: &Path) -> PathBuf {
        self.cache.join("results").join(flat(folder))
    }

    // Builds and results of a solutions folder that was moved or deleted would stay forever.
    pub fn prune(&self) {
        for kind in ["build", "results"] {
            let Ok(entries) = std::fs::read_dir(self.cache.join(kind)) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry
                    .file_name()
                    .to_string_lossy()
                    .replace('%', std::path::MAIN_SEPARATOR_STR);
                if !Path::new(&name).is_dir() {
                    let _ = std::fs::remove_dir_all(entry.path());
                }
            }
        }
    }
}

// Different solution folders must not collide in the cache, so each gets its own subfolder.
fn flat(folder: &Path) -> String {
    folder
        .display()
        .to_string()
        .replace(std::path::MAIN_SEPARATOR, "%")
}

impl Config {
    pub fn load(dirs: &Dirs) -> Result<Config> {
        let path = dirs.config.join("config.json");
        if !path.exists() {
            for (name, content) in DEFAULTS {
                let file = dirs.config.join(name);
                if let Some(parent) = file.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                if !file.exists() {
                    std::fs::write(&file, content)
                        .with_context(|| format!("writing {}", file.display()))?;
                }
            }
        }
        let shown = path.display();
        let json = std::fs::read_to_string(&path).with_context(|| format!("reading {shown}"))?;
        let config: Config =
            serde_json::from_str(&json).with_context(|| format!("{shown} is invalid"))?;
        check_placeholders("editor", &config.editor)?;
        for (name, lang) in &config.langs {
            for (field, template) in [
                ("build", &lang.build),
                ("run", &lang.run),
                ("submit", &lang.submit),
            ] {
                check_placeholders(&format!("langs.{name}.{field}"), template)?;
            }
        }
        let keymap = config.keymap();
        for key in keymap.keys() {
            let count = key.chars().count();
            if count == 0 || count > 2 {
                bail!("{shown} binds {key:?}, keys are one or two characters like q or gg");
            }
            // A key bound alone can't also start a two key binding, it would never wait.
            if let Some(first) = key.chars().next()
                && count == 2
                && keymap.contains_key(&first.to_string())
            {
                bail!("{shown} binds both {first:?} and {key:?}, which starts with it");
            }
        }
        if !config.langs.contains_key(&config.default_lang) {
            bail!(
                "{shown} picks default_lang {:?} but langs has no such entry",
                config.default_lang
            );
        }
        let mut exts = HashMap::new();
        for (name, lang) in &config.langs {
            // The extension is how a file finds its lang, so two langs can't share one.
            if crossterm::style::Color::try_from(lang.color.as_str()).is_err() {
                bail!(
                    "{shown} gives lang {name} the color {:?}, the colors are black, red, green, yellow, blue, magenta, cyan, white, grey and their dark_ versions",
                    lang.color
                );
            }
            if let Some(other) = exts.insert(lang.ext.as_str(), name) {
                bail!(
                    "{shown} gives langs {other} and {name} the same ext {:?}",
                    lang.ext
                );
            }
        }
        Ok(config)
    }

    pub fn save_handle(&mut self, dirs: &Dirs, handle: Handle) -> Result<()> {
        let path = dirs.config.join("config.json");
        let json = std::fs::read_to_string(&path)?;
        let mut value: serde_json::Value = serde_json::from_str(&json)?;
        value["handle"] = serde_json::Value::String(handle.0.clone());
        std::fs::write(&path, serde_json::to_string_pretty(&value)? + "\n")
            .with_context(|| format!("writing {}", path.display()))?;
        self.handle = handle;
        Ok(())
    }

    pub fn keymap(&self) -> HashMap<String, Command> {
        let mut keymap: HashMap<String, Command> = match &self.keys {
            Some(keys) => keys.clone(),
            None => KEYS
                .iter()
                .map(|(key, command)| (key.to_string(), *command))
                .collect(),
        };
        keymap.extend(self.more_keys.clone());
        keymap
    }

    pub fn lang_of(&self, source: &Path) -> Option<&Lang> {
        let ext = source.extension()?.to_str()?;
        self.langs.values().find(|lang| lang.ext == ext)
    }
}

const PLACEHOLDERS: [&str; 3] = ["source", "samples", "build"];

fn check_placeholders(field: &str, template: &str) -> Result<()> {
    for name in placeholders(template) {
        if !PLACEHOLDERS.contains(&name) {
            bail!(
                "config {field} uses {{{name}}}, the placeholders are {{source}}, {{samples}} and {{build}}"
            );
        }
    }
    Ok(())
}

// A placeholder is {word}; shell braces like `{ echo; }` have a space after the brace and are left alone.
fn placeholders(template: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find(|c: char| !c.is_ascii_lowercase()) else {
            break;
        };
        if close > 0 && rest[close..].starts_with('}') {
            found.push(&rest[..close]);
        }
    }
    found
}

#[derive(Clone)]
pub struct Paths {
    pub source: PathBuf,
    pub samples: PathBuf,
    pub build: PathBuf,
}

pub enum Quote {
    Shell,
    Plain,
}

impl Paths {
    pub fn expand(&self, template: &str, quote: Quote) -> String {
        let Paths {
            source,
            samples,
            build,
        } = self;
        let mut out = template.to_string();
        for (name, path) in [("source", source), ("samples", samples), ("build", build)] {
            let path = path.display().to_string();
            let value = match quote {
                Quote::Shell => shell_quote(&path),
                Quote::Plain => path,
            };
            out = out.replace(&format!("{{{name}}}"), &value);
        }
        out
    }
}

fn shell_quote(text: &str) -> String {
    // A ~ only expands at the start of a word, so it is safe anywhere else too.
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+~".contains(c);
    if !text.is_empty() && text.chars().all(safe) {
        return text.to_string();
    }
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_placeholders_but_not_shell_braces() {
        let found = placeholders("{ echo '/*'; cat {source}; } > {build}/x {Bad} {}");
        assert_eq!(found, ["source", "build"]);
    }

    #[test]
    fn quotes_paths_only_when_needed() {
        assert_eq!(shell_quote("/tmp/a-b/main.cpp"), "/tmp/a-b/main.cpp");
        assert_eq!(shell_quote("/tmp/my dir/it's"), "'/tmp/my dir/it'\\''s'");
    }
}
