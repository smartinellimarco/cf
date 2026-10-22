mod codeforces;
mod config;
mod list;
mod run;
mod statement;
mod watch;

use std::process::ExitCode;

use std::fs::File;
use std::io::{BufRead, BufReader, Write};

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};

use crate::codeforces::{Client, Handle, Refresh};
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
    #[arg(help = "List file, URL, or - for stdin; defaults to the last list")]
    source: Option<String>,
    #[arg(help = "Name to save a URL or stdin list under")]
    name: Option<String>,
    #[arg(long, help = "Only count accepted submissions from now on")]
    fresh: bool,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Refresh the problem set and your submissions")]
    Sync,
}

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
            cli.source.as_deref(),
            cli.name.as_deref(),
            cli.fresh,
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

fn practice(
    client: &mut Client,
    dirs: &Dirs,
    config: Config,
    source: Option<&str>,
    name: Option<&str>,
    fresh: bool,
) -> Result<()> {
    let mut state = client.state();
    let folder = std::env::current_dir()?;
    let path = list::resolve(client, state.last.get(&folder), source, name)?;
    let ids = list::load(&path)?;

    let key = folder.join(&path);
    if fresh {
        state.fresh.insert(key.clone(), codeforces::now());
    }
    let since = state.fresh.get(&key).copied().unwrap_or(0);
    state.last.insert(folder, path);
    client.save_state(&state)?;

    let problems = client.problems(Refresh::Cached)?;
    let submissions = client.submissions(&config.handle)?;
    let mut watch = Watch::new(client, config, dirs, ids, since, problems, submissions)?;
    watch.run()
}
