//! One temporal grammar for both memory layers (0.8.7).
//!
//! Curated notes and recorded messages are different registers with different
//! scoring scales — the 0.8.1 register lesson says never blend their numbers.
//! Their *clocks* are the same clock, though, so the way a caller says "since
//! last week" must not depend on which layer it is asking. This module owns
//! that grammar and nothing else: it turns a human time expression into a
//! [`TimeWindow`], and names the orderings a caller may ask for.
//!
//! The daemon resolves relative expressions rather than making the assistant
//! do date arithmetic — "last week" is a handle the model pulls, not a
//! calculation it performs, and a model that miscounts a month silently
//! returns the wrong decade of a project. That is mechanism, not intelligence:
//! resolution is deterministic, clock-anchored, and identical for every
//! caller.
//!
//! Every relative expression resolves to a single INSTANT — the start of the
//! span it names. So `after: "last week"` reads "since one week ago", which is
//! what a person asking for last week's work means; a caller wanting the
//! bounded calendar week passes both `after` and `before`.

use serde::{Deserialize, Serialize};

const MINUTE: i64 = 60;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
const WEEK: i64 = 7 * DAY;

/// A half-open time window: `after` is INCLUSIVE, `before` is EXCLUSIVE.
///
/// Half-open is what makes adjacent windows tile without overlap — two
/// version windows fitted back to back (see `Engine::version_window`) must not
/// both claim the switch instant, or a note written at the boundary shows up
/// in both releases.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeWindow {
    pub after: Option<i64>,
    pub before: Option<i64>,
}

impl TimeWindow {
    /// No bounds — the whole timeline. Filtering by an open window is a no-op,
    /// which is what keeps every existing caller on the unfiltered fast path.
    pub fn is_open(&self) -> bool {
        self.after.is_none() && self.before.is_none()
    }

    /// Does `ts` fall inside? Always true for an open window.
    pub fn contains(&self, ts: i64) -> bool {
        self.after.is_none_or(|a| ts >= a) && self.before.is_none_or(|b| ts < b)
    }

    /// Intersect two windows — the tighter bound on each side wins. Used when
    /// a caller passes `during_version` AND an explicit `after`/`before`: the
    /// explicit bound narrows the release window rather than replacing it.
    pub fn intersect(self, other: Self) -> Self {
        Self {
            after: match (self.after, other.after) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            },
            before: match (self.before, other.before) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            },
        }
    }

    /// True when the bounds cross — an empty window. Worth reporting to the
    /// caller as such: silently returning nothing looks identical to "the
    /// graph is silent", which is the one verdict this project refuses to
    /// fake.
    pub fn is_empty(&self) -> bool {
        matches!((self.after, self.before), (Some(a), Some(b)) if a >= b)
    }
}

/// How a caller wants results ordered. Relevance is the default everywhere;
/// the other two are explicit handles, never inferred from the query text.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SearchOrder {
    /// Score order — the calibrated ranking every verdict is defined against.
    #[default]
    Relevance,
    /// Oldest first. "What came first" is a question about the timeline, not
    /// about relevance, and answering it by reading a score-ordered list is
    /// how a reader invents a sequence that was never there.
    Chronological,
    /// Newest first — the update-question handle: "what is the CURRENT value
    /// of X" wants the latest statement, not the best-matching one.
    ///
    /// Note the asymmetry this sits on, which predates it: CURATED hybrid
    /// search already carries an implicit recency tilt (a multiplicative
    /// bonus, +15% ceiling, 30-day half-life — `policy::SEARCH_RECENCY_BOOST`,
    /// there to break near-ties toward current knowledge), while HISTORY
    /// search carries none, scoring purely on distance and the cross-encoder.
    /// So recency is expressed twice in one layer and once in the other, at
    /// different strengths. This ordering is the explicit handle in both;
    /// unifying the implicit tilt would change default curated ranking, which
    /// needs a measured run before it may ship.
    Recent,
}

impl SearchOrder {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "relevance" | "score" | "default" => Some(Self::Relevance),
            "chronological" | "oldest" | "asc" => Some(Self::Chronological),
            "recent" | "newest" | "desc" => Some(Self::Recent),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Relevance => "relevance",
            Self::Chronological => "chronological",
            Self::Recent => "recent",
        }
    }

    /// Does this ordering discard the score ranking? Both time orderings do,
    /// which is why the caller is told the verdict still reads the *scores* —
    /// re-sorting a delivered set never changes what cleared the line.
    pub fn is_temporal(self) -> bool {
        !matches!(self, Self::Relevance)
    }
}

/// Parse one time expression into unix seconds, anchored at `now_ts`.
///
/// Accepted, in the order tried:
/// - a full ISO-8601 instant (`2026-08-14T09:15:32Z`, offsets and fractional
///   seconds included) — exact to the second;
/// - a bare day (`2026-08-14`) or raw unix seconds — midnight UTC for a day;
/// - a relative expression (`now`, `today`, `yesterday`, `last week`,
///   `last 3 days`, `2 hours ago`, `a month ago`, `this year`).
///
/// Returns `None` for anything it does not recognise — callers surface that as
/// an error naming the accepted shapes rather than silently ignoring a bound,
/// because a dropped bound turns a scoped question into an unscoped answer.
pub fn parse_instant(spec: &str, now_ts: i64) -> Option<i64> {
    let t = spec.trim();
    if t.is_empty() {
        return None;
    }
    if let Some(ts) = crate::harvest::parse_iso8601(t) {
        return Some(ts);
    }
    if let Some(ts) = crate::store::parse_day(t) {
        return Some(ts);
    }
    parse_relative(&t.to_ascii_lowercase(), now_ts)
}

