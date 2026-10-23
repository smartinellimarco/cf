mod codeforces;
mod config;
mod fetch;
mod list;
mod local;
mod run;
mod statement;
mod sweep;
mod watch;

use std::process::ExitCode;

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::codeforces::{Client, Handle, Problem, ProblemId, Refresh, Submission};
use crate::config::{Config, Dirs};
use crate::list::List;
use crate::watch::{Setup, Watch};

#[derive(Parser)]
#[command(
    about = "Rustlings-style Codeforces practice",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(help = "Problems to walk, as ids or links; defaults to the solutions in the folder")]
    problems: Vec<String>,
    #[arg(
        short,
        long,
        conflicts_with = "problems",
        help = "List to walk: a file, a URL, - for stdin, a folder, or a name from config.json"
    )]
    list: Option<String>,
    #[arg(
        short,
        long,
        default_value = ".",
        help = "Folder with your solutions, where new ones are created"
    )]
    dir: PathBuf,
    #[arg(long, help = "Language for new solutions, instead of default_lang")]
    lang: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Refresh the problem set and your submissions")]
    Sync,
}

const PROBLEMSET_MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);

fn main() -> ExitCode {
    let cli = Cli::parse();
    match start(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn start(cli: Cli) -> Result<()> {
    let dirs = Dirs::new()?;
    dirs.prune();
    let mut config = Config::load(&dirs)?;
    let mut client = Client::new(dirs.cache.clone())?;
    if config.handle.0.is_empty() {
        let handle = ask_handle()?;
        client.check_handle(&handle)?;
        config.save_handle(&dirs, handle)?;
    }
    match &cli.command {
        Some(Command::Sync) => {
            client.problems(Refresh::Fetch)?;
            client.submissions(&config.handle)?;
            Ok(())
        }
        None => practice(&mut client, &dirs, config, cli),
    }
}

// Reads from the terminal itself because stdin may be a piped list.
fn ask_handle() -> Result<Handle> {
    let Ok(mut tty) = File::options().read(true).write(true).open("/dev/tty") else {
        bail!("no Codeforces handle yet and no terminal to ask for it");
    };
    write!(tty, "Codeforces handle: ")?;
    let mut line = String::new();
    BufReader::new(&tty).read_line(&mut line)?;
    let handle = line.trim();
    if handle.is_empty() {
        bail!("cf needs your Codeforces handle to know what you solved");
    }
    let valid = |c: char| c.is_ascii_alphanumeric() || "_-.".contains(c);
    if !handle.chars().all(valid) {
        bail!("{handle:?} is not a Codeforces handle");
    }
    Ok(Handle(handle.to_string()))
}

fn unknown(ids: &[ProblemId], problems: &[Problem]) -> bool {
    let mut known = HashSet::new();
    for problem in problems {
        if let Some(id) = problem.id() {
            known.insert(id);
        }
    }
    for id in ids {
        if !id.is_gym() && !known.contains(id) {
            return true;
        }
    }
    false
}

// The list decides the columns; the folder fills in the local results and Codeforces the
// submissions, wherever they meet.
fn practice(client: &mut Client, dirs: &Dirs, config: Config, cli: Cli) -> Result<()> {
    if !cli.dir.is_dir() {
        bail!("{} is not a folder", cli.dir.display());
    }
    let root = std::fs::canonicalize(&cli.dir)?;
    if let Some(lang) = &cli.lang
        && !config.langs.contains_key(lang)
    {
        bail!("--lang {lang} has no entry in langs in config.json");
    }
    let folder = cli.list.is_none() && cli.problems.is_empty();
    let list = match &cli.list {
        Some(source) => list::resolve(client, &config, source)?,
        None if folder => {
            let mut ids: Vec<ProblemId> = local::find(&root, &config).into_keys().collect();
            ids.sort();
            List {
                source: root.display().to_string(),
                ids,
            }
        }
        None => {
            let mut ids = Vec::new();
            for text in &cli.problems {
                let id = problem_id(text)?;
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
            List {
                source: "the command line".to_string(),
                ids,
            }
        }
    };

    let mut problems = client.problems(Refresh::Cached)?;
    // A problem the API doesn't list would otherwise trigger a 2 MB download on every start.
    if unknown(&list.ids, &problems) && client.problems_age() > PROBLEMSET_MAX_AGE {
        problems = client.problems(Refresh::Fetch)?;
    }
    let known: HashSet<ProblemId> = problems.iter().filter_map(Problem::id).collect();
    let submissions = client.submissions(&config.handle)?;

    // Gym problems and anything the problemset doesn't have are left out.
    let ids: Vec<ProblemId> = list
        .ids
        .iter()
        .filter(|id| known.contains(id))
        .cloned()
        .collect();
    if ids.is_empty() {
        if folder {
            bail!(
                "no solutions in {} yet, start one with `cf <problem>`",
                list.source
            );
        }
        bail!("no problemset problems in {}", list.source);
    }
    // In the folder, pick up where you last submitted.
    let start = if folder {
        last_submitted(&submissions, &ids)
    } else {
        None
    };

    let setup = Setup {
        root,
        ids,
        start,
        lang: cli.lang,
        problems,
        submissions,
    };
    Watch::new(client, config, dirs, setup)?.run()
}

// Submissions come newest first.
fn last_submitted(submissions: &[Submission], ids: &[ProblemId]) -> Option<ProblemId> {
    submissions
        .iter()
        .filter_map(|submission| submission.problem.id())
        .find(|id| ids.contains(id))
}

fn problem_id(text: &str) -> Result<ProblemId> {
    match statement::ids(text).as_slice() {
        [id] => Ok(id.clone()),
        _ => bail!("{text:?} is not a problem id like 4A or a problem link"),
    }
}
