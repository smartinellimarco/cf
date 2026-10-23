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
    pub lang: String,
    pub langs: HashMap<String, Lang>,
    #[serde(default)]
    pub lists: HashMap<String, String>,
}

#[derive(Deserialize, Clone)]
pub struct Lang {
    pub ext: String,
    pub comment: String,
    pub build: String,
    pub run: String,
    pub submit: String,
    pub judge: String,
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

    // Builds of different solution folders must not collide, so each folder gets its own subfolder.
    pub fn builds(&self, folder: &Path) -> PathBuf {
        let name = folder
            .display()
            .to_string()
            .replace(std::path::MAIN_SEPARATOR, "%");
        self.cache.join("build").join(name)
    }
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
        if !config.langs.contains_key(&config.lang) {
            bail!(
                "{shown} picks lang {:?} but langs has no such entry",
                config.lang
            );
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

    pub fn lang(&self) -> &Lang {
        &self.langs[&self.lang]
    }
}

const PLACEHOLDERS: [&str; 3] = ["dir", "source", "build"];

fn check_placeholders(field: &str, template: &str) -> Result<()> {
    for name in placeholders(template) {
        if !PLACEHOLDERS.contains(&name) {
            bail!(
                "config {field} uses {{{name}}}, the placeholders are {{dir}}, {{source}} and {{build}}"
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

pub struct Paths {
    pub dir: PathBuf,
    pub source: PathBuf,
    pub build: PathBuf,
}

pub enum Quote {
    Shell,
    Plain,
}

impl Paths {
    pub fn expand(&self, template: &str, quote: Quote) -> String {
        let Paths { dir, source, build } = self;
        let mut out = template.to_string();
        for (name, path) in [("dir", dir), ("source", source), ("build", build)] {
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
    let safe = |c: char| c.is_ascii_alphanumeric() || "/._-+".contains(c);
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