fn parse_relative(t: &str, now_ts: i64) -> Option<i64> {
    match t {
        "now" => return Some(now_ts),
        "today" => return Some(start_of(Unit::Day, now_ts)),
        "yesterday" => return Some(start_of(Unit::Day, now_ts) - DAY),
        _ => {}
    }
    let words: Vec<&str> = t.split_whitespace().collect();
    match words.as_slice() {
        // "this week" / "this month" — the start of the calendar period.
        ["this", unit] => Some(start_of(Unit::parse(unit)?, now_ts)),
        // "last week" / "past month" — one unit back.
        ["last" | "past" | "previous", unit] => shift_back(now_ts, 1, Unit::parse(unit)?),
        // "last 3 days" / "past 2 weeks".
        ["last" | "past" | "previous", n, unit] => {
            shift_back(now_ts, n.parse().ok()?, Unit::parse(unit)?)
        }
        // "a week ago" / "an hour ago" — before the numeric arm, which would
        // otherwise match the same shape and then fail to parse "a" as a count.
        ["a" | "an", unit, "ago"] => shift_back(now_ts, 1, Unit::parse(unit)?),
        // "3 days ago".
        [n, unit, "ago"] => shift_back(now_ts, n.parse().ok()?, Unit::parse(unit)?),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Unit {
    Second,
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Unit {
    fn parse(w: &str) -> Option<Self> {
        Some(match w.trim_end_matches('s') {
            "second" | "sec" => Self::Second,
            "minute" | "min" => Self::Minute,
            "hour" | "hr" => Self::Hour,
            "day" => Self::Day,
            "week" | "wk" => Self::Week,
            "month" | "mon" => Self::Month,
            "year" | "yr" => Self::Year,
            _ => return None,
        })
    }
}

/// Shift back by `n` units. Months and years use CALENDAR arithmetic — "3
/// months ago" from the 31st lands on the last valid day of that month, not
/// 90 fixed days earlier, because a project's release cadence is calendar-
/// shaped and an approximate month quietly misses a whole cycle.
fn shift_back(now_ts: i64, n: i64, unit: Unit) -> Option<i64> {
    if n < 0 {
        return None;
    }
    let fixed = match unit {
        Unit::Second => 1,
        Unit::Minute => MINUTE,
        Unit::Hour => HOUR,
        Unit::Day => DAY,
        Unit::Week => WEEK,
        Unit::Month | Unit::Year => 0,
    };
    if fixed > 0 {
        return n.checked_mul(fixed).map(|d| now_ts - d);
    }
    let months = if unit == Unit::Year { n * 12 } else { n };
    let secs_into_day = now_ts.rem_euclid(DAY);
    let (y, m, d) = civil_from_days(now_ts.div_euclid(DAY));
    let total = y * 12 + (m - 1) - months;
    let (ny, nm) = (total.div_euclid(12), total.rem_euclid(12) + 1);
    let nd = d.min(days_in_month(ny, nm));
    Some(days_from_civil(ny, nm, nd) * DAY + secs_into_day)
}

/// The start of the calendar period `now_ts` sits in. Weeks start Monday —
/// the ISO convention, and the one a working week matches.
fn start_of(unit: Unit, now_ts: i64) -> i64 {
    let days = now_ts.div_euclid(DAY);
    match unit {
        Unit::Second => now_ts,
        Unit::Minute => now_ts - now_ts.rem_euclid(MINUTE),
        Unit::Hour => now_ts - now_ts.rem_euclid(HOUR),
        Unit::Day => days * DAY,
        // 1970-01-01 was a Thursday, so (days + 3) mod 7 counts from Monday.
        Unit::Week => (days - (days + 3).rem_euclid(7)) * DAY,
        Unit::Month => {
            let (y, m, _) = civil_from_days(days);
            days_from_civil(y, m, 1) * DAY
        }
        Unit::Year => {
            let (y, _, _) = civil_from_days(days);
            days_from_civil(y, 1, 1) * DAY
        }
    }
}

fn days_in_month(y: i64, m: i64) -> i64 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        _ => 28,
    }
}

/// Days-from-civil (Howard Hinnant's algorithm) — the same one `parse_day`
/// inlines, kept here as a named function because the inverse below needs a
/// partner to round-trip against.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y2 = if m <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// Civil-from-days — Hinnant's inverse, needed for calendar-correct month and
/// year shifts.
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Build a window from two optional expressions, anchored at `now_ts`.
/// An unparseable bound is an error naming the offender — never a dropped
/// filter.
pub fn window(after: Option<&str>, before: Option<&str>, now_ts: i64) -> crate::Result<TimeWindow> {
    let one = |spec: Option<&str>, field: &str| -> crate::Result<Option<i64>> {
        let Some(spec) = spec.map(str::trim).filter(|s| !s.is_empty()) else {
            return Ok(None);
        };
        parse_instant(spec, now_ts).map(Some).ok_or_else(|| {
            crate::Error::Config(format!(
                "{field} {spec:?} is not a time I can read — use a day \
                 (2026-08-14), an ISO instant (2026-08-14T09:15:32Z), or a \
                 relative expression (today, yesterday, last week, last 3 \
                 days, 2 hours ago, a month ago, this year)"
            ))
        })
    };
    let w = TimeWindow {
        after: one(after, "after")?,
        before: one(before, "before")?,
    };
    if w.is_empty() {
        return Err(crate::Error::Config(format!(
            "the window is empty: after ({}) is not before before ({})",
            after.unwrap_or_default(),
            before.unwrap_or_default()
        )));
    }
    Ok(w)
}
