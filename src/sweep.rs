use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::codeforces;
use crate::config::{Lang, Paths};
use crate::run::{self, Outcome, Report};
use crate::statement::Files;

// `key` names the source file, like 4A.cpp, so two implementations of a problem never collide.
pub struct Job {
    pub key: String,
    pub paths: Paths,
    pub files: Files,
    pub lang: Lang,
}

pub struct Done {
    pub key: String,
    pub report: Report,
}

// What the samples last said about a problem, and for which inputs.
#[derive(Serialize, Deserialize)]
pub struct Stored {
    pub hash: String,
    pub passed: bool,
    pub at: i64,
}

#[derive(Default)]
struct Queue {
    jobs: VecDeque<Job>,
    running: HashMap<String, Arc<AtomicBool>>,
    stopped: bool,
}

type Shared = Arc<(Mutex<Queue>, Condvar)>;

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

    pub fn add(&self, jobs: Vec<Job>) {
        let (queue, wake) = &*self.shared;
        let mut queue = queue.lock().unwrap();
        queue.jobs.extend(jobs);
        wake.notify_all();
    }

    // The problem on screen jumps the queue, and a stale run of it is killed.
    pub fn first(&self, job: Job) {
        let (queue, wake) = &*self.shared;
        let mut queue = queue.lock().unwrap();
        if let Some(cancel) = queue.running.get(&job.key) {
            cancel.store(true, Ordering::Relaxed);
        }
        queue.jobs.retain(|queued| queued.key != job.key);
        queue.jobs.push_front(job);
        wake.notify_all();
    }
}

// Samples run in their own process groups, which outlive cf unless they are killed here.
impl Drop for Runner {
    fn drop(&mut self) {
        {
            let (queue, wake) = &*self.shared;
            let mut queue = queue.lock().unwrap();
            queue.stopped = true;
            for cancel in queue.running.values() {
                cancel.store(true, Ordering::Relaxed);
            }
            wake.notify_all();
        }
        for worker in self.workers.drain(..) {
            let _ = worker.join();
        }
    }
}

fn work(shared: &Shared, results: &Path, done: &Sender<Done>) {
    let (queue, wake) = &**shared;
    loop {
        let (job, cancel) = {
            let mut queue = queue.lock().unwrap();
            loop {
                if queue.stopped {
                    return;
                }
                let free = queue
                    .jobs
                    .iter()
                    .position(|job| !queue.running.contains_key(&job.key));
                if let Some(at) = free {
                    let job = queue.jobs.remove(at).unwrap();
                    let cancel = Arc::new(AtomicBool::new(false));
                    queue.running.insert(job.key.clone(), cancel.clone());
                    break (job, cancel);
                }
                queue = wake.wait(queue).unwrap();
            }
        };

        let hash = hash(&job.paths, &job.lang, &job.files);
        let report = match run::check(&job.paths, &job.lang, &job.files, &cancel) {
            Ok(report) => report,
            Err(error) => Report::Failed(format!("{error:#}")),
        };
        {
            let mut queue = queue.lock().unwrap();
            queue.running.remove(&job.key);
            wake.notify_all();
        }
        if cancel.load(Ordering::Relaxed) {
            continue;
        }
        if let (Ok(hash), Some(passed)) = (hash, passed(&report)) {
            let at = codeforces::now();
            let _ = save(results, &job.key, &Stored { hash, passed, at });
        }
        let _ = done.send(Done {
            key: job.key,
            report,
        });
    }
}

// None when the samples say nothing: no samples to run, or cf itself failed.
pub fn passed(report: &Report) -> Option<bool> {
    match report {
        Report::Ran(outcomes) => Some(
            outcomes
                .iter()
                .all(|(_, outcome)| matches!(outcome, Outcome::Pass { .. })),
        ),
        Report::BuildFailed(_) => Some(false),
        Report::Building | Report::Failed(_) | Report::NoSamples => None,
    }
}

// Covers everything a run depends on, so an unchanged hash means the old result still holds.
pub fn hash(paths: &Paths, lang: &Lang, files: &Files) -> Result<String> {
    let mut hasher = blake3::Hasher::new();
    let mut part = |bytes: &[u8]| {
        hasher.update(&(bytes.len() as u64).to_le_bytes());
        hasher.update(bytes);
    };
    part(lang.build.as_bytes());
    part(lang.run.as_bytes());
    part(files.input.as_deref().unwrap_or_default().as_bytes());
    part(files.output.as_deref().unwrap_or_default().as_bytes());
    part(&std::fs::read(&paths.source)?);
    for k in run::samples(&paths.samples)? {
        part(&std::fs::read(paths.samples.join(format!("in{k}.txt")))?);
        part(&std::fs::read(paths.samples.join(format!("ans{k}.txt")))?);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

pub fn load(results: &Path, key: &str) -> Option<Stored> {
    let json = std::fs::read_to_string(results.join(format!("{key}.json"))).ok()?;
    serde_json::from_str(&json).ok()
}

fn save(results: &Path, key: &str, result: &Stored) -> Result<()> {
    std::fs::create_dir_all(results)?;
    std::fs::write(
        results.join(format!("{key}.json")),
        serde_json::to_string(result)?,
    )?;
    Ok(())
}
