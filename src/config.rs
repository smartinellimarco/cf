use std::collections::HashMap;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::codeforces::Handle;

#[derive(Deserialize)]
pub struct Config {
    pub handle: Handle,
    #[serde(default = "editor")]
    pub editor: String,
    pub lang: String,
    pub langs: HashMap<String, Lang>,
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
    "$EDITOR {file}".to_string()
}

impl Config {
    pub fn load() -> Result<Config> {
        let Ok(json) = std::fs::read_to_string("cf.json") else {
            bail!("no cf.json here, run `cf init <handle>` in the folder for your solutions");
        };
        let config: Config = serde_json::from_str(&json).context("cf.json is invalid")?;
        if config.handle.0.is_empty() {
            bail!("cf.json has an empty handle");
        }
        if !config.langs.contains_key(&config.lang) {
            bail!(
                "cf.json picks lang {:?} but langs has no such entry",
                config.lang
            );
        }
        Ok(config)
    }

    pub fn lang(&self) -> &Lang {
        &self.langs[&self.lang]
    }
}
