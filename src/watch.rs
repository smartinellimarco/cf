use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::style::{Color, Stylize};
use crossterm::{cursor, execute, queue, terminal};
use notify_debouncer_mini::notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{DebounceEventResult, Debouncer, new_debouncer};

use crate::codeforces::{self, Client, Handle, Page, Problem, ProblemId, Submission};
use crate::config::{Command, Config, Dirs, Lang, Paths, Quote};
use crate::fetch::{Fetched, Fetcher};
use crate::local;
use crate::run::{Outcome, Report, Usage};
use crate::statement::{self, Rules, Statement};
use crate::sweep::{self, Done, Job, Runner, Stored};

const TICK: Duration = Duration::from_millis(100);
const POLL_EVERY: Duration = Duration::from_secs(5);
const POLL_FOR: Duration = Duration::from_secs(15 * 60);
// Allows for clock skew between this machine and Codeforces.
const SLACK_SECONDS: i64 = 30;
const COLUMNS_PER_BAND: usize = 50;
const BANDS_SHOWN: usize = 2;
const ATTEMPTS_SHOWN: usize = 5;
// nf-md-circle_outline, nf-md-circle and nf-md-dots_vertical.
const RING: &str = "\u{f0766}";
const DOT: &str = "\u{f0765}";
const MORE: &str = "\u{f01d9}";
// nf-md-circle_small, for a problem with nothing to show yet.
const EMPTY: &str = "\u{f09df}";
// cli-spinners' dots2, a ring of dots one cell wide like the other glyphs.
const SPINNER: [&str; 8] = ["⣾", "⣽", "⣻", "⢿", "⡿", "⣟", "⣯", "⣷"];
const SPIN: Duration = Duration::from_millis(80);
// Goes after a glyph instead of a space: ghostty draws a nerd font glyph two cells wide when the
// next cell is a plain space, and one cell wide otherwise, like between the cursor's brackets.
const GAP: &str = "\u{a0}";
const FLASH: Duration = Duration::from_secs(1);
const COLUMN_MAX: usize = 40;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Solved,
    Tried,
    Todo,
}

// Where a problem's statement and samples stand; a problem missing from the map has none yet.
enum Fetch {
    Fetching,
    Ready(Statement),
    Failed(String),
}

