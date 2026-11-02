//! Dotify (workplace clock-in / clock-out) wire types and the pure rules the
//! timer pill follows: token clean-up, the `{message, data}` envelope, UTC
//! timestamps, and how a fresh `check-status` reconciles with what the pill
//! shows. No I/O; the engine does the HTTP (`docs/dotify-api-reference.md`).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The Dotify API. Keel talks to production only.
pub const PROD_BASE: &str = "https://api.dotify.dot.co.id/api/v1";
/// The retired development API: a connection saved against it (from when
/// Settings had a Production / Development switch) now goes to production.
const RETIRED_DEV_BASE: &str = "https://api.dotify-dev.dot.co.id/api/v1";

/// Where to send calls for a connection stored with base `stored`: the
/// retired development API resolves to production; anything else (in
/// tests, a mock server) as stored.
pub fn resolve_base(stored: &str) -> &str {
    if stored.trim_end_matches('/').eq_ignore_ascii_case(RETIRED_DEV_BASE) { PROD_BASE } else { stored }
}

/// A pasted token, with or without `Bearer `, quotes or whitespace.
pub fn normalize_token(raw: &str) -> String {
    let t = raw.trim().trim_matches(|c| c == '"' || c == '\'').trim();
    let t = match t.get(..7) {
        Some(head) if head.eq_ignore_ascii_case("bearer ") => t[7..].trim(),
        _ => t,
    };
    t.to_string()
}

/// Name and email a Dotify JWT carries (payload `name`, `email`), without
/// checking its signature: only for showing who is connected.
pub fn token_identity(token: &str) -> Option<(String, String)> {
    let payload = token.split('.').nth(1)?;
    let bytes = base64url_decode(payload)?;
    let v: Value = serde_json::from_slice(&bytes).ok()?;
    let email = v["email"].as_str().unwrap_or_default().to_string();
    let name = v["name"].as_str().map(str::to_string).unwrap_or_else(|| email.clone());
    (!name.is_empty() || !email.is_empty()).then_some((name, email))
}

/// Seconds since the epoch when the token expires (`exp`), if it says.
pub fn token_expiry(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    let v: Value = serde_json::from_slice(&base64url_decode(payload)?).ok()?;
    v["exp"].as_i64()
}

fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for ch in s.bytes() {
        let v = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'-' | b'+' => 62,
            b'_' | b'/' => 63,
            b'=' => break,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// The `data` of a `{message, data}` answer (the whole body when it has no
/// envelope, like `/health`).
pub fn envelope_data(body: &Value) -> &Value {
    match body.get("data") {
        Some(data) if body.get("message").is_some() => data,
        _ => body,
    }
}

/// The `message` of an error answer: a string, or the first of an array
/// (validation errors), else `fallback`.
pub fn envelope_message(body: &Value, fallback: &str) -> String {
    match &body["message"] {
        Value::String(s) if !s.is_empty() => s.clone(),
        Value::Array(items) => items.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; "),
        _ => fallback.to_string(),
    }
}

/// How a call failed, as the UI needs to tell them apart. Sent over RPC as
/// `"<kind>: <text>"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DotifyError {
    /// 401: the token is expired or revoked.
    Auth(String),
    /// Couldn't reach the server (offline, DNS, timeout).
    Offline(String),
    /// 400 / 403 / 404 / 409 / 422: the server said no (confirmed period, …).
    Rejected(String),
    /// 5xx or an answer that doesn't parse.
    Server(String),
}

impl DotifyError {
    pub fn to_wire(&self) -> String {
        match self {
            DotifyError::Auth(m) => format!("auth: {m}"),
            DotifyError::Offline(m) => format!("offline: {m}"),
            DotifyError::Rejected(m) => format!("rejected: {m}"),
            DotifyError::Server(m) => format!("server: {m}"),
        }
    }

    /// Back from an RPC error message (which may carry a prefix of its own).
    pub fn from_wire(text: &str) -> Self {
        for (tag, make) in [
            ("auth: ", DotifyError::Auth as fn(String) -> DotifyError),
            ("offline: ", DotifyError::Offline),
            ("rejected: ", DotifyError::Rejected),
            ("server: ", DotifyError::Server),
        ] {
            if let Some(at) = text.find(tag) {
                return make(text[at + tag.len()..].to_string());
            }
        }
        DotifyError::Server(text.to_string())
    }

