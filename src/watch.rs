use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::Stylize;
use crossterm::{cursor, execute, queue, terminal};
use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};

use crate::codeforces::{self, Client, Page, Problem, ProblemId, Submission};
use crate::config::{Config, Dirs, Lang, Paths, Quote};
use crate::run::{self, Outcome, Report};
use crate::statement::{self, Kind};

const TICK: Duration = Duration::from_millis(100);
const POLL_EVERY: Duration = Duration::from_secs(5);
const POLL_FOR_SECONDS: i64 = 15 * 60;
// Allows for clock skew between this machine and Codeforces.
const SLACK_SECONDS: i64 = 30;
const OUTPUT_LINES: usize = 20;
const NAME_WIDTH: usize = 40;
const COLUMN_MAX: usize = 40;

enum Status {
    Solved,
    Tried,
    Todo,
}

enum View {
    Problem,
    List { cursor: usize },
}

enum Verdict {
    Idle,
    Waiting,
    Offline,
    Testing { passed: u32 },
    Accepted,
    Rejected { verdict: String, test: u32 },
}

enum Action {
    Continue,
    Quit,
}

struct Pending {
    since: i64,
    polled: Option<Instant>,
}

struct Screen;

impl Screen {
    fn enter() -> Result<Screen> {
        terminal::enable_raw_mode().context("cf needs a terminal")?;
        execute!(
            std::io::stdout(),
            terminal::EnterAlternateScreen,
            cursor::Hide
        )?;
        Ok(Screen)
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        let _ = execute!(
            std::io::stdout(),
            terminal::LeaveAlternateScreen,
            cursor::Show
        );
        let _ = terminal::disable_raw_mode();
    }
}

pub struct Watch<'a> {
    client: &'a mut Client,
    config: Config,
    lang: Lang,
    ids: Vec<ProblemId>,
    since: i64,
    catalog: HashMap<ProblemId, Problem>,
    submissions: Vec<Submission>,
    current: usize,
    view: View,
    report: Report,
    note: String,
    verdict: Verdict,
    pending: Option<Pending>,
    stale: bool,
    events: Receiver<DebounceEventResult>,
    debouncer: Debouncer<RecommendedWatcher>,
    watched: Option<PathBuf>,
    clipboard: Option<arboard::Clipboard>,
    root: PathBuf,
    templates: PathBuf,
    builds: PathBuf,
    strays: Vec<String>,
}

