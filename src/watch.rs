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
use crate::config::{Config, Lang};
use crate::run::{self, Outcome, Report};
use crate::statement::{self, Kind};

const TICK: Duration = Duration::from_millis(100);
const POLL_EVERY: Duration = Duration::from_secs(5);
const POLL_FOR_SECONDS: i64 = 15 * 60;
// Allows for clock skew between this machine and Codeforces.
const SLACK_SECONDS: i64 = 30;
const LIST_ROWS: usize = 20;
const OUTPUT_LINES: usize = 20;

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
    path: PathBuf,
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
}

impl<'a> Watch<'a> {
    pub fn new(
        client: &'a mut Client,
        config: Config,
        path: PathBuf,
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
        let mut watch = Watch {
            client,
            config,
            lang,
            path,
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
                self.report = match run::check(&self.dir(), &self.lang) {
                    Ok(report) => report,
                    Err(error) => Report::Failed(format!("{error:#}")),
                };
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
                KeyCode::Enter => {
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
            let template_path = PathBuf::from("templates").join(self.source());
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
        let path = self.dir().join(&self.lang.submit);
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
        let screen = match self.view {
            View::List { cursor } => self.list(cursor),
            View::Problem => self.problem(),
        };
        let mut stdout = std::io::stdout();
        queue!(
            stdout,
            cursor::MoveTo(0, 0),
            terminal::Clear(terminal::ClearType::All)
        )?;
        // Raw mode turns off the newline translation, so lines need an explicit carriage return.
        stdout.write_all(screen.replace('\n', "\r\n").as_bytes())?;
        stdout.flush()?;
        Ok(())
    }

    fn list(&self, cursor: usize) -> String {
        let mut out = format!(
            "{}  {}/{}\n\n",
            self.path.display().to_string().bold(),
            self.solved(),
            self.ids.len()
        );
        let top = cursor.saturating_sub(LIST_ROWS / 2);
        let bottom = (top + LIST_ROWS).min(self.ids.len());
        for at in top..bottom {
            let id = &self.ids[at];
            let mark = match self.status(id) {
                Status::Solved => "✓".green(),
                Status::Tried => "✗".red(),
                Status::Todo => "·".dim(),
            };
            let pointer = if at == cursor { ">" } else { " " };
            let (name, rating) = match self.catalog.get(id) {
                Some(problem) => (
                    problem.name.as_str(),
                    problem.rating.map(|r| r.to_string()).unwrap_or_default(),
                ),
                None => ("", String::new()),
            };
            let _ = writeln!(
                out,
                "{pointer} {mark} {:<8} {name:<48} {}",
                id.to_string(),
                rating.dim()
            );
        }
        let _ = write!(out, "\n{}\n", "j/k move · enter open · l back".dim());
        out
    }

    fn problem(&self) -> String {
        let id = self.id();
        let mut out = format!("{}", id.to_string().bold());
        if let Some(problem) = self.catalog.get(id) {
            let _ = write!(out, " · {}", problem.name);
            if let Some(rating) = problem.rating {
                let _ = write!(out, " · {rating}");
            }
            if !problem.tags.is_empty() {
                let _ = write!(out, " {}", format!("· {}", problem.tags.join(", ")).dim());
            }
        }
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "{}",
            format!(
                "{}/{} in {}",
                self.solved(),
                self.ids.len(),
                self.path.display()
            )
            .dim()
        );
        let url = id.url(Page::Problem);
        // OSC 8 makes the link clickable in terminals that support it.
        let _ = writeln!(out, "\x1b]8;;{url}\x1b\\{url}\x1b]8;;\x1b\\");
        let dir = self.dir().display().to_string();
        let file = format!("{dir}/{}", self.source());
        let _ = writeln!(
            out,
            "{}\n",
            self.config
                .editor
                .replace("{file}", &file)
                .replace("{dir}", &dir)
        );

        match &self.report {
            Report::Building => out.push_str("building…\n"),
            Report::Failed(message) => {
                let _ = writeln!(out, "{}", message.as_str().red());
            }
            Report::BuildFailed(log) => {
                let _ = writeln!(out, "{}\n{}", "build failed".red(), clip(log));
            }
            Report::NoSamples => out.push_str(
                "no samples to run, interactive problems can only be judged on Codeforces\n",
            ),
            Report::Ran(outcomes) => {
                for (k, outcome) in outcomes {
                    match outcome {
                        Outcome::Pass { elapsed } => {
                            let _ = writeln!(
                                out,
                                "{}  {} ms",
                                format!("✓ {k}").green(),
                                elapsed.as_millis()
                            );
                        }
                        Outcome::Wrong {
                            elapsed,
                            expected,
                            got,
                        } => {
                            let _ = writeln!(
                                out,
                                "{}  {} ms",
                                format!("✗ {k}  wrong answer").red(),
                                elapsed.as_millis()
                            );
                            let _ = writeln!(
                                out,
                                "{}\n{}{}\n{}",
                                "expected".dim(),
                                clip(expected),
                                "got".dim(),
                                clip(got)
                            );
                        }
                        Outcome::Crash { elapsed, stderr } => {
                            let _ = writeln!(
                                out,
                                "{}  {} ms\n{}",
                                format!("✗ {k}  runtime error").red(),
                                elapsed.as_millis(),
                                clip(stderr)
                            );
                        }
                        Outcome::Timeout => {
                            let _ = writeln!(out, "{}", format!("✗ {k}  over 5 s").red());
                        }
                    }
                }
            }
        }
        if self.report.passed() {
            let _ = writeln!(
                out,
                "\n{} · submit {} as {}",
                "samples ok".green(),
                self.lang.submit,
                self.lang.judge
            );
        }

        let verdict = match &self.verdict {
            Verdict::Idle => String::new(),
            Verdict::Waiting => "waiting for your submission…".to_string(),
            Verdict::Offline => "can't reach Codeforces, retrying…".to_string(),
            Verdict::Testing { passed } => format!("testing… {passed} passed"),
            Verdict::Accepted => format!("{}  n next", "Accepted".green()),
            Verdict::Rejected { verdict, test } => {
                format!("{}", format!("{verdict} on test {test}").red())
            }
        };
        for line in [self.note.as_str(), verdict.as_str()] {
            if !line.is_empty() {
                let _ = writeln!(out, "{line}");
            }
        }
        let _ = write!(
            out,
            "\n{}\n",
            "y copy · o submit page · l list · n next · r rerun · q quit".dim()
        );
        out
    }
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