    /// From an HTTP status and body.
    pub fn from_status(code: u16, body: &Value) -> Self {
        match code {
            401 => DotifyError::Auth(envelope_message(body, "Dotify token expired")),
            400 | 403 | 404 | 409 | 422 => DotifyError::Rejected(envelope_message(body, &format!("Dotify answered {code}"))),
            _ => DotifyError::Server(envelope_message(body, &format!("Dotify answered {code}"))),
        }
    }

    pub fn text(&self) -> &str {
        match self {
            DotifyError::Auth(m) | DotifyError::Offline(m) | DotifyError::Rejected(m) | DotifyError::Server(m) => m,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Project {
    pub id: i64,
    pub name: String,
    /// `#RRGGBB`, when Dotify sends one (it doesn't today).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// A short project code, when Dotify sends one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
}

impl Project {
    pub fn new(id: i64, name: impl Into<String>) -> Self {
        Project { id, name: name.into(), color: None, code: None }
    }

    /// From a project object (`id`, `name`, optional `color` / `code`).
    pub fn parse(p: &Value) -> Option<Self> {
        let text = |k: &str| p[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        Some(Project {
            id: p["id"].as_i64()?,
            name: p["name"].as_str().unwrap_or_default().to_string(),
            color: text("color").filter(|c| parse_hex_color(c).is_some()),
            code: text("code"),
        })
    }
}

/// `#RGB` / `#RRGGBB` → `0xRRGGBBFF`.
pub fn parse_hex_color(s: &str) -> Option<u32> {
    let h = s.trim().strip_prefix('#')?;
    let full = match h.len() {
        3 => h.chars().flat_map(|c| [c, c]).collect::<String>(),
        6 => h.to_string(),
        _ => return None,
    };
    u32::from_str_radix(&full, 16).ok().map(|v| (v << 8) | 0xFF)
}

/// A time entry (`time_entity`). Times are epoch ms, parsed from UTC.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Entry {
    pub id: i64,
    pub description: String,
    pub project: Option<Project>,
    pub start_ms: i64,
    pub end_ms: Option<i64>,
    /// Hours, as the server counted them (0 while running).
    pub duration_h: f64,
    pub status: String,
    /// `attendance_confirmation` present (or its id): the period is locked.
    pub locked: bool,
}

impl Entry {
    pub fn parse(v: &Value) -> Option<Self> {
        let start_ms = parse_utc_ms(v["start_date"].as_str()?)?;
        let project = Project::parse(&v["project"]);
        let locked = v["attendance_confirmation"]["status"].as_str() == Some("CONFIRMED");
        Some(Entry {
            id: v["id"].as_i64()?,
            description: v["description"].as_str().unwrap_or_default().to_string(),
            project,
            start_ms,
            end_ms: v["end_date"].as_str().and_then(parse_utc_ms),
            duration_h: v["duration"].as_f64().unwrap_or(0.),
            status: v["status"].as_str().unwrap_or_default().to_string(),
            locked,
        })
    }

    /// Elapsed ms at `now` (to its end once stopped).
    pub fn elapsed_ms(&self, now: i64) -> i64 {
        (self.end_ms.unwrap_or(now) - self.start_ms).max(0)
    }
}

/// `GET /tracker/check-status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Status {
    pub running: Option<Entry>,
    pub overtime: bool,
}

impl Status {
    pub fn parse(data: &Value) -> Self {
        let running = if data["is_start_timer"].as_bool().unwrap_or(false) {
            Entry::parse(&data["time_entity"])
        } else {
            None
        };
        Status { running, overtime: data["is_over_time"].as_bool().unwrap_or(false) }
    }
}

/// `GET /time-entities/summary`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub struct Summary {
    pub week_h: f64,
    pub week_normal_h: f64,
    pub period_h: f64,
    pub period_normal_h: f64,
}

impl Summary {
    pub fn parse(data: &Value) -> Self {
        let f = |k: &str| data[k].as_f64().unwrap_or(0.);
        Summary {
            week_h: f("employee_week_hours"),
            week_normal_h: f("this_week_normal_hours"),
            period_h: f("employee_period_hours"),
            period_normal_h: f("this_period_normal_hours"),
        }
    }
}

pub fn parse_projects(data: &Value) -> Vec<Project> {
    data.as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(Project::parse)
                .collect()
        })
        .unwrap_or_default()
}

