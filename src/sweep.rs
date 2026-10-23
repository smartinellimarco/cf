use std::collections::{HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::config::{Lang, Paths};
use crate::run::{self, Report};
use crate::statement::Rules;

// One run of a solution's samples. `hash` names the result and `key`, the file name like 4A.cpp,
// names the build folder, which two runs must never share at once.
pub struct Job {
    pub hash: String,
    pub key: String,
    pub paths: Paths,
    pub rules: Rules,
    pub lang: Lang,
}

// A run's report, plus anything the build left next to the source instead of its build folder.
#[derive(Serialize, Deserialize, Clone)]
pub struct Stored {
    pub report: Report,
    pub strays: Vec<String>,
}

pub struct Done {
    pub hash: String,
    pub stored: Stored,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    running: HashSet<String>,
    stopped: bool,
}

type Shared = Arc<(Mutex<Queue>, Condvar, AtomicBool)>;

pub struct Runner {
    shared: Shared,
    workers: Vec<JoinHandle<()>>,
    pub done: Receiver<Done>,
}

impl Runner {
    pub fn new(results: PathBuf) -> Runner {
        let shared: Shared = Arc::default();
        let (sender, done) = mpsc::channel();
        // One core stays free for the editor and the screen.
        let count = std::thread::available_parallelism()
            .map_or(1, |cores| cores.get() - 1)
            .max(1);
        let mut workers = Vec::new();
        for _ in 0..count {
            let shared = shared.clone();
            let results = results.clone();
            let sender = sender.clone();
            workers.push(std::thread::spawn(move || {
                work(&shared, &results, &sender);
            }));
        }
        Runner {
            shared,
            workers,
            done,
        }
    }

    // The problem on screen goes first.
    pub fn add(&self, job: Job, first: bool) {
        let (queue, wake, _) = &*self.shared;
        let mut queue = queue.lock().unwrap();
        if first {
            queue.jobs.push_front(job);
        } else {
            queue.jobs.push_back(job);
        }
        wake.notify_all();
    }
}

// Samples run in their own process groups, which outlive cf unless they are killed here.
impl Drop for Runner {
    fn drop(&mut self) {
        let (queue, wake, stop) = &*self.shared;
        stop.store(true, Ordering::Relaxed);
        queue.lock().unwrap().stopped = true;
        wake.notify_all();
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn work(shared: &Shared, results: &Path, done: &Sender<Done>) {
    let (queue, wake, stop) = &**shared;
    loop {
        let job = {
            let mut queue = queue.lock().unwrap();
            loop {
                if queue.stopped {
                    return;
                }
                let free = queue
                    .jobs
                    .iter()
                    .position(|job| !queue.running.contains(&job.key));
                if let Some(at) = free {
                    let job = queue.jobs.remove(at).unwrap();
                    queue.running.insert(job.key.clone());
                    break job;
                }
                queue = wake.wait(queue).unwrap();
            }
        };

        let folder = job.paths.source.parent().map(Path::to_path_buf);
        let before = entries(folder.as_deref());
        let report = match run::check(&job.paths, &job.lang, &job.rules, stop) {
            Ok(report) => report,
            Err(error) => Report::Failed(format!("{error:#}")),
        };
        let strays = entries(folder.as_deref())
            .into_iter()
            .filter(|name| !before.contains(name) && *name != job.key)
            .collect();
        let stored = Stored { report, strays };
        {
            let mut queue = queue.lock().unwrap();
            queue.running.remove(&job.key);
            wake.notify_all();
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        // A failure of cf itself says nothing about the solution, so only memory keeps it.
        if !matches!(stored.report, Report::Failed(_)) {
            let _ = save(results, &job.hash, &stored);
        }
        let hash = job.hash;
        if done.send(Done { hash, stored }).is_err() {
            return;
        }
    }
}

fn entries(folder: Option<&Path>) -> Vec<String> {
    let Some(Ok(entries)) = folder.map(std::fs::read_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

// Covers everything a run depends on, so equal hashes mean equal results.
pub fn hash(paths: &Paths, lang: &Lang, rules: &Rules) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut part = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    part(lang.build.as_bytes());
    part(lang.run.as_bytes());
    part(rules.input.as_deref().unwrap_or_default().as_bytes());
    part(rules.output.as_deref().unwrap_or_default().as_bytes());
    part(&rules.time.unwrap_or_default().as_nanos().to_le_bytes());
    part(&std::fs::read(&paths.source)?);
    for k in run::samples(&paths.samples)? {
        part(&std::fs::read(paths.samples.join(format!("in{k}.txt")))?);
        part(&std::fs::read(paths.samples.join(format!("ans{k}.txt")))?);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn load(results: &Path, hash: &str) -> Option<Stored> {
    let json = std::fs::read_to_string(results.join(format!("{hash}.json"))).ok()?;
    serde_json::from_str(&json).ok()
}

pub fn forget(results: &Path, hash: &str) {
    let _ = std::fs::remove_file(results.join(format!("{hash}.json")));
}

// Results of contents no solution has anymore would pile up with every save.
pub fn prune(results: &Path, keep: &HashSet<&String>) {
    let Ok(entries) = std::fs::read_dir(results) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let hash = name.trim_end_matches(".json").to_string();
        if !keep.contains(&hash) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn save(results: &Path, hash: &str, stored: &Stored) -> Result<()> {
    std::fs::create_dir_all(results)?;
    std::fs::write(
        results.join(format!("{hash}.json")),
        serde_json::to_string(stored)?,
    )?;
    Ok(())
}
