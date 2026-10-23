mod codeforces;
mod config;
mod list;
mod run;
mod statement;
mod sweep;
mod watch;

use std::process::ExitCode;

use std::collections::HashSet;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::time::Duration;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::codeforces::{Client, Handle, Last, Problem, ProblemId, Refresh};
use crate::config::{Config, Dirs};
use crate::watch::Watch;

#[derive(Parser)]
#[command(
    about = "Rustlings-style Codeforces practice",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(help = "Problem id or link to open; defaults to what was open last")]
    problem: Option<String>,
    #[arg(
        short,
        long,
        help = "List to walk: a file, a URL, - for stdin, or a name from config.json"
    )]
    list: Option<String>,
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
    let mut config = Config::load(&dirs)?;
    let mut client = Client::new(dirs.cache.clone())?;
    if config.handle.0.is_empty() {
        let handle = ask_handle()?;
        client.check_handle(&handle)?;
        config.save_handle(&dirs, handle)?;
    }
    match cli.command {
        Some(Command::Sync) => {
            client.problems(Refresh::Fetch)?;
            client.submissions(&config.handle)?;
            Ok(())
        }
        None => practice(
            &mut client,
            &dirs,
            config,
            cli.problem.as_deref(),
            cli.list.as_deref(),
        ),
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

fn practice(
    client: &mut Client,
    dirs: &Dirs,
    config: Config,
    problem: Option<&str>,
    list: Option<&str>,
) -> Result<()> {
    let mut state = client.state();
    let folder = std::env::current_dir()?;
    let last = state.last.remove(&folder);
    let (list, mut start) = match (list, problem, last) {
        (Some(source), _, _) => (Some(list::resolve(client, &config.lists, source)?), None),
        (None, Some(_), _) => (None, None),
        (None, None, Some(Last { list, problem })) => {
            let list = match list {
                Some(list) => Some(list::reload(&config.lists, list)?),
                None => None,
            };
            (list, Some(problem))
        }
        (None, None, None) => bail!("nothing open yet, run `cf <problem>` or `cf -l <list>`"),
    };
    if let Some(text) = problem {
        start = Some(problem_id(text)?);
    }

    let mut problems = client.problems(Refresh::Cached)?;
    let wanted = match &list {
        Some(list) => list.ids.clone(),
        None => start.iter().cloned().collect(),
    };
    // A problem the API doesn't list would otherwise trigger a 2 MB download on every start.
    if unknown(&wanted, &problems) && client.problems_age() > PROBLEMSET_MAX_AGE {
        problems = client.problems(Refresh::Fetch)?;
    }
    let ids = match &list {
        Some(list) => list.ids.clone(),
        None => problemset(&problems),
    };
    if let Some(id) = &start
        && !ids.contains(id)
    {
        match (&list, problem) {
            (Some(list), Some(_)) => bail!("{id} is not in {}", list.source),
            (None, _) if id.is_gym() => {
                bail!("{id} is a gym problem, cf only opens problemset problems")
            }
            (None, _) => bail!("{id} is not in the Codeforces problemset"),
            // The list changed since cf last had it open.
            (Some(_), None) => start = None,
        }
    }

    let submissions = client.submissions(&config.handle)?;
    let mut watch = Watch::new(client, config, dirs, ids, start, problems, submissions)?;
    let problem = watch.run()?;
    state.last.insert(folder, Last { list, problem });
    client.save_state(&state)
}

fn problem_id(text: &str) -> Result<ProblemId> {
    match statement::ids(text).as_slice() {
        [id] => Ok(id.clone()),
        _ => bail!("{text:?} is not a problem id like 4A or a problem link"),
    }
}

// The whole problemset, oldest contest first, for walking it without a list.
fn problemset(problems: &[Problem]) -> Vec<ProblemId> {
    let mut ids: Vec<ProblemId> = problems.iter().filter_map(Problem::id).collect();
    ids.sort();
    ids.dedup();
    ids
}
