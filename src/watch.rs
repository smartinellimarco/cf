use std::collections::{HashMap, HashSet};
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
use crate::run::{Outcome, Report};
use crate::statement::{self, Files, Kind, Statement};
use crate::sweep::{self, Done, Job, Runner};

const TICK: Duration = Duration::from_millis(100);
const POLL_EVERY: Duration = Duration::from_secs(5);
const POLL_FOR: Duration = Duration::from_secs(15 * 60);
// Allows for clock skew between this machine and Codeforces.
const SLACK_SECONDS: i64 = 30;
const OUTPUT_LINES: usize = 20;
const NAME_WIDTH: usize = 40;
const COLUMNS_PER_BAND: usize = 50;
const BANDS_SHOWN: usize = 2;
const ATTEMPTS_SHOWN: usize = 5;
// Ids of the submissions sent from cf while the samples passed.
const PASSING: &str = "passing.json";
const FLASH: Duration = Duration::from_secs(1);
const COLUMN_MAX: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Solved,
    Tried,
    Todo,
    Gym,
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
    // Sent from cf while the samples passed.
    passing: bool,
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

pub struct Watch<'a> {
    client: &'a mut Client,
    config: Config,
    lang: Lang,
    ids: Vec<ProblemId>,
    history: HashMap<ProblemId, Vec<Attempt>>,
    passing: HashSet<u64>,
    sending: bool,
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
    events: Receiver<DebounceEventResult>,
    debouncer: Debouncer<RecommendedWatcher>,
    watched: Option<PathBuf>,
    clipboard: Option<arboard::Clipboard>,
    root: PathBuf,
    templates: PathBuf,
    builds: PathBuf,
    strays: Vec<String>,
    before: Vec<String>,
    files: Files,
    runner: Runner,
    locals: HashMap<ProblemId, Local>,
    results: PathBuf,
}

impl<'a> Watch<'a> {
    pub fn new(
        client: &'a mut Client,
        config: Config,
        dirs: &Dirs,
        ids: Vec<ProblemId>,
        start: Option<ProblemId>,
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
        let results = dirs.results(&root);
        let runner = Runner::new(&lang, results.clone());
        let mut watch = Watch {
            client,
            config,
            lang,
            ids,
            history: HashMap::new(),
            passing: HashSet::new(),
            sending: false,
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
            events,
            debouncer,
            watched: None,
            clipboard: None,
            templates: dirs.templates(),
            builds: dirs.builds(&root),
            root,
            strays: Vec::new(),
            before: Vec::new(),
            files: Files::default(),
            runner,
            locals: HashMap::new(),
            results,
        };
        watch.passing = watch.client.load(PASSING).unwrap_or_default();
        watch.history = history(&watch.submissions, &watch.passing);
        watch.current = match start.and_then(|id| watch.ids.iter().position(|at| *at == id)) {
            Some(at) => at,
            None => watch.next(0),
        };
        Ok(watch)
    }

    pub fn run(&mut self) -> Result<ProblemId> {
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
                                Action::Quit => return Ok(self.id().clone()),
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
                self.before = self.entries();
                self.locals.insert(self.id().clone(), Local::Pending);
                self.runner.first(Job {
                    id: self.id().clone(),
                    paths: self.paths(),
                    files: self.files.clone(),
                });
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
            ('r', View::Problem) => self.open(),
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

    fn dir(&self) -> PathBuf {
        PathBuf::from("problems").join(self.id().to_string())
    }

    fn paths(&self) -> Paths {
        self.paths_of(self.id())
    }

    fn paths_of(&self, id: &ProblemId) -> Paths {
        let dir = self.root.join("problems").join(id.to_string());
        Paths {
            source: dir.join(self.source()),
            build: self.builds.join(id.to_string()),
            dir,
        }
    }

    // Runs the samples of every other problem with code here, so the list shows how each stands.
    fn sweep(&mut self) {
        let Ok(entries) = std::fs::read_dir(self.root.join("problems")) else {
            return;
        };
        let mut ids = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(Ok(id)) = name.to_str().map(str::parse::<ProblemId>) else {
                continue;
            };
            if id != *self.id() && self.paths_of(&id).source.is_file() {
                ids.push(id);
            }
        }
        // Nearest to the current problem first, problems outside the list last.
        ids.sort_by_key(|id| match self.ids.iter().position(|at| at == id) {
            Some(at) => at.abs_diff(self.current),
            None => usize::MAX,
        });
        let mut jobs = Vec::new();
        for id in ids {
            let paths = self.paths_of(&id);
            let files: Files = self
                .client
                .load(&format!("statements/{id}.json"))
                .unwrap_or_default();
            let hash = sweep::hash(&paths, &self.lang, &files).ok();
            match sweep::load(&self.results, &id) {
                Some(stored) if Some(&stored.hash) == hash.as_ref() => {
                    let local = Local::Ran {
                        passed: stored.passed,
                        at: stored.at,
                    };
                    self.locals.insert(id, local);
                }
                _ => {
                    self.locals.insert(id.clone(), Local::Pending);
                    jobs.push(Job { id, paths, files });
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
                    .insert(done.id.clone(), Local::Ran { passed, at })
            }
            None => self.locals.remove(&done.id),
        };
        if done.id == *self.id() {
            self.report = done.report;
            self.strays = self.strays(&self.before);
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
        if id.is_gym() {
            return Status::Gym;
        }
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
                Status::Solved | Status::Gym => {}
                Status::Tried | Status::Todo => return at,
            }
        }
        from % self.ids.len()
    }

