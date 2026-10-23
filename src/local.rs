use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use ignore::WalkBuilder;

use crate::codeforces::ProblemId;
use crate::config::Config;

// Every `<id>.<ext>` file under `dir` whose extension belongs to a lang, newest first per problem.
// The same id and extension twice keeps only the most recently edited one.
pub fn find(dir: &Path, config: &Config) -> HashMap<ProblemId, Vec<PathBuf>> {
    let mut found: HashMap<ProblemId, Vec<(SystemTime, PathBuf)>> = HashMap::new();
    // Without require_git(false) a .gitignore only counts inside a git repository.
    for entry in WalkBuilder::new(dir).require_git(false).build().flatten() {
        let path = entry.path();
        if !path.is_file() || config.lang_of(path).is_none() {
            continue;
        }
        let Some(Ok(id)) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::parse::<ProblemId>)
        else {
            continue;
        };
        if id.is_gym() {
            continue;
        }
        let edited = entry
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        found
            .entry(id)
            .or_default()
            .push((edited, path.to_path_buf()));
    }

    let mut sources = HashMap::new();
    for (id, mut files) in found {
        files.sort_by_key(|(edited, _)| std::cmp::Reverse(*edited));
        let mut kept: Vec<PathBuf> = Vec::new();
        for (_, path) in files {
            if !kept
                .iter()
                .any(|other| other.extension() == path.extension())
            {
                kept.push(path);
            }
        }
        sources.insert(id, kept);
    }
    sources
}
