//! What the palette's "info" panel says: where this build came from, stamped by CI when it
//! compiles (the `Build` step of `.forgejo/workflows/ci.yml`), and where it runs on this PC.

use chrono::{DateTime, Datelike, FixedOffset, NaiveDate, TimeDelta, Utc, Weekday};
use chrono_humanize::{Accuracy, HumanTime, Tense};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// What a line says when a local build has no stamp for it
const NOT_FROM_CI: &str = "development build, not from CI";
const NO_CHECKOUT: &str = "no checkout on this PC";

#[derive(Clone, Copy)]
struct Stamps {
    repo: Option<&'static str>,
    server: Option<&'static str>,
    version: Option<&'static str>,
    built: Option<&'static str>,
}

const STAMPS: Stamps = Stamps {
    repo: option_env!("GITHUB_REPOSITORY"),
    server: option_env!("GITHUB_SERVER_URL"),
    version: option_env!("SPT_BUILD_VERSION"),
    built: option_env!("SPT_BUILD_TIME"),
};

/// Read at startup: once an update moves the `current` junction, resolving the path again would
/// name the new release while the old one is still running.
static INSTALLED: LazyLock<String> =
    LazyLock::new(|| installed(std::env::current_exe(), |exe| std::fs::canonicalize(exe)));

/// Note where this process runs from, before anything can move it
pub fn read_installed_path() {
    LazyLock::force(&INSTALLED);
}

/// This build's version, as the info panel's "Version" line gives it
pub fn version() -> String {
    STAMPS
        .version
        .filter(|s| !s.is_empty())
        .unwrap_or(NOT_FROM_CI)
        .to_string()
}

/// The panel's lines, as label and value
pub fn lines() -> Vec<(&'static str, String)> {
    lines_from(STAMPS, &INSTALLED, Utc::now(), &|path| path.try_exists())
}

fn lines_from(
    stamps: Stamps,
    installed: &str,
    now: DateTime<Utc>,
    exists: &dyn Fn(&Path) -> std::io::Result<bool>,
) -> Vec<(&'static str, String)> {
    let repo = stamps.repo.filter(|s| !s.is_empty());
    let server = stamps.server.filter(|s| !s.is_empty());
    let version = stamps.version.filter(|s| !s.is_empty());
    let built = stamps.built.filter(|s| !s.is_empty());
    let or_not_from_ci = |value: Option<String>| value.unwrap_or_else(|| NOT_FROM_CI.to_string());
    vec![
        ("Repo", or_not_from_ci(repo.map(str::to_string))),
        (
            "Link",
            or_not_from_ci(
                repo.zip(server)
                    .map(|(repo, server)| format!("{}/{repo}", server.trim_end_matches('/'))),
            ),
        ),
        ("Version", or_not_from_ci(version.map(str::to_string))),
        ("Built", or_not_from_ci(built.map(|b| built_line(b, now)))),
        ("Installed", installed.to_string()),
        ("Checkout", checkout(repo, exists)),
    ]
}

fn built_line(stamp: &str, now: DateTime<Utc>) -> String {
    let Ok(built) = DateTime::parse_from_rfc3339(stamp) else {
        return format!("could not read the build time {stamp:?}");
    };
    let built = built.with_timezone(&Utc);
    format!(
        "{} Pacific ({})",
        pacific(built).format("%b %-d, %-I:%M %p"),
        elapsed(now - built)
    )
}

/// Daylight time runs from 2 AM on March's second Sunday to 2 AM on November's first
fn pacific(utc: DateTime<Utc>) -> DateTime<FixedOffset> {
    let sunday = |month, nth, hour| {
        NaiveDate::from_weekday_of_month_opt(utc.year(), month, Weekday::Sun, nth)
            .and_then(|date| date.and_hms_opt(hour, 0, 0))
            .map(|time| time.and_utc())
    };
    let daylight = sunday(3, 2, 10)
        .zip(sunday(11, 1, 9))
        .is_some_and(|(start, end)| (start..end).contains(&utc));
    let hours = if daylight { 7 } else { 8 };
    utc.with_timezone(&FixedOffset::west_opt(hours * 3600).expect("within a day of UTC"))
}

