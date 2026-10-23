use std::collections::HashMap;
use std::io::{IsTerminal, Read};
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::codeforces::{Client, ProblemId};
use crate::statement;

#[derive(Serialize, Deserialize)]
pub struct List {
    pub source: String,
    pub ids: Vec<ProblemId>,
}

pub fn resolve(client: &Client, named: &HashMap<String, String>, source: &str) -> Result<List> {
    let text = if source == "-" {
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            bail!("nothing piped into stdin, try `pbpaste | cf -l -`");
        }
        let mut text = String::new();
        stdin.read_to_string(&mut text).context("reading stdin")?;
        text
    } else {
        let target = named.get(source).map_or(source, String::as_str);
        if is_url(target) {
            client.get(target)?
        } else if Path::new(target).is_file() {
            read(target)?
        } else {
            bail!("{source} is not a file, a URL or a list in config.json");
        }
    };
    let ids = statement::ids(&text);
    if ids.is_empty() {
        let label = if source == "-" { "stdin" } else { source };
        bail!("no problems in {label}, a list needs ids like 4A or problem links");
    }
    Ok(List {
        source: source.to_string(),
        ids,
    })
}

// Files are read again since they may have been edited; URLs and stdin keep the ids they had.
pub fn reload(named: &HashMap<String, String>, list: List) -> Result<List> {
    let target = named
        .get(&list.source)
        .map_or(list.source.as_str(), String::as_str);
    if is_url(target) || !Path::new(target).is_file() {
        return Ok(list);
    }
    let ids = statement::ids(&read(target)?);
    if ids.is_empty() {
        bail!("no problems in {target}, a list needs ids like 4A or problem links");
    }
    Ok(List { ids, ..list })
}

fn is_url(source: &str) -> bool {
    source.starts_with("http://") || source.starts_with("https://")
}

fn read(path: &str) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {path}"))
}