// What the report area shows for the open problem, derived from the state.
enum Shown<'a> {
    Downloading,
    FetchFailed(&'a str),
    NoSolution,
    Building,
    Done(&'a Stored),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Local {
    Running,
    Pass,
    Fail,
    NoSamples,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mark {
    Accepted,
    Rejected,
    CompilationError,
    Testing,
}

struct Attempt {
    mark: Mark,
    at: i64,
    submission: Submission,
}

enum Motion {
    Next,
    Prev,
    First,
    Last,
    Find { forward: bool, standing: Standing },
}

// What ]f, ]s, ]t and ]e look for: the first three by the newest submission on Codeforces, the
// last one by the result here.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    Failed,
    Solved,
    Untouched,
    LocalError,
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

// A grid column: its cells and whether it is the current problem.
type Column = (Vec<String>, bool);

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
    pub ids: Vec<ProblemId>,
    pub start: Option<ProblemId>,
    // Language for new files instead of default_lang, from --lang.
    pub lang: Option<String>,
    pub problems: Vec<Problem>,
    pub submissions: Vec<Submission>,
}

// The state is what is stored: statements, local files, results by content hash, the work in
// flight and the submissions. Everything on screen derives from it, every event only changes it
// and then `reconcile` decides what work is missing.
pub struct Watch<'a> {
    client: &'a mut Client,
    config: Config,
    ids: Vec<ProblemId>,
    catalog: HashMap<ProblemId, Problem>,
    submissions: Vec<Submission>,
    history: HashMap<ProblemId, Vec<Attempt>>,
    statements: HashMap<ProblemId, Fetch>,
    // Local solutions per problem, newest first, and the one picked with s.
    files: HashMap<ProblemId, Vec<PathBuf>>,
    chosen: HashMap<ProblemId, PathBuf>,
    hashes: HashMap<PathBuf, String>,
    results: HashMap<String, Stored>,
    work: HashSet<String>,
    runner: Runner,
    fetcher: Fetcher,
    events: Receiver<DebounceEventResult>,
    // Kept alive for as long as the events above are wanted.
    _debouncer: Debouncer<RecommendedWatcher>,
    // The view: where the cursor is and what it says.
    current: usize,
    // The cursor's row in the current column: 0 is local, 1 the newest submission. `want` is the
    // row asked for with j/k, kept across problems like vim keeps a column.
    row: usize,
    want: usize,
    popup: bool,
    prefix: Option<char>,
    keymap: HashMap<String, Command>,
    note: String,
    copied: Option<Instant>,
    verdict: Verdict,
    updates: Option<Receiver<Update>>,
    // When cf started, to step the spinner.
    started: Instant,
    clipboard: Option<arboard::Clipboard>,
    // The last frame drawn and the terminal size it was drawn for.
    frame: Vec<String>,
    size: (u16, u16),
    root: PathBuf,
    templates: PathBuf,
    samples: PathBuf,
    builds: PathBuf,
    results_dir: PathBuf,
    // Language for new solutions instead of default_lang, from --lang.
    lang: Option<String>,
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
        // Editors save by renaming a temp file over the source, so whole folders are watched:
        // the solutions anywhere under the root and every problem's samples.
        let (sender, events) = mpsc::channel();
        let mut debouncer = new_debouncer(TICK, move |result| {
            let _ = sender.send(result);
        })
        .context("starting the file watcher")?;
        std::fs::create_dir_all(dirs.samples())?;
        for dir in [&root, &dirs.samples()] {
            debouncer
                .watcher()
                .watch(dir, RecursiveMode::Recursive)
                .with_context(|| format!("watching {}", dir.display()))?;
        }
        let results_dir = dirs.results(&root);
        let keymap = config.keymap();
        let mut watch = Watch {
            fetcher: Fetcher::new(client.clone(), dirs.samples()),
            client,
            history: history(&submissions),
            files: local::find(&root, &config),
            config,
            ids,
            catalog,
            submissions,
            statements: HashMap::new(),
            chosen: HashMap::new(),
            hashes: HashMap::new(),
            results: HashMap::new(),
            work: HashSet::new(),
            runner: Runner::new(results_dir.clone()),
            events,
            _debouncer: debouncer,
            current: 0,
            row: 0,
            want: 0,
            popup: true,
            prefix: None,
            keymap,
            note: String::new(),
            copied: None,
            verdict: Verdict::Idle,
            updates: None,
            started: Instant::now(),
            clipboard: None,
            frame: Vec::new(),
            size: (0, 0),
            templates: dirs.templates(),
            samples: dirs.samples(),
            builds: dirs.builds(&root),
            results_dir,
            root,
            lang,
        };
        watch.current = match start.and_then(|id| watch.ids.iter().position(|at| *at == id)) {
            Some(at) => at,
            None => watch.next(0),
        };
        Ok(watch)
    }

    pub fn run(&mut self) -> Result<()> {
        let _screen = Screen::enter()?;
        self.reconcile();
        let mut dirty = true;
        loop {
            if dirty {
                self.render()?;
            }
            // The spinner moves for as long as there is work.
            dirty = !self.work.is_empty()
                || self
                    .statements
                    .values()
                    .any(|fetch| matches!(fetch, Fetch::Fetching));
            let mut changed = false;

            if event::poll(TICK)? {
                match event::read()? {
                    Event::Key(key) => {
                        if let KeyEventKind::Press = key.kind {
                            match self.press(key) {
                                Action::Quit => break,
                                Action::Continue => changed = true,
                            }
                        }
                    }
                    Event::Resize(..) => dirty = true,
                    Event::FocusGained | Event::FocusLost | Event::Mouse(_) | Event::Paste(_) => {}
                }
            }

            for result in self.events.try_iter().collect::<Vec<_>>() {
                if let Err(error) = result {
                    self.note = format!("file watch failed: {error}");
                }
                self.files = local::find(&self.root, &self.config);
                changed = true;
            }
            for Fetched { id, statement } in self.fetcher.done.try_iter().collect::<Vec<_>>() {
                let fetch = match statement {
                    Ok(statement) => Fetch::Ready(statement),
                    Err(error) => Fetch::Failed(error),
                };
                self.statements.insert(id, fetch);
                changed = true;
            }
            for Done { hash, stored } in self.runner.done.try_iter().collect::<Vec<_>>() {
                self.work.remove(&hash);
                self.results.insert(hash, stored);
                changed = true;
            }
            changed |= self.poll();
            if self.copied.is_some_and(|at| at.elapsed() > FLASH) {
                self.copied = None;
                changed = true;
            }

            if changed {
                self.reconcile();
                dirty = true;
            }
        }
        let keep: HashSet<&String> = self.hashes.values().collect();
        sweep::prune(&self.results_dir, &keep);
        Ok(())
    }

    // The one rule: fetch what is missing, the open problem first, and run every solution whose
    // content has no result yet. Edits, downloads, finished runs and failures all land here.
    fn reconcile(&mut self) {
        let current = self.id().clone();
        let mut ids: Vec<ProblemId> = self.files.keys().cloned().collect();
        // Nearest to the open problem first, so its neighbours are ready when you get there.
        ids.sort_by_key(|id| match self.ids.iter().position(|at| at == id) {
            Some(at) => at.abs_diff(self.current),
            None => usize::MAX,
        });
        if !ids.contains(&current) {
            ids.insert(0, current.clone());
        }
        self.hashes.clear();
        for id in ids {
            let first = id == current;
            // A statement is only ready while its samples are on disk.
            if let Some(Fetch::Ready(_)) = self.statements.get(&id)
                && !self.samples.join(id.to_string()).is_dir()
            {
                self.statements.remove(&id);
            }
            if !self.statements.contains_key(&id) {
                let fetch = match self.cached(&id) {
                    Some(statement) => Fetch::Ready(statement),
                    None => {
                        if first {
                            self.fetcher.first(id.clone());
                        } else {
                            self.fetcher.add(id.clone());
                        }
                        Fetch::Fetching
                    }
                };
                self.statements.insert(id.clone(), fetch);
            }
            let Some(Fetch::Ready(statement)) = self.statements.get(&id) else {
                continue;
            };
            let rules = statement.rules.clone();
            for source in self.files.get(&id).cloned().unwrap_or_default() {
                let lang = self.lang(&source);
                let paths = self.paths(&id, &source);
                let Ok(hash) = sweep::hash(&paths, &lang, &rules) else {
                    continue;
                };
                self.hashes.insert(source.clone(), hash.clone());
                if self.results.contains_key(&hash) || self.work.contains(&hash) {
                    continue;
                }
                if let Some(stored) = sweep::load(&self.results_dir, &hash) {
                    self.results.insert(hash, stored);
                    continue;
                }
                self.work.insert(hash.clone());
                let job = Job {
                    hash,
                    key: key(&source),
                    paths,
                    rules: rules.clone(),
                    lang,
                };
                self.runner.add(job, first);
            }
        }
    }

    // A statement downloaded before, with its samples still in the cache.
    fn cached(&self, id: &ProblemId) -> Option<Statement> {
        if !self.samples.join(id.to_string()).is_dir() {
            return None;
        }
        self.client.load(&format!("statements/{id}.json"))
    }

    fn press(&mut self, key: KeyEvent) -> Action {
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        let KeyCode::Char(char) = key.code else {
            self.prefix = None;
            if key.code == KeyCode::Esc {
                self.popup = false;
            }
            return Action::Continue;
        };
        // Bindings like gg and ]f take two keys, vim style: the first one waits for the second.
        let typed = match self.prefix.take() {
            Some(prefix) => format!("{prefix}{char}"),
            None => char.to_string(),
        };
        if let Some(command) = self.keymap.get(&typed).copied() {
            return self.command(command);
        }
        if typed.chars().count() == 1 && self.keymap.keys().any(|key| key.starts_with(&typed)) {
            self.prefix = Some(char);
        }
        Action::Continue
    }

    fn command(&mut self, command: Command) -> Action {
        let find = |forward, standing| Motion::Find { forward, standing };
        match command {
            Command::Next => self.go(Motion::Next),
            Command::Prev => self.go(Motion::Prev),
            Command::Up => self.climb(1),
            Command::Down => self.climb(-1),
            Command::First => self.go(Motion::First),
            Command::Last => self.go(Motion::Last),
            Command::NextFailed => self.go(find(true, Standing::Failed)),
            Command::PrevFailed => self.go(find(false, Standing::Failed)),
            Command::NextSolved => self.go(find(true, Standing::Solved)),
            Command::PrevSolved => self.go(find(false, Standing::Solved)),
            Command::NextUntouched => self.go(find(true, Standing::Untouched)),
            Command::PrevUntouched => self.go(find(false, Standing::Untouched)),
            Command::NextLocalError => self.go(find(true, Standing::LocalError)),
            Command::PrevLocalError => self.go(find(false, Standing::LocalError)),
            Command::Copy => self.copy(),
            Command::Submit => self.submit(),
            Command::Web => self.browse(),
            Command::Start => self.start(),
            Command::Swap => self.swap(),
            Command::Rerun => self.rerun(),
            Command::Quit => return Action::Quit,
        }
        Action::Continue
    }

    // The key shown for a command: the shortest one bound to it, if any.
    fn key(&self, command: Command) -> Option<String> {
        self.keymap
            .iter()
            .filter(|(_, bound)| **bound == command)
            .map(|(key, _)| key.clone())
            .min_by_key(|key| (key.chars().count(), key.clone()))
    }

    fn rows(&self) -> usize {
        let attempts = self.history.get(self.id()).map_or(0, Vec::len);
        attempts.min(ATTEMPTS_SHOWN)
    }

    fn climb(&mut self, step: isize) {
        self.row = self.row.saturating_add_signed(step).min(self.rows());
        self.want = self.row;
        self.popup = true;
    }

    fn go(&mut self, motion: Motion) {
        self.popup = true;
        let from = self.current;
        let last = self.ids.len() - 1;
        let to = match motion {
            Motion::Next => Some((from + 1).min(last)),
            Motion::Prev => Some(from.saturating_sub(1)),
            Motion::First => Some(0),
            Motion::Last => Some(last),
            Motion::Find { forward, standing } => {
                let found = |at: &usize| self.is(&self.ids[*at], standing);
                if forward {
                    (from + 1..=last).find(found)
                } else {
                    (0..from).rev().find(found)
                }
            }
        };
        let Some(to) = to else {
            let name = match motion {
                Motion::Find {
                    standing: Standing::Solved,
                    ..
                } => "solved",
                Motion::Find {
                    standing: Standing::Failed,
                    ..
                } => "failed",
                Motion::Find {
                    standing: Standing::LocalError,
                    ..
                } => "locally failing",
                _ => "untouched",
            };
            let side = match motion {
                Motion::Find { forward: true, .. } => "after",
                _ => "before",
            };
            self.note = format!("no {name} problem {side} this one");
            return;
        };
        if to != self.current {
            self.current = to;
            self.row = self.want.min(self.rows());
            self.note.clear();
            self.verdict = Verdict::Idle;
            self.updates = None;
        }
    }

    fn id(&self) -> &ProblemId {
        &self.ids[self.current]
    }

    fn source(&self) -> Option<&PathBuf> {
        self.source_of(self.id())
    }

    // The solution picked with s, or the most recently edited one.
    fn source_of(&self, id: &ProblemId) -> Option<&PathBuf> {
        let files = self.files.get(id)?;
        match self.chosen.get(id) {
            Some(chosen) if files.contains(chosen) => Some(chosen),
            _ => files.first(),
        }
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

    fn swap(&mut self) {
        let Some(files) = self.files.get(self.id()) else {
            return;
        };
        let Some(source) = self.source() else {
            return;
        };
        let at = files.iter().position(|file| file == source).unwrap_or(0);
        let next = files[(at + 1) % files.len()].clone();
        self.chosen.insert(self.id().clone(), next);
    }

    // r forgets the open solution's result so it runs again, or retries a failed download.
    fn rerun(&mut self) {
        let id = self.id().clone();
        if let Some(Fetch::Failed(_)) = self.statements.get(&id) {
            self.statements.remove(&id);
            return;
        }
        let Some(hash) = self
            .source()
            .and_then(|source| self.hashes.get(source))
            .cloned()
        else {
            return;
        };
        self.results.remove(&hash);
        sweep::forget(&self.results_dir, &hash);
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

    // One column per problem with its attempts stacked upwards, newest at the bottom. Also
    // returns the problems on screen, which the legend counts when there is no list.
    fn progress(&self, width: usize) -> Vec<String> {
        let per_band = (width / 2).clamp(1, COLUMNS_PER_BAND);
        let mut columns = Vec::new();
        for (at, id) in self.ids.iter().enumerate() {
            columns.push((self.column(id), at == self.current));
        }
        // Fixed bands, so moving shifts the current mark and not the grid.
        let count = columns.len().div_ceil(per_band);
        let first = (self.current / per_band)
            .saturating_sub(BANDS_SHOWN / 2)
            .min(count.saturating_sub(BANDS_SHOWN));
        let bands: Vec<&[Column]> = columns
            .chunks(per_band)
            .skip(first)
            .take(BANDS_SHOWN)
            .collect();
        let mut cursor = None;
        let mut lines = Vec::new();
        for chunk in bands {
            let mut band = Vec::new();
            // Always the full height, so what comes below never moves.
            let mut cursor_at = None;
            for row in (0..=ATTEMPTS_SHOWN + 1).rev() {
                // Two brackets with their tips toward the cells: cf's rows, then local's row that
                // closes on the baseline.
                let (label, edge) = match row {
                    0 => ("local", "┌"),
                    1 => ("cf", "└"),
                    _ if row == ATTEMPTS_SHOWN + 1 => ("", "┌"),
                    _ => ("", "│"),
                };
                let mut line = format!("{} {} ", format!("{label:>5}").dim(), edge.dim());
                for (cells, is_current) in chunk.iter() {
                    // Every cell takes two columns: a glyph and its gap, or a spinner frame.
                    let cell = cells.get(row).map_or("", String::as_str);
                    let gap = GAP.repeat(2usize.saturating_sub(visible(cell)));
                    // The cursor's brackets take the gaps around its cell, so nothing shifts.
                    let cursor = *is_current && row == self.row && !cell.is_empty();
                    if cursor {
                        // Where the popup hangs: right after the closing bracket.
                        cursor_at = Some((band.len(), visible(&line) + 2));
                    }
                    if cursor && !gap.is_empty() && (line.ends_with(' ') || line.ends_with(GAP)) {
                        line.pop();
                        let _ = write!(line, "{}{cell}{}", "[".bold(), "]".bold());
                    } else {
                        let _ = write!(line, "{cell}{gap}");
                    }
                }
                band.push(line);
            }
            let mut base = format!("{} ", "      └".dim());
            for (_, is_current) in chunk {
                let mark = if *is_current {
                    "━".bold()
                } else {
                    "─".dim()
                };
                let _ = write!(base, "{mark} ");
            }
            band.push(base);
            if let Some((row, column)) = cursor_at {
                cursor = Some((lines.len() + row, column));
            }
            lines.extend(band);
        }
        // The grid centers on its own width; the popup is then painted over it like a completion
        // menu, next to the cursor.
        let widest = lines.iter().map(|line| visible(line)).max().unwrap_or(0);
        let pad = " ".repeat(width.saturating_sub(widest) / 2);
        let popup = self.popup_lines();
        if let Some((row, column)) = cursor
            && !popup.is_empty()
        {
            // Above the cursor, its bottom on the cursor's row, so the rows below stay readable.
            let top = (row + 1).saturating_sub(popup.len());
            for (at, box_line) in popup.iter().enumerate() {
                if let Some(line) = lines.get_mut(top + at) {
                    *line = overlay(line, column, box_line);
                }
            }
        }
        lines.iter().map(|line| format!("{pad}{line}")).collect()
    }

    // What the cell under the cursor knows, in a box; nothing for an empty cell or after esc.
    fn popup_lines(&self) -> Vec<String> {
        if !self.popup {
            return Vec::new();
        }
        let content = if self.row == 0 {
            self.local_popup()
        } else {
            self.submission_popup()
        };
        let Some((title, body)) = content else {
            return Vec::new();
        };
        let inner = body
            .iter()
            .map(|line| visible(line))
            .chain([visible(&title) + 2])
            .max()
            .unwrap_or(0);
        let mut lines = vec![format!(
            "╭─ {title} {}╮",
            "─".repeat(inner.saturating_sub(visible(&title) + 1))
        )];
        for line in body {
            let pad = " ".repeat(inner - visible(&line));
            lines.push(format!("│ {line}{pad} │"));
        }
        lines.push(format!("╰{}╯", "─".repeat(inner + 2)));
        lines
    }

    fn submission_popup(&self) -> Option<(String, Vec<String>)> {
        let attempts = self.history.get(self.id())?;
        let attempt = attempts.iter().rev().nth(self.row - 1)?;
        let submission = &attempt.submission;
        let title = format!(
            "#{} · {}",
            submission.id,
            ago(submission.creation_time_seconds)
        );
        let verdict = verdict_text(submission);
        let numbers = format!(
            "{} ms · {:.1} MB",
            submission.time_consumed_millis,
            submission.memory_consumed_bytes as f64 / 1e6
        );
        Some((
            title,
            vec![
                format!("{} · {verdict}", submission.programming_language),
                numbers,
            ],
        ))
    }

    fn local_popup(&self) -> Option<(String, Vec<String>)> {
        let sources = self.files.get(self.id())?;
        let active = self.source()?;
        let mut files = Vec::new();
        for source in sources {
            let lang = self.lang(source);
            // Checked when the config loads.
            let color = Color::try_from(lang.color.as_str()).unwrap_or(Color::Reset);
            let name = format!("{} {}", lang.icon.as_str().with(color), key(source));
            files.push(if source == active {
                format!("[{name}]")
            } else {
                format!(" {name} ").dim().to_string()
            });
        }
        let mut body = vec![files.concat()];
        if let Some(Stored {
            report: Report::Ran(outcomes),
            ..
        }) = self.stored(self.id())
        {
            let slowest = outcomes
                .iter()
                .filter_map(|(_, outcome)| outcome.usage())
                .max_by_key(|usage| usage.elapsed);
            let peak = outcomes
                .iter()
                .filter_map(|(_, outcome)| outcome.usage())
                .map(|usage| usage.memory)
                .max();
            if let (Some(slowest), Some(memory)) = (slowest, peak) {
                body.push(numbers(Usage {
                    elapsed: slowest.elapsed,
                    memory,
                }));
            }
        }
        if sources.len() > 1 {
            body.push("s swaps the solution".dim().to_string());
        }
        Some(("local".to_string(), body))
    }

    // Each symbol with how many problems are in that state: the whole list, or the grid on
    // screen when walking the problemset.
    fn legend(&self) -> Vec<String> {
        let ids = &self.ids;
        let remote = |mark: Mark| {
            ids.iter()
                .filter(|id| self.remote(id) == Some(mark))
                .count()
        };
        let local = |state: Local| {
            ids.iter()
                .filter(|id| self.local(id) == Some(state))
                .count()
        };
        let unsolved = ids.iter().filter(|id| self.local(id).is_none()).count();
        let entry =
            |symbol: String, count: usize, label: &str| format!("{symbol}{GAP}{count} {label}");
        let remote = [
            entry(ring(Mark::Accepted), remote(Mark::Accepted), "accepted"),
            entry(ring(Mark::Rejected), remote(Mark::Rejected), "rejected"),
            entry(
                ring(Mark::CompilationError),
                remote(Mark::CompilationError),
                "compilation error",
            ),
            entry(ring(Mark::Testing), remote(Mark::Testing), "testing"),
            format!("{}{GAP}more than five", MORE.dark_grey()),
        ];
        let local = [
            entry(local_dot(Local::Pass), local(Local::Pass), "pass"),
            entry(local_dot(Local::Fail), local(Local::Fail), "fail"),
            entry(local_dot(Local::Running), local(Local::Running), "running"),
            entry(
                local_dot(Local::NoSamples),
                local(Local::NoSamples),
                "no samples",
            ),
            entry(EMPTY.dark_grey().to_string(), unsolved, "no solution"),
        ];
        vec![
            format!("{} {}", "remote".bold(), remote.join("   ")),
            format!("{}  {}", "local".bold(), local.join("   ")),
        ]
    }

    fn stored(&self, id: &ProblemId) -> Option<&Stored> {
        let hash = self.hashes.get(self.source_of(id)?)?;
        self.results.get(hash)
    }

    // Spinning until the statement is here and the content has a result; a solution's result
    // never shows for contents other than the one it ran on.
    fn local(&self, id: &ProblemId) -> Option<Local> {
        self.source_of(id)?;
        match self.statements.get(id) {
            Some(Fetch::Failed(_)) => None,
            Some(Fetch::Ready(_)) => Some(match self.stored(id) {
                Some(stored) => match &stored.report {
                    Report::NoSamples => Local::NoSamples,
                    report if report.passed() => Local::Pass,
                    _ => Local::Fail,
                },
                None => Local::Running,
            }),
            Some(Fetch::Fetching) | None => Some(Local::Running),
        }
    }

    fn shown_report(&self) -> Shown<'_> {
        match self.statements.get(self.id()) {
            Some(Fetch::Failed(error)) => Shown::FetchFailed(error),
            Some(Fetch::Ready(_)) if self.source().is_none() => Shown::NoSolution,
            Some(Fetch::Ready(_)) => match self.stored(self.id()) {
                Some(stored) => Shown::Done(stored),
                None => Shown::Building,
            },
            Some(Fetch::Fetching) | None => Shown::Downloading,
        }
    }

    // A problem's standing on Codeforces: accepted once is accepted, otherwise its last verdict.
    fn remote(&self, id: &ProblemId) -> Option<Mark> {
        let attempts = self.history.get(id)?;
        if attempts
            .iter()
            .any(|attempt| matches!(attempt.mark, Mark::Accepted))
        {
            return Some(Mark::Accepted);
        }
        attempts.last().map(|attempt| attempt.mark)
    }

    // The codeforces motions follow the newest submission, the ring right above the local row.
    fn is(&self, id: &ProblemId, standing: Standing) -> bool {
        let latest = self
            .history
            .get(id)
            .and_then(|attempts| attempts.last())
            .map(|attempt| attempt.mark);
        match standing {
            Standing::Failed => {
                matches!(latest, Some(Mark::Rejected | Mark::CompilationError))
            }
            Standing::Solved => latest == Some(Mark::Accepted),
            Standing::Untouched => latest.is_none(),
            Standing::LocalError => self.local(id) == Some(Local::Fail),
        }
    }

    // Bottom-up cells: the first row is always local, the solution's result or nothing there;
    // above it the attempts newest first, so the mark for older hidden ones sits on top.
    // Rings are submissions and the filled circle is the local result, all nerd font glyphs.
    fn column(&self, id: &ProblemId) -> Vec<String> {
        let mut cells = vec![match self.local(id) {
            Some(local) => self.local_cell(local),
            None => EMPTY.dark_grey().to_string(),
        }];
        let attempts = self.history.get(id).map_or(&[][..], Vec::as_slice);
        for attempt in attempts.iter().rev().take(ATTEMPTS_SHOWN) {
            cells.push(ring(attempt.mark));
        }
        if attempts.len() > ATTEMPTS_SHOWN {
            cells.push(MORE.dark_grey().to_string());
        }
        cells
    }

    // A running solution spins through a circle filling up.
    fn local_cell(&self, local: Local) -> String {
        match local {
            Local::Running => {
                let frame = self.started.elapsed().as_millis() / SPIN.as_millis();
                let at = frame as usize % SPINNER.len();
                SPINNER[at].dark_grey().to_string()
            }
            Local::Pass | Local::Fail | Local::NoSamples => local_dot(local),
        }
    }

    // Starts a solution in the language for new ones, or switches to it when there is one already.
    fn start(&mut self) {
        let id = self.id().clone();
        let Some(Fetch::Ready(statement)) = self.statements.get(&id) else {
            self.note = "the statement isn't here yet".to_string();
            return;
        };
        let lang = &self.config.langs[&self.new_lang()];
        let file = self.root.join(format!("{id}.{}", lang.ext));
        let existing = self
            .files
            .get(&id)
            .and_then(|files| files.iter().find(|f| f.extension() == file.extension()))
            .cloned();
        let file = match existing {
            Some(existing) => existing,
            None => match self.create(statement, &file) {
                Ok(()) => file,
                Err(error) => {
                    self.note = format!("{error:#}");
                    return;
                }
            },
        };
        self.files = local::find(&self.root, &self.config);
        self.chosen.insert(id, file);
    }

    fn new_lang(&self) -> String {
        self.lang
            .clone()
            .unwrap_or_else(|| self.config.default_lang.clone())
    }

    fn create(&self, statement: &Statement, file: &Path) -> Result<()> {
        let id = self.id();
        let lang = &self.config.langs[&self.new_lang()];
        let template_path = self.templates.join(format!("main.{}", lang.ext));
        let template = std::fs::read_to_string(&template_path)
            .with_context(|| format!("reading {}", template_path.display()))?;
        // A file the glob skipped, say an ignored one, is kept as it is.
        if !file.exists() {
            let header = statement::comment(&lang.comment, statement, &id.url(Page::Problem));
            std::fs::write(file, header + &template)?;
        }
        Ok(())
    }

    fn copy(&mut self) {
        let Shown::Done(stored) = self.shown_report() else {
            return;
        };
        if let Report::Failed(_) | Report::BuildFailed(_) = stored.report {
            self.note = "the last build failed, fix it before copying".to_string();
            return;
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

    // Writes only the lines that changed since the last frame, like a terminal ui's double buffer,
    // inside one synchronized update; rewriting every line each tick makes busy screens flicker.
    fn render(&mut self) -> Result<()> {
        let size = terminal::size()?;
        let (columns, rows) = size;
        let width = usize::from(columns);
        let mut lines = Vec::new();
        for line in self.problem(width).lines() {
            lines.extend(wrap(line, width));
        }
        lines.truncate(usize::from(rows));
        let mut stdout = std::io::stdout();
        queue!(stdout, terminal::BeginSynchronizedUpdate)?;
        if size != self.size {
            self.size = size;
            self.frame.clear();
            queue!(stdout, terminal::Clear(terminal::ClearType::All))?;
        }
        for (at, line) in lines.iter().enumerate() {
            if self.frame.get(at) != Some(line) {
                queue!(stdout, cursor::MoveTo(0, at as u16))?;
                stdout.write_all(line.as_bytes())?;
                queue!(stdout, terminal::Clear(terminal::ClearType::UntilNewLine))?;
            }
        }
        if self.frame.len() > lines.len() {
            queue!(
                stdout,
                cursor::MoveTo(0, lines.len() as u16),
                terminal::Clear(terminal::ClearType::FromCursorDown)
            )?;
        }
        queue!(stdout, terminal::EndSynchronizedUpdate)?;
        stdout.flush()?;
        self.frame = lines;
        Ok(())
    }

    // Only the first failing sample shows, so the report fits its fixed height. The head lines
    // center one by one, the block (a diff or a log) centers as a whole to keep its columns.
    fn report(&self) -> (Vec<String>, Vec<String>) {
        let head = |line: String| (vec![line], Vec::new());
        let block = |text: &str| text.lines().map(str::to_string).collect::<Vec<_>>();
        let report = match self.shown_report() {
            Shown::Downloading => return head("Downloading the statement…".to_string()),
            Shown::FetchFailed(error) => {
                return head(format!("{error}, press r to retry").red().to_string());
            }
            Shown::NoSolution => return head("No solution yet, e to start one".to_string()),
            Shown::Building => return head("Building…".to_string()),
            Shown::Done(stored) => &stored.report,
        };
        match report {
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
                let (what, details) = match first {
                    Outcome::Pass { .. } => return (lines, Vec::new()),
                    Outcome::Wrong { expected, got, .. } => {
                        ("wrong answer".to_string(), diff(expected, got))
                    }
                    Outcome::Crash { stderr, .. } => ("runtime error".to_string(), block(stderr)),
                    Outcome::Timeout => ("over 5 s".to_string(), Vec::new()),
                    Outcome::Slow { limit, .. } => (
                        format!("over the {} ms time limit", limit.as_millis()),
                        Vec::new(),
                    ),
                };
                let mut line = format!("Sample {k} ✗ {what}").red().to_string();
                // How many more samples fail besides the one shown.
                if !rest.is_empty() {
                    let _ = write!(line, " {}", format!("+{}", rest.len()).red().bold());
                }
                if let Some(usage) = first.usage() {
                    let _ = write!(line, "  {}", numbers(usage).dim());
                }
                lines.push(line);
                (lines, details)
            }
        }
    }

    fn messages(&self) -> Vec<String> {
        let mut messages = Vec::new();
        let verdict = match &self.verdict {
            Verdict::Idle => String::new(),
            Verdict::Waiting => "Waiting for your submission…".to_string(),
            Verdict::Retrying { reason } => format!("{reason}, retrying…"),
            Verdict::Testing { passed } => format!("Testing… {passed} passed"),
            Verdict::Accepted => format!(
                "{} Enter `l` to move on to the next problem",
                "Accepted ✓".green().bold()
            ),
            Verdict::Rejected { verdict, test } => {
                format!("{}", format!("{verdict} on test {test}").red().bold())
            }
        };
        // The verdict takes the submit hint's line once a submission is on its way.
        if !verdict.is_empty() {
            messages.push(verdict);
        } else if let Shown::Done(stored) = self.shown_report()
            && stored.report.passed()
        {
            let shown = self.shown();
            let lang = self.lang(&shown.source);
            messages.push(format!(
                "Submit `{}`",
                shown.expand(&lang.submit, Quote::Plain)
            ));
        }
        if !self.note.is_empty() {
            messages.push(self.note.clone());
        }
        if let Shown::Done(stored) = self.shown_report()
            && !stored.strays.is_empty()
        {
            let stray = format!(
                "Build wrote {} next to the source instead of {{build}}",
                stored.strays.join(", ")
            );
            messages.push(stray.yellow().to_string());
        }
        if let Some(Fetch::Ready(statement)) = self.statements.get(self.id())
            && let Rules { input, output, .. } = &statement.rules
            && (input.is_some() || output.is_some())
        {
            let input = input.as_deref().unwrap_or("stdin");
            let output = output.as_deref().unwrap_or("stdout");
            let files = format!("This problem reads {input} and writes {output}");
            messages.push(files.yellow().to_string());
        }

        messages
    }

    // Like rustlings the view flows top to bottom, and the report keeps a fixed height so
    // nothing below it moves when it changes. Every line is centered.
    // Everything with a fixed height goes first and the report, which grows with what it shows,
    // goes last, so nothing moves and no space is reserved for it. Every line is centered.
    fn problem(&self, width: usize) -> String {
        let mut lines = Vec::new();
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
        let title = format!("\x1b]8;;{url}\x1b\\{}\x1b]8;;\x1b\\", title.bold());
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
        let entries = |commands: &[(Command, &str)]| {
            commands
                .iter()
                .filter_map(|(command, label)| Some((self.key(*command)?, label.to_string())))
                .collect::<Vec<_>>()
        };
        let actions = prompt(&entries(&[
            (Command::Copy, &copy),
            (Command::Submit, "submit"),
            (Command::Web, "web"),
            (Command::Start, "start"),
            (Command::Swap, "swap"),
            (Command::Rerun, "rerun"),
            (Command::Quit, "quit"),
        ]));
        let motions = prompt(&entries(&[
            (Command::Prev, "prev"),
            (Command::Next, &next),
            (Command::Down, "down"),
            (Command::Up, "up"),
            (Command::First, "first"),
            (Command::Last, "last"),
            (Command::NextFailed, "failed"),
            (Command::NextSolved, "solved"),
            (Command::NextUntouched, "untouched"),
            (Command::NextLocalError, "local error"),
        ]));
        lines.push(center(&actions, width));
        lines.push(center(&motions, width));
        lines.push(String::new());

        lines.extend(self.progress(width));
        lines.push(String::new());
        lines.extend(center_block(&self.legend(), width));
        lines.push(String::new());

        // One row for all messages, so the report under it always starts at the same one.
        lines.push(center(&self.messages().join("  ·  "), width));
        lines.push(String::new());

        let (head, details) = self.report();
        lines.extend(head.iter().map(|line| center(line, width)));
        if !details.is_empty() {
            lines.push(String::new());
            lines.extend(center_block(&details, width));
        }
        lines.join("\n")
    }
}

fn prompt(keys: &[(String, String)]) -> String {
    let mut out = String::new();
    for (key, label) in keys {
        let _ = write!(out, "{} {label}  ", key.as_str().bold());
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

// Like the judge shows them: 46 ms · 3.2 MB.
fn numbers(usage: Usage) -> String {
    // Fixed widths so the line doesn't shift as the numbers grow; runs stop at 5 s anyway.
    format!(
        "{:>4} ms · {:>5.1} MB",
        usage.elapsed.as_millis().min(9999),
        (usage.memory as f64 / 1e6).min(999.9)
    )
}

fn ago(seconds: i64) -> String {
    let elapsed = codeforces::now() - seconds;
    match elapsed {
        ..3600 => format!("{} min ago", elapsed / 60),
        3600..86_400 => format!("{} h ago", elapsed / 3600),
        _ => format!("{} days ago", elapsed / 86_400),
    }
}

// The API's verdicts read like WRONG_ANSWER; the judge shows them with the failing test.
fn verdict_text(submission: &Submission) -> String {
    let Some(verdict) = submission.verdict.as_deref() else {
        return "in queue".to_string();
    };
    let words = verdict.to_lowercase().replace('_', " ");
    let mut chars = words.chars();
    let words = match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => words,
    };
    match verdict {
        "OK" => "Accepted".to_string(),
        "COMPILATION_ERROR" | "TESTING" => words,
        _ => format!("{words} on test {}", submission.passed_test_count + 1),
    }
}

fn ring(mark: Mark) -> String {
    let ring = match mark {
        Mark::Accepted => RING.green(),
        Mark::Rejected => RING.red(),
        Mark::CompilationError => RING.dark_grey(),
        Mark::Testing => RING.yellow(),
    };
    ring.to_string()
}

fn local_dot(local: Local) -> String {
    let dot = match local {
        Local::Pass => DOT.green(),
        Local::Fail => DOT.red(),
        Local::Running => SPINNER[0].dark_grey(),
        Local::NoSamples => DOT.dim(),
    };
    dot.to_string()
}

// Paints `top` over `line` from display column `at`, keeping what lies before and after it.
fn overlay(line: &str, at: usize, top: &str) -> String {
    let columns = columns(line);
    let mut out: String = columns.iter().take(at).map(String::as_str).collect();
    out.push_str(&" ".repeat(at.saturating_sub(columns.len())));
    // Resets around the box, so neither side's colors bleed into the other.
    let _ = write!(out, "\x1b[0m{top}\x1b[0m");
    for column in columns.iter().skip(at + visible(top)) {
        out.push_str(column);
    }
    out
}

// Splits a styled line into its display columns, each carrying the escapes that precede it.
fn columns(line: &str) -> Vec<String> {
    let mut columns = Vec::new();
    let mut pending = String::new();
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        pending.push(c);
        if c == '\x1b' {
            skip_escape(&mut chars, &mut pending);
        } else {
            columns.push(std::mem::take(&mut pending));
        }
    }
    if let Some(last) = columns.last_mut() {
        last.push_str(&pending);
    }
    columns
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
            submission: submission.clone(),
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
