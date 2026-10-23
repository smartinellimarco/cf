use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ureq::Agent;

// The API allows one call every two seconds.
const API_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Deserialize, Clone, PartialEq, Eq)]
#[serde(transparent)]
pub struct Handle(pub String);

impl fmt::Display for Handle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct ProblemId {
    pub contest: u32,
    pub index: String,
}

impl fmt::Display for ProblemId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}{}", self.contest, self.index)
    }
}

impl FromStr for ProblemId {
    type Err = ();

    fn from_str(s: &str) -> Result<ProblemId, ()> {
        let Some(split) = s.find(|c: char| !c.is_ascii_digit()) else {
            return Err(());
        };
        let (contest, index) = s.split_at(split);
        let Ok(contest) = contest.parse() else {
            return Err(());
        };
        let mut chars = index.chars();
        let Some(letter) = chars.next() else {
            return Err(());
        };
        let rest = chars.as_str();
        if !letter.is_ascii_uppercase()
            || rest.len() > 1
            || !rest.chars().all(|c| c.is_ascii_digit())
        {
            return Err(());
        }
        Ok(ProblemId {
            contest,
            index: index.to_string(),
        })
    }
}

// Gym contests are numbered from 100000 and live under /gym/, usually with PDF statements.
const FIRST_GYM_CONTEST: u32 = 100_000;

impl ProblemId {
    pub fn is_gym(&self) -> bool {
        self.contest >= FIRST_GYM_CONTEST
    }

    pub fn url(&self, page: Page) -> String {
        let ProblemId { contest, index } = self;
        match page {
            Page::Problem => format!("https://codeforces.com/contest/{contest}/problem/{index}"),
            Page::Submit => format!(
                "https://codeforces.com/contest/{contest}/submit?submittedProblemIndex={index}"
            ),
        }
    }
}

pub enum Page {
    Problem,
    Submit,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    pub contest_id: Option<u32>,
    pub index: String,
    pub name: String,
    pub rating: Option<u32>,
}

impl Problem {
    pub fn id(&self) -> Option<ProblemId> {
        let contest = self.contest_id?;
        Some(ProblemId {
            contest,
            index: self.index.clone(),
        })
    }
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Submission {
    pub creation_time_seconds: i64,
    pub problem: Problem,
    pub verdict: Option<String>,
    #[serde(default)]
    pub passed_test_count: u32,
}

#[derive(Deserialize)]
struct Response<T> {
    status: String,
    comment: Option<String>,
    result: Option<T>,
}

#[derive(Deserialize)]
struct Problemset {
    problems: Vec<Problem>,
}

pub enum Refresh {
    Cached,
    Fetch,
}

#[derive(Clone)]
pub struct Client {
    agent: Agent,
    cache: PathBuf,
    last_call: Option<Instant>,
}

pub fn unreachable(error: &anyhow::Error) -> bool {
    error.is::<ureq::Error>()
}

pub fn now() -> i64 {
    let elapsed = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default();
    elapsed.as_secs() as i64
}

impl Client {
    pub fn new(cache: PathBuf) -> Result<Client> {
        std::fs::create_dir_all(&cache).with_context(|| format!("creating {}", cache.display()))?;
        // The API reports errors like an unknown handle in the body of a 4xx response.
        let agent = Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(20)))
            .http_status_as_error(false)
            .build()
            .into();
        Ok(Client {
            agent,
            cache,
            last_call: None,
        })
    }

    pub fn get(&self, url: &str) -> Result<String> {
        let (status, body) = self.fetch(url)?;
        if !(200..300).contains(&status) {
            bail!("fetching {url}: http status {status}");
        }
        Ok(body)
    }

    fn fetch(&self, url: &str) -> Result<(u16, String)> {
        // Without a browser user agent Cloudflare answers problem pages with a challenge.
        let mut response = self
            .agent
            .get(url)
            .header("User-Agent", "Mozilla/5.0")
            .call()
            .with_context(|| format!("fetching {url}"))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(64 << 20)
            .read_to_string()?;
        Ok((status, body))
    }

    fn api<T: DeserializeOwned>(&mut self, url: &str) -> Result<(T, String)> {
        if let Some(last_call) = self.last_call {
            std::thread::sleep(API_INTERVAL.saturating_sub(last_call.elapsed()));
        }
        self.last_call = Some(Instant::now());
        let (_, body) = self.fetch(url)?;
        let result = parse(&body)?;
        Ok((result, body))
    }

    fn cached<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let body = std::fs::read_to_string(self.cache.join(name)).ok()?;
        parse(&body).ok()
    }

    fn store(&self, name: &str, body: &str) -> Result<()> {
        let path = self.cache.join(name);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))
    }

    pub fn load<T: DeserializeOwned>(&self, name: &str) -> Option<T> {
        let json = std::fs::read_to_string(self.cache.join(name)).ok()?;
        serde_json::from_str(&json).ok()
    }

    pub fn save<T: Serialize>(&self, name: &str, value: &T) -> Result<()> {
        self.store(name, &serde_json::to_string(value)?)
    }

    pub fn problems(&mut self, refresh: Refresh) -> Result<Vec<Problem>> {
        if let Refresh::Cached = refresh
            && let Some(set) = self.cached::<Problemset>("problems.json")
        {
            return Ok(set.problems);
        }
        let (set, body) =
            self.api::<Problemset>("https://codeforces.com/api/problemset.problems")?;
        self.store("problems.json", &body)?;
        Ok(set.problems)
    }

    pub fn problems_age(&self) -> Duration {
        let modified =
            std::fs::metadata(self.cache.join("problems.json")).and_then(|m| m.modified());
        let Ok(modified) = modified else {
            return Duration::MAX;
        };
        modified.elapsed().unwrap_or_default()
    }

    pub fn submissions(&mut self, handle: &Handle) -> Result<Vec<Submission>> {
        let name = format!("status-{handle}.json");
        let url = format!("https://codeforces.com/api/user.status?handle={handle}");
        match self.api::<Vec<Submission>>(&url) {
            Ok((submissions, body)) => {
                self.store(&name, &body)?;
                Ok(submissions)
            }
            Err(error) => match self.cached(&name) {
                Some(submissions) => Ok(submissions),
                None => Err(error),
            },
        }
    }

    pub fn check_handle(&mut self, handle: &Handle) -> Result<()> {
        let url = format!("https://codeforces.com/api/user.info?handles={handle}");
        let _: Vec<serde::de::IgnoredAny> = self.api(&url)?.0;
        Ok(())
    }

    pub fn latest(&mut self, handle: &Handle) -> Result<Vec<Submission>> {
        let url = format!("https://codeforces.com/api/user.status?handle={handle}&count=5");
        let (submissions, _) = self.api(&url)?;
        Ok(submissions)
    }
}

fn parse<T: DeserializeOwned>(body: &str) -> Result<T> {
    let response: Response<T> =
        serde_json::from_str(body).context("unexpected Codeforces API response")?;
    let Response {
        status,
        comment,
        result,
    } = response;
    match (status.as_str(), result) {
        ("OK", Some(result)) => Ok(result),
        _ => bail!("Codeforces API: {}", comment.unwrap_or(status)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tells_a_dead_connection_from_an_api_error() {
        let mut client = Client::new(std::env::temp_dir()).unwrap();
        // Port 9 is discard, closed on any normal machine, so the connection is refused.
        let refused = client.api::<()>("http://127.0.0.1:9/").unwrap_err();
        assert!(unreachable(&refused));
        assert!(!unreachable(
            &parse::<()>(r#"{"status":"FAILED","comment":"x"}"#).unwrap_err()
        ));
    }
}
