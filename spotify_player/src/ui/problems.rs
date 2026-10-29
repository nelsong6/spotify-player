//! The row at the top of the screen that says what's wrong, while anything is

use super::{Frame, Line, Modifier, Rect, SharedState, Style};
use crate::problem::{Kind, Problem};
use std::time::Duration;

/// A request a person asked for shows once it has run this long
const SLOW_ACTION: Duration = Duration::from_secs(1);
/// How long each of several problems shows before the next
const TURN_SECS: i64 = 4;
/// A lasting problem says when it started once it has lasted this long
const SAY_SINCE_AFTER_MS: i64 = 60_000;

/// What's wrong now, the most urgent first: this PC's problems, the session's, and a request
/// still waiting for an answer
pub fn shown(state: &SharedState) -> Vec<String> {
    let now = chrono::Utc::now().timestamp_millis();
    let mut problems = crate::problem::current();
    #[cfg(feature = "cast")]
    problems.extend(
        crate::screen::pc::session_problems()
            .into_iter()
            .filter(|problem| problem.is_current(now)),
    );
    let mut shown: Vec<(Kind, String)> = problems
        .iter()
        .map(|problem| (problem.kind, describe(problem, now)))
        .collect();
    if let Some((action, running)) = state.requests.lock().slowest_action(SLOW_ACTION) {
        shown.push((
            Kind::Command,
            format!(
                "{}: waiting for an answer, {}",
                capitalized(action),
                duration(running)
            ),
        ));
    }
    shown.sort_by_key(|(kind, _)| *kind);
    shown.into_iter().map(|(_, text)| text).collect()
}

fn describe(problem: &Problem, now: i64) -> String {
    match problem.until_ms {
        Some(until) if problem.kind == Kind::Waiting => {
            let left = Duration::from_millis(u64::try_from(until - now).unwrap_or(0));
            format!("{}: {} left", problem.text, duration(left))
        }
        None if now - problem.since_ms >= SAY_SINCE_AFTER_MS => {
            match chrono::DateTime::from_timestamp_millis(problem.since_ms) {
                Some(since) => format!(
                    "{} (since {})",
                    problem.text,
                    since.with_timezone(&chrono::Local).format("%-I:%M %p")
                ),
                None => problem.text.clone(),
            }
        }
        _ => problem.text.clone(),
    }
}

fn capitalized(text: &str) -> String {
    let mut chars = text.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

fn duration(duration: Duration) -> String {
    let secs = duration.as_secs();
    match secs {
        0..60 => format!("{secs} s"),
        60..3600 => format!("{} min", secs.div_ceil(60)),
        _ => format!("{} h {} min", secs / 3600, secs % 3600 / 60),
    }
}

/// Draw `shown` across `rect`, one row. Several take turns, the most urgent first, numbered so
/// none goes unseen.
pub fn render(frame: &mut Frame, shown: &[String], rect: Rect) {
    if shown.is_empty() {
        return;
    }
    let turn = chrono::Utc::now().timestamp() / TURN_SECS;
    let index = usize::try_from(turn).unwrap_or(0) % shown.len();
    let text = match shown.len() {
        1 => format!(" ! {}", shown[0]),
        n => format!(" ! {}/{n} {}", index + 1, shown[index]),
    };
    let line = Line::from(text).style(
        Style::default()
            .fg(ratatui::style::Color::White)
            .bg(ratatui::style::Color::Red)
            .add_modifier(Modifier::BOLD),
    );
    frame.render_widget(line, rect);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wait_says_how_long_is_left_and_a_lasting_problem_since_when() {
        let now = 10_000_000;
        let waiting = Problem {
            kind: Kind::Waiting,
            text: "rate limit".to_string(),
            since_ms: now,
            until_ms: Some(now + 21_500),
        };
        assert_eq!(describe(&waiting, now), "rate limit: 21 s left");

        let lasting = Problem {
            kind: Kind::NoPlayer,
            text: "no player".to_string(),
            since_ms: now - 2 * SAY_SINCE_AFTER_MS,
            until_ms: None,
        };
        assert!(describe(&lasting, now).starts_with("no player (since "));

        let fresh = Problem {
            since_ms: now,
            ..lasting
        };
        assert_eq!(describe(&fresh, now), "no player");
    }

    #[test]
    fn durations_read_in_seconds_minutes_or_hours() {
        assert_eq!(duration(Duration::from_secs(5)), "5 s");
        assert_eq!(duration(Duration::from_secs(61)), "2 min");
        assert_eq!(duration(Duration::from_secs(68_401)), "19 h 0 min");
    }
}
