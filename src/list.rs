use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use crate::codeforces::{Client, ProblemId};
use crate::statement;

pub fn resolve(
    client: &Client,
    last: Option<&PathBuf>,
    source: Option<&str>,
    name: Option<&str>,
) -> Result<PathBuf> {
    let Some(source) = source else {
        // The folder may have been recreated since, so a remembered list that is gone counts as none.
        let Some(last) = last.filter(|path| path.is_file()) else {
            bail!("no list yet, run `cf <file | url | ->`");
        };
        return Ok(last.clone());
    };

    let (text, name) = if source == "-" {
        let mut stdin = std::io::stdin();
        if stdin.is_terminal() {
            bail!("nothing piped into stdin, try `pbpaste | cf - <name>`");
        }
        let Some(name) = name else {
            bail!("a list from stdin needs a name, `cf - <name>`");
        };
        let mut text = String::new();
        stdin.read_to_string(&mut text).context("reading stdin")?;
        (text, name.to_string())
    } else if source.starts_with("http://") || source.starts_with("https://") {
        let name = match name {
            Some(name) => name.to_string(),
            None => url_name(source),
        };
        (client.get(source)?, name)
    } else {
        let path = PathBuf::from(source);
        if !path.is_file() {
            bail!("{source} is not a file");
        }
        return Ok(path);
    };

    if name.is_empty() || name.contains(['/', '\\']) || name.starts_with('.') {
        bail!("{name:?} is not a usable list name");
    }
    let ids = statement::ids(&text);
    if ids.is_empty() {
        let label = if source == "-" { "stdin" } else { source };
        bail!("no problems in {label}, a list needs ids like 4A or problem links");
    }
    let path = PathBuf::from("lists").join(format!("{name}.txt"));
    let mut out = String::new();
    for id in ids {
        out.push_str(&id.to_string());
        out.push('\n');
    }
    std::fs::create_dir_all("lists")?;
    std::fs::write(&path, out).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

fn url_name(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let segment = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
    segment.to_string()
}

pub fn load(path: &Path) -> Result<Vec<ProblemId>> {
    let text =
        std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let ids = statement::ids(&text);
    if ids.is_empty() {
        bail!(
            "no problems in {}, a list needs ids like 4A or problem links",
            path.display()
        );
    }
    Ok(ids)
}