impl<'a> Watch<'a> {
    pub fn new(
        client: &'a mut Client,
        config: Config,
        dirs: &Dirs,
        ids: Vec<ProblemId>,
        since: i64,
        problems: Vec<Problem>,
        submissions: Vec<Submission>,
    ) -> Result<Watch<'a>> {
        let mut catalog = HashMap::new();
        for problem in problems {
            let Some(id) = problem.id() else {
                continue;
            };
            catalog.insert(id, problem);
        }
        let (sender, events) = mpsc::channel();
        let debouncer = new_debouncer(TICK, move |result| {
            let _ = sender.send(result);
        })
        .context("starting the file watcher")?;
        let lang = config.lang().clone();
        let root = std::env::current_dir()?;
        let mut watch = Watch {
            client,
            config,
            lang,
            ids,
            since,
            catalog,
            submissions,
            current: 0,
            view: View::Problem,
            report: Report::Building,
            note: String::new(),
            verdict: Verdict::Idle,
            pending: None,
            stale: false,
            events,
            debouncer,
            watched: None,
            clipboard: None,
            templates: dirs.templates(),
            builds: dirs.builds(&root),
            root,
            strays: Vec::new(),
        };
        watch.current = watch.next(0);
        Ok(watch)
    }

    pub fn run(&mut self) -> Result<()> {
        let _screen = Screen::enter()?;
        self.open();
        let mut dirty = true;
        loop {
            if dirty {
                self.render()?;
            }
            dirty = false;

            if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) => {
                        if let KeyEventKind::Press = key.kind {
                            match self.press(key) {
                                Action::Quit => return Ok(()),
                                Action::Continue => dirty = true,
                            }
                        }
                    }
                    Event::Resize(..) => dirty = true,
                    Event::FocusGained | Event::FocusLost | Event::Mouse(_) | Event::Paste(_) => {}
                }
            }

            let events: Vec<DebounceEventResult> = self.events.try_iter().collect();
            for result in events {
                match result {
                    Ok(events) => {
                        for event in events {
                            if self.relevant(&event.path) {
                                self.stale = true;
                            }
                        }
                    }
                    Err(error) => {
                        self.note = format!("file watch failed: {error}, press r to rerun");
                        dirty = true;
                    }
                }
            }

            if self.stale {
                self.stale = false;
                self.report = Report::Building;
                self.render()?;
                let before = self.entries();
                self.report = match run::check(&self.paths(), &self.lang) {
                    Ok(report) => report,
                    Err(error) => Report::Failed(format!("{error:#}")),
                };
                self.strays = self.strays(&before);
                dirty = true;
            }

            if self.poll() {
                dirty = true;
            }
        }
    }

    fn press(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        match self.view {
            View::List { cursor } => match key.code {
                KeyCode::Char('j') | KeyCode::Down => {
                    self.view = View::List {
                        cursor: (cursor + 1).min(self.ids.len() - 1),
                    };
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.view = View::List {
                        cursor: cursor.saturating_sub(1),
                    };
                }
                KeyCode::Char('g') => self.view = View::List { cursor: 0 },
                KeyCode::Char('G') => {
                    self.view = View::List {
                        cursor: self.ids.len() - 1,
                    }
                }
                KeyCode::Enter | KeyCode::Char('c') => {
                    self.current = cursor;
                    self.view = View::Problem;
                    self.open();
                }
                KeyCode::Char('l') | KeyCode::Char('q') | KeyCode::Esc => self.view = View::Problem,
                _ => {}
            },
            View::Problem => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => return Action::Quit,
                KeyCode::Char('y') => self.copy(),
                KeyCode::Char('o') => self.submit(),
                KeyCode::Char('l') => {
                    self.view = View::List {
                        cursor: self.current,
                    }
                }
                KeyCode::Char('n') => {
                    self.current = self.next(self.current + 1);
                    self.open();
                }
                KeyCode::Char('r') => self.open(),
                _ => {}
            },
        }
        Action::Continue
    }

    fn id(&self) -> &ProblemId {
        &self.ids[self.current]
    }

    fn dir(&self) -> PathBuf {
        PathBuf::from("problems").join(self.id().to_string())
    }

    fn paths(&self) -> Paths {
        let dir = self.root.join(self.dir());
        Paths {
            source: dir.join(self.source()),
            build: self.builds.join(self.id().to_string()),
            dir,
        }
    }

    // Relative to the solutions folder, for paths shown on screen.
    fn shown(&self) -> Paths {
        let dir = self.dir();
        Paths {
            source: dir.join(self.source()),
            build: self.builds.join(self.id().to_string()),
            dir,
        }
    }

    fn entries(&self) -> Vec<String> {
        let mut names = Vec::new();
        let Ok(entries) = std::fs::read_dir(self.dir()) else {
            return names;
        };
        for entry in entries.flatten() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names
    }

    // Anything new next to the source after a build was written outside the build folder.
    fn strays(&self, before: &[String]) -> Vec<String> {
        let mut strays = Vec::new();
        for name in self.entries() {
            let sample =
                name.ends_with(".txt") && (name.starts_with("in") || name.starts_with("ans"));
            let known = sample || name == self.source();
            let new = !before.contains(&name) || self.strays.contains(&name);
            if !known && new {
                strays.push(name);
            }
        }
        strays
    }

    fn source(&self) -> String {
        format!("main.{}", self.lang.ext)
    }

    fn status(&self, id: &ProblemId) -> Status {
        let mut tried = false;
        for submission in &self.submissions {
            if submission.creation_time_seconds < self.since
                || submission.problem.id().as_ref() != Some(id)
            {
                continue;
            }
            if submission.verdict.as_deref() == Some("OK") {
                return Status::Solved;
            }
            tried = true;
        }
        if tried { Status::Tried } else { Status::Todo }
    }

    fn next(&self, from: usize) -> usize {
        for offset in 0..self.ids.len() {
            let at = (from + offset) % self.ids.len();
            match self.status(&self.ids[at]) {
                Status::Solved => {}
                Status::Tried | Status::Todo => return at,
            }
        }
        from % self.ids.len()
    }

    fn solved(&self) -> usize {
        let mut count = 0;
        for id in &self.ids {
            match self.status(id) {
                Status::Solved => count += 1,
                Status::Tried | Status::Todo => {}
            }
        }
        count
    }

    fn relevant(&self, path: &Path) -> bool {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return false;
        };
        let sample = name.ends_with(".txt") && (name.starts_with("in") || name.starts_with("ans"));
        sample || name == self.source()
    }

    fn open(&mut self) {
        self.note.clear();
        self.strays.clear();
        self.verdict = Verdict::Idle;
        self.pending = None;
        match self.prepare() {
            Ok(()) => self.stale = true,
            Err(error) => self.report = Report::Failed(format!("{error:#}, press r to retry")),
        }
    }

    fn prepare(&mut self) -> Result<()> {
        let dir = self.dir();
        let file = dir.join(self.source());
        if !file.exists() {
            let url = self.id().url(Page::Problem);
            let statement = statement::parse(&self.client.get(&url)?)?;
            let template_path = self.templates.join(self.source());
            let template = std::fs::read_to_string(&template_path)
                .with_context(|| format!("reading {}", template_path.display()))?;
            std::fs::create_dir_all(&dir)?;
            match statement.kind {
                Kind::Standard => {
                    for (k, sample) in statement.samples.iter().enumerate() {
                        std::fs::write(dir.join(format!("in{}.txt", k + 1)), &sample.input)?;
                        std::fs::write(dir.join(format!("ans{}.txt", k + 1)), &sample.answer)?;
                    }
                }
                Kind::Interactive => {}
            }
            let header = statement::comment(&self.lang.comment, &statement, &url);
            std::fs::write(&file, header + &template)?;
        }

        if let Some(old) = self.watched.take() {
            let _ = self.debouncer.watcher().unwatch(&old);
        }
        // Editors save by renaming a temp file over the source, so watch the folder and not the file.
        self.debouncer
            .watcher()
            .watch(&dir, RecursiveMode::NonRecursive)?;
        self.watched = Some(dir);
        Ok(())
    }

    fn copy(&mut self) {
        match self.report {
            Report::Ran(_) | Report::NoSamples => {}
            Report::Building | Report::Failed(_) | Report::BuildFailed(_) => {
                self.note = "the last build failed, fix it before copying".to_string();
                return;
            }
        }
        let path = PathBuf::from(self.paths().expand(&self.lang.submit, Quote::Plain));
        let Ok(source) = std::fs::read_to_string(&path) else {
            self.note = format!(
                "nothing to copy yet, {} comes from a successful build",
                self.lang.submit
            );
            return;
        };
        if self.clipboard.is_none() {
            match arboard::Clipboard::new() {
                Ok(clipboard) => self.clipboard = Some(clipboard),
                Err(error) => {
                    self.note = format!("clipboard unavailable: {error}");
                    return;
                }
            }
        }
        let Some(clipboard) = &mut self.clipboard else {
            return;
        };
        match clipboard.set_text(source) {
            Ok(()) => {
                self.note = format!("copied {}", self.lang.submit);
                self.expect();
            }
            Err(error) => self.note = format!("copy failed: {error}"),
        }
    }

    fn submit(&mut self) {
        match open::that(self.id().url(Page::Submit)) {
            Ok(()) => self.expect(),
            Err(error) => self.note = format!("could not open the browser: {error}"),
        }
    }

    fn expect(&mut self) {
        if self.pending.is_none() {
            self.pending = Some(Pending {
                since: codeforces::now(),
                polled: None,
            });
            self.verdict = Verdict::Waiting;
        }
    }

    fn poll(&mut self) -> bool {
        let Some(Pending { since, polled }) = &mut self.pending else {
            return false;
        };
        if polled.is_some_and(|at| at.elapsed() < POLL_EVERY) {
            return false;
        }
        *polled = Some(Instant::now());
        let since = *since;
        if codeforces::now() - since > POLL_FOR_SECONDS {
            self.pending = None;
            self.verdict = Verdict::Idle;
            return true;
        }

        let Ok(latest) = self.client.latest(&self.config.handle) else {
            self.verdict = Verdict::Offline;
            return true;
        };
        for submission in latest {
            if submission.creation_time_seconds < since - SLACK_SECONDS
                || submission.problem.id().as_ref() != Some(self.id())
            {
                continue;
            }
            let passed = submission.passed_test_count;
            match submission.verdict.as_deref() {
                None | Some("TESTING") => self.verdict = Verdict::Testing { passed },
                Some("OK") => {
                    self.verdict = Verdict::Accepted;
                    self.pending = None;
                    self.submissions.push(submission);
                }
                Some(verdict) => {
                    self.verdict = Verdict::Rejected {
                        verdict: verdict.to_string(),
                        test: passed + 1,
                    };
                    self.pending = None;
                    self.submissions.push(submission);
                }
            }
            return true;
        }
        self.verdict = Verdict::Waiting;
        true
    }

    fn render(&self) -> Result<()> {
        let (columns, rows) = terminal::size()?;
        let width = usize::from(columns);
        let (body, footer) = match self.view {
            View::List { cursor } => self.list(cursor, usize::from(rows), width),
            View::Problem => self.problem(width),
        };
        let footer: Vec<&str> = footer.lines().collect();
        let room = usize::from(rows).saturating_sub(footer.len());
        let mut stdout = std::io::stdout();
        queue!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(terminal::ClearType::All)
        )?;
        let mut wrapped = Vec::new();
        for line in body.lines() {
            wrapped.extend(wrap(line, width));
        }
        for (at, line) in wrapped.iter().take(room).enumerate() {
            queue!(stdout, cursor::MoveTo(0, at as u16))?;
            stdout.write_all(line.as_bytes())?;
        }
        for (at, line) in footer.iter().enumerate() {
            queue!(stdout, cursor::MoveTo(0, (room + at) as u16))?;
            stdout.write_all(line.as_bytes())?;
        }
        stdout.flush()?;
        Ok(())
    }

    fn list(&self, cursor: usize, rows: usize, width: usize) -> (String, String) {
        let visible = rows.saturating_sub(3).max(1);
        let top = cursor
            .saturating_sub(visible / 2)
            .min(self.ids.len().saturating_sub(visible));
        let mut body = format!(
            "  Current  State    {:<8} {:<NAME_WIDTH$} Rating\n",
            "Id", "Name"
        );
        for at in top..(top + visible).min(self.ids.len()) {
            let id = &self.ids[at];
            let (name, rating) = match self.catalog.get(id) {
                Some(problem) => (
                    problem.name.as_str(),
                    problem.rating.map(|r| r.to_string()).unwrap_or_default(),
                ),
                None => ("", String::new()),
            };
            let name: String = name.chars().take(NAME_WIDTH).collect();
            let current = if at == self.current {
                ">>>>>>>  ".red().to_string()
            } else {
                " ".repeat(9)
            };
            let state = match self.status(id) {
                Status::Solved => "DONE   ".green(),
                Status::Tried => "TRIED  ".red(),
                Status::Todo => "PENDING".yellow(),
            };
            let row = format!(
                "{current}{state}  {:<8} {name:<NAME_WIDTH$} {rating}",
                id.to_string()
            );
            if at == cursor {
                let _ = writeln!(body, "> {}", row.reverse().bold());
            } else {
                let _ = writeln!(body, "  {row}");
            }
        }
        let footer = format!(
            "{}\n{}",
            progress(self.solved(), self.ids.len(), width),
            hotkeys(&[
                ("j/k", "down/up"),
                ("g/G", "first/last"),
                ("c", "continue at"),
                ("q", "quit list"),
            ])
        );
        (body, footer)
    }

    fn problem(&self, width: usize) -> (String, String) {
        let mut body = String::new();
        match &self.report {
            Report::Building => body.push_str("Building…\n"),
            Report::Failed(message) => {
                let _ = writeln!(body, "{}", message.as_str().red());
            }
            Report::BuildFailed(log) => {
                let _ = writeln!(body, "{}\n\n{}", "Build failed".red().bold(), clip(log));
            }
            Report::NoSamples => body.push_str(
                "No samples to run, interactive problems can only be judged on Codeforces\n",
            ),
            Report::Ran(outcomes) => {
                for (k, outcome) in outcomes {
                    match outcome {
                        Outcome::Pass { elapsed } => {
                            let _ = writeln!(
                                body,
                                "{} {}",
                                format!("Sample {k} ✓").green(),
                                format!("{} ms", elapsed.as_millis()).dim()
                            );
                        }
                        Outcome::Wrong {
                            elapsed,
                            expected,
                            got,
                        } => {
                            let _ = writeln!(
                                body,
                                "{} {}",
                                format!("Sample {k} ✗ wrong answer").red(),
                                format!("{} ms", elapsed.as_millis()).dim()
                            );
                            body.push_str(&diff(expected, got));
                        }
                        Outcome::Crash { elapsed, stderr } => {
                            let _ = writeln!(
                                body,
                                "{} {}\n{}",
                                format!("Sample {k} ✗ runtime error").red(),
                                format!("{} ms", elapsed.as_millis()).dim(),
                                clip(stderr)
                            );
                        }
                        Outcome::Timeout => {
                            let _ = writeln!(body, "{}", format!("Sample {k} ✗ over 5 s").red());
                        }
                    }
                }
            }
        }

        if !self.strays.is_empty() {
            let _ = writeln!(
                body,
                "\n{}",
                format!(
                    "Build wrote {} next to the source instead of {{build}}",
                    self.strays.join(", ")
                )
                .yellow()
            );
        }
        if self.report.passed() {
            let _ = writeln!(body, "\n{}", "Samples pass ✓".green().bold());
            let _ = writeln!(
                body,
                "Submit `{}` as {}, the verdict shows up here",
                self.shown().expand(&self.lang.submit, Quote::Plain),
                self.lang.judge
            );
        }
        let verdict = match &self.verdict {
            Verdict::Idle => String::new(),
            Verdict::Waiting => "Waiting for your submission…".to_string(),
            Verdict::Offline => "Can't reach Codeforces, retrying…".to_string(),
            Verdict::Testing { passed } => format!("Testing… {passed} passed"),
            Verdict::Accepted => format!(
                "{}\nEnter `n` to move on to the next problem",
                "Accepted ✓".green().bold()
            ),
            Verdict::Rejected { verdict, test } => {
                format!("{}", format!("{verdict} on test {test}").red().bold())
            }
        };
        for line in [self.note.as_str(), verdict.as_str()] {
            if !line.is_empty() {
                let _ = writeln!(body, "\n{line}");
            }
        }

        let id = self.id();
        let url = id.url(Page::Problem);
        let mut title = id.to_string();
        if let Some(problem) = self.catalog.get(id) {
            let _ = write!(title, " {}", problem.name);
            if let Some(rating) = problem.rating {
                let _ = write!(title, " ({rating})");
            }
        }
        let editor = self.shown().expand(&self.config.editor, Quote::Shell);
        let next = match self.verdict {
            Verdict::Accepted => "next".underlined().to_string(),
            Verdict::Idle
            | Verdict::Waiting
            | Verdict::Offline
            | Verdict::Testing { .. }
            | Verdict::Rejected { .. } => "next".to_string(),
        };
        // OSC 8 makes the link clickable in terminals that support it.
        let footer = format!(
            "\n{}\nCurrent problem: \x1b]8;;{url}\x1b\\{title}\x1b]8;;\x1b\\\n\n    {editor}\n\n{}",
            progress(self.solved(), self.ids.len(), width),
            prompt(&[
                ("y", "copy"),
                ("o", "submit"),
                ("n", &next),
                ("r", "rerun"),
                ("l", "list"),
                ("q", "quit"),
            ])
        );
        // Like rustlings, the watch view flows top to bottom and only the list pins its footer.
        body.push_str(&footer);
        (body, String::new())
    }
}