fn elapsed(since: TimeDelta) -> String {
    if since < -TimeDelta::minutes(1) {
        "this PC's clock is behind the build".to_string()
    } else if since < TimeDelta::minutes(1) {
        "just now".to_string()
    } else {
        HumanTime::from(since).to_text_en(Accuracy::Rough, Tense::Past)
    }
}

fn installed(
    exe: std::io::Result<PathBuf>,
    resolve: impl FnOnce(&Path) -> std::io::Result<PathBuf>,
) -> String {
    let exe = match exe {
        Ok(exe) => exe,
        Err(err) => return format!("could not read the executable's path: {err}"),
    };
    match resolve(&exe) {
        Ok(real) => without_verbatim_prefix(&real.display().to_string()),
        Err(err) => format!("{} (could not resolve its real path: {err})", exe.display()),
    }
}

/// Windows canonicalizes to `\\?\C:\...`, which is no path a person would type
fn without_verbatim_prefix(path: &str) -> String {
    if let Some(share) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{share}")
    } else {
        path.strip_prefix(r"\\?\").unwrap_or(path).to_string()
    }
}

fn checkout(repo: Option<&str>, exists: &dyn Fn(&Path) -> std::io::Result<bool>) -> String {
    let Some(name) = repo.and_then(|repo| repo.rsplit('/').next()) else {
        return format!("{NOT_FROM_CI}, so its repo isn't known");
    };
    let folder = format!(r"D:\repos\{name}");
    match exists(&Path::new(&folder).join(".git")) {
        Ok(true) => folder,
        Ok(false) => NO_CHECKOUT.to_string(),
        Err(err) => format!("could not check {folder}: {err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const STAMPED: Stamps = Stamps {
        repo: Some("nelsong6/spotify-player"),
        server: Some("https://forgejo.romaine.life"),
        version: Some("v0.25.1-109-gabcdef1"),
        built: Some("2026-11-02T23:14:00Z"),
    };
    const UNSTAMPED: Stamps = Stamps {
        repo: None,
        server: None,
        version: None,
        built: None,
    };

    fn at(time: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(time).unwrap().to_utc()
    }

    fn value(lines: &[(&'static str, String)], label: &str) -> String {
        let line = lines.iter().find(|(l, _)| *l == label);
        line.unwrap().1.clone()
    }

    fn lines_of(stamps: Stamps) -> Vec<(&'static str, String)> {
        lines_from(
            stamps,
            r"C:\spt\spotify_player.exe",
            at("2026-11-04T23:14:00Z"),
            &|_| Ok(true),
        )
    }

    #[test]
    fn a_stamped_build_names_where_it_came_from() {
        let lines = lines_of(STAMPED);
        let labels: Vec<_> = lines.iter().map(|(label, _)| *label).collect();
        assert_eq!(
            labels,
            ["Repo", "Link", "Version", "Built", "Installed", "Checkout"]
        );
        assert_eq!(value(&lines, "Repo"), "nelsong6/spotify-player");
        assert_eq!(
            value(&lines, "Link"),
            "https://forgejo.romaine.life/nelsong6/spotify-player"
        );
        assert_eq!(value(&lines, "Version"), "v0.25.1-109-gabcdef1");
        assert_eq!(
            value(&lines, "Built"),
            "Nov 2, 3:14 PM Pacific (2 days ago)"
        );
        assert_eq!(value(&lines, "Installed"), r"C:\spt\spotify_player.exe");
        assert_eq!(value(&lines, "Checkout"), r"D:\repos\spotify-player");
    }

    #[test]
    fn an_unstamped_build_says_so_on_every_stamped_line() {
        let lines = lines_of(UNSTAMPED);
        for label in ["Repo", "Link", "Version", "Built"] {
            assert_eq!(value(&lines, label), NOT_FROM_CI);
        }
        assert_eq!(value(&lines, "Installed"), r"C:\spt\spotify_player.exe");
        assert_eq!(
            value(&lines, "Checkout"),
            "development build, not from CI, so its repo isn't known"
        );
    }

    #[test]
    fn an_empty_stamp_counts_as_missing() {
        let lines = lines_of(Stamps {
            repo: Some(""),
            server: Some(""),
            version: Some(""),
            built: Some(""),
        });
        for label in ["Repo", "Link", "Version", "Built"] {
            assert_eq!(value(&lines, label), NOT_FROM_CI);
        }
    }

    #[test]
    fn a_link_needs_both_its_stamps_and_ignores_a_trailing_slash() {
        let lines = lines_of(Stamps {
            server: None,
            ..STAMPED
        });
        assert_eq!(value(&lines, "Link"), NOT_FROM_CI);
        let lines = lines_of(Stamps {
            server: Some("https://forgejo.romaine.life/"),
            ..STAMPED
        });
        assert_eq!(
            value(&lines, "Link"),
            "https://forgejo.romaine.life/nelsong6/spotify-player"
        );
    }

    #[test]
    fn an_unreadable_build_time_is_stated() {
        assert_eq!(
            built_line("yesterday", at("2026-11-04T23:14:00Z")),
            "could not read the build time \"yesterday\""
        );
    }

    #[test]
    fn pacific_time_follows_daylight_saving() {
        let shown = |time| pacific(at(time)).format("%b %-d, %-I:%M %p").to_string();
        // 2026: daylight time from March 8, 10:00 UTC to November 1, 09:00 UTC
        assert_eq!(shown("2026-03-08T09:59:00Z"), "Mar 8, 1:59 AM");
        assert_eq!(shown("2026-03-08T10:00:00Z"), "Mar 8, 3:00 AM");
        assert_eq!(shown("2026-11-01T08:59:00Z"), "Nov 1, 1:59 AM");
        assert_eq!(shown("2026-11-01T09:00:00Z"), "Nov 1, 1:00 AM");
        assert_eq!(shown("2027-01-01T00:30:00Z"), "Dec 31, 4:30 PM");
    }

    #[test]
    fn elapsed_time_reads_as_plain_language() {
        assert_eq!(elapsed(TimeDelta::seconds(20)), "just now");
        assert_eq!(elapsed(TimeDelta::minutes(5)), "5 minutes ago");
        assert_eq!(elapsed(TimeDelta::hours(3)), "3 hours ago");
        assert_eq!(elapsed(TimeDelta::days(2)), "2 days ago");
        assert_eq!(
            elapsed(TimeDelta::hours(-2)),
            "this PC's clock is behind the build"
        );
    }

    #[test]
    fn the_installed_line_is_the_real_path_or_says_what_failed() {
        let denied = || std::io::Error::other("denied");
        let exe = PathBuf::from(r"C:\at-home\spt\current\spt.exe");
        assert_eq!(
            installed(Ok(exe.clone()), |_| {
                Ok(PathBuf::from(r"\\?\C:\at-home\spt\v1\spt.exe"))
            }),
            r"C:\at-home\spt\v1\spt.exe"
        );
        assert_eq!(
            installed(Ok(exe.clone()), |_| Err(denied())),
            r"C:\at-home\spt\current\spt.exe (could not resolve its real path: denied)"
        );
        assert_eq!(
            installed(Err(denied()), |_| Ok(exe)),
            "could not read the executable's path: denied"
        );
    }

    #[test]
    fn a_share_keeps_its_two_leading_slashes() {
        assert_eq!(
            without_verbatim_prefix(r"\\?\UNC\NelsonNAS\tools\spt.exe"),
            r"\\NelsonNAS\tools\spt.exe"
        );
    }

    #[test]
    fn the_checkout_line_needs_a_dot_git() {
        let repo = Some("nelsong6/spotify-player");
        let asked = std::sync::Mutex::new(PathBuf::new());
        let found = checkout(repo, &|path| {
            *asked.lock().unwrap() = path.to_path_buf();
            Ok(true)
        });
        assert_eq!(found, r"D:\repos\spotify-player");
        assert!(asked.lock().unwrap().ends_with(".git"));

        assert_eq!(checkout(repo, &|_| Ok(false)), NO_CHECKOUT);
        assert_eq!(
            checkout(repo, &|_| Err(std::io::Error::other("drive not ready"))),
            r"could not check D:\repos\spotify-player: drive not ready"
        );
    }
}