/// `GET /time-entities`: weeks of entries, flattened, newest first.
pub fn parse_entries(data: &Value) -> Vec<Entry> {
    let mut out: Vec<Entry> = data
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|week| week["time_entities"].as_array().cloned().unwrap_or_default())
        .filter_map(|e| Entry::parse(&e))
        .collect();
    out.sort_by_key(|e| std::cmp::Reverse(e.start_ms));
    out
}

/// Hours a day's entries add up to, for the local day starting at
/// `day_start_ms` (a running entry counts up to `now`).
pub fn hours_on(entries: &[Entry], day_start_ms: i64, now: i64) -> f64 {
    let day_end = day_start_ms + 86_400_000;
    let ms: i64 = entries
        .iter()
        .map(|e| {
            let (s, t) = (e.start_ms.max(day_start_ms), e.end_ms.unwrap_or(now).min(day_end));
            (t - s).max(0)
        })
        .sum();
    ms as f64 / 3_600_000.
}

// ── Time ─────────────────────────────────────────────────────────────────

/// `2026-10-08T02:06:41.000Z` (or with `+07:00`, or without fraction) →
/// epoch ms.
pub fn parse_utc_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let (date, rest) = s.split_once('T')?;
    let mut d = date.split('-');
    let (y, mo, da): (i64, i64, i64) = (d.next()?.parse().ok()?, d.next()?.parse().ok()?, d.next()?.parse().ok()?);
    let (time, offset_min) = if let Some(t) = rest.strip_suffix('Z') {
        (t, 0)
    } else if let Some(at) = rest.rfind(['+', '-']).filter(|&at| at >= 8) {
        let (t, off) = rest.split_at(at);
        let sign = if off.starts_with('-') { -1 } else { 1 };
        let off = off[1..].replace(':', "");
        let (oh, om): (i64, i64) = (off.get(..2)?.parse().ok()?, off.get(2..4).unwrap_or("00").parse().ok()?);
        (t, sign * (oh * 60 + om))
    } else {
        (rest, 0)
    };
    let mut parts = time.split(':');
    let h: i64 = parts.next()?.parse().ok()?;
    let mi: i64 = parts.next()?.parse().ok()?;
    let sec_s = parts.next().unwrap_or("0");
    let (sec, frac) = sec_s.split_once('.').unwrap_or((sec_s, ""));
    let sec: i64 = sec.parse().ok()?;
    let ms: i64 = if frac.is_empty() { 0 } else { format!("{frac:0<3}")[..3].parse().ok()? };
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) || h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let days = days_from_civil(y, mo, da);
    Some(((days * 86_400 + h * 3600 + mi * 60 + sec) - offset_min * 60) * 1000 + ms)
}

/// Epoch ms → `2026-10-08T02:06:41.000Z`.
pub fn format_utc(ms: i64) -> String {
    let (secs, millis) = (ms.div_euclid(1000), ms.rem_euclid(1000));
    let (days, sod) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z", sod / 3600, sod / 60 % 60, sod % 60)
}

/// Wall-clock `HH:MM` at UTC offset `offset_s` (WIB: 25 200).
pub fn local_hm(ms: i64, offset_s: i64) -> String {
    let secs = (ms / 1000 + offset_s).rem_euclid(86_400);
    format!("{:02}:{:02}", secs / 3600, secs / 60 % 60)
}

/// Midnight (local, at `offset_s`) of the day holding `ms`, as epoch ms.
pub fn local_day_start(ms: i64, offset_s: i64) -> i64 {
    let local = ms / 1000 + offset_s;
    (local - local.rem_euclid(86_400) - offset_s) * 1000
}

