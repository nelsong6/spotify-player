//! What's wrong right now, so the screen can say so. The session and each PC keep their own list;
//! the session's goes to every PC over the shared screen, and a PC shows both. A problem stays
//! until the code that found it sees it fixed, or until its deadline passes.

use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::sync::watch;

/// Kinds of problem, the most urgent first: the screen shows the first one
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A PC can't reach the session's screen
    Screen,
    /// The session has no Spotify player, so nothing can play
    NoPlayer,
    /// A PC gets no audio from the session
    Audio,
    /// A PC can't open a speaker
    Output,
    /// The radio stopped on its own
    RadioStopped,
    /// Something pressed on a PC failed
    Command,
    /// The session can't renew its Spotify sign-in
    SignIn,
    /// The radio's saved place couldn't be read
    SavedSession,
    /// The session can't keep a kept playlist
    Keeper,
    /// The session can't hear what Spotify plays elsewhere
    Follow,
    /// The session can't keep the play history
    History,
    /// The session waits on Spotify's rate limit
    Waiting,
    /// A kind from a newer release
    #[serde(other)]
    Other,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub kind: Kind,
    pub text: String,
    /// When this kind of problem started, in milliseconds since the Unix epoch
    pub since_ms: i64,
    /// When it stops being shown, for a problem that passes by itself
    #[serde(default)]
    pub until_ms: Option<i64>,
}

impl Problem {
    pub fn is_current(&self, now_ms: i64) -> bool {
        self.until_ms.is_none_or(|until| until > now_ms)
    }
}

fn list() -> &'static watch::Sender<Vec<Problem>> {
    static LIST: OnceLock<watch::Sender<Vec<Problem>>> = OnceLock::new();
    LIST.get_or_init(|| watch::channel(Vec::new()).0)
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn put(kind: Kind, text: String, until_ms: Option<i64>) {
    let now = now_ms();
    list().send_if_modified(|problems| {
        let existing = problems.iter_mut().find(|problem| problem.kind == kind);
        match existing {
            Some(problem)
                if problem.is_current(now)
                    && problem.text == text
                    && problem.until_ms == until_ms =>
            {
                false
            }
            Some(problem) => {
                if !problem.is_current(now) {
                    problem.since_ms = now;
                }
                problem.text = text;
                problem.until_ms = until_ms;
                true
            }
            None => {
                problems.push(Problem {
                    kind,
                    text,
                    since_ms: now,
                    until_ms,
                });
                problems.sort_by_key(|problem| problem.kind);
                true
            }
        }
    });
}

/// Report a problem that lasts until [`clear`]ed
pub fn set(kind: Kind, text: impl Into<String>) {
    put(kind, text.into(), None);
}

/// Report a problem that passes by itself after `duration`
pub fn show_for(kind: Kind, text: impl Into<String>, duration: Duration) {
    put(kind, text.into(), Some(now_ms() + millis(duration)));
}

/// Report a wait that ends after `duration`, keeping a longer one already reported
pub fn wait(kind: Kind, text: impl Into<String>, duration: Duration) {
    let until = now_ms() + millis(duration);
    let longer = list()
        .borrow()
        .iter()
        .find(|problem| problem.kind == kind)
        .and_then(|problem| problem.until_ms)
        .is_some_and(|current| current > until);
    if !longer {
        put(kind, text.into(), Some(until));
    }
}

pub fn clear(kind: Kind) {
    list().send_if_modified(|problems| {
        let before = problems.len();
        problems.retain(|problem| problem.kind != kind);
        problems.len() != before
    });
}

/// The text of a current problem of `kind`
pub fn get(kind: Kind) -> Option<String> {
    let now = now_ms();
    list()
        .borrow()
        .iter()
        .find(|problem| problem.kind == kind && problem.is_current(now))
        .map(|problem| problem.text.clone())
}

/// This process's current problems, the most urgent first
pub fn current() -> Vec<Problem> {
    let now = now_ms();
    list()
        .borrow()
        .iter()
        .filter(|problem| problem.is_current(now))
        .cloned()
        .collect()
}

/// This process's problems, woken on each change
#[cfg_attr(not(feature = "streaming"), allow(dead_code))]
pub fn subscribe() -> watch::Receiver<Vec<Problem>> {
    list().subscribe()
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX / 2)
}

/// An error's last cause, which says what went wrong without the steps that led to it
pub fn reason(err: &anyhow::Error) -> String {
    err.root_cause().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // the list is the process's, shared with other tests, so one test walks through kinds no
    // other test reports
    fn mine() -> Vec<Problem> {
        current()
            .into_iter()
            .filter(|problem| {
                [Kind::NoPlayer, Kind::Keeper, Kind::Other, Kind::Follow].contains(&problem.kind)
            })
            .collect()
    }

    #[test]
    fn problems_are_kept_sorted_and_pass_or_clear() {
        set(Kind::Keeper, "can't refill Bike");
        set(Kind::NoPlayer, "no player");
        let kinds: Vec<Kind> = mine().iter().map(|problem| problem.kind).collect();
        assert_eq!(kinds, [Kind::NoPlayer, Kind::Keeper]);

        // the same problem reported again keeps when it started
        let since = mine()[0].since_ms;
        std::thread::sleep(Duration::from_millis(5));
        set(Kind::NoPlayer, "no player");
        assert_eq!(mine()[0].since_ms, since);

        // a longer wait isn't shortened by a shorter one
        wait(Kind::Other, "rate limit", Duration::from_secs(60));
        let until = get_until(Kind::Other);
        wait(Kind::Other, "rate limit", Duration::from_secs(1));
        assert_eq!(get_until(Kind::Other), until);

        show_for(Kind::Follow, "play failed", Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert_eq!(get(Kind::Follow), None);

        clear(Kind::NoPlayer);
        clear(Kind::Keeper);
        clear(Kind::Other);
        clear(Kind::Follow);
        assert!(mine().is_empty());
    }

    fn get_until(kind: Kind) -> Option<i64> {
        mine()
            .into_iter()
            .find(|problem| problem.kind == kind)
            .and_then(|problem| problem.until_ms)
    }

    #[test]
    fn a_kind_from_a_newer_release_still_parses() {
        let problem: Problem =
            serde_json::from_str(r#"{"kind":"something_new","text":"x","since_ms":0}"#).unwrap();
        assert_eq!(problem.kind, Kind::Other);
    }
}
