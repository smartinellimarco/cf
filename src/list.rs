use std::collections::HashMap;
use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::codeforces::{Client, ProblemId};
use crate::config::Config;
use crate::{local, statement};

pub struct List {
    pub source: String,
    pub ids: Vec<ProblemId>,
}

pub fn resolve(client: &Client, config: &Config, source: &str) -> Result<List> {
    let named: &HashMap<String, String> = &config.lists;
    let target = named.get(source).map_or(source, String::as_str);
    let ids = if source == "-" {
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            bail!("nothing piped into stdin, try `pbpaste | cf -l -`");
        }
        let mut text = String::new();
        stdin.read_to_string(&mut text).context("reading stdin")?;
        statement::ids(&text)
    } else if is_url(target) {
        statement::ids(&client.get(target)?)
    } else if Path::new(target).is_dir() {
        let mut ids: Vec<ProblemId> = local::find(Path::new(target), config).into_keys().collect();
        ids.sort();
        ids
    } else if Path::new(target).is_file() {
        let text = std::fs::read_to_string(target).with_context(|| format!("reading {target}"))?;
        statement::ids(&text)
    } else {
        bail!("{source} is not a file, a folder, a URL or a list in config.json");
    };
    if ids.is_empty() {
        let label = if source == "-" { "stdin" } else { source };
        bail!("no problems in {label}, a list needs ids like 4A or problem links");
    }
    Ok(List {
        source: source.to_string(),
        ids,
    })
}

fn is_url(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}