/// 1 = Monday … 7 = Sunday, local.
pub fn local_weekday(ms: i64, offset_s: i64) -> u32 {
    let days = (ms / 1000 + offset_s).div_euclid(86_400);
    // 1970-01-01 was a Thursday.
    ((days + 3).rem_euclid(7) + 1) as u32
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// `4h 15m`, `45m`, `0m`.
pub fn hours_label(hours: f64) -> String {
    let mins = (hours * 60.).round().max(0.) as i64;
    match (mins / 60, mins % 60) {
        (0, m) => format!("{m}m"),
        (h, 0) => format!("{h}h"),
        (h, m) => format!("{h}h {m}m"),
    }
}

// ── Sync ─────────────────────────────────────────────────────────────────

/// What the pill shows for Dotify.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Tracker {
    /// Not known yet (connecting, first sync).
    #[default]
    Unknown,
    Idle,
    Running(Entry),
}

/// A local change in flight, so a sync that lands mid-request doesn't
/// flip the pill back.
#[derive(Debug, Clone, PartialEq)]
pub enum Pending {
    ClockIn { started_ms: i64 },
    ClockOut,
}

/// What changed when a status came in, for the UI to react to.
#[derive(Debug, Clone, PartialEq)]
pub enum SyncChange {
    None,
    /// Started elsewhere (web, phone) or first seen running.
    StartedElsewhere,
    /// Stopped elsewhere.
    StoppedElsewhere,
    /// Same run, details (project, description) changed.
    Updated,
}

/// Fold a fresh `check-status` into the pill's state. While a clock-in or
/// clock-out is in flight the optimistic state stands (`None`).
pub fn reconcile(current: &Tracker, pending: Option<&Pending>, fresh: &Status) -> (Tracker, SyncChange) {
    if pending.is_some() {
        return (current.clone(), SyncChange::None);
    }
    match (current, &fresh.running) {
        (Tracker::Running(old), Some(new)) if old.id == new.id => {
            let change = if old == new { SyncChange::None } else { SyncChange::Updated };
            (Tracker::Running(new.clone()), change)
        }
        (Tracker::Running(_), Some(new)) => (Tracker::Running(new.clone()), SyncChange::StartedElsewhere),
        (Tracker::Running(_), None) => (Tracker::Idle, SyncChange::StoppedElsewhere),
        (Tracker::Idle, Some(new)) => (Tracker::Running(new.clone()), SyncChange::StartedElsewhere),
        (Tracker::Unknown, Some(new)) => (Tracker::Running(new.clone()), SyncChange::None),
        (_, None) => (Tracker::Idle, SyncChange::None),
    }
}

