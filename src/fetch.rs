use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};

use anyhow::Result;

use crate::codeforces::{self, Client, Page, ProblemId};
use crate::statement::{self, Kind, Statement};

// Codeforces turns away bursts of requests, so a failed download waits and tries again.
const ATTEMPTS: u32 = 3;

pub struct Fetched {
    pub id: ProblemId,
    pub statement: Result<Statement, String>,
}

type Shared = Arc<(Mutex<VecDeque<ProblemId>>, Condvar)>;

// Downloads statements and samples off the screen's thread, so moving between problems never waits.
pub struct Fetcher {
    shared: Shared,
    pub done: Receiver<Fetched>,
}

impl Fetcher {
    pub fn new(client: Client, samples: PathBuf) -> Fetcher {
        let shared: Shared = Arc::default();
        let (sender, done) = mpsc::channel();
        let queue = shared.clone();
        std::thread::spawn(move || work(&queue, client, &samples, &sender));
        Fetcher { shared, done }
    }

    pub fn add(&self, id: ProblemId) {
        let (queue, wake) = &*self.shared;
        let mut queue = queue.lock().unwrap();
        if !queue.contains(&id) {
            queue.push_back(id);
            wake.notify_all();
        }
    }

    // The problem on screen goes first; the ones passed by keep their place behind it.
    pub fn first(&self, id: ProblemId) {
        let (queue, wake) = &*self.shared;
        let mut queue = queue.lock().unwrap();
        queue.retain(|queued| *queued != id);
        queue.push_front(id);
        wake.notify_all();
    }
}

fn work(shared: &Shared, mut client: Client, samples: &Path, done: &Sender<Fetched>) {
    let (queue, wake) = &**shared;
    loop {
        let id = {
            let mut queue = queue.lock().unwrap();
            loop {
                if let Some(id) = queue.pop_front() {
                    break id;
                }
                queue = wake.wait(queue).unwrap();
            }
        };
        let statement = fetch(&mut client, samples, &id).map_err(|error| format!("{error:#}"));
        if done.send(Fetched { id, statement }).is_err() {
            return;
        }
    }
}

fn fetch(client: &mut Client, samples: &Path, id: &ProblemId) -> Result<Statement> {
    let url = id.url(Page::Problem);
    let mut attempt = 1;
    let statement = loop {
        match client.get(&url).and_then(|page| statement::parse(&page)) {
            Ok(statement) => break statement,
            Err(_) if attempt < ATTEMPTS => {
                attempt += 1;
                std::thread::sleep(codeforces::API_INTERVAL);
            }
            Err(error) => return Err(error),
        }
    };
    let dir = samples.join(id.to_string());
    // Samples already there may hold your own cases, so they are written only once.
    if !dir.is_dir() {
        std::fs::create_dir_all(&dir)?;
        if let Kind::Standard = statement.kind {
            for (k, sample) in statement.samples.iter().enumerate() {
                std::fs::write(dir.join(format!("in{}.txt", k + 1)), &sample.input)?;
                std::fs::write(dir.join(format!("ans{}.txt", k + 1)), &sample.answer)?;
            }
        }
    }
    client.save(&format!("statements/{id}.json"), &statement)?;
    Ok(statement)
}
