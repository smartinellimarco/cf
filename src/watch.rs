use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::Stylize;
use crossterm::{cursor, execute, queue, terminal};
use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};

use crate::codeforces::{self, Client, Handle, Page, Problem, ProblemId, Submission};
use crate::config::{Config, Dirs, Lang, Paths, Quote};
use crate::fetch::{Fetched, Fetcher};
use crate::local;
use crate::run::{Outcome, Report};
use crate::statement::{self, Files, Statement};
use crate::sweep::{self, Done, Job, Runner};

const TICK: Duration = Duration::from_millis(100);
const POLL_EVERY: Duration = Duration::from_secs(5);
const POLL_FOR: Duration = Duration::from_secs(15 * 60);
// Allows for clock skew between this machine and Codeforces.
const SLACK_SECONDS: i64 = 30;
const REPORT_LINES: usize = 12;
// Notes and the verdict keep their own rows, wherever the report ends.
const MESSAGE_LINES: usize = 3;
const NAME_WIDTH: usize = 40;
const COLUMNS_PER_BAND: usize = 50;
const BANDS_SHOWN: usize = 2;
const ATTEMPTS_SHOWN: usize = 5;
// nf-md-circle_outline, nf-md-circle and nf-md-dots_vertical.
const RING: &str = "\u{f0766}";
const DOT: &str = "\u{f0765}";
const MORE: &str = "\u{f01d9}";
// nf-md-circle_small, for a problem with nothing to show yet.
const EMPTY: &str = "\u{f09df}";
const FLASH: Duration = Duration::from_secs(1);
const COLUMN_MAX: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Solved,
    Tried,
    Todo,
}

enum Local {
    Pending,
    Ran { passed: bool, at: i64 },
}

#[derive(Clone, Copy)]
enum Mark {
    Accepted,
    Rejected,
    CompilationError,
    Testing,
}

struct Attempt {
    mark: Mark,
    at: i64,
}

enum Motion {
    Next,
    Prev,
    First,
    Last,
    Find { forward: bool, status: Status },
}

enum View {
    Problem,
    List { cursor: usize },
}

enum Verdict {
    Idle,
    Waiting,
    Retrying { reason: String },
    Testing { passed: u32 },
    Accepted,
    Rejected { verdict: String, test: u32 },
}

enum Action {
    Continue,
    Quit,
}

// A verdict, plus the submission once it is final.
type Update = (Verdict, Option<Submission>);

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

pub struct Setup {
    pub root: PathBuf,
    // Walking a list rather than the problemset.
    pub listed: bool,
    pub ids: Vec<ProblemId>,
    pub start: Option<ProblemId>,
    // Language for new files instead of default_lang, from --lang.
    pub lang: Option<String>,
    pub problems: Vec<Problem>,
    pub submissions: Vec<Submission>,
}

pub struct Watch<'a> {
    client: &'a mut Client,
    listed: bool,
    config: Config,
    ids: Vec<ProblemId>,
    history: HashMap<ProblemId, Vec<Attempt>>,
    prefix: Option<char>,
    catalog: HashMap<ProblemId, Problem>,
    submissions: Vec<Submission>,
    current: usize,
    view: View,
    report: Report,
    note: String,
    copied: Option<Instant>,
    verdict: Verdict,
    updates: Option<Receiver<Update>>,
    stale: bool,
    // r runs the samples again even when nothing changed.
    rerun: bool,
    events: Receiver<DebounceEventResult>,
    debouncer: Debouncer<RecommendedWatcher>,
    watched: Vec<PathBuf>,
    clipboard: Option<arboard::Clipboard>,
    root: PathBuf,
    templates: PathBuf,
    samples: PathBuf,
    builds: PathBuf,
    strays: Vec<String>,
    before: Vec<String>,
    files: Files,
    runner: Runner,
    fetcher: Fetcher,
    // The statement on screen, once downloaded; starting a solution copies it on top.
    statement: Option<Statement>,
    // Local implementations per problem, newest first, and which one is on screen.
    sources: HashMap<ProblemId, Vec<PathBuf>>,
    active: HashMap<ProblemId, usize>,
    lang: Option<String>,
    locals: HashMap<String, Local>,
    results: PathBuf,
}

