use std::fs::File;
use std::io::Read;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config::{Lang, Paths, Quote};
use crate::statement::Rules;

const BUILD_TIMEOUT: Duration = Duration::from_secs(120);
const RUN_TIMEOUT: Duration = Duration::from_secs(5);
// A runaway print loop stops being read here and then hits the timeout.
const OUTPUT_LIMIT: u64 = 16 << 20;
const REAL_ERROR: f64 = 1e-6;

// What a sample run cost, like the judge reports it: time and peak memory.
#[derive(Serialize, Deserialize, Clone, Copy)]
pub struct Usage {
    pub elapsed: Duration,
    pub memory: u64,
}

#[derive(Serialize, Deserialize)]
pub enum Outcome {
    Pass {
        usage: Usage,
    },
    Wrong {
        usage: Usage,
        expected: String,
        got: String,
    },
    Crash {
        usage: Usage,
        stderr: String,
    },
    Timeout,
    // Right answer, but slower than the problem's time limit.
    Slow {
        usage: Usage,
        limit: Duration,
    },
}

impl Outcome {
    pub fn usage(&self) -> Option<Usage> {
        match self {
            Outcome::Pass { usage }
            | Outcome::Wrong { usage, .. }
            | Outcome::Crash { usage, .. }
            | Outcome::Slow { usage, .. } => Some(*usage),
            Outcome::Timeout => None,
        }
    }
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
                        Outcome::Wrong { .. }
                        | Outcome::Crash { .. }
                        | Outcome::Timeout
                        | Outcome::Slow { .. } => {
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
    usage: Usage,
    stdout: String,
    stderr: String,
}

// Setting `cancel` kills whatever is running and makes the rest return at once.
pub fn check(paths: &Paths, lang: &Lang, rules: &Rules, cancel: &AtomicBool) -> Result<Report> {
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
        let stdin = match &rules.input {
            Some(name) => {
                std::fs::copy(&input, cwd.join(name))?;
                Stdio::null()
            }
            None => Stdio::from(File::open(&input)?),
        };
        if let Some(name) = &rules.output {
            let _ = std::fs::remove_file(cwd.join(name));
        }
        let outcome = match sh(cwd, &run, stdin, RUN_TIMEOUT, cancel)? {
            None => Outcome::Timeout,
            Some(Finished {
                success: false,
                usage,
                stderr,
                ..
            }) => Outcome::Crash { usage, stderr },
            Some(Finished {
                success: true,
                usage,
                stdout,
                ..
            }) => {
                let got = match &rules.output {
                    Some(name) => std::fs::read_to_string(cwd.join(name)).unwrap_or_default(),
                    None => stdout,
                };
                let too_slow = rules.time.filter(|limit| usage.elapsed > *limit);
                if !same_tokens(&got, &expected) {
                    Outcome::Wrong {
                        usage,
                        expected,
                        got,
                    }
                } else if let Some(limit) = too_slow {
                    Outcome::Slow { usage, limit }
                } else {
                    Outcome::Pass { usage }
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
    let Some((status, memory)) = wait(&child, timeout, cancel)? else {
        // sh -c forks the real program, so kill the whole group or it keeps running.
        Command::new("kill")
            .args(["-KILL", "--", &format!("-{}", child.id())])
            .status()?;
        child.wait()?;
        return Ok(None);
    };
    let usage = Usage {
        elapsed: start.elapsed(),
        memory,
    };
    Ok(Some(Finished {
        success: status.success(),
        usage,
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

// wait4 instead of Child::wait because it also reports the peak memory, which counts the
// program sh started since sh waits for it.
fn wait(
    child: &Child,
    timeout: Duration,
    cancel: &AtomicBool,
) -> Result<Option<(ExitStatus, u64)>> {
    let pid = child.id() as libc::pid_t;
    let start = Instant::now();
    while start.elapsed() < timeout && !cancel.load(Ordering::Relaxed) {
        let mut status = 0;
        // SAFETY: rusage is plain old data, all zeros is a valid value.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: both pointers are to locals that outlive the call, and pid is our own child.
        let reaped = unsafe { libc::wait4(pid, &mut status, libc::WNOHANG, &mut usage) };
        if reaped < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        if reaped == pid {
            // macOS reports bytes, Linux kilobytes.
            let unit = if cfg!(target_os = "macos") { 1 } else { 1024 };
            let memory = u64::try_from(usage.ru_maxrss).unwrap_or(0) * unit;
            return Ok(Some((ExitStatus::from_raw(status), memory)));
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