fn progress(done: usize, total: usize, width: usize) -> String {
    const PREFIX: &str = "Progress: [";
    let suffix = format!("] {done:>3}/{total}");
    let Some(bar) = width
        .checked_sub(PREFIX.len() + suffix.len())
        .filter(|bar| *bar >= 10)
    else {
        return format!("Progress: {done}/{total}");
    };
    let filled = bar * done / total.max(1);
    let mut out = format!("{PREFIX}{}", "#".repeat(filled).green());
    if filled < bar {
        out.push_str(&format!(
            "{}{}",
            ">".green(),
            "-".repeat(bar - filled - 1).red()
        ));
    }
    out.push_str(&suffix);
    out
}

fn prompt(keys: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (key, label) in keys {
        let _ = write!(out, "{}:{label} / ", key.bold());
    }
    out.truncate(out.len() - " / ".len());
    out.push_str(" ? ");
    out
}

fn hotkeys(keys: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (key, label) in keys {
        let _ = write!(out, "{}:{label} / ", key.yellow().bold());
    }
    out.truncate(out.len() - " / ".len());
    out
}

fn clip(text: &str) -> String {
    let mut out = String::new();
    for (at, line) in text.lines().enumerate() {
        if at == OUTPUT_LINES {
            out.push_str("…\n");
            break;
        }
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn diff(expected: &str, got: &str) -> String {
    let expected: Vec<&str> = expected.lines().collect();
    let got: Vec<&str> = got.lines().collect();
    let mut width = "expected".len();
    for line in &expected {
        width = width.max(line.chars().count());
    }
    let width = width.min(COLUMN_MAX) + 4;
    let mut out = format!(
        "{}{}\n",
        format!("{:<width$}", "expected").dim(),
        "got".dim()
    );
    let rows = expected.len().max(got.len());
    for at in 0..rows.min(OUTPUT_LINES) {
        let left = expected.get(at).copied().unwrap_or("");
        let right = got.get(at).copied().unwrap_or("");
        let left: String = left.chars().take(COLUMN_MAX).collect();
        let right = if expected.get(at) == got.get(at) {
            right.to_string()
        } else {
            right.red().to_string()
        };
        let _ = writeln!(out, "{left:<width$}{right}");
    }
    if rows > OUTPUT_LINES {
        out.push_str("…\n");
    }
    out
}

// Splits a line at the terminal width so a long one doesn't spill into the row drawn below it.
// Escape sequences take no columns, so they are copied through without counting.
fn wrap(line: &str, width: usize) -> Vec<String> {
    let mut rows = vec![String::new()];
    let mut column = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        let Some(row) = rows.last_mut() else {
            break;
        };
        if c == '\x1b' {
            row.push(c);
            for c in chars.by_ref() {
                row.push(c);
                if c.is_ascii_alphabetic() || c == '\\' {
                    break;
                }
            }
            continue;
        }
        if column == width.max(1) {
            rows.push(String::new());
            column = 0;
        }
        let Some(row) = rows.last_mut() else {
            break;
        };
        row.push(c);
        column += 1;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_at_width_without_counting_colors() {
        assert_eq!(wrap("abcdefg", 3), ["abc", "def", "g"]);
        let red = format!("{}", "abcd".red());
        assert_eq!(wrap(&red, 4).len(), 1);
    }
}