impl<'a> Watch<'a> {
    pub fn new(
        client: &'a mut Client,
        config: Config,
        dirs: &Dirs,
        setup: Setup,
    ) -> Result<Watch<'a>> {
        let Setup {
            root,
            listed,
            ids,
            start,
            lang,
            problems,
            submissions,
        } = setup;
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
        let results = dirs.results(&root);
        let fetcher = Fetcher::new(client.clone(), dirs.samples());
        let sources = local::find(&root, &config);
        let mut watch = Watch {
            client,
            listed,
            history: history(&submissions),
            config,
            ids,
            prefix: None,
            catalog,
            submissions,
            current: 0,
            view: View::Problem,
            report: Report::Building,
            note: String::new(),
            copied: None,
            verdict: Verdict::Idle,
            updates: None,
            stale: false,
            rerun: false,
            events,
            debouncer,
            watched: Vec::new(),
            clipboard: None,
            templates: dirs.templates(),
            samples: dirs.samples(),
            builds: dirs.builds(&root),
            root,
            strays: Vec::new(),
            before: Vec::new(),
            files: Files::default(),
            runner: Runner::new(results.clone()),
            fetcher,
            statement: None,
            sources,
            active: HashMap::new(),
            lang,
            locals: HashMap::new(),
            results,
        };
        watch.current = match start.and_then(|id| watch.ids.iter().position(|at| *at == id)) {
            Some(at) => at,
            None => watch.next(0),
        };
        Ok(watch)
    }

    pub fn run(&mut self) -> Result<()> {
        let _screen = Screen::enter()?;
        self.open();
        self.sweep();
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

            if self.stale
                && let Some(source) = self.source().cloned()
            {
                self.stale = false;
                let job = self.job(self.id(), &source);
                let hash = sweep::hash(&job.paths, &job.lang, &job.files).ok();
                let stored = sweep::load(&self.results, &job.key)
                    .filter(|stored| !self.rerun && Some(&stored.hash) == hash.as_ref());
                self.rerun = false;
                match stored {
                    Some(stored) => {
                        if let Some(passed) = sweep::passed(&stored.report) {
                            let at = stored.at;
                            self.locals.insert(job.key, Local::Ran { passed, at });
                        }
                        self.report = stored.report;
                    }
                    None => {
                        self.report = Report::Building;
                        self.before = self.entries();
                        self.locals.insert(job.key.clone(), Local::Pending);
                        self.runner.first(job);
                    }
                }
                dirty = true;
            }

            let fetched: Vec<Fetched> = self.fetcher.done.try_iter().collect();
            for Fetched { id, statement } in fetched {
                if id != *self.id() {
                    continue;
                }
                match statement {
                    Ok(statement) => self.ready(statement),
                    Err(error) => {
                        self.report = Report::Failed(format!("{error}, press r to retry"))
                    }
                }
                dirty = true;
            }

            let done: Vec<Done> = self.runner.done.try_iter().collect();
            for done in done {
                self.finish(done);
                dirty = true;
            }

            if self.poll() {
                dirty = true;
            }

            if self.copied.is_some_and(|at| at.elapsed() > FLASH) {
                self.copied = None;
                dirty = true;
            }
        }
    }

    fn press(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        let KeyCode::Char(char) = key.code else {
            self.prefix = None;
            match (key.code, &self.view) {
                (KeyCode::Down, View::List { .. }) => self.go(Motion::Next),
                (KeyCode::Up, View::List { .. }) => self.go(Motion::Prev),
                (KeyCode::Enter, View::List { cursor }) => self.continue_at(*cursor),
                (KeyCode::Esc, View::List { .. }) => self.view = View::Problem,
                (KeyCode::Esc, View::Problem) => return Action::Quit,
                _ => {}
            }
            return Action::Continue;
        };
        // Motions like gg and ]f take two keys, vim style.
        if let Some(prefix) = self.prefix.take() {
            let motion = match (prefix, char) {
                ('g', 'g') => Motion::First,
                ('g', 'e') => Motion::Last,
                (']' | '[', 'f' | 's' | 't') => Motion::Find {
                    forward: prefix == ']',
                    status: match char {
                        'f' => Status::Tried,
                        's' => Status::Solved,
                        _ => Status::Todo,
                    },
                },
                _ => return Action::Continue,
            };
            self.go(motion);
            return Action::Continue;
        }
        match (char, &self.view) {
            ('g' | ']' | '[', _) => self.prefix = Some(char),
            ('n' | 'j', _) => self.go(Motion::Next),
            ('p' | 'k', _) => self.go(Motion::Prev),
            ('c', View::List { cursor }) => self.continue_at(*cursor),
            ('l' | 'q', View::List { .. }) => self.view = View::Problem,
            ('q', View::Problem) => return Action::Quit,
            ('y', View::Problem) => self.copy(),
            ('o', View::Problem) => self.submit(),
            ('l', View::Problem) => {
                self.view = View::List {
                    cursor: self.current,
                }
            }
            ('r', View::Problem) => {
                self.rerun = true;
                self.open();
            }
            ('s', View::Problem) => self.swap(),
            ('e', View::Problem) => self.start(),
            ('w', View::Problem) => self.browse(),
            _ => {}
        }
        Action::Continue
    }

    fn continue_at(&mut self, cursor: usize) {
        self.current = cursor;
        self.view = View::Problem;
        self.open();
    }

    // Moves the list cursor, or the current problem outside the list.
    fn go(&mut self, motion: Motion) {
        let from = match self.view {
            View::List { cursor } => cursor,
            View::Problem => self.current,
        };
        let last = self.ids.len() - 1;
        let to = match motion {
            Motion::Next => Some((from + 1).min(last)),
            Motion::Prev => Some(from.saturating_sub(1)),
            Motion::First => Some(0),
            Motion::Last => Some(last),
            Motion::Find { forward, status } => {
                let found = |at: &usize| self.status(&self.ids[*at]) == status;
                if forward {
                    (from + 1..=last).find(found)
                } else {
                    (0..from).rev().find(found)
                }
            }
        };
        let Some(to) = to else {
            // The list view has no room for notes, and one left behind would show up later.
            if let View::List { .. } = self.view {
                return;
            }
            let name = match motion {
                Motion::Find {
                    status: Status::Solved,
                    ..
                } => "solved",
                Motion::Find {
                    status: Status::Tried,
                    ..
                } => "failed",
                _ => "untouched",
            };
            let side = match motion {
                Motion::Find { forward: true, .. } => "after",
                _ => "before",
            };
            self.note = format!("no {name} problem {side} this one");
            return;
        };
        match self.view {
            View::List { .. } => self.view = View::List { cursor: to },
            View::Problem if to != self.current => {
                self.current = to;
                self.open();
            }
            View::Problem => {}
        }
    }

    fn id(&self) -> &ProblemId {
        &self.ids[self.current]
    }

    fn source(&self) -> Option<&PathBuf> {
        self.source_of(self.id())
    }

    fn source_of(&self, id: &ProblemId) -> Option<&PathBuf> {
        let at = self.active.get(id).copied().unwrap_or(0);
        self.sources.get(id)?.get(at)
    }

    fn lang(&self, source: &Path) -> Lang {
        // Every source was found or created through a lang, so the lookup can't miss.
        self.config.lang_of(source).cloned().unwrap()
    }

    fn paths(&self, id: &ProblemId, source: &Path) -> Paths {
        Paths {
            source: source.to_path_buf(),
            samples: self.samples.join(id.to_string()),
            build: self.builds.join(key(source)),
        }
    }

    fn job(&self, id: &ProblemId, source: &Path) -> Job {
        let files = if id == self.id() {
            self.files.clone()
        } else {
            let statement: Option<Statement> = self.client.load(&format!("statements/{id}.json"));
            statement
                .map(|statement| statement.files)
                .unwrap_or_default()
        };
        Job {
            key: key(source),
            paths: self.paths(id, source),
            files,
            lang: self.lang(source),
        }
    }

    fn swap(&mut self) {
        let count = self.sources.get(self.id()).map_or(0, Vec::len);
        if count < 2 {
            return;
        }
        let at = self.active.entry(self.id().clone()).or_insert(0);
        *at = (*at + 1) % count;
        self.open();
    }

    // Runs the samples of every other local solution, so the grid shows how each stands.
    fn sweep(&mut self) {
        let current = self.source().cloned();
        let mut pending = Vec::new();
        for (id, sources) in &self.sources {
            for source in sources {
                if Some(source) != current.as_ref() {
                    pending.push((id.clone(), source.clone()));
                }
            }
        }
        // Nearest to the current problem first, problems outside the list last.
        pending.sort_by_key(|(id, _)| match self.ids.iter().position(|at| at == id) {
            Some(at) => at.abs_diff(self.current),
            None => usize::MAX,
        });
        let mut jobs = Vec::new();
        for (id, source) in pending {
            let job = self.job(&id, &source);
            let hash = sweep::hash(&job.paths, &job.lang, &job.files).ok();
            match sweep::load(&self.results, &job.key) {
                Some(stored) if Some(&stored.hash) == hash.as_ref() => {
                    if let Some(passed) = sweep::passed(&stored.report) {
                        let at = stored.at;
                        self.locals.insert(job.key, Local::Ran { passed, at });
                    }
                }
                _ => {
                    self.locals.insert(job.key.clone(), Local::Pending);
                    jobs.push(job);
                }
            }
        }
        self.runner.add(jobs);
    }

    fn finish(&mut self, done: Done) {
        match sweep::passed(&done.report) {
            Some(passed) => {
                let at = codeforces::now();
                self.locals
                    .insert(done.key.clone(), Local::Ran { passed, at })
            }
            None => self.locals.remove(&done.key),
        };
        if self.source().is_some_and(|source| key(source) == done.key) {
            self.report = done.report;
            self.strays = self.strays(&self.before);
        }
    }

    // The source relative to where cf runs, for the commands shown on screen.
    // Without a solution yet, the paths are those of the one e would start.
    fn shown(&self) -> Paths {
        let source = match self.source() {
            Some(source) => source.clone(),
            None => {
                let ext = &self.config.langs[&self.new_lang()].ext;
                self.root.join(format!("{}.{ext}", self.id()))
            }
        };
        let mut paths = self.paths(self.id(), &source);
        if let Ok(cwd) = std::env::current_dir()
            && let Ok(relative) = source.strip_prefix(&cwd)
        {
            paths.source = relative.to_path_buf();
        }
        // The cache is deep under the home folder, so ~ keeps the command on one line.
        if let Some(home) = std::env::home_dir() {
            for path in [&mut paths.samples, &mut paths.build] {
                if let Ok(relative) = path.strip_prefix(&home) {
                    *path = PathBuf::from("~").join(relative);
                }
            }
        }
        paths
    }

    fn entries(&self) -> Vec<String> {
        let mut names = Vec::new();
        let Some(dir) = self.source().and_then(|source| source.parent()) else {
            return names;
        };
        let Ok(entries) = std::fs::read_dir(dir) else {
            return names;
        };
        for entry in entries.flatten() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
        names
    }

    // Anything new next to the source after a build was written outside the build folder.
    fn strays(&self, before: &[String]) -> Vec<String> {
        let Some(source) = self.source() else {
            return Vec::new();
        };
        let mut strays = Vec::new();
        for name in self.entries() {
            let new = !before.contains(&name) || self.strays.contains(&name);
            if name != key(source) && new {
                strays.push(name);
            }
        }
        strays
    }

    fn status(&self, id: &ProblemId) -> Status {
        let Some(attempts) = self.history.get(id) else {
            return Status::Todo;
        };
        if attempts
            .iter()
            .any(|attempt| matches!(attempt.mark, Mark::Accepted))
        {
            Status::Solved
        } else {
            Status::Tried
        }
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

    // One column per problem with its attempts stacked upwards, oldest at the bottom, and the
    // count of solved problems under it.
    fn progress(&self, width: usize) -> (Vec<String>, String) {
        let count = if self.listed {
            format!("{}/{}", self.solved(), self.ids.len())
        } else {
            let tried = self
                .ids
                .iter()
                .filter(|id| self.status(id) != Status::Todo)
                .count();
            format!("{}/{tried} tried", self.solved())
        };
        let per_band = (width / 2).clamp(1, COLUMNS_PER_BAND);
        let mut columns = Vec::new();
        for (at, id) in self.ids.iter().enumerate() {
            let several = self
                .sources
                .get(id)
                .is_some_and(|sources| sources.len() > 1);
            columns.push((self.column(id), at == self.current, several));
        }
        // A list shows in fixed bands; the problemset is too long, so one band slides with you.
        let bands: Vec<&[(Vec<String>, bool, bool)]> = if self.listed {
            let count = columns.len().div_ceil(per_band);
            let first = (self.current / per_band)
                .saturating_sub(BANDS_SHOWN / 2)
                .min(count.saturating_sub(BANDS_SHOWN));
            columns
                .chunks(per_band)
                .skip(first)
                .take(BANDS_SHOWN)
                .collect()
        } else {
            let first = self
                .current
                .saturating_sub(per_band / 2)
                .min(columns.len().saturating_sub(per_band));
            vec![&columns[first..(first + per_band).min(columns.len())]]
        };
        let mut lines = Vec::new();
        for chunk in bands {
            // Always the full height, so what comes below never moves.
            for row in (0..=ATTEMPTS_SHOWN).rev() {
                let mut line = String::new();
                for (cells, _, _) in chunk {
                    match cells.get(row) {
                        Some(cell) => line.push_str(cell),
                        None => line.push(' '),
                    }
                    line.push(' ');
                }
                lines.push(line);
            }
            // A cross on the baseline marks a problem with more than one solution to swap between.
            let mut base = String::new();
            for (_, is_current, several) in chunk {
                let mark = match (is_current, several) {
                    (true, true) => "╋".bold(),
                    (true, false) => "━".bold(),
                    (false, true) => "┼".dim(),
                    (false, false) => "─".dim(),
                };
                let _ = write!(base, "{mark} ");
            }
            lines.push(base);
        }
        let checking = self
            .locals
            .values()
            .filter(|local| matches!(local, Local::Pending))
            .count();
        let count = if checking > 0 {
            format!("{count}  {}", format!("· checking {checking}").dark_grey())
        } else {
            count
        };
        (lines, count)
    }

    // Bottom-up cells: the last attempts, then the local result when it is newer than them.
    // Rings are submissions and the filled circle is the local result, all nerd font glyphs.
    fn column(&self, id: &ProblemId) -> Vec<String> {
        let mut cells = Vec::new();
        let attempts = self.history.get(id).map_or(&[][..], Vec::as_slice);
        for attempt in attempts {
            let ring = match attempt.mark {
                Mark::Accepted => RING.green(),
                Mark::Rejected => RING.red(),
                Mark::CompilationError => RING.dark_grey(),
                Mark::Testing => RING.yellow(),
            };
            cells.push(ring.to_string());
        }
        let last = attempts.last().map_or(i64::MIN, |attempt| attempt.at);
        let local = self
            .source_of(id)
            .and_then(|source| self.locals.get(&key(source)));
        match local {
            Some(Local::Pending) => cells.push(DOT.dark_grey().to_string()),
            Some(Local::Ran { passed, at }) if *at > last => {
                let dot = if *passed { DOT.green() } else { DOT.red() };
                cells.push(dot.to_string());
            }
            Some(Local::Ran { .. }) | None => {}
        }
        if cells.is_empty() {
            cells.push(EMPTY.dark_grey().to_string());
        }
        // Five cells show; more get a mark on a row of its own above them.
        if cells.len() > ATTEMPTS_SHOWN {
            cells.drain(..cells.len() - ATTEMPTS_SHOWN);
            cells.push(MORE.dark_grey().to_string());
        }
        cells
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
        let samples = self.samples.join(self.id().to_string());
        let source = self.source().is_some_and(|source| source == path);
        source || path.parent() == Some(samples.as_path())
    }

    fn open(&mut self) {
        self.note.clear();
        self.strays.clear();
        self.verdict = Verdict::Idle;
        self.updates = None;
        for old in self.watched.drain(..) {
            let _ = self.debouncer.watcher().unwatch(&old);
        }
        let id = self.id().clone();
        let samples = self.samples.join(id.to_string());
        let cached: Option<Statement> = self
            .client
            .load(&format!("statements/{id}.json"))
            .filter(|_| samples.is_dir());
        match cached {
            Some(statement) => self.ready(statement),
            None => {
                self.statement = None;
                self.files = Files::default();
                self.report = Report::Downloading;
                self.fetcher.first(id);
            }
        }
    }

    fn ready(&mut self, statement: Statement) {
        self.files = statement.files.clone();
        self.statement = Some(statement);
        if self.source().is_none() {
            self.report = Report::NoSolution;
            return;
        }
        match self.watch() {
            Ok(()) => self.stale = true,
            Err(error) => self.report = Report::Failed(format!("{error:#}, press r to retry")),
        }
    }

    // Editors save by renaming a temp file over the source, so watch its folder and not the file.
    fn watch(&mut self) -> Result<()> {
        let Some(source) = self.source() else {
            return Ok(());
        };
        let folder = source.parent().unwrap_or(&self.root).to_path_buf();
        let samples = self.samples.join(self.id().to_string());
        for dir in [folder, samples] {
            self.debouncer
                .watcher()
                .watch(&dir, RecursiveMode::NonRecursive)?;
            self.watched.push(dir);
        }
        Ok(())
    }

    // Starts a solution in the language for new ones, or switches to it when there is one already.
    fn start(&mut self) {
        let Some(statement) = self.statement.take() else {
            self.note = "the statement is still downloading".to_string();
            return;
        };
        let id = self.id().clone();
        let name = self.new_lang();
        let ext = self.config.langs[&name].ext.clone();
        let existing = self.sources.get(&id).and_then(|sources| {
            sources
                .iter()
                .position(|source| source.extension().and_then(|e| e.to_str()) == Some(&ext))
        });
        let result = match existing {
            Some(at) => {
                self.active.insert(id, at);
                Ok(())
            }
            None => self.create(&id, &name, &statement),
        };
        match result {
            Ok(()) => self.open(),
            Err(error) => {
                self.statement = Some(statement);
                self.note = format!("{error:#}");
            }
        }
    }

    fn new_lang(&self) -> String {
        self.lang
            .clone()
            .unwrap_or_else(|| self.config.default_lang.clone())
    }

    fn create(&mut self, id: &ProblemId, name: &str, statement: &Statement) -> Result<()> {
        let url = id.url(Page::Problem);
        let lang = &self.config.langs[name];
        let template_path = self.templates.join(format!("main.{}", lang.ext));
        let template = std::fs::read_to_string(&template_path)
            .with_context(|| format!("reading {}", template_path.display()))?;
        let file = self.root.join(format!("{id}.{}", lang.ext));
        // A file the glob skipped, say an ignored one, is kept as it is.
        if !file.exists() {
            let header = statement::comment(&lang.comment, statement, &url);
            std::fs::write(&file, header + &template)?;
        }
        let sources = self.sources.entry(id.clone()).or_default();
        sources.insert(0, file);
        self.active.insert(id.clone(), 0);
        Ok(())
    }

    fn copy(&mut self) {
        match self.report {
            Report::Ran(_) | Report::NoSamples => {}
            Report::Downloading | Report::NoSolution => return,
            Report::Building | Report::Failed(_) | Report::BuildFailed(_) => {
                self.note = "the last build failed, fix it before copying".to_string();
                return;
            }
        }
        let Some(file) = self.source().cloned() else {
            return;
        };
        let lang = self.lang(&file);
        let path = PathBuf::from(
            self.paths(self.id(), &file)
                .expand(&lang.submit, Quote::Plain),
        );
        let Ok(source) = std::fs::read_to_string(&path) else {
            self.note = format!(
                "nothing to copy yet, {} comes from a successful build",
                lang.submit
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
                self.note.clear();
                self.copied = Some(Instant::now());
                self.expect();
            }
            Err(error) => self.note = format!("copy failed: {error}"),
        }
    }

    fn browse(&mut self) {
        if let Err(error) = open::that(self.id().url(Page::Problem)) {
            self.note = format!("could not open the browser: {error}");
        }
    }

    fn submit(&mut self) {
        match open::that(self.id().url(Page::Submit)) {
            Ok(()) => self.expect(),
            Err(error) => self.note = format!("could not open the browser: {error}"),
        }
    }

    fn expect(&mut self) {
        if self.updates.is_none() {
            let (sender, updates) = mpsc::channel();
            let client = self.client.clone();
            let handle = self.config.handle.clone();
            let id = self.id().clone();
            let since = codeforces::now();
            std::thread::spawn(move || poll(client, handle, id, since, sender));
            self.updates = Some(updates);
            self.verdict = Verdict::Waiting;
        }
    }

    fn poll(&mut self) -> bool {
        let Some(updates) = &self.updates else {
            return false;
        };
        let mut received = Vec::new();
        let finished = loop {
            match updates.try_recv() {
                Ok(update) => received.push(update),
                Err(TryRecvError::Empty) => break false,
                Err(TryRecvError::Disconnected) => break true,
            }
        };
        if finished {
            self.updates = None;
        }
        let changed = !received.is_empty();
        for (verdict, submission) in received {
            self.verdict = verdict;
            self.submissions.extend(submission);
        }
        self.history = history(&self.submissions);
        changed
    }

    fn render(&self) -> Result<()> {
        let (columns, rows) = terminal::size()?;
        let width = usize::from(columns);
        let (body, footer) = match self.view {
            View::List { cursor } => self.list(cursor, usize::from(rows), width),
            View::Problem => self.problem(width),
        };
        let mut wrapped_footer = Vec::new();
        for line in footer.lines() {
            wrapped_footer.extend(wrap(line, width));
        }
        let footer = wrapped_footer;
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
        let (grid, count) = self.progress(width);
        let mut footer = center_block(&grid, width).join("\n");
        let _ = write!(footer, "\n{}", center(&count, width));
        let _ = write!(
            footer,
            "\n{}",
            hotkeys(&[
                ("n", "down"),
                ("p", "up"),
                ("gg", "first"),
                ("ge", "last"),
                ("]f", "failed"),
                ("]s", "solved"),
                ("]t", "untouched"),
                ("c", "continue at"),
                ("q", "quit list"),
            ])
        );
        let visible = rows.saturating_sub(footer.lines().count() + 1).max(1);
        let top = cursor
            .saturating_sub(visible / 2)
            .min(self.ids.len().saturating_sub(visible));
        let mut body = format!(
            "  Current  State    Local  {:<8} {:<NAME_WIDTH$} Rating\n",
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
            let local = self
                .source_of(id)
                .and_then(|source| self.locals.get(&key(source)));
            let local = match local {
                Some(Local::Ran { passed: true, .. }) => "✓".green(),
                Some(Local::Ran { passed: false, .. }) => "✗".red(),
                Some(Local::Pending) => "·".dim(),
                None => " ".stylize(),
            };
            let row = format!(
                "{current}{state}  {local}      {:<8} {name:<NAME_WIDTH$} {rating}",
                id.to_string()
            );
            if at == cursor {
                let _ = writeln!(body, "> {}", row.reverse().bold());
            } else {
                let _ = writeln!(body, "  {row}");
            }
        }
        (body, footer)
    }

    // Only the first failing sample shows, so the report fits its fixed height. The head lines
    // center one by one, the block (a diff or a log) centers as a whole to keep its columns.
    fn report(&self) -> (Vec<String>, Vec<String>) {
        let head = |line: String| (vec![line], Vec::new());
        let block = |text: &str| text.lines().map(str::to_string).collect::<Vec<_>>();
        match &self.report {
            Report::Downloading => head("Downloading the statement…".to_string()),
            Report::NoSolution => head("No solution yet, e to start one".to_string()),
            Report::Building => head("Building…".to_string()),
            Report::Failed(message) => head(message.as_str().red().to_string()),
            Report::BuildFailed(log) => (vec!["Build failed".red().bold().to_string()], block(log)),
            Report::NoSamples => head(
                "No samples to run, interactive problems can only be judged on Codeforces"
                    .to_string(),
            ),
            Report::Ran(outcomes) => {
                let mut failing = outcomes
                    .iter()
                    .filter(|(_, outcome)| !matches!(outcome, Outcome::Pass { .. }));
                let Some((k, first)) = failing.next() else {
                    let pass = format!("Samples pass ✓ ({})", outcomes.len());
                    return head(pass.green().bold().to_string());
                };
                let rest: Vec<String> = failing.map(|(k, _)| k.to_string()).collect();
                let mut lines = Vec::new();
                let (what, elapsed, details) = match first {
                    Outcome::Pass { .. } => return (lines, Vec::new()),
                    Outcome::Wrong {
                        elapsed,
                        expected,
                        got,
                    } => ("wrong answer", Some(elapsed), diff(expected, got)),
                    Outcome::Crash { elapsed, stderr } => {
                        ("runtime error", Some(elapsed), block(stderr))
                    }
                    Outcome::Timeout => ("over 5 s", None, Vec::new()),
                };
                let mut line = format!("Sample {k} ✗ {what}").red().to_string();
                if let Some(elapsed) = elapsed {
                    let _ = write!(line, "  {}", format!("{} ms", elapsed.as_millis()).dim());
                }
                lines.push(line);
                if !rest.is_empty() {
                    lines.push(
                        format!("also failing: {}", rest.join(", "))
                            .red()
                            .to_string(),
                    );
                }
                (lines, details)
            }
        }
    }

    fn messages(&self) -> Vec<String> {
        let mut messages = Vec::new();
        // Most important first, since only MESSAGE_LINES of them show.
        let verdict = match &self.verdict {
            Verdict::Idle => String::new(),
            Verdict::Waiting => "Waiting for your submission…".to_string(),
            Verdict::Retrying { reason } => format!("{reason}, retrying…"),
            Verdict::Testing { passed } => format!("Testing… {passed} passed"),
            Verdict::Accepted => format!(
                "{} Enter `n` to move on to the next problem",
                "Accepted ✓".green().bold()
            ),
            Verdict::Rejected { verdict, test } => {
                format!("{}", format!("{verdict} on test {test}").red().bold())
            }
        };
        for line in [&self.note, &verdict] {
            if !line.is_empty() {
                messages.push(line.clone());
            }
        }
        if !self.strays.is_empty() {
            let stray = format!(
                "Build wrote {} next to the source instead of {{build}}",
                self.strays.join(", ")
            );
            messages.push(stray.yellow().to_string());
        }
        let Files { input, output } = &self.files;
        if input.is_some() || output.is_some() {
            let input = input.as_deref().unwrap_or("stdin");
            let output = output.as_deref().unwrap_or("stdout");
            let files = format!("This problem reads {input} and writes {output}");
            messages.push(files.yellow().to_string());
        }
        if self.report.passed() {
            let shown = self.shown();
            let lang = self.lang(&shown.source);
            messages.push(format!(
                "Submit `{}`, the verdict shows up here",
                shown.expand(&lang.submit, Quote::Plain)
            ));
        }
        messages
    }

    // Like rustlings the view flows top to bottom, and the report keeps a fixed height so
    // nothing below it moves when it changes. Every line is centered.
    fn problem(&self, width: usize) -> (String, String) {
        let mut messages: Vec<String> = self.messages().iter().map(|m| center(m, width)).collect();
        messages.resize(MESSAGE_LINES, String::new());
        let (head, details) = self.report();
        let mut report: Vec<String> = head.iter().map(|line| center(line, width)).collect();
        if !details.is_empty() {
            report.push(String::new());
            report.extend(center_block(&details, width));
        }
        let mut lines = Vec::new();
        for line in report {
            lines.extend(wrap(&line, width));
        }
        if lines.len() > REPORT_LINES {
            lines.truncate(REPORT_LINES - 1);
            lines.push(center(&"…".dim().to_string(), width));
        }
        lines.resize(REPORT_LINES, String::new());

        let (grid, count) = self.progress(width);
        lines.extend(center_block(&grid, width));
        lines.push(center(&count, width));
        lines.push(String::new());
        lines.extend(messages);
        lines.push(String::new());
        lines.push(center(&legend(), width));

        let id = self.id();
        let url = id.url(Page::Problem);
        let mut title = id.to_string();
        if let Some(problem) = self.catalog.get(id) {
            let _ = write!(title, " {}", problem.name);
            if let Some(rating) = problem.rating {
                let _ = write!(title, "  {rating}");
            }
        }
        // OSC 8 makes the title a link in terminals that support it.
        let mut title = format!("\x1b]8;;{url}\x1b\\{}\x1b]8;;\x1b\\", title.bold());
        if let Some(sources) = self.sources.get(id) {
            let active = self.active.get(id).copied().unwrap_or(0);
            let mut icons = Vec::new();
            for (at, source) in sources.iter().enumerate() {
                let icon = &self.lang(source).icon;
                let icon = if at == active {
                    format!("[{icon}]").bold().to_string()
                } else {
                    format!(" {icon} ").dim().to_string()
                };
                icons.push(icon);
            }
            let _ = write!(title, "   {}", icons.concat());
        }
        lines.push(center(&title, width));
        lines.push(String::new());
        // A background like inline code in markdown, from the terminal's own palette.
        let editor = self.shown().expand(&self.config.editor, Quote::Shell);
        lines.push(center(
            &format!(" {editor} ").on_dark_grey().to_string(),
            width,
        ));
        lines.push(String::new());

        let next = match self.verdict {
            Verdict::Accepted => "next".underlined().to_string(),
            Verdict::Idle
            | Verdict::Waiting
            | Verdict::Retrying { .. }
            | Verdict::Testing { .. }
            | Verdict::Rejected { .. } => "next".to_string(),
        };
        let copy = match self.copied {
            Some(_) => "copied ✓".green().to_string(),
            None => "copy".to_string(),
        };
        let keys = prompt(&[
            ("y", &copy),
            ("o", "submit"),
            ("w", "web"),
            ("e", "start"),
            ("s", "swap"),
            ("n", &next),
            ("p", "prev"),
            ("r", "rerun"),
            ("l", "list"),
            ("q", "quit"),
        ]);
        lines.push(center(&keys, width));
        (lines.join("\n"), String::new())
    }
}

