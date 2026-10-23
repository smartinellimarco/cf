use std::fs::File;
use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{Lang, Paths, Quote};
use crate::statement::Files;

const BUILD_TIMEOUT: Duration = Duration::from_secs(120);
const RUN_TIMEOUT: Duration = Duration::from_secs(5);
// A runaway print loop stops being read here and then hits the timeout.
const OUTPUT_LIMIT: u64 = 16 << 20;
const REAL_ERROR: f64 = 1e-6;

#[derive(Serialize, Deserialize)]
pub enum Outcome {
    Pass {
        elapsed: Duration,
    },
    Wrong {
        elapsed: Duration,
        expected: String,
        got: String,
    },
    Crash {
        elapsed: Duration,
        stderr: String,
    },
    Timeout,
}

#[derive(Serialize, Deserialize)]
pub enum Report {
    Downloading,
    NoSolution,
    Building,
    Failed(String),
    BuildFailed(String),
    NoSamples,
    Ran(Vec<(u32, Outcome)>),
}

impl Report {
    pub fn passed(&self) -> bool {
        match self {
            Report::Ran(outcomes) => {
                for (_, outcome) in outcomes {
                    match outcome {
                        Outcome::Pass { .. } => {}
                        Outcome::Wrong { .. } | Outcome::Crash { .. } | Outcome::Timeout => {
                            return false;
                        }
                    }
                }
                true
            }
            Report::Downloading
            | Report::NoSolution
            | Report::Building
            | Report::Failed(_)
            | Report::BuildFailed(_)
            | Report::NoSamples => false,
        }
    }
}

struct Finished {
    success: bool,
    elapsed: Duration,
    stdout: String,
    stderr: String,
}

// Setting `cancel` kills whatever is running and makes the rest return at once.
pub fn check(paths: &Paths, lang: &Lang, files: &Files, cancel: &AtomicBool) -> Result<Report> {
    let dir = paths.samples.as_path();
    // Commands run inside build/ so whatever a compiler drops in its working directory stays ignored.
    let cwd = paths.build.as_path();
    std::fs::create_dir_all(cwd)?;
    let build = paths.expand(&lang.build, Quote::Shell);
    let Some(build) = sh(cwd, &build, Stdio::null(), BUILD_TIMEOUT, cancel)? else {
        return Ok(Report::BuildFailed("build timed out".to_string()));
    };
    if !build.success {
        return Ok(Report::BuildFailed(format!(
            "{}{}",
            build.stdout, build.stderr
        )));
    }

    let samples = samples(dir)?;
    if samples.is_empty() {
        return Ok(Report::NoSamples);
    }
    let run = paths.expand(&lang.run, Quote::Shell);
    let mut outcomes = Vec::new();
    for k in samples {
        let input = dir.join(format!("in{k}.txt"));
        let expected = std::fs::read_to_string(dir.join(format!("ans{k}.txt")))?;
        let stdin = match &files.input {
            Some(name) => {
                std::fs::copy(&input, cwd.join(name))?;
                Stdio::null()
            }
            None => Stdio::from(File::open(&input)?),
        };
        if let Some(name) = &files.output {
            let _ = std::fs::remove_file(cwd.join(name));
        }
        let outcome = match sh(cwd, &run, stdin, RUN_TIMEOUT, cancel)? {
            None => Outcome::Timeout,
            Some(Finished {
                success: false,
                elapsed,
                stderr,
                ..
            }) => Outcome::Crash { elapsed, stderr },
            Some(Finished {
                success: true,
                elapsed,
                stdout,
                ..
            }) => {
                let got = match &files.output {
                    Some(name) => std::fs::read_to_string(cwd.join(name)).unwrap_or_default(),
                    None => stdout,
                };
                if same_tokens(&got, &expected) {
                    Outcome::Pass { elapsed }
                } else {
                    Outcome::Wrong {
                        elapsed,
                        expected,
                        got,
                    }
                }
            }
        };
        outcomes.push((k, outcome));
    }
    Ok(Report::Ran(outcomes))
}

pub fn samples(dir: &Path) -> Result<Vec<u32>> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let name = entry?.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Some(k) = name
            .strip_prefix("in")
            .and_then(|rest| rest.strip_suffix(".txt"))
        else {
            continue;
        };
        let Ok(k) = k.parse() else {
            continue;
        };
        if dir.join(format!("ans{k}.txt")).is_file() {
            found.push(k);
        }
    }
    found.sort();
    Ok(found)
}

// Codeforces' default checker compares whitespace-separated tokens.
fn same_tokens(got: &str, expected: &str) -> bool {
    let mut got = got.split_whitespace();
    let mut expected = expected.split_whitespace();
    loop {
        match (got.next(), expected.next()) {
            (None, None) => return true,
            (Some(got), Some(expected)) if same_token(got, expected) => {}
            _ => return false,
        }
    }
}

// Real answers pass within 1e-6 absolute or relative error, like Codeforces' rcmp6 checker.
// Only an expected token with a decimal point is read as a real; integers must match exactly.
fn same_token(got: &str, expected: &str) -> bool {
    if got == expected {
        return true;
    }
    if !expected.contains('.') {
        return false;
    }
    let (Ok(got), Ok(expected)) = (got.parse::<f64>(), expected.parse::<f64>()) else {
        return false;
    };
    (got - expected).abs() <= REAL_ERROR * expected.abs().max(1.0)
}

fn sh(
    dir: &Path,
    command: &str,
    stdin: Stdio,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<Option<Finished>> {
    let start = Instant::now();
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(dir)
        .stdin(stdin)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .context("running sh")?;
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let Some(status) = wait(&mut child, timeout, cancel)? else {
        // sh -c forks the real program, so kill the whole group or it keeps running.
        Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .status()?;
        child.wait()?;
        return Ok(None);
    };
    let elapsed = start.elapsed();
    Ok(Some(Finished {
        success: status.success(),
        elapsed,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    }))
}

fn drain(pipe: Option<impl Read + Send + 'static>) -> JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(pipe) = pipe {
            let _ = pipe.take(OUTPUT_LIMIT).read_to_end(&mut bytes);
        }
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn wait(
    child: &mut Child,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<Option<std::process::ExitStatus>> {
    let start = Instant::now();
    while start.elapsed() < timeout && !cancel.load(Ordering::Relaxed) {
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_tokens_like_the_default_checker() {
        assert!(same_tokens("1 2\n3\n", "1 2 3"));
        assert!(!same_tokens("1 2", "1 2 3"));
    }

    #[test]
    fn compares_reals_within_the_checker_error() {
        assert!(same_tokens("0.5000001", "0.500000"));
        assert!(same_tokens("1000000.5", "1000000.000000"));
        assert!(!same_tokens("0.501", "0.500000"));
        assert!(!same_tokens("3", "2"));
        assert!(!same_tokens("2.0", "2"));
    }
}