    // One column per problem with its attempts stacked upwards, oldest at the bottom.
    fn progress(&self, width: usize) -> String {
        const PREFIX: &str = "Progress: ";
        let count = format!("  {}/{}", self.solved(), self.playable());
        let room = width.saturating_sub(PREFIX.len() + count.len());
        let per_band = (room / 2).clamp(1, COLUMNS_PER_BAND);
        let mut columns = Vec::new();
        let mut current = 0;
        for (at, id) in self.ids.iter().enumerate() {
            if id.is_gym() {
                continue;
            }
            if at == self.current {
                current = columns.len();
            }
            columns.push((self.column(id), at == self.current));
        }
        // The whole problemset is thousands of columns, so only the bands around the current one show.
        let bands = columns.len().div_ceil(per_band);
        let first = (current / per_band)
            .saturating_sub(BANDS_SHOWN / 2)
            .min(bands.saturating_sub(BANDS_SHOWN));
        let mut out = String::new();
        for (band, chunk) in columns
            .chunks(per_band)
            .enumerate()
            .skip(first)
            .take(BANDS_SHOWN)
        {
            let height = chunk
                .iter()
                .map(|(cells, _)| cells.len())
                .max()
                .unwrap_or(0);
            for row in (0..height).rev() {
                out.push_str(&" ".repeat(PREFIX.len()));
                for (cells, _) in chunk {
                    match cells.get(row) {
                        Some(cell) => out.push_str(cell),
                        None => out.push(' '),
                    }
                    out.push(' ');
                }
                out.push('\n');
            }
            let lead = if band == first {
                PREFIX.to_string()
            } else {
                " ".repeat(PREFIX.len())
            };
            out.push_str(&lead);
            for (_, is_current) in chunk {
                let base = if *is_current {
                    "━━".bold().to_string()
                } else {
                    "──".dim().to_string()
                };
                out.push_str(&base);
            }
            out.push('\n');
        }
        out.pop();
        out.push_str(&count);
        out
    }

    // Bottom-up cells: the last attempts, then the local result when it is newer than them.
    fn column(&self, id: &ProblemId) -> Vec<String> {
        let mut cells = Vec::new();
        let attempts = self.history.get(id).map_or(&[][..], Vec::as_slice);
        for attempt in attempts {
            let ring = if attempt.passing { "◉" } else { "○" };
            let ring = match attempt.mark {
                Mark::Accepted => ring.green(),
                Mark::Rejected => ring.red(),
                Mark::CompilationError => ring.dark_grey(),
                Mark::Testing => ring.yellow(),
            };
            cells.push(ring.to_string());
        }
        let last = attempts.last().map_or(i64::MIN, |attempt| attempt.at);
        match self.locals.get(id) {
            Some(Local::Pending) => cells.push("•".dim().to_string()),
            Some(Local::Ran { passed, at }) if *at > last => {
                let dot = if *passed { "•".green() } else { "•".red() };
                cells.push(dot.to_string());
            }
            Some(Local::Ran { .. }) | None => {}
        }
        if cells.len() > ATTEMPTS_SHOWN {
            cells.drain(..cells.len() - (ATTEMPTS_SHOWN - 1));
            cells.push("⋮".dim().to_string());
        }
        cells
    }

    fn playable(&self) -> usize {
        let mut count = 0;
        for id in &self.ids {
            if !id.is_gym() {
                count += 1;
            }
        }
        count
    }