fn prompt(keys: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (key, label) in keys {
        let _ = write!(out, "{} {label}  ", key.bold());
    }
    out.truncate(out.len() - "  ".len());
    out.push_str(" ? ");
    out
}

fn hotkeys(keys: &[(&str, &str)]) -> String {
    let mut out = String::new();
    for (key, label) in keys {
        let _ = write!(out, "{}:{label}  ", key.yellow().bold());
    }
    out.truncate(out.len() - "  ".len());
    out
}

fn diff(expected: &str, got: &str) -> Vec<String> {
    let expected: Vec<&str> = expected.lines().collect();
    let got: Vec<&str> = got.lines().collect();
    let mut width = "expected".len();
    for line in &expected {
        width = width.max(line.chars().count());
    }
    let width = width.min(COLUMN_MAX) + 4;
    let mut out = vec![format!(
        "{}{}",
        format!("{:<width$}", "expected").bold(),
        "got".bold()
    )];
    let rows = expected.len().max(got.len());
    for at in 0..rows {
        let left = expected.get(at).copied().unwrap_or("");
        let right = got.get(at).copied().unwrap_or("");
        let left: String = left.chars().take(COLUMN_MAX).collect();
        let right = if expected.get(at) == got.get(at) {
            right.to_string()
        } else {
            right.red().to_string()
        };
        out.push(format!("{left:<width$}{right}"));
    }
    out
}

