mod codeforces;
mod config;
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
use crate::watch::{Setup, Watch};

#[derive(Parser)]
#[command(
    about = "Rustlings-style Codeforces practice",
    args_conflicts_with_subcommands = true
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(help = "Problem id or link to open; defaults to your last submission")]
    problem: Option<String>,
    #[arg(
        short,
        long,
        help = "List to walk: a file, a URL, - for stdin, or a name from config.json"
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
    let list = match &cli.list {
        Some(source) => Some(list::resolve(client, &config, source)?),
        None => None,
    };
    let start = match &cli.problem {
        Some(text) => Some(problem_id(text)?),
        None => None,
    };

    let mut problems = client.problems(Refresh::Cached)?;
    let wanted = match &list {
        Some(list) => list.ids.clone(),
        None => start.iter().cloned().collect(),
    };
    // A problem the API doesn't list would otherwise trigger a 2 MB download on every start.
    if unknown(&wanted, &problems) && client.problems_age() > PROBLEMSET_MAX_AGE {
        problems = client.problems(Refresh::Fetch)?;
    }
    let known: HashSet<ProblemId> = problems.iter().filter_map(Problem::id).collect();
    let submissions = client.submissions(&config.handle)?;

    // Gym problems and anything the problemset doesn't have are left out.
    let (ids, start) = match (&list, start) {
        (Some(list), start) => {
            let ids: Vec<ProblemId> = list
                .ids
                .iter()
                .filter(|id| known.contains(id))
                .cloned()
                .collect();
            if ids.is_empty() {
                bail!("no problemset problems in {}", list.source);
            }
            if let Some(id) = &start
                && !ids.contains(id)
            {
                bail!("{id} is not in {}", list.source);
            }
            (ids, start)
        }
        (None, Some(id)) if id.is_gym() => bail!("{id} is a gym problem, cf ignores those"),
        (None, Some(id)) if !known.contains(&id) => {
            bail!("{id} is not in the Codeforces problemset")
        }
        (None, start) => {
            let anchor = start.unwrap_or_else(|| last_submitted(&submissions, &known));
            (timeline(&known, &submissions, &anchor), Some(anchor))
        }
    };

    let setup = Setup {
        root,
        listed: list.is_some(),
        ids,
        start,
        lang: cli.lang,
        problems,
        submissions,
    };
    Watch::new(client, config, dirs, setup)?.run()
}

// Submissions come newest first; with none to a problemset problem, start at the very first one.
fn last_submitted(submissions: &[Submission], known: &HashSet<ProblemId>) -> ProblemId {
    for submission in submissions {
        if let Some(id) = submission.problem.id()
            && known.contains(&id)
        {
            return id;
        }
    }
    ProblemId {
        contest: 1,
        index: "A".to_string(),
    }
}

fn problem_id(text: &str) -> Result<ProblemId> {
    match statement::ids(text).as_slice() {
        [id] => Ok(id.clone()),
        _ => bail!("{text:?} is not a problem id like 4A or a problem link"),
    }
}

// Without a list: the problems you submitted to, then the problemset from where you start on.
fn timeline(
    known: &HashSet<ProblemId>,
    submissions: &[Submission],
    anchor: &ProblemId,
) -> Vec<ProblemId> {
    let mut ids: Vec<ProblemId> = known.iter().filter(|id| *id >= anchor).cloned().collect();
    for submission in submissions {
        if let Some(id) = submission.problem.id()
            && known.contains(&id)
        {
            ids.push(id);
        }
    }
    ids.sort();
    ids.dedup();
    ids
}