    fn solved(&self) -> usize {
        let mut count = 0;
        for id in &self.ids {
            match self.status(id) {
                Status::Solved => count += 1,
                Status::Tried | Status::Todo | Status::Gym => {}
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
        self.updates = None;
        if self.id().is_gym() {
            self.report = Report::Failed(format!(
                "{} is a gym problem, cf can't open those because their statements aren't public pages",
                self.id()
            ));
            return;
        }
        match self.prepare() {
            Ok(()) => self.stale = true,
            Err(error) => self.report = Report::Failed(format!("{error:#}, press r to retry")),
        }
    }

    fn prepare(&mut self) -> Result<()> {
        let dir = self.dir();
        let file = dir.join(self.source());
        let cached = format!("statements/{}.json", self.id());
        // Problems opened before cf cached their files still need one download to learn them.
        self.files = match self.client.load(&cached).filter(|_| file.exists()) {
            Some(files) => files,
            None => {
                let url = self.id().url(Page::Problem);
                let statement = statement::parse(&self.client.get(&url)?)?;
                if !file.exists() {
                    self.create(&dir, &file, &statement, &url)?;
                }
                self.client.save(&cached, &statement.files)?;
                statement.files
            }
        };

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

    fn create(&self, dir: &Path, file: &Path, statement: &Statement, url: &str) -> Result<()> {
        let template_path = self.templates.join(self.source());
        let template = std::fs::read_to_string(&template_path)
            .with_context(|| format!("reading {}", template_path.display()))?;
        std::fs::create_dir_all(dir)?;
        match statement.kind {
            Kind::Standard => {
                for (k, sample) in statement.samples.iter().enumerate() {
                    std::fs::write(dir.join(format!("in{}.txt", k + 1)), &sample.input)?;
                    std::fs::write(dir.join(format!("ans{}.txt", k + 1)), &sample.answer)?;
                }
            }
            Kind::Interactive => {}
        }
        let header = statement::comment(&self.lang.comment, statement, url);
        std::fs::write(file, header + &template)?;
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
                self.note.clear();
                self.copied = Some(Instant::now());
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
        if self.updates.is_none() {
            let (sender, updates) = mpsc::channel();
            self.sending = matches!(
                self.locals.get(self.id()),
                Some(Local::Ran { passed: true, .. })
            );
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
            if let Some(submission) = submission {
                if self.sending {
                    self.passing.insert(submission.id);
                    let _ = self.client.save(PASSING, &self.passing);
                }
                self.submissions.push(submission);
            }
        }
        self.history = history(&self.submissions, &self.passing);
        changed
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
        let footer = format!(
            "{}\n{}",
            self.progress(width),
            hotkeys(&[
                ("n/p", "down/up"),
                ("gg/ge", "first/last"),
                ("]f/]s/]t", "next failed/solved/untouched"),
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
                Status::Gym => "GYM    ".dim(),
            };
            let local = match self.locals.get(id) {
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
        let Files { input, output } = &self.files;
        if input.is_some() || output.is_some() {
            let input = input.as_deref().unwrap_or("stdin");
            let output = output.as_deref().unwrap_or("stdout");
            let _ = writeln!(
                body,
                "\n{}",
                format!("This problem reads {input} and writes {output}").yellow()
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
            Verdict::Retrying { reason } => format!("{reason}, retrying…"),
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
            | Verdict::Retrying { .. }
            | Verdict::Testing { .. }
            | Verdict::Rejected { .. } => "next".to_string(),
        };
        // OSC 8 makes the link clickable in terminals that support it.
        let copy = match self.copied {
            Some(_) => "copied ✓".green().to_string(),
            None => "copy".to_string(),
        };
        let footer = format!(
            "\n{}\nCurrent problem: \x1b]8;;{url}\x1b\\{title}\x1b]8;;\x1b\\\n\n    {editor}\n\n{}",
            self.progress(width),
            prompt(&[
                ("y", &copy),
                ("o", "submit"),
                ("n/p", &format!("{next}/prev")),
                ("]f/]s/]t", "failed/solved/untouched"),
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

fn history(submissions: &[Submission], passing: &HashSet<u64>) -> HashMap<ProblemId, Vec<Attempt>> {
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
            passing: passing.contains(&submission.id),
        });
    }
    for attempts in history.values_mut() {
        attempts.sort_by_key(|attempt| attempt.at);
    }
    history
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
