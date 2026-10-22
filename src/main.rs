mod codeforces;
mod config;
mod list;
mod run;
mod statement;
mod watch;

use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};

use crate::codeforces::{Client, Refresh};
use crate::config::Config;
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
    #[command(about = "Set up the current folder for solutions")]
    Init { handle: String },
    #[command(about = "Refresh the problem set and your submissions")]
    Sync,
}

const ASSETS: [(&str, &str); 3] = [
    ("cf.json", include_str!("../init/cf.json")),
    (".gitignore", include_str!("../init/gitignore")),
    (
        "templates/main.cpp",
        include_str!("../init/templates/main.cpp"),
    ),
];

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
    let mut client = Client::new()?;
    match cli.command {
        Some(Command::Init { handle }) => init(&mut client, &handle),
        Some(Command::Sync) => {
            let config = Config::load()?;
            client.problems(Refresh::Fetch)?;
            client.submissions(&config.handle)?;
            Ok(())
        }
        None => practice(
            &mut client,
            cli.source.as_deref(),
            cli.name.as_deref(),
            cli.fresh,
        ),
    }
}

fn init(client: &mut Client, handle: &str) -> Result<()> {
    if std::fs::exists("cf.json")? {
        bail!("cf.json already exists here");
    }
    for (path, content) in ASSETS {
        if let Some(dir) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(dir)?;
        }
        let content = content.replace("HANDLE", handle);
        std::fs::write(path, content).with_context(|| format!("writing {path}"))?;
    }
    std::fs::create_dir_all("lists")?;
    std::fs::create_dir_all("problems")?;
    client.problems(Refresh::Fetch)?;
    Ok(())
}

fn practice(
    client: &mut Client,
    source: Option<&str>,
    name: Option<&str>,
    fresh: bool,
) -> Result<()> {
    let config = Config::load()?;
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
    let mut watch = Watch::new(client, config, ids, since, problems, submissions)?;
    watch.run()
}