fn legend() -> String {
    format!(
        "{} accepted  {} rejected  {} compilation error  {} testing  {} samples here  {} untouched",
        RING.green(),
        RING.red(),
        RING.dark_grey(),
        RING.yellow(),
        DOT.green(),
        EMPTY.dark_grey()
    )
}

fn center(line: &str, width: usize) -> String {
    let pad = width.saturating_sub(visible(line)) / 2;
    format!("{}{line}", " ".repeat(pad))
}

// Lines that must stay aligned with each other move together, by the widest one.
fn center_block(lines: &[String], width: usize) -> Vec<String> {
    let widest = lines.iter().map(|line| visible(line)).max().unwrap_or(0);
    let pad = " ".repeat(width.saturating_sub(widest) / 2);
    lines.iter().map(|line| format!("{pad}{line}")).collect()
}

fn visible(line: &str) -> usize {
    let mut count = 0;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            skip_escape(&mut chars, &mut String::new());
        } else {
            count += 1;
        }
    }
    count
}

// Copies an escape sequence through: CSI ends at a letter, OSC (links) at ESC \\ or BEL.
fn skip_escape(chars: &mut std::str::Chars, into: &mut String) {
    let Some(kind) = chars.next() else {
        return;
    };
    into.push(kind);
    let mut previous = kind;
    for c in chars.by_ref() {
        into.push(c);
        let end = match kind {
            ']' => c == '\x07' || (previous == '\x1b' && c == '\\'),
            _ => c.is_ascii_alphabetic(),
        };
        if end {
            break;
        }
        previous = c;
    }
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
            skip_escape(&mut chars, row);
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

fn history(submissions: &[Submission]) -> HashMap<ProblemId, Vec<Attempt>> {
    let mut history: HashMap<ProblemId, Vec<Attempt>> = HashMap::new();
    for submission in submissions {
        let Some(id) = submission.problem.id() else {
            continue;
        };
        let mark = match submission.verdict.as_deref() {
            Some("OK") => Mark::Accepted,
            Some("COMPILATION_ERROR") => Mark::CompilationError,
            None | Some("TESTING") => Mark::Testing,
            Some(_) => Mark::Rejected,
        };
        history.entry(id).or_default().push(Attempt {
            mark,
            at: submission.creation_time_seconds,
        });
    }
    for attempts in history.values_mut() {
        attempts.sort_by_key(|attempt| attempt.at);
    }
    history
}

// The file name, like 4A.cpp, names a solution's build folder and stored result.
fn key(source: &Path) -> String {
    source
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

// Runs on its own thread so a slow or missing connection never blocks the screen.
fn poll(mut client: Client, handle: Handle, id: ProblemId, since: i64, updates: Sender<Update>) {
    let mut online = Duration::ZERO;
    while online < POLL_FOR {
        std::thread::sleep(POLL_EVERY);
        let update = match client.latest(&handle) {
            Ok(latest) => {
                online += POLL_EVERY;
                verdict(latest, &id, since)
            }
            // Time without a connection doesn't count against the limit.
            Err(error) if codeforces::unreachable(&error) => (
                Verdict::Retrying {
                    reason: "Can't reach Codeforces".to_string(),
                },
                None,
            ),
            Err(error) => {
                online += POLL_EVERY;
                let reason = format!("{error:#}");
                (Verdict::Retrying { reason }, None)
            }
        };
        let done = update.1.is_some();
        if updates.send(update).is_err() || done {
            return;
        }
    }
    let _ = updates.send((Verdict::Idle, None));
}

fn verdict(latest: Vec<Submission>, id: &ProblemId, since: i64) -> Update {
    for submission in latest {
        if submission.creation_time_seconds < since - SLACK_SECONDS
            || submission.problem.id().as_ref() != Some(id)
        {
            continue;
        }
        let passed = submission.passed_test_count;
        return match submission.verdict.as_deref() {
            None | Some("TESTING") => (Verdict::Testing { passed }, None),
            Some("OK") => (Verdict::Accepted, Some(submission)),
            Some(verdict) => {
                let verdict = Verdict::Rejected {
                    verdict: verdict.to_string(),
                    test: passed + 1,
                };
                (verdict, Some(submission))
            }
        };
    }
    (Verdict::Waiting, None)
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