/// Whether to nudge "sessions active but not clocked in": a workday
/// (Mon–Fri), connected and idle, and Keel busy for at least 15 minutes,
/// at most once per local day.
pub fn should_nudge(tracker: &Tracker, busy_since_ms: Option<i64>, last_nudge_ms: Option<i64>, now: i64, offset_s: i64) -> bool {
    if !matches!(tracker, Tracker::Idle) || local_weekday(now, offset_s) > 5 {
        return false;
    }
    let Some(since) = busy_since_ms else { return false };
    if now - since < 15 * 60_000 {
        return false;
    }
    let today = local_day_start(now, offset_s);
    last_nudge_ms.is_none_or(|last| local_day_start(last, offset_s) != today)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_saved_development_connection_resolves_to_production() {
        assert_eq!(resolve_base("https://api.dotify-dev.dot.co.id/api/v1"), PROD_BASE);
        assert_eq!(resolve_base("https://api.dotify-dev.dot.co.id/api/v1/"), PROD_BASE);
        assert_eq!(resolve_base(PROD_BASE), PROD_BASE);
        assert_eq!(resolve_base("http://127.0.0.1:9/api/v1"), "http://127.0.0.1:9/api/v1");
    }

    use super::*;
    use serde_json::json;

    const WIB: i64 = 7 * 3600;

    #[test]
    fn tokens_lose_their_bearer_prefix() {
        assert_eq!(normalize_token("Bearer abc.def.ghi"), "abc.def.ghi");
        assert_eq!(normalize_token("  bearer   abc "), "abc");
        assert_eq!(normalize_token("\"abc\""), "abc");
        assert_eq!(normalize_token("abc"), "abc");
        assert_eq!(normalize_token(""), "");
    }

    #[test]
    fn identity_comes_from_the_jwt_payload() {
        // {"name":"Ada L","email":"ada@example.com","exp":2000000000}
        let payload = "eyJuYW1lIjoiQWRhIEwiLCJlbWFpbCI6ImFkYUBleGFtcGxlLmNvbSIsImV4cCI6MjAwMDAwMDAwMH0";
        let token = format!("x.{payload}.sig");
        assert_eq!(token_identity(&token), Some(("Ada L".into(), "ada@example.com".into())));
        assert_eq!(token_expiry(&token), Some(2_000_000_000));
        assert_eq!(token_identity("not-a-jwt"), None);
    }

    #[test]
    fn envelope_and_errors() {
        let body = json!({"message": "Check status successfully", "data": {"is_start_timer": false}});
        assert_eq!(envelope_data(&body)["is_start_timer"], false);
        let health = json!({"uptime": 1.0, "message": "API OK", "timestamp": 1});
        assert_eq!(envelope_data(&health)["message"], "API OK");
        let locked = json!({"message": "Cannot edit time entity from a confirmed attendance period", "data": null});
        assert_eq!(
            DotifyError::from_status(400, &locked),
            DotifyError::Rejected("Cannot edit time entity from a confirmed attendance period".into())
        );
        assert!(matches!(DotifyError::from_status(401, &json!({})), DotifyError::Auth(_)));
        assert!(matches!(DotifyError::from_status(502, &json!({})), DotifyError::Server(_)));
        let e = DotifyError::Rejected("nope".into());
        assert_eq!(DotifyError::from_wire(&e.to_wire()), e);
        assert_eq!(DotifyError::from_wire("bad params: auth: expired"), DotifyError::Auth("expired".into()));
        assert_eq!(envelope_message(&json!({"message": ["a", "b"]}), "x"), "a; b");
    }

    #[test]
    fn utc_times_and_wib() {
        let ms = parse_utc_ms("2026-10-08T02:06:41.000Z").unwrap();
        assert_eq!(format_utc(ms), "2026-10-08T02:06:41.000Z");
        assert_eq!(local_hm(ms, WIB), "09:06");
        assert_eq!(parse_utc_ms("2026-10-08T09:06:41+07:00"), Some(ms));
        assert_eq!(parse_utc_ms("2026-10-08T02:06:41.5Z"), Some(ms + 500));
        // 08:00 WIB = 01:00 UTC; 17:00 WIB = 10:00 UTC.
        let eight = parse_utc_ms("2026-10-08T01:00:00.000Z").unwrap();
        assert_eq!(local_hm(eight, WIB), "08:00");
        assert_eq!(format_utc(local_day_start(eight, WIB)), "2026-10-07T17:00:00.000Z");
        // 2026-10-09 is a Friday; 2026-10-10 a Saturday.
        assert_eq!(local_weekday(parse_utc_ms("2026-10-09T03:00:00Z").unwrap(), WIB), 5);
        assert_eq!(local_weekday(parse_utc_ms("2026-10-09T18:00:00Z").unwrap(), WIB), 6);
        assert_eq!(parse_utc_ms("garbage"), None);
        assert_eq!(hours_label(4.25), "4h 15m");
        assert_eq!(hours_label(0.75), "45m");
        assert_eq!(hours_label(8.0), "8h");
    }

    fn running(id: i64, desc: &str) -> Status {
        Status::parse(&json!({
            "is_start_timer": true, "is_over_time": false,
            "time_entity": {"id": id, "description": desc, "project": {"id": 222, "name": "DLTX"},
                             "start_date": "2026-10-08T02:06:41.000Z", "end_date": null}
        }))
    }

    #[test]
    fn status_and_summary_parse() {
        let s = running(1, "work");
        let e = s.running.clone().unwrap();
        assert_eq!((e.id, e.project.clone().unwrap().name.as_str(), e.end_ms), (1, "DLTX", None));
        assert_eq!(e.elapsed_ms(e.start_ms + 90_000), 90_000);
        let idle = Status::parse(&json!({"is_start_timer": false, "is_over_time": true, "time_entity": null}));
        assert_eq!(idle, Status { running: None, overtime: true });
        let sum = Summary::parse(&json!({"employee_week_hours": 42.62, "this_week_normal_hours": 40.0,
            "employee_period_hours": 124.2, "this_period_normal_hours": 176.0}));
        assert_eq!(sum.week_h, 42.62);
        let entries = parse_entries(&json!([{"week": "w", "time_entities": [
            {"id": 1, "start_date": "2026-10-07T01:00:00.000Z", "end_date": "2026-10-07T03:00:00.000Z", "duration": 2.0},
            {"id": 2, "start_date": "2026-10-08T01:00:00.000Z", "end_date": null,
             "attendance_confirmation": {"status": "CONFIRMED"}}]}]));
        assert_eq!(entries.iter().map(|e| e.id).collect::<Vec<_>>(), vec![2, 1]);
        assert!(entries[0].locked);
        let day = local_day_start(entries[1].start_ms, WIB);
        assert_eq!(hours_on(&entries, day, entries[0].start_ms), 2.0);
    }

    #[test]
    fn sync_follows_the_server() {
        let (t, c) = reconcile(&Tracker::Idle, None, &running(1, "a"));
        assert!(matches!(t, Tracker::Running(_)));
        assert_eq!(c, SyncChange::StartedElsewhere);
        let (t2, c) = reconcile(&t, None, &running(1, "b"));
        assert_eq!(c, SyncChange::Updated);
        let (t3, c) = reconcile(&t2, None, &Status::default());
        assert_eq!((t3.clone(), c), (Tracker::Idle, SyncChange::StoppedElsewhere));
        // A clock-out in flight: the stale "running" answer doesn't flip it back.
        let (t4, c) = reconcile(&Tracker::Idle, Some(&Pending::ClockOut), &running(1, "b"));
        assert_eq!((t4, c), (Tracker::Idle, SyncChange::None));
        let (t5, c) = reconcile(&Tracker::Unknown, None, &running(9, "x"));
        assert!(matches!(t5, Tracker::Running(_)));
        assert_eq!(c, SyncChange::None);
        assert_eq!(reconcile(&Tracker::Unknown, None, &Status::default()).0, Tracker::Idle);
    }

    #[test]
    fn nudge_rules() {
        let fri_10 = parse_utc_ms("2026-10-09T03:00:00Z").unwrap();
        let busy = Some(fri_10 - 16 * 60_000);
        assert!(should_nudge(&Tracker::Idle, busy, None, fri_10, WIB));
        assert!(!should_nudge(&Tracker::Idle, Some(fri_10 - 5 * 60_000), None, fri_10, WIB));
        assert!(!should_nudge(&Tracker::Unknown, busy, None, fri_10, WIB));
        assert!(!should_nudge(&Tracker::Idle, busy, Some(fri_10 - 3_600_000), fri_10, WIB));
        let sat = fri_10 + 86_400_000;
        assert!(!should_nudge(&Tracker::Idle, Some(sat - 3_600_000), None, sat, WIB));
    }

    #[test]
    fn project_color_is_optional() {
        assert_eq!(parse_hex_color("#1E90FF"), Some(0x1E90_FFFF));
        assert_eq!(parse_hex_color("#abc"), Some(0xAABB_CCFF));
        assert_eq!(parse_hex_color("blue"), None);
        let p = Project::parse(&json!({ "id": 7, "name": "Keel", "color": "#ff0000", "code": "KL" })).unwrap();
        assert_eq!(p.color.as_deref(), Some("#ff0000"));
        assert_eq!(p.code.as_deref(), Some("KL"));
        let bare = Project::parse(&json!({ "id": 8, "name": "Bare" })).unwrap();
        assert_eq!(bare, Project::new(8, "Bare"));
    }
}
