//! Normalizes the two host-clock values a codex-rs client renders into
//! `<environment_context>` blocks — `<timezone>` (`iana_time_zone::get_timezone`)
//! and `<current_date>` (`chrono::Local`, `%Y-%m-%d`) — so one pooled upstream
//! account never presents self-contradicting local clocks. Every other byte of
//! the block (cwd, shell, filesystem, network, subagents, environments) is left
//! untouched: downstream users rely on them.
//!
//! Mirrors codex-rs `core/src/context/world_state/environment.rs`:
//! * the first block of a thread (and every re-render after compaction) is a
//!   full block carrying `<cwd>`/`<shell>`; later turns only get a *diff* block
//!   when `shell_version`/`current_date`/`timezone`/`network`/`filesystem`
//!   changed (`render_diff`, `:144-148`), and that diff restates every scalar;
//! * blocks are `role: user` messages recorded at turn start (with the other
//!   world-state sections, in section registration order, before the prompt)
//!   or between a tool output and the next model sample
//!   (`record_step_world_state_if_changed`).
//!
//! The pass replays `input[]` in order and keeps the invariant "the last
//! effective `<current_date>` equals today in the declared timezone at every
//! sampling instant": rewritten diffs that no longer say anything new are
//! removed, and day changes that only exist in the target timezone get a
//! synthesized diff at the position the official client would have used.
//! Dates come from each item's own instant (`create_time`, then its UUIDv7 id,
//! then the next stamped item), so the same history always maps to the same
//! output regardless of when Aether replays it.

use std::collections::HashSet;
use std::str::FromStr;
use std::sync::{Mutex, OnceLock};

use chrono::{NaiveDate, NaiveTime, TimeZone};
use chrono_tz::Tz;
use serde_json::{Map, Value};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::codex_runtime_identity::{
    hash16, message_text, prompt_text, sha256, starts_with_wrapper_tag, uuid_v7_from_parts,
    uuid_v7_unix_millis,
};

const ENVIRONMENT_CONTEXT_OPEN_TAG: &str = "<environment_context>";
const ENVIRONMENT_CONTEXT_CLOSE_TAG: &str = "</environment_context>";
const ENVIRONMENT_CONTEXT_ITEM_KIND: &str = "environments.environment_context";
const SYNTHESIZED_ID_DOMAIN: &[u8] = b"aether-codex-env-ctx\0";

/// Fallback when no candidate passes validation (the user's decision: US at
/// minimum, never a Chinese zone).
pub(crate) const FALLBACK_ENVIRONMENT_TIMEZONE: Tz = Tz::America__New_York;

/// Zones that must never appear outbound even when the host is misconfigured.
/// Compared case-insensitively against the candidate and its canonical name.
const DENIED_ENVIRONMENT_TIMEZONES: &[&str] = &[
    "Asia/Shanghai",
    "Asia/Chongqing",
    "Asia/Chungking",
    "Asia/Harbin",
    "Asia/Urumqi",
    "Asia/Kashgar",
    "PRC",
    "Asia/Hong_Kong",
    "Hongkong",
    "Asia/Macau",
    "Asia/Macao",
    "Asia/Taipei",
    "ROC",
];

/// Developer/user wrapper tags of world-state sections codex-rs registers
/// *before* `environments` (`core/src/session/world_state.rs`
/// `build_world_state_for_step`, plus extension sections which are inserted
/// at the permissions index). A synthesized environment diff goes after the
/// last of these in the turn's pre-prompt run and before everything else
/// (apps, plugins, tools, multi-agent, managed developer instructions).
/// Opening marker of the AgentsMd user message (codex-rs `UserInstructions::type_markers`).
const AGENTS_MD_MARKER: &str = "# AGENTS.md instructions";

const SECTIONS_BEFORE_ENVIRONMENTS: &[&str] = &[
    "model_instructions",
    "personality",
    "personality_spec",
    "context_window",
    "context_window_guidance",
    "realtime_conversation",
    "user_instructions",
    "permissions",
    "permissions_instructions",
    "compact_permissions",
    "collaboration_mode",
    "collaboration_mode_instructions",
    "persistent_mode",
    "skills_instructions",
];

// ---------------------------------------------------------------------------
// Timezone policy
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EnvironmentTimezoneSource {
    /// `AETHER_CODEX_ENVIRONMENT_TIMEZONE`.
    EnvOverride,
    /// `TZ` (compose passes the host zone to the `app` service).
    TzEnv,
    /// `/etc/localtime` symlink target.
    LocalTime,
    /// [`FALLBACK_ENVIRONMENT_TIMEZONE`].
    Fallback,
}

impl EnvironmentTimezoneSource {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::EnvOverride => "env_override",
            Self::TzEnv => "tz_env",
            Self::LocalTime => "localtime",
            Self::Fallback => "fallback",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct EnvironmentTimezonePolicy {
    pub(crate) tz: Tz,
    pub(crate) source: EnvironmentTimezoneSource,
    /// `(source, candidate, reason)` for every candidate that failed.
    pub(crate) rejected: Vec<(EnvironmentTimezoneSource, String, &'static str)>,
}

/// Accepts only a real `Region/City` IANA zone outside the deny list. `UTC`,
/// `Etc/*`, `GMT`, `EST`, link names such as `PRC`/`ROC`/`Hongkong` are
/// rejected: a personal machine never reports those, and the official client
/// only falls back to `Etc/UTC` when its own probe fails.
pub(crate) fn validate_environment_timezone(candidate: &str) -> Result<Tz, &'static str> {
    let candidate = candidate.trim().trim_start_matches(':');
    if candidate.is_empty() {
        return Err("empty");
    }
    let denied = |name: &str| {
        DENIED_ENVIRONMENT_TIMEZONES
            .iter()
            .any(|denied| denied.eq_ignore_ascii_case(name))
    };
    if denied(candidate) {
        return Err("denied_region");
    }
    let tz = Tz::from_str(candidate).map_err(|_| "unknown_iana_name")?;
    let canonical = tz.name();
    if denied(canonical) {
        return Err("denied_region");
    }
    if !canonical.contains('/') || canonical.starts_with("Etc/") {
        return Err("not_a_region_city_zone");
    }
    Ok(tz)
}

/// First candidate (in order) that passes [`validate_environment_timezone`],
/// else the fallback. Pure so the priority and the deny list are testable.
pub(crate) fn resolve_environment_timezone(
    candidates: &[(EnvironmentTimezoneSource, Option<String>)],
) -> EnvironmentTimezonePolicy {
    let mut rejected = Vec::new();
    for (source, candidate) in candidates {
        let Some(candidate) = candidate
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
        else {
            continue;
        };
        match validate_environment_timezone(candidate) {
            Ok(tz) => {
                return EnvironmentTimezonePolicy {
                    tz,
                    source: *source,
                    rejected,
                };
            }
            Err(reason) => rejected.push((*source, candidate.to_string(), reason)),
        }
    }
    EnvironmentTimezonePolicy {
        tz: FALLBACK_ENVIRONMENT_TIMEZONE,
        source: EnvironmentTimezoneSource::Fallback,
        rejected,
    }
}

fn localtime_link_zone() -> Option<String> {
    let target = std::fs::read_link("/etc/localtime").ok()?;
    let target = target.to_str()?;
    let (_, zone) = target.rsplit_once("zoneinfo/")?;
    Some(zone.to_string())
}

/// Process-wide policy, resolved once. Logs the outcome so operators can see
/// which zone the gateway presents (`codex_env_tz_resolved`) and why any
/// configured value was refused (`codex_env_tz_rejected`).
pub(crate) fn process_environment_timezone() -> &'static EnvironmentTimezonePolicy {
    static POLICY: OnceLock<EnvironmentTimezonePolicy> = OnceLock::new();
    POLICY.get_or_init(|| {
        let candidates = [
            (
                EnvironmentTimezoneSource::EnvOverride,
                std::env::var("AETHER_CODEX_ENVIRONMENT_TIMEZONE").ok(),
            ),
            (EnvironmentTimezoneSource::TzEnv, std::env::var("TZ").ok()),
            (EnvironmentTimezoneSource::LocalTime, localtime_link_zone()),
        ];
        let policy = resolve_environment_timezone(&candidates);
        for (source, candidate, reason) in &policy.rejected {
            warn!(
                event_name = "codex_env_tz_rejected",
                log_type = "event",
                source = source.as_str(),
                candidate = candidate.as_str(),
                reason,
                "codex environment timezone candidate rejected"
            );
        }
        info!(
            event_name = "codex_env_tz_resolved",
            log_type = "event",
            timezone = policy.tz.name(),
            source = policy.source.as_str(),
            "codex environment_context timezone resolved"
        );
        policy
    })
}

/// `AETHER_CODEX_ENVIRONMENT_CONTEXT_REWRITE=off|0|false|disabled|no` disables
/// the pass without touching the identity synthesis.
pub(crate) fn rewrite_switch_enabled(value: Option<&str>) -> bool {
    !matches!(
        value
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref(),
        Some("off" | "0" | "false" | "disabled" | "no")
    )
}

pub(crate) fn environment_context_rewrite_enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| {
        rewrite_switch_enabled(
            std::env::var("AETHER_CODEX_ENVIRONMENT_CONTEXT_REWRITE")
                .ok()
                .as_deref(),
        )
    })
}

// ---------------------------------------------------------------------------
// Public rewrite API
// ---------------------------------------------------------------------------

pub(crate) struct EnvironmentContextRewriteInput<'a> {
    pub(crate) tz: Tz,
    /// Wall clock of this request (`SystemTime::now()`); only used for items
    /// that carry no instant of their own.
    pub(crate) now_unix_ms: u64,
    /// Timestamp of the outbound turn id (UUIDv7), same source as the blob's
    /// `turn_started_at_unix_ms`.
    pub(crate) turn_started_at_unix_ms: Option<u64>,
    /// The outbound turn id itself: passthrough `turn_id` of a block
    /// synthesized after a tool output when no earlier item of the body
    /// carries one (incremental WebSocket steps), so the next turn's full
    /// replay renders the same item.
    pub(crate) turn_id: Option<&'a str>,
    /// Deterministic seed for synthesized ids; the outbound thread id.
    pub(crate) outbound_thread_id: &'a str,
    /// Whether a trailing tool output may get a day-change block appended
    /// (the request is about to sample). `false` for compaction requests.
    pub(crate) allow_tail_append: bool,
    /// Effective state carried over from the previous step on the same
    /// WebSocket connection; used only when the body carries no block of its
    /// own (incremental `previous_response_id` steps).
    pub(crate) prior_state: Option<&'a EnvironmentEffectiveState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scalar {
    ShellVersion,
    CurrentDate,
    Timezone,
    Network,
    Filesystem,
    Subagents,
}

impl Scalar {
    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "shell_version" => Self::ShellVersion,
            "current_date" => Self::CurrentDate,
            "timezone" => Self::Timezone,
            "network" => Self::Network,
            "filesystem" => Self::Filesystem,
            "subagents" => Self::Subagents,
            _ => return None,
        })
    }
}

/// What the model currently believes about the environment scalars after
/// replaying the history, plus the shape to mint new diffs in.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct EnvironmentEffectiveState {
    pub(crate) date: Option<String>,
    pub(crate) timezone: Option<String>,
    /// `None` after a `<shell_version status="unavailable" />` marker too.
    shell_version: Option<String>,
    /// Raw `<network …>…</network>` line without the two-space indent.
    network: Option<String>,
    /// Raw `<filesystem>…</filesystem>` line without the two-space indent.
    filesystem: Option<String>,
    /// Raw multi-line `  <subagents>\n…\n  </subagents>` block.
    subagents: Option<String>,
    /// Scalar set of the last client diff kept; synthesized diffs copy it so
    /// an older client build that only sends `<current_date>` stays uniform.
    last_diff_shape: Option<Vec<Scalar>>,
    /// The client renders `<current_date>` at all (a block without it means
    /// the pass never synthesizes day changes).
    pub(crate) carries_current_date: bool,
    /// Some environment item used `content_item_kinds`; synthesized items
    /// then carry it too.
    uses_content_item_kinds: bool,
}

impl EnvironmentEffectiveState {
    fn scalar_value(&self, scalar: Scalar) -> Option<&str> {
        match scalar {
            Scalar::ShellVersion => self.shell_version.as_deref(),
            Scalar::CurrentDate => self.date.as_deref(),
            Scalar::Timezone => self.timezone.as_deref(),
            Scalar::Network => self.network.as_deref(),
            Scalar::Filesystem => self.filesystem.as_deref(),
            Scalar::Subagents => self.subagents.as_deref(),
        }
    }

    fn set_scalar(&mut self, scalar: Scalar, value: Option<String>) {
        match scalar {
            Scalar::ShellVersion => self.shell_version = value,
            Scalar::CurrentDate => self.date = value,
            Scalar::Timezone => self.timezone = value,
            Scalar::Network => self.network = value,
            Scalar::Filesystem => self.filesystem = value,
            Scalar::Subagents => self.subagents = value,
        }
    }

    /// Shape for a synthesized diff: the last client diff's, else the current
    /// codex-rs `render_diff` shape over what is known. `subagents` is dynamic
    /// (agents spawn and finish) so it is only repeated when the client's own
    /// diffs carried it.
    fn synthesized_shape(&self) -> Vec<Scalar> {
        if let Some(shape) = &self.last_diff_shape {
            return shape.clone();
        }
        [
            Scalar::ShellVersion,
            Scalar::CurrentDate,
            Scalar::Timezone,
            Scalar::Network,
            Scalar::Filesystem,
        ]
        .into_iter()
        .filter(|scalar| self.scalar_value(*scalar).is_some())
        .collect()
    }
}

/// Which source decided an item's instant; indexes into
/// [`EnvironmentContextRewriteReport::instant_sources`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InstantSource {
    CreateTime = 0,
    OwnId = 1,
    NextItem = 2,
    TurnStart = 3,
    Heuristic = 4,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct EnvironmentContextRewriteReport {
    pub(crate) blocks_seen: u32,
    pub(crate) timezone_rewritten: u32,
    pub(crate) date_rewritten: u32,
    pub(crate) blocks_removed: u32,
    pub(crate) blocks_inserted: u32,
    pub(crate) blocks_appended: u32,
    pub(crate) instant_sources: [u32; 5],
    pub(crate) unknown_child_tags: u32,
    pub(crate) user_location_removed: u32,
    /// `msg_…` / `at_…` prefix-cache ids re-derived from the outbound thread.
    pub(crate) prefix_cache_ids_rewritten: u32,
}

impl EnvironmentContextRewriteReport {
    pub(crate) fn is_noop(&self) -> bool {
        *self == Self::default()
    }

    pub(crate) fn changed_history(&self) -> bool {
        self.blocks_removed + self.blocks_inserted + self.blocks_appended > 0
    }

    fn count_instant(&mut self, source: InstantSource) {
        self.instant_sources[source as usize] += 1;
    }
}

/// Emits `codex_env_context_rewritten` when the pass did anything.
pub(crate) fn log_environment_context_report(
    surface: &'static str,
    outbound_thread_id: &str,
    report: &EnvironmentContextRewriteReport,
) {
    if report.is_noop() {
        return;
    }
    let thread = hash16(outbound_thread_id);
    if report.changed_history() {
        info!(
            event_name = "codex_env_context_rewritten",
            log_type = "event",
            surface,
            thread = thread.as_str(),
            blocks_seen = report.blocks_seen,
            timezone_rewritten = report.timezone_rewritten,
            date_rewritten = report.date_rewritten,
            blocks_removed = report.blocks_removed,
            blocks_inserted = report.blocks_inserted,
            blocks_appended = report.blocks_appended,
            instant_source_create_time = report.instant_sources[0],
            instant_source_own_id = report.instant_sources[1],
            instant_source_next_item = report.instant_sources[2],
            instant_source_turn_start = report.instant_sources[3],
            instant_source_heuristic = report.instant_sources[4],
            unknown_child_tags = report.unknown_child_tags,
            user_location_removed = report.user_location_removed,
            prefix_cache_ids_rewritten = report.prefix_cache_ids_rewritten,
            "codex environment_context history normalized"
        );
    } else {
        debug!(
            event_name = "codex_env_context_rewritten",
            log_type = "event",
            surface,
            thread = thread.as_str(),
            blocks_seen = report.blocks_seen,
            timezone_rewritten = report.timezone_rewritten,
            date_rewritten = report.date_rewritten,
            instant_source_create_time = report.instant_sources[0],
            instant_source_own_id = report.instant_sources[1],
            instant_source_next_item = report.instant_sources[2],
            instant_source_turn_start = report.instant_sources[3],
            instant_source_heuristic = report.instant_sources[4],
            unknown_child_tags = report.unknown_child_tags,
            user_location_removed = report.user_location_removed,
            prefix_cache_ids_rewritten = report.prefix_cache_ids_rewritten,
            "codex environment_context values normalized"
        );
    }
}

/// Rewrites `<timezone>`/`<current_date>` in every `<environment_context>`
/// block of `body.input`, removes diffs made redundant by the rewrite, inserts
/// the day-change diffs the target zone requires, and strips
/// `tools[].user_location`. Returns the effective state after replay (for the
/// next incremental WebSocket step) when the body had anything to replay.
pub(crate) fn apply_codex_environment_context(
    body: &mut Value,
    input: &EnvironmentContextRewriteInput<'_>,
) -> (
    EnvironmentContextRewriteReport,
    Option<EnvironmentEffectiveState>,
) {
    let mut report = EnvironmentContextRewriteReport::default();
    strip_web_search_user_location(body, &mut report);
    report.prefix_cache_ids_rewritten =
        rewrite_prefix_cache_item_ids(body, input.outbound_thread_id) as u32;
    let Some(items) = body.get_mut("input") else {
        return (report, None);
    };
    match items {
        Value::String(text) => {
            if let Some(parsed) = parse_environment_block(text) {
                report.blocks_seen += 1;
                let instant = input.turn_started_at_unix_ms.unwrap_or(input.now_unix_ms);
                report.count_instant(InstantSource::TurnStart);
                let date = date_in(input.tz, instant);
                let rewritten = rewrite_block(text, &parsed, input.tz, &date, &mut report);
                *text = rewritten;
            }
            (report, None)
        }
        Value::Array(_) => {
            let Value::Array(old) = std::mem::take(items) else {
                unreachable!("matched Value::Array above");
            };
            let mut replay = Replay::new(input, &mut report);
            let out = replay.run(old);
            let state = replay.finish();
            *items = Value::Array(out);
            (report, Some(state))
        }
        _ => (report, None),
    }
}

// ---------------------------------------------------------------------------
// Block parsing / rewriting (string splices only)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum EnvLine<'a> {
    /// `value` is `None` for the `<shell_version status="unavailable" />`
    /// removal marker; `raw` is the line (or multi-line block) without the
    /// two-space indent, exactly as the client rendered it.
    Scalar {
        scalar: Scalar,
        value: Option<&'a str>,
        raw: &'a str,
    },
    /// `<cwd>`, `<status>`, `<shell>`, or an `<environments>` container: the
    /// block is a full render (or an environment change) — never redundant.
    EnvironmentSpecific,
    Unknown {
        tag: String,
    },
}

#[derive(Debug)]
struct ParsedLine<'a> {
    /// Byte span of the line within the block text, excluding the trailing
    /// newline.
    start: usize,
    end: usize,
    line: EnvLine<'a>,
}

#[derive(Debug)]
struct ParsedEnvironmentBlock<'a> {
    lines: Vec<ParsedLine<'a>>,
    has_environment_specific: bool,
    has_unknown: bool,
}

impl ParsedEnvironmentBlock<'_> {
    fn scalars(&self) -> impl Iterator<Item = (Scalar, Option<&str>, &str)> + '_ {
        self.lines.iter().filter_map(|line| match &line.line {
            EnvLine::Scalar { scalar, value, raw } => Some((*scalar, *value, *raw)),
            _ => None,
        })
    }

    fn scalar_value(&self, wanted: Scalar) -> Option<Option<&str>> {
        self.scalars()
            .find(|(scalar, _, _)| *scalar == wanted)
            .map(|(_, value, _)| value)
    }

    fn shape(&self) -> Vec<Scalar> {
        self.scalars().map(|(scalar, _, _)| scalar).collect()
    }
}

/// Recognizes exactly the codex-rs rendering: the open tag, a body of
/// two-space indented top-level children each on its own line, the close tag.
/// Anything else is `Unknown` and only ever gets its two known lines edited.
fn parse_environment_block(text: &str) -> Option<ParsedEnvironmentBlock<'_>> {
    let body = text
        .strip_prefix(ENVIRONMENT_CONTEXT_OPEN_TAG)?
        .strip_suffix(ENVIRONMENT_CONTEXT_CLOSE_TAG)?;
    let base = ENVIRONMENT_CONTEXT_OPEN_TAG.len();
    let mut parsed = ParsedEnvironmentBlock {
        lines: Vec::new(),
        has_environment_specific: false,
        has_unknown: false,
    };
    if body.is_empty() {
        return Some(parsed);
    }
    let mut offset = if body.starts_with('\n') {
        1
    } else {
        parsed.has_unknown = true;
        0
    };
    while offset < body.len() {
        let rest = &body[offset..];
        let line_end = rest.find('\n').map_or(body.len(), |at| offset + at);
        let line_text = &body[offset..line_end];
        let mut next_offset = line_end + 1;
        let line = match child_tag(line_text) {
            Some(tag @ ("cwd" | "status" | "shell")) if is_simple_element(line_text, tag) => {
                EnvLine::EnvironmentSpecific
            }
            Some("environments") if line_text == "  <environments>" => {
                match find_closing_line(body, next_offset, "  </environments>") {
                    Some(close_end) => {
                        next_offset = close_end + 1;
                        EnvLine::EnvironmentSpecific
                    }
                    None => EnvLine::Unknown {
                        tag: "environments".to_string(),
                    },
                }
            }
            Some("subagents") if line_text == "  <subagents>" => {
                match find_closing_line(body, next_offset, "  </subagents>") {
                    Some(close_end) => {
                        let raw = &body[offset..close_end];
                        next_offset = close_end + 1;
                        parsed.lines.push(ParsedLine {
                            start: base + offset,
                            end: base + close_end,
                            line: EnvLine::Scalar {
                                scalar: Scalar::Subagents,
                                value: Some(raw),
                                raw,
                            },
                        });
                        offset = next_offset;
                        continue;
                    }
                    None => EnvLine::Unknown {
                        tag: "subagents".to_string(),
                    },
                }
            }
            Some("shell_version")
                if line_text.starts_with("  <shell_version status=")
                    && line_text.ends_with("/>") =>
            {
                EnvLine::Scalar {
                    scalar: Scalar::ShellVersion,
                    value: None,
                    raw: &line_text[2..],
                }
            }
            Some(tag @ ("shell_version" | "current_date" | "timezone"))
                if is_simple_element(line_text, tag) =>
            {
                let value = &line_text[3 + tag.len() + 1..line_text.len() - (tag.len() + 3)];
                EnvLine::Scalar {
                    scalar: Scalar::from_tag(tag).expect("known scalar tag"),
                    value: Some(value),
                    raw: &line_text[2..],
                }
            }
            Some(tag @ ("network" | "filesystem"))
                if line_text.ends_with(&format!("</{tag}>")) || line_text.ends_with("/>") =>
            {
                EnvLine::Scalar {
                    scalar: Scalar::from_tag(tag).expect("known scalar tag"),
                    value: Some(&line_text[2..]),
                    raw: &line_text[2..],
                }
            }
            Some(tag) => EnvLine::Unknown {
                tag: tag.to_string(),
            },
            None => EnvLine::Unknown { tag: String::new() },
        };
        match &line {
            EnvLine::EnvironmentSpecific => parsed.has_environment_specific = true,
            EnvLine::Unknown { .. } => parsed.has_unknown = true,
            EnvLine::Scalar { .. } => {}
        }
        parsed.lines.push(ParsedLine {
            start: base + offset,
            end: base + line_end,
            line,
        });
        offset = next_offset;
    }
    Some(parsed)
}

/// Tag name of a two-space indented top-level child line.
fn child_tag(line: &str) -> Option<&str> {
    let inner = line.strip_prefix("  <")?;
    if inner.starts_with(' ') || inner.starts_with('/') {
        return None;
    }
    let end = inner.find(['>', ' ', '/']).unwrap_or(inner.len());
    let tag = &inner[..end];
    (!tag.is_empty() && tag.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')).then_some(tag)
}

/// `  <tag>value</tag>` with a single-line value.
fn is_simple_element(line: &str, tag: &str) -> bool {
    let open = format!("  <{tag}>");
    let close = format!("</{tag}>");
    line.len() >= open.len() + close.len() && line.starts_with(&open) && line.ends_with(&close)
}

/// Byte offset (exclusive) of the end of the `closing` line, searching from
/// `from`; `None` when the container never closes at the top level.
fn find_closing_line(body: &str, mut from: usize, closing: &str) -> Option<usize> {
    while from < body.len() {
        let rest = &body[from..];
        let line_end = rest.find('\n').map_or(body.len(), |at| from + at);
        if &body[from..line_end] == closing {
            return Some(line_end);
        }
        from = line_end + 1;
    }
    None
}

fn xml_escaped(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Splices the two known lines; every other byte of `text` is preserved.
fn rewrite_block(
    text: &str,
    parsed: &ParsedEnvironmentBlock<'_>,
    tz: Tz,
    date: &str,
    report: &mut EnvironmentContextRewriteReport,
) -> String {
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    for line in &parsed.lines {
        let EnvLine::Scalar {
            scalar,
            value: Some(value),
            ..
        } = &line.line
        else {
            continue;
        };
        let (tag, new_value) = match scalar {
            Scalar::Timezone => ("timezone", tz.name()),
            Scalar::CurrentDate => ("current_date", date),
            _ => continue,
        };
        if *value == new_value {
            continue;
        }
        match scalar {
            Scalar::Timezone => report.timezone_rewritten += 1,
            Scalar::CurrentDate => report.date_rewritten += 1,
            _ => {}
        }
        edits.push((
            line.start,
            line.end,
            format!("  <{tag}>{}</{tag}>", xml_escaped(new_value)),
        ));
    }
    if edits.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len() + 16);
    let mut cursor = 0;
    for (start, end, replacement) in edits {
        out.push_str(&text[cursor..start]);
        out.push_str(&replacement);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

fn report_unknown_child_tag(tag: &str) {
    static SEEN: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let first_sighting = SEEN
        .get_or_init(Default::default)
        .lock()
        .map(|mut seen| seen.insert(tag.to_string()))
        .unwrap_or(true);
    if first_sighting {
        warn!(
            event_name = "codex_env_unknown_child_tag",
            log_type = "event",
            tag,
            "unknown <environment_context> child; block kept verbatim apart from timezone/date"
        );
    } else {
        debug!(
            event_name = "codex_env_unknown_child_tag",
            log_type = "event",
            tag,
            "unknown <environment_context> child"
        );
    }
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

pub(crate) fn date_in(tz: Tz, unix_ms: u64) -> String {
    let millis = i64::try_from(unix_ms).unwrap_or(i64::MAX);
    tz.timestamp_millis_opt(millis)
        .single()
        .or_else(|| tz.timestamp_millis_opt(0).single())
        .map(|at| at.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "1970-01-01".to_string())
}

/// Last resort when nothing in the request carries an instant: the source
/// block's own date at noon in its own zone, moved to the target zone.
fn heuristic_date(tz: Tz, source_date: Option<&str>, source_tz: Option<&str>) -> Option<String> {
    let date = NaiveDate::parse_from_str(source_date?, "%Y-%m-%d").ok()?;
    let source_tz = source_tz
        .and_then(|name| Tz::from_str(name.trim()).ok())
        .unwrap_or(Tz::UTC);
    let noon = date.and_time(NaiveTime::from_hms_opt(12, 0, 0)?);
    let at = source_tz.from_local_datetime(&noon).earliest()?;
    Some(at.with_timezone(&tz).format("%Y-%m-%d").to_string())
}

fn uuid_tail(id: &str) -> &str {
    id.rsplit_once('_').map_or(id, |(_, tail)| tail)
}

fn id_unix_millis(id: &str) -> Option<u64> {
    uuid_v7_unix_millis(uuid_tail(id))
}

// ---------------------------------------------------------------------------
// Item helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ItemKind {
    /// A user message with real prompt text.
    RealPrompt,
    /// Developer message, or a user message wrapped in an injected tag
    /// (`<environment_context>`, `<user_instructions>`, …): turn-start
    /// context recorded before the prompt.
    WrapperMessage,
    /// `*_output` items.
    ToolOutput,
    /// Reasoning, calls, assistant messages, compaction, …
    ModelSide,
    /// Compaction summaries, empty messages, non-objects.
    Other,
}

fn classify(item: &Value) -> ItemKind {
    let Some(object) = item.as_object() else {
        return ItemKind::Other;
    };
    let kind = object.get("type").and_then(Value::as_str);
    match kind {
        None | Some("message") => {}
        // A compaction request samples nothing; no world-state diff precedes it.
        Some("compaction_trigger") => return ItemKind::Other,
        Some(kind) if kind.ends_with("_output") => return ItemKind::ToolOutput,
        Some(_) => return ItemKind::ModelSide,
    }
    match object.get("role").and_then(Value::as_str).map(str::trim) {
        Some("user") => {
            let Some(text) = object.get("content").and_then(message_text) else {
                return ItemKind::Other;
            };
            let trimmed = text.trim();
            if is_agents_md_message(trimmed) || starts_with_wrapper_tag(trimmed) {
                ItemKind::WrapperMessage
            } else if prompt_text(trimmed).is_some() {
                ItemKind::RealPrompt
            } else {
                ItemKind::Other
            }
        }
        Some("developer") | Some("system") => ItemKind::WrapperMessage,
        Some("assistant") => ItemKind::ModelSide,
        _ => ItemKind::Other,
    }
}

fn passthrough(item: &Value) -> Option<&Map<String, Value>> {
    item.get("internal_chat_message_metadata_passthrough")?
        .as_object()
}

fn turn_id_of(item: &Value) -> Option<&str> {
    passthrough(item)?.get("turn_id")?.as_str()
}

fn item_id(item: &Value) -> Option<&str> {
    item.get("id").and_then(Value::as_str)
}

/// `create_time` (seconds, float or integer) first, then the item's own
/// UUIDv7 id. Server-minted ids (`rs_…`, `fc_…`) are not UUIDs and yield
/// `None`.
fn item_instant(item: &Value) -> Option<(u64, InstantSource)> {
    if let Some(create_time) = passthrough(item).and_then(|meta| meta.get("create_time")) {
        if let Some(secs) = create_time
            .as_f64()
            .filter(|secs| secs.is_finite() && *secs > 0.0)
        {
            return Some(((secs * 1000.0).round() as u64, InstantSource::CreateTime));
        }
    }
    item_id(item)
        .and_then(id_unix_millis)
        .map(|ms| (ms, InstantSource::OwnId))
}

fn next_instant(items: &[Value], from: usize) -> Option<u64> {
    items[from.min(items.len())..]
        .iter()
        .find_map(|item| item_instant(item).map(|(ms, _)| ms))
}

/// Text of a message content part when it is a text part.
fn part_text(part: &Value) -> Option<&str> {
    let object = part.as_object()?;
    match object.get("type").and_then(Value::as_str) {
        None | Some("input_text") | Some("text") => object.get("text")?.as_str(),
        _ => None,
    }
}

/// Positions of `<environment_context>` blocks in a message: `Some(index)`
/// for content parts, `None` for a string `content`.
fn environment_block_positions(item: &Value) -> Vec<Option<usize>> {
    let Some(object) = item.as_object() else {
        return Vec::new();
    };
    let is_message = matches!(
        object.get("type").and_then(Value::as_str),
        None | Some("message")
    );
    if !is_message {
        return Vec::new();
    }
    match object.get("content") {
        Some(Value::String(text)) if text.starts_with(ENVIRONMENT_CONTEXT_OPEN_TAG) => vec![None],
        Some(Value::Array(parts)) => parts
            .iter()
            .enumerate()
            .filter(|(_, part)| {
                part_text(part).is_some_and(|text| text.starts_with(ENVIRONMENT_CONTEXT_OPEN_TAG))
            })
            .map(|(index, _)| Some(index))
            .collect(),
        _ => Vec::new(),
    }
}

fn block_text(item: &Value, position: Option<usize>) -> Option<&str> {
    match position {
        None => item.get("content")?.as_str(),
        Some(index) => part_text(item.get("content")?.get(index)?),
    }
}

fn set_block_text(item: &mut Value, position: Option<usize>, text: String) {
    let Some(content) = item.get_mut("content") else {
        return;
    };
    match position {
        None => *content = Value::String(text),
        Some(index) => {
            if let Some(part) = content.get_mut(index).and_then(Value::as_object_mut) {
                part.insert("text".to_string(), Value::String(text));
            }
        }
    }
}

/// Removes a content part (keeping `content_item_kinds` aligned) or the whole
/// content when it was a string. Returns `true` when the item is now empty
/// and must be dropped.
fn remove_block(item: &mut Value, position: Option<usize>) -> bool {
    let Some(object) = item.as_object_mut() else {
        return false;
    };
    let Some(index) = position else {
        return true;
    };
    let empty = match object.get_mut("content").and_then(Value::as_array_mut) {
        Some(parts) if index < parts.len() => {
            parts.remove(index);
            parts.is_empty()
        }
        _ => return false,
    };
    if let Some(kinds) = object
        .get_mut("internal_chat_message_metadata_passthrough")
        .and_then(Value::as_object_mut)
        .and_then(|meta| meta.get_mut("content_item_kinds"))
        .and_then(Value::as_array_mut)
    {
        if index < kinds.len() {
            kinds.remove(index);
        }
    }
    empty
}

fn uses_content_item_kinds(item: &Value) -> bool {
    passthrough(item).is_some_and(|meta| meta.contains_key("content_item_kinds"))
}

/// The wrapper tag of a message's text, if any (`<skills_instructions>…` →
/// `skills_instructions`).
fn wrapper_tag(item: &Value) -> Option<String> {
    let text = item.get("content").and_then(message_text)?;
    let text = text.trim();
    let rest = text.strip_prefix('<')?;
    let end = rest.find(['>', ' '])?;
    let tag = &rest[..end];
    (!tag.is_empty() && tag.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'))
        .then(|| tag.to_string())
}

/// The AgentsMd section renders as a `role:"user"` message without a wrapper
/// tag: `# AGENTS.md instructions[ for <dir>]\n\n<INSTRUCTIONS>…` (codex-rs
/// `core/src/context/user_instructions.rs`). It is the only user-role
/// fragment without a `<tag>` marker, and it sits before Environments.
fn is_agents_md_message(trimmed_text: &str) -> bool {
    trimmed_text.starts_with(AGENTS_MD_MARKER)
}

fn belongs_before_environments(item: &Value) -> bool {
    if let Some(tag) = wrapper_tag(item) {
        return SECTIONS_BEFORE_ENVIRONMENTS.contains(&tag.as_str());
    }
    item.get("content")
        .and_then(message_text)
        .is_some_and(|text| is_agents_md_message(text.trim()))
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

struct Replay<'a, 'r> {
    input: &'a EnvironmentContextRewriteInput<'a>,
    report: &'r mut EnvironmentContextRewriteReport,
    state: EnvironmentEffectiveState,
}

impl<'a, 'r> Replay<'a, 'r> {
    fn new(
        input: &'a EnvironmentContextRewriteInput<'a>,
        report: &'r mut EnvironmentContextRewriteReport,
    ) -> Self {
        Self {
            input,
            report,
            state: EnvironmentEffectiveState::default(),
        }
    }

    fn finish(self) -> EnvironmentEffectiveState {
        self.state
    }

    fn run(&mut self, mut old: Vec<Value>) -> Vec<Value> {
        let has_own_blocks = old
            .iter()
            .any(|item| !environment_block_positions(item).is_empty());
        if !has_own_blocks {
            if let Some(prior) = self.input.prior_state {
                self.state = prior.clone();
            }
        }
        let mut out: Vec<Value> = Vec::with_capacity(old.len() + 2);
        for index in 0..old.len() {
            let mut item = std::mem::take(&mut old[index]);
            let kind = classify(&item);

            let positions = environment_block_positions(&item);
            if !positions.is_empty() {
                if uses_content_item_kinds(&item) {
                    self.state.uses_content_item_kinds = true;
                }
                let mut removals = Vec::new();
                for position in positions {
                    if self.process_block(&mut item, position, &old, index) {
                        removals.push(position);
                    }
                }
                let mut drop_item = false;
                for position in removals.into_iter().rev() {
                    drop_item |= remove_block(&mut item, position);
                }
                if drop_item {
                    continue;
                }
            }

            if kind == ItemKind::RealPrompt {
                self.insert_turn_start_diff(&mut out, &item, &old, index);
            }
            out.push(item);
            if kind == ItemKind::ToolOutput {
                self.insert_after_tool_output(&mut out, &old, index);
            }
        }
        out
    }

    /// Rewrites one block in place. Returns `true` when the block is a diff
    /// that became redundant and must be removed.
    fn process_block(
        &mut self,
        item: &mut Value,
        position: Option<usize>,
        old: &[Value],
        index: usize,
    ) -> bool {
        let Some(text) = block_text(item, position).map(str::to_string) else {
            return false;
        };
        let Some(parsed) = parse_environment_block(&text) else {
            return false;
        };
        self.report.blocks_seen += 1;
        for line in &parsed.lines {
            if let EnvLine::Unknown { tag } = &line.line {
                self.report.unknown_child_tags += 1;
                report_unknown_child_tag(tag);
            }
        }
        let source_date = parsed.scalar_value(Scalar::CurrentDate).flatten();
        let source_tz = parsed.scalar_value(Scalar::Timezone).flatten();
        let date = if source_date.is_some() {
            Some(self.block_date(item, old, index, source_date, source_tz))
        } else {
            None
        };
        let rewritten = rewrite_block(
            &text,
            &parsed,
            self.input.tz,
            date.as_deref().unwrap_or(""),
            self.report,
        );
        if rewritten != text {
            set_block_text(item, position, rewritten);
        }

        let tz_name = self.input.tz.name();
        let is_full = parsed.has_environment_specific;
        if is_full {
            let mut next = EnvironmentEffectiveState {
                last_diff_shape: None,
                carries_current_date: date.is_some(),
                uses_content_item_kinds: self.state.uses_content_item_kinds,
                ..Default::default()
            };
            for (scalar, value, raw) in parsed.scalars() {
                next.set_scalar(
                    scalar,
                    scalar_effective_value(scalar, value, raw, date.as_deref(), tz_name),
                );
            }
            self.state = next;
            return false;
        }

        if parsed.has_unknown {
            self.merge_diff(&parsed, date.as_deref(), tz_name);
            return false;
        }
        let comparable = parsed
            .scalars()
            .filter(|(scalar, _, _)| *scalar != Scalar::Subagents)
            .collect::<Vec<_>>();
        let redundant = !comparable.is_empty()
            && comparable.iter().all(|(scalar, value, raw)| {
                let rendered =
                    scalar_effective_value(*scalar, *value, raw, date.as_deref(), tz_name);
                rendered.as_deref() == self.state.scalar_value(*scalar)
            });
        if redundant {
            // The diff says nothing new, but its scalar set is still the
            // shape this client build renders diffs in.
            self.state.last_diff_shape = Some(parsed.shape());
            self.report.blocks_removed += 1;
            return true;
        }
        self.merge_diff(&parsed, date.as_deref(), tz_name);
        false
    }

    fn merge_diff(
        &mut self,
        parsed: &ParsedEnvironmentBlock<'_>,
        date: Option<&str>,
        tz_name: &str,
    ) {
        for (scalar, value, raw) in parsed.scalars() {
            self.state.set_scalar(
                scalar,
                scalar_effective_value(scalar, value, raw, date, tz_name),
            );
        }
        if parsed.scalar_value(Scalar::CurrentDate).is_some() {
            self.state.carries_current_date = true;
        }
        let shape = parsed.shape();
        if !shape.is_empty() {
            self.state.last_diff_shape = Some(shape);
        }
    }

    /// The date a block must carry: derived from the block's own instant,
    /// else the next stamped item, else the request instant, else the
    /// source date moved across zones.
    fn block_date(
        &mut self,
        item: &Value,
        old: &[Value],
        index: usize,
        source_date: Option<&str>,
        source_tz: Option<&str>,
    ) -> String {
        if let Some((ms, source)) = item_instant(item) {
            self.report.count_instant(source);
            return date_in(self.input.tz, ms);
        }
        if let Some(ms) = next_instant(old, index + 1) {
            self.report.count_instant(InstantSource::NextItem);
            return date_in(self.input.tz, ms);
        }
        if let Some(date) = heuristic_date(self.input.tz, source_date, source_tz) {
            // Nothing in this request is stamped: a legacy client. The
            // request instant only applies to the turn being submitted now
            // (no model sample or tool output after this block).
            let is_tail = old[index + 1..].iter().all(|later| {
                !matches!(classify(later), ItemKind::ModelSide | ItemKind::ToolOutput)
            });
            if !is_tail {
                self.report.count_instant(InstantSource::Heuristic);
                return date;
            }
        }
        self.report.count_instant(InstantSource::TurnStart);
        date_in(
            self.input.tz,
            self.input
                .turn_started_at_unix_ms
                .unwrap_or(self.input.now_unix_ms),
        )
    }

    /// Turn start: the official client records the world-state diff with the
    /// other section diffs, in section order, before the prompt.
    fn insert_turn_start_diff(
        &mut self,
        out: &mut Vec<Value>,
        prompt: &Value,
        old: &[Value],
        index: usize,
    ) {
        if !self.state.carries_current_date || self.state.date.is_none() {
            return;
        }
        let prompt_turn = turn_id_of(prompt);
        let mut run_start = out.len();
        while run_start > 0 {
            let candidate = &out[run_start - 1];
            if classify(candidate) != ItemKind::WrapperMessage {
                break;
            }
            if let (Some(a), Some(b)) = (prompt_turn, turn_id_of(candidate)) {
                if a != b {
                    break;
                }
            }
            run_start -= 1;
        }
        // The client's own (kept) block in this turn's batch already set the
        // date at its instant; the official client renders at most one.
        if out[run_start..]
            .iter()
            .any(|item| !environment_block_positions(item).is_empty())
        {
            return;
        }
        let position = out[run_start..]
            .iter()
            .rposition(belongs_before_environments)
            .map_or(run_start, |offset| run_start + offset + 1);

        let anchor_key = anchor_key(prompt);
        let digest = self.digest(&anchor_key);
        let delta = 1 + (u64::from_be_bytes(digest[16..24].try_into().expect("8 bytes")) % 64);
        let prev_in_run = (position > run_start).then(|| &out[position - 1]);
        let prev_instant = prev_in_run.and_then(|prev| item_instant(prev).map(|(ms, _)| ms));
        let next_instant_ms = out[position..]
            .iter()
            .chain(std::iter::once(prompt))
            .find_map(|item| item_instant(item).map(|(ms, _)| ms))
            .or_else(|| next_instant(old, index + 1));
        let (candidate_ms, same_batch_prev) = match (prev_instant, next_instant_ms) {
            (Some(prev_ms), Some(next_ms)) if prev_ms <= next_ms => {
                (prev_ms, prev_in_run.and_then(item_id))
            }
            (Some(prev_ms), None) => (prev_ms, prev_in_run.and_then(item_id)),
            (_, Some(next_ms)) => (next_ms.saturating_sub(delta), None),
            (None, None) => (
                self.input
                    .turn_started_at_unix_ms
                    .unwrap_or(self.input.now_unix_ms),
                None,
            ),
        };
        let date = date_in(self.input.tz, candidate_ms);
        if Some(date.as_str()) == self.state.date.as_deref() {
            return;
        }
        let template = out[position..]
            .iter()
            .chain(std::iter::once(prompt))
            .find(|item| classify(item) != ItemKind::Other)
            .unwrap_or(prompt);
        let synthesized = self.synthesize_item(
            template,
            prompt_turn,
            candidate_ms,
            &digest,
            same_batch_prev,
            &date,
        );
        out.insert(position, synthesized);
        self.state.date = Some(date);
        self.report.blocks_inserted += 1;
    }

    /// Mid-turn: after the last tool output of a step, right before the model
    /// samples again (`record_step_world_state_if_changed`), or at the tail of
    /// a request that is about to sample.
    fn insert_after_tool_output(&mut self, out: &mut Vec<Value>, old: &[Value], index: usize) {
        if !self.state.carries_current_date || self.state.date.is_none() {
            return;
        }
        let next = old.get(index + 1);
        let is_tail = next.is_none();
        match next {
            None if !self.input.allow_tail_append => return,
            Some(next) if classify(next) != ItemKind::ModelSide => return,
            None | Some(_) => {}
        }
        let Some(tool_output) = out.last() else {
            return;
        };
        let anchor_key = anchor_key(tool_output);
        let digest = self.digest(&anchor_key);
        let delta = 1 + (u64::from_be_bytes(digest[16..24].try_into().expect("8 bytes")) % 64);
        let candidate_ms = match item_instant(tool_output) {
            Some((ms, _)) => ms + delta,
            None if is_tail => self
                .input
                .turn_started_at_unix_ms
                .unwrap_or(self.input.now_unix_ms),
            None => return,
        };
        let date = date_in(self.input.tz, candidate_ms);
        if Some(date.as_str()) == self.state.date.as_deref() {
            return;
        }
        let turn_id = out
            .iter()
            .rev()
            .find_map(turn_id_of)
            .or(self.input.turn_id)
            .map(str::to_string);
        let template = out
            .iter()
            .rev()
            .find(|item| {
                matches!(
                    classify(item),
                    ItemKind::RealPrompt | ItemKind::WrapperMessage
                )
            })
            .cloned();
        let template = template.as_ref().unwrap_or(tool_output);
        let synthesized = self.synthesize_item(
            template,
            turn_id.as_deref(),
            candidate_ms,
            &digest,
            None,
            &date,
        );
        out.push(synthesized);
        self.state.date = Some(date);
        if is_tail {
            self.report.blocks_appended += 1;
        } else {
            self.report.blocks_inserted += 1;
        }
    }

    fn digest(&self, anchor_key: &str) -> [u8; 32] {
        sha256(&[
            SYNTHESIZED_ID_DOMAIN,
            self.input.outbound_thread_id.as_bytes(),
            b"\0",
            anchor_key.as_bytes(),
        ])
    }

    fn synthesized_text(&self, date: &str) -> String {
        let mut text = String::from(ENVIRONMENT_CONTEXT_OPEN_TAG);
        text.push('\n');
        for scalar in self.state.synthesized_shape() {
            match scalar {
                Scalar::ShellVersion => {
                    if let Some(version) = &self.state.shell_version {
                        text.push_str("  <shell_version>");
                        text.push_str(&xml_escaped(version));
                        text.push_str("</shell_version>\n");
                    }
                }
                Scalar::CurrentDate => {
                    text.push_str("  <current_date>");
                    text.push_str(&xml_escaped(date));
                    text.push_str("</current_date>\n");
                }
                Scalar::Timezone => {
                    if self.state.timezone.is_some() {
                        text.push_str("  <timezone>");
                        text.push_str(&xml_escaped(self.input.tz.name()));
                        text.push_str("</timezone>\n");
                    }
                }
                Scalar::Network | Scalar::Filesystem => {
                    if let Some(raw) = self.state.scalar_value(scalar) {
                        text.push_str("  ");
                        text.push_str(raw);
                        text.push('\n');
                    }
                }
                Scalar::Subagents => {
                    if let Some(raw) = &self.state.subagents {
                        text.push_str(raw);
                        text.push('\n');
                    }
                }
            }
        }
        text.push_str(ENVIRONMENT_CONTEXT_CLOSE_TAG);
        text
    }

    /// A `role: user` message shaped like `template` (key order, whether an
    /// `id` / passthrough / `create_time` exists) carrying the synthesized
    /// block. The id is a UUIDv7 at `unix_ms` derived from the thread and the
    /// anchor, so replaying the same history mints the same id.
    fn synthesize_item(
        &self,
        template: &Value,
        turn_id: Option<&str>,
        unix_ms: u64,
        digest: &[u8; 32],
        same_batch_prev: Option<&str>,
        date: &str,
    ) -> Value {
        let text = self.synthesized_text(date);
        let template_keys: Vec<&str> = template
            .as_object()
            .map(|object| object.keys().map(String::as_str).collect())
            .unwrap_or_default();
        let template_has = |key: &str| template_keys.contains(&key);
        let mut item = Map::new();
        let content = Value::Array(vec![serde_json::json!({
            "type": "input_text",
            "text": text,
        })]);
        let mut wrote_type = false;
        let mut wrote_role = false;
        let mut wrote_content = false;
        let mut wrote_meta = false;
        let passthrough_value = self.synthesized_passthrough(template, turn_id, unix_ms);
        let id_value = template_has("id").then(|| {
            Value::String(format!(
                "msg_{}",
                synthesize_uuid(unix_ms, digest, same_batch_prev)
            ))
        });
        for key in &template_keys {
            match *key {
                "type" => {
                    item.insert("type".to_string(), Value::String("message".to_string()));
                    wrote_type = true;
                }
                "id" => {
                    if let Some(id) = &id_value {
                        item.insert("id".to_string(), id.clone());
                    }
                }
                "role" => {
                    item.insert("role".to_string(), Value::String("user".to_string()));
                    wrote_role = true;
                }
                "content" => {
                    item.insert("content".to_string(), content.clone());
                    wrote_content = true;
                }
                "internal_chat_message_metadata_passthrough" => {
                    if let Some(meta) = &passthrough_value {
                        item.insert(key.to_string(), meta.clone());
                    }
                    wrote_meta = true;
                }
                _ => {}
            }
        }
        if !wrote_type {
            item.insert("type".to_string(), Value::String("message".to_string()));
        }
        if !wrote_role {
            item.insert("role".to_string(), Value::String("user".to_string()));
        }
        if !wrote_content {
            item.insert("content".to_string(), content);
        }
        if !wrote_meta {
            if let Some(meta) = passthrough_value {
                item.insert(
                    "internal_chat_message_metadata_passthrough".to_string(),
                    meta,
                );
            }
        }
        Value::Object(item)
    }

    fn synthesized_passthrough(
        &self,
        template: &Value,
        turn_id: Option<&str>,
        unix_ms: u64,
    ) -> Option<Value> {
        let template_meta = passthrough(template);
        let mut meta = Map::new();
        if let Some(turn_id) = turn_id {
            meta.insert("turn_id".to_string(), Value::String(turn_id.to_string()));
        }
        if template_meta.is_some_and(|meta| meta.contains_key("create_time")) {
            if let Some(secs) = serde_json::Number::from_f64(unix_ms as f64 / 1000.0) {
                meta.insert("create_time".to_string(), Value::Number(secs));
            }
        }
        if self.state.uses_content_item_kinds {
            meta.insert(
                "content_item_kinds".to_string(),
                Value::Array(vec![Value::String(
                    ENVIRONMENT_CONTEXT_ITEM_KIND.to_string(),
                )]),
            );
        }
        (!meta.is_empty() || template_meta.is_some()).then(|| Value::Object(meta))
    }
}

fn scalar_effective_value(
    scalar: Scalar,
    value: Option<&str>,
    raw: &str,
    date: Option<&str>,
    tz_name: &str,
) -> Option<String> {
    match scalar {
        Scalar::CurrentDate => date.map(str::to_string),
        Scalar::Timezone => Some(tz_name.to_string()),
        Scalar::ShellVersion => value.map(str::to_string),
        Scalar::Network | Scalar::Filesystem | Scalar::Subagents => Some(raw.to_string()),
    }
}

/// Stable per-item key for id derivation: the item's id, else its call id,
/// else a hash of its text.
fn anchor_key(item: &Value) -> String {
    if let Some(id) = item_id(item) {
        return id.to_string();
    }
    if let Some(call_id) = item.get("call_id").and_then(Value::as_str) {
        return format!("call:{call_id}");
    }
    let text = item
        .get("content")
        .and_then(message_text)
        .or_else(|| {
            item.get("output")
                .and_then(|output| output.as_str().map(str::to_string))
        })
        .unwrap_or_default();
    format!("text:{}", hash16(&text))
}

/// Namespace the client derives its `msg_…` / `at_…` prefix-cache item ids
/// from: `uuid5(NAMESPACE_OID, thread_id)`, exactly as `build_responses_request`
/// does before hashing the payloads.
fn prefix_cache_namespace(outbound_thread_id: &str) -> Uuid {
    Uuid::new_v5(&Uuid::NAMESPACE_OID, outbound_thread_id.as_bytes())
}

/// Re-derives the prefix-cache item ids (`msg_…` base instructions, `at_…`
/// additional tools) from the outbound thread.
///
/// A client mints these once per thread — `uuid5(uuid5(NAMESPACE_OID,
/// thread_id), payload)` — so under identity synthesis they carry the inbound
/// thread forever: every swapped conversation ships one account a pair of ids
/// it never generated, and two folded conversations keep two different pairs
/// inside one thread. Only UUIDv5 suffixes are touched; the UUIDv7 ids of
/// replayed items are the client's own and stay as they are. Returns the
/// number of ids rewritten.
fn rewrite_prefix_cache_item_ids(body: &mut Value, outbound_thread_id: &str) -> usize {
    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return 0;
    };
    let namespace = prefix_cache_namespace(outbound_thread_id);
    let mut rewritten = 0;
    for item in items {
        let Some(id) = item_id(item) else {
            continue;
        };
        let Some((prefix, suffix)) = id.split_once('_') else {
            continue;
        };
        if !matches!(prefix, "msg" | "at") {
            continue;
        }
        let Ok(existing) = Uuid::parse_str(suffix) else {
            continue;
        };
        if existing.get_version_num() != 5 {
            continue;
        }
        // Same payloads the client hashes: the tools array for `at_…`, the
        // base instruction text for `msg_…`. Anything else keeps the shape.
        let payload = match prefix {
            "at" => item
                .get("tools")
                .and_then(|tools| serde_json::to_string(tools).ok()),
            _ => item
                .get("content")
                .and_then(message_text)
                .filter(|text| !text.is_empty()),
        };
        let Some(payload) = payload else {
            continue;
        };
        let derived = Uuid::new_v5(&namespace, payload.as_bytes());
        let derived = format!("{prefix}_{derived}");
        if derived == id {
            continue;
        }
        if let Some(object) = item.as_object_mut() {
            object.insert("id".to_string(), Value::String(derived));
            rewritten += 1;
        }
    }
    rewritten
}

/// UUIDv7 at `unix_ms`. When the previous item of the same batch shares the
/// millisecond, the new id keeps its high random bits and bumps the low 48 by
/// a hash-derived increment, the way a `ContextV7` counter sequence looks.
fn synthesize_uuid(unix_ms: u64, digest: &[u8; 32], same_batch_prev: Option<&str>) -> String {
    if let Some(prev) = same_batch_prev.and_then(|id| Uuid::parse_str(uuid_tail(id)).ok()) {
        if prev.get_version_num() == 7 && id_unix_millis(&prev.to_string()) == Some(unix_ms) {
            let mut random = *prev.as_bytes();
            let low = u64::from_be_bytes([
                0, 0, random[10], random[11], random[12], random[13], random[14], random[15],
            ]);
            let increment = 1
                + (u64::from_be_bytes(digest[24..32].try_into().expect("8 bytes")) % (1u64 << 36));
            let bumped = (low + increment) & ((1u64 << 48) - 1);
            random[10..16].copy_from_slice(&bumped.to_be_bytes()[2..8]);
            return uuid_v7_from_parts(unix_ms, &random);
        }
    }
    let mut random = [0u8; 16];
    random.copy_from_slice(&digest[..16]);
    uuid_v7_from_parts(unix_ms, &random)
}

/// `tools[].user_location` (codex-api `search.rs`) carries the user's
/// timezone; only sent when a user configured it explicitly.
fn strip_web_search_user_location(body: &mut Value, report: &mut EnvironmentContextRewriteReport) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    for tool in tools.iter_mut().filter_map(Value::as_object_mut) {
        let is_web_search = tool
            .get("type")
            .and_then(Value::as_str)
            .is_some_and(|kind| kind.starts_with("web_search"));
        if is_web_search && tool.remove("user_location").is_some() {
            report.user_location_removed += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NY: Tz = Tz::America__New_York;
    const FILESYSTEM: &str = "<filesystem><workspace_roots><root>D:\\workspace\\new-api</root></workspace_roots><permission_profile type=\"disabled\"><file_system type=\"unrestricted\" /></permission_profile></filesystem>";

    fn uuid_at(ms: u64, seed: u8) -> String {
        uuid_v7_from_parts(ms, &[seed; 16])
    }

    fn message(id: Option<&str>, role: &str, text: &str, turn: &str) -> Value {
        let mut item = Map::new();
        item.insert("type".into(), json!("message"));
        if let Some(id) = id {
            item.insert("id".into(), json!(id));
        }
        item.insert("role".into(), json!(role));
        item.insert(
            "content".into(),
            json!([{"type": "input_text", "text": text}]),
        );
        item.insert(
            "internal_chat_message_metadata_passthrough".into(),
            json!({"turn_id": turn}),
        );
        Value::Object(item)
    }

    fn env_message(ms: u64, seed: u8, text: &str, turn: &str) -> Value {
        message(
            Some(&format!("msg_{}", uuid_at(ms, seed))),
            "user",
            text,
            turn,
        )
    }

    fn prompt(ms: u64, seed: u8, text: &str, turn: &str) -> Value {
        message(
            Some(&format!("msg_{}", uuid_at(ms, seed))),
            "user",
            text,
            turn,
        )
    }

    fn developer(ms: u64, seed: u8, text: &str, turn: &str) -> Value {
        message(
            Some(&format!("msg_{}", uuid_at(ms, seed))),
            "developer",
            text,
            turn,
        )
    }

    fn tool_output(ms: u64, seed: u8) -> Value {
        json!({
            "type": "function_call_output",
            "id": format!("fco_{}", uuid_at(ms, seed)),
            "call_id": format!("call_{seed}"),
            "output": "ok"
        })
    }

    fn function_call(seed: u8) -> Value {
        json!({
            "type": "function_call",
            "id": format!("fc_{seed:032x}"),
            "call_id": format!("call_{seed}"),
            "name": "shell",
            "arguments": "{}"
        })
    }

    fn full_block(date: &str, tz: &str) -> String {
        format!(
            "<environment_context>\n  <cwd>D:\\workspace\\new-api</cwd>\n  <shell>powershell</shell>\n  <current_date>{date}</current_date>\n  <timezone>{tz}</timezone>\n  {FILESYSTEM}\n  <subagents>\n    - nginx_prechange_analysis_1: Laplace\n  </subagents>\n</environment_context>"
        )
    }

    fn diff_block(date: &str, tz: &str) -> String {
        format!(
            "<environment_context>\n  <current_date>{date}</current_date>\n  <timezone>{tz}</timezone>\n  {FILESYSTEM}\n</environment_context>"
        )
    }

    fn input_for<'a>(
        tz: Tz,
        now_ms: u64,
        allow_tail_append: bool,
    ) -> EnvironmentContextRewriteInput<'a> {
        EnvironmentContextRewriteInput {
            tz,
            now_unix_ms: now_ms,
            turn_started_at_unix_ms: Some(now_ms),
            turn_id: None,
            outbound_thread_id: "thread-outbound",
            allow_tail_append,
            prior_state: None,
        }
    }

    fn texts(body: &Value) -> Vec<String> {
        body["input"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| match item.get("content") {
                Some(Value::Array(parts)) => parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("|"),
                Some(Value::String(text)) => text.clone(),
                _ => item["type"].as_str().unwrap_or("").to_string(),
            })
            .collect()
    }

    // 2026-08-12 03:00 UTC = 2026-08-11 23:00 New York = 2026-08-12 11:00 Shanghai.
    const AUG12_0300_UTC: u64 = 1_786_503_600_000;
    // 2026-08-12 05:00 UTC = 2026-08-12 01:00 New York.
    const AUG12_0500_UTC: u64 = 1_786_510_800_000;

    #[test]
    fn epoch_constants_are_what_the_comments_say() {
        assert_eq!(date_in(Tz::UTC, AUG12_0300_UTC), "2026-08-12");
        assert_eq!(date_in(NY, AUG12_0300_UTC), "2026-08-11");
        assert_eq!(date_in(Tz::Asia__Shanghai, AUG12_0300_UTC), "2026-08-12");
        assert_eq!(date_in(NY, AUG12_0500_UTC), "2026-08-12");
    }

    // --- timezone policy ---------------------------------------------------

    #[test]
    fn timezone_policy_takes_the_first_valid_candidate_in_priority_order() {
        let policy = resolve_environment_timezone(&[
            (EnvironmentTimezoneSource::EnvOverride, None),
            (
                EnvironmentTimezoneSource::TzEnv,
                Some("Europe/Amsterdam".into()),
            ),
            (
                EnvironmentTimezoneSource::LocalTime,
                Some("America/Chicago".into()),
            ),
        ]);
        assert_eq!(policy.tz, Tz::Europe__Amsterdam);
        assert_eq!(policy.source, EnvironmentTimezoneSource::TzEnv);
        assert!(policy.rejected.is_empty());

        let policy = resolve_environment_timezone(&[
            (
                EnvironmentTimezoneSource::EnvOverride,
                Some(" :America/Los_Angeles ".into()),
            ),
            (
                EnvironmentTimezoneSource::TzEnv,
                Some("Europe/Amsterdam".into()),
            ),
        ]);
        assert_eq!(policy.tz, Tz::America__Los_Angeles);
        assert_eq!(policy.source, EnvironmentTimezoneSource::EnvOverride);
    }

    #[test]
    fn timezone_policy_rejects_utc_like_and_invalid_names() {
        for name in [
            "UTC",
            "Etc/UTC",
            "GMT",
            "EST",
            "EST5EDT",
            "Etc/GMT+8",
            "",
            "Mars/Olympus",
        ] {
            assert!(
                validate_environment_timezone(name).is_err(),
                "{name} must be rejected"
            );
        }
        let policy = resolve_environment_timezone(&[
            (EnvironmentTimezoneSource::TzEnv, Some("UTC".into())),
            (EnvironmentTimezoneSource::LocalTime, Some("Etc/UTC".into())),
        ]);
        assert_eq!(policy.tz, FALLBACK_ENVIRONMENT_TIMEZONE);
        assert_eq!(policy.source, EnvironmentTimezoneSource::Fallback);
        assert_eq!(
            policy.rejected,
            vec![
                (
                    EnvironmentTimezoneSource::TzEnv,
                    "UTC".to_string(),
                    "not_a_region_city_zone"
                ),
                (
                    EnvironmentTimezoneSource::LocalTime,
                    "Etc/UTC".to_string(),
                    "not_a_region_city_zone"
                ),
            ]
        );
    }

    #[test]
    fn timezone_policy_never_yields_a_denied_zone() {
        for denied in DENIED_ENVIRONMENT_TIMEZONES {
            assert_eq!(
                validate_environment_timezone(denied),
                Err("denied_region"),
                "{denied}"
            );
            assert_eq!(
                validate_environment_timezone(&denied.to_ascii_lowercase()),
                Err("denied_region"),
                "{denied} lowercase"
            );
        }
        let policy = resolve_environment_timezone(&[
            (
                EnvironmentTimezoneSource::EnvOverride,
                Some("Asia/Shanghai".into()),
            ),
            (
                EnvironmentTimezoneSource::TzEnv,
                Some("Asia/Hong_Kong".into()),
            ),
            (EnvironmentTimezoneSource::LocalTime, Some("PRC".into())),
        ]);
        assert_eq!(policy.tz, FALLBACK_ENVIRONMENT_TIMEZONE);
        assert_eq!(policy.rejected.len(), 3);
        assert!(policy
            .rejected
            .iter()
            .all(|(_, _, reason)| *reason == "denied_region"));
        assert_eq!(
            validate_environment_timezone("Asia/Tokyo"),
            Ok(Tz::Asia__Tokyo)
        );
        assert_eq!(
            validate_environment_timezone("Europe/Amsterdam"),
            Ok(Tz::Europe__Amsterdam)
        );
    }

    #[test]
    fn rewrite_switch_defaults_on_and_honors_off_values() {
        assert!(rewrite_switch_enabled(None));
        assert!(rewrite_switch_enabled(Some("on")));
        assert!(rewrite_switch_enabled(Some("1")));
        for off in ["off", "0", "false", " OFF ", "disabled", "no"] {
            assert!(!rewrite_switch_enabled(Some(off)), "{off}");
        }
    }

    // --- parsing / byte fidelity ------------------------------------------

    #[test]
    fn parser_classifies_every_official_shape() {
        let full = full_block("2026-08-10", "Asia/Shanghai");
        let parsed = parse_environment_block(&full).unwrap();
        assert!(parsed.has_environment_specific);
        assert!(!parsed.has_unknown);
        assert_eq!(
            parsed.shape(),
            vec![
                Scalar::CurrentDate,
                Scalar::Timezone,
                Scalar::Filesystem,
                Scalar::Subagents
            ]
        );
        assert_eq!(
            parsed.scalar_value(Scalar::Subagents).flatten(),
            Some("  <subagents>\n    - nginx_prechange_analysis_1: Laplace\n  </subagents>")
        );

        let diff = diff_block("2026-08-12", "Asia/Shanghai");
        let parsed = parse_environment_block(&diff).unwrap();
        assert!(!parsed.has_environment_specific);
        assert_eq!(
            parsed.shape(),
            vec![Scalar::CurrentDate, Scalar::Timezone, Scalar::Filesystem]
        );

        let date_only = "<environment_context>\n  <current_date>2026-08-12</current_date>\n</environment_context>";
        let parsed = parse_environment_block(date_only).unwrap();
        assert_eq!(parsed.shape(), vec![Scalar::CurrentDate]);

        let multi = "<environment_context>\n  <environments>\n    <environment id=\"local\" primary=\"true\">\n      <cwd>/a &amp; b</cwd>\n      <shell>zsh</shell>\n    </environment>\n    <environment id=\"remote\" status=\"unavailable\" />\n  </environments>\n  <shell_version status=\"unavailable\" />\n  <current_date>2026-08-12</current_date>\n  <timezone>UTC</timezone>\n  <network><allowed_domains><domain>example.com</domain></allowed_domains></network>\n</environment_context>";
        let parsed = parse_environment_block(multi).unwrap();
        assert!(parsed.has_environment_specific);
        assert!(!parsed.has_unknown);
        assert_eq!(parsed.scalar_value(Scalar::ShellVersion), Some(None));
        assert_eq!(
            parsed.shape(),
            vec![
                Scalar::ShellVersion,
                Scalar::CurrentDate,
                Scalar::Timezone,
                Scalar::Network
            ]
        );

        let empty = "<environment_context>\n</environment_context>";
        let parsed = parse_environment_block(empty).unwrap();
        assert!(parsed.lines.is_empty());
        assert!(!parsed.has_unknown);

        let inline = "<environment_context><cwd>/Users/alice/repo</cwd></environment_context>";
        let parsed = parse_environment_block(inline).unwrap();
        assert!(parsed.has_unknown);
        assert!(!parsed.has_environment_specific);

        let unknown = "<environment_context>\n  <cwd>/x</cwd>\n  <locale>zh-CN</locale>\n  <timezone>Asia/Shanghai</timezone>\n</environment_context>";
        let parsed = parse_environment_block(unknown).unwrap();
        assert!(parsed.has_unknown);
        assert!(parsed.has_environment_specific);

        assert!(parse_environment_block("<user_instructions>x</user_instructions>").is_none());
        assert!(parse_environment_block(
            "<environment_context>\n  <cwd>/x</cwd>\n</environment_context>\n"
        )
        .is_none());
    }

    #[test]
    fn rewrite_only_touches_the_two_lines_byte_for_byte() {
        let mut report = EnvironmentContextRewriteReport::default();
        let cases = [
            full_block("2026-08-10", "Asia/Shanghai"),
            diff_block("2026-08-12", "Asia/Hong_Kong"),
            "<environment_context>\n  <environments>\n    <environment id=\"local\" primary=\"true\">\n      <cwd>C:\\Users\\x &amp; y\\repo</cwd>\n      <shell>powershell</shell>\n    </environment>\n  </environments>\n  <shell_version>7.4</shell_version>\n  <current_date>2026-08-12</current_date>\n  <timezone>UTC</timezone>\n  <network><allowed_domains><domain>a.b</domain></allowed_domains></network>\n  <filesystem><workspace_roots><root>C:\\x</root></workspace_roots></filesystem>\n  <subagents>\n    - a: b\n    - c: d\n  </subagents>\n</environment_context>".to_string(),
            "<environment_context>\n  <cwd>/x</cwd>\n  <locale>zh-CN</locale>\n  <timezone>Asia/Shanghai</timezone>\n</environment_context>".to_string(),
        ];
        for text in cases {
            let parsed = parse_environment_block(&text).unwrap();
            let rewritten = rewrite_block(&text, &parsed, NY, "2026-08-11", &mut report);
            let expected = text
                .lines()
                .map(|line| {
                    if line.starts_with("  <timezone>") {
                        "  <timezone>America/New_York</timezone>".to_string()
                    } else if line.starts_with("  <current_date>") {
                        "  <current_date>2026-08-11</current_date>".to_string()
                    } else {
                        line.to_string()
                    }
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert_eq!(rewritten, expected);
        }
        // cwd-only and empty blocks: zero edits.
        for text in [
            "<environment_context><cwd>/Users/alice/repo</cwd></environment_context>",
            "<environment_context>\n  <cwd>/x</cwd>\n  <shell>zsh</shell>\n</environment_context>",
            "<environment_context>\n</environment_context>",
        ] {
            let parsed = parse_environment_block(text).unwrap();
            assert_eq!(
                rewrite_block(text, &parsed, NY, "2026-08-11", &mut report),
                text
            );
        }
    }

    #[test]
    fn dates_follow_the_target_zone_across_dst_and_midnight() {
        // 2026-03-08 06:59:59.999 UTC = 01:59:59.999 EST; +1ms jumps to EDT.
        let before_spring = 1_772_952_000_000u64 - 1;
        assert_eq!(date_in(NY, before_spring), "2026-03-08");
        assert_eq!(date_in(NY, before_spring + 1), "2026-03-08");
        // 2026-11-01 05:59:59.999 UTC = 01:59:59.999 EDT; +1 ms = 01:00 EST.
        let before_fall = 1_793_512_800_000u64 - 1;
        assert_eq!(date_in(NY, before_fall), "2026-11-01");
        assert_eq!(date_in(NY, before_fall + 1), "2026-11-01");
        // New York midnight 2026-08-12 = 04:00 UTC.
        let ny_midnight = 1_786_507_200_000u64;
        assert_eq!(date_in(NY, ny_midnight - 1), "2026-08-11");
        assert_eq!(date_in(NY, ny_midnight), "2026-08-12");
        assert_eq!(date_in(Tz::Europe__Amsterdam, ny_midnight), "2026-08-12");
        assert_eq!(
            heuristic_date(NY, Some("2026-08-12"), Some("Asia/Shanghai")),
            Some("2026-08-12".to_string())
        );
        assert_eq!(
            heuristic_date(
                Tz::Asia__Tokyo,
                Some("2026-08-12"),
                Some("America/Los_Angeles")
            ),
            Some("2026-08-13".to_string())
        );
        assert_eq!(heuristic_date(NY, None, None), None);
    }

    // --- instants -----------------------------------------------------------

    #[test]
    fn instants_prefer_create_time_then_own_uuidv7_and_skip_server_ids() {
        let mut item = env_message(AUG12_0300_UTC, 1, "x", "t");
        assert_eq!(
            item_instant(&item),
            Some((AUG12_0300_UTC, InstantSource::OwnId))
        );
        item["internal_chat_message_metadata_passthrough"]["create_time"] = json!(1_786_510_800.25);
        assert_eq!(
            item_instant(&item),
            Some((1_786_510_800_250, InstantSource::CreateTime))
        );
        for id in ["fco_", "ctco_", ""] {
            let item = json!({"type": "function_call_output", "id": format!("{id}{}", uuid_at(AUG12_0500_UTC, 2))});
            assert_eq!(
                item_instant(&item),
                Some((AUG12_0500_UTC, InstantSource::OwnId))
            );
        }
        for id in ["rs_03d98a0c1f", "fc_0123", "msg_03d98abcdef", "not-a-uuid"] {
            assert_eq!(item_instant(&json!({"id": id})), None, "{id}");
        }
        let v4 = json!({"id": format!("msg_{}", Uuid::new_v4())});
        assert_eq!(item_instant(&v4), None);
    }

    // --- replay ---------------------------------------------------------------

    #[test]
    fn full_block_is_rewritten_from_its_own_instant_and_never_removed() {
        let mut body = json!({"input": [
            env_message(AUG12_0300_UTC, 1, &full_block("2026-08-12", "Asia/Shanghai"), "t1"),
            prompt(AUG12_0300_UTC + 500, 2, "hello", "t1"),
        ]});
        let input = input_for(NY, AUG12_0300_UTC + 1000, true);
        let (report, state) = apply_codex_environment_context(&mut body, &input);
        let state = state.unwrap();
        assert_eq!(body["input"].as_array().unwrap().len(), 2);
        assert_eq!(
            texts(&body)[0],
            full_block("2026-08-11", "America/New_York")
        );
        assert_eq!(report.blocks_seen, 1);
        assert_eq!(report.timezone_rewritten, 1);
        assert_eq!(report.date_rewritten, 1);
        assert_eq!(report.instant_sources[InstantSource::OwnId as usize], 1);
        assert_eq!(report.blocks_inserted, 0);
        assert_eq!(state.date.as_deref(), Some("2026-08-11"));
        assert_eq!(state.timezone.as_deref(), Some("America/New_York"));
        assert!(state.carries_current_date);
        assert_eq!(state.filesystem.as_deref(), Some(FILESYSTEM));
        assert!(state.subagents.is_some());
        assert_eq!(state.last_diff_shape, None);
    }

    #[test]
    fn redundant_client_diff_is_removed_and_day_change_is_inserted_where_it_belongs() {
        // Shanghai flips to 08-12 at 16:00 UTC 08-11 while New York is still
        // 08-11: the client's diff carries nothing new after the rewrite.
        let t0 = AUG12_0300_UTC - 20 * 3_600_000; // 2026-08-11 07:00 UTC, 08-11 in both zones
        let mut body = json!({"input": [
            env_message(t0, 1, &full_block("2026-08-11", "Asia/Shanghai"), "t1"),
            prompt(t0 + 100, 2, "first", "t1"),
            json!({"type": "message", "id": "msg_03d98a", "role": "assistant", "content": [{"type": "output_text", "text": "hi"}]}),
            developer(AUG12_0300_UTC, 3, "<skills_instructions>\n## Skills\n</skills_instructions>", "t2"),
            env_message(AUG12_0300_UTC, 4, &diff_block("2026-08-12", "Asia/Shanghai"), "t2"),
            prompt(AUG12_0300_UTC + 3_300, 5, "second", "t2"),
            json!({"type": "message", "id": "msg_03d98b", "role": "assistant", "content": [{"type": "output_text", "text": "ok"}]}),
            // Third turn at 01:00 New York 08-12: the client (Shanghai, still
            // 08-12) sends no diff; the target zone needs one.
            developer(AUG12_0500_UTC, 6, "<skills_instructions>\n## Skills\n</skills_instructions>", "t3"),
            developer(AUG12_0500_UTC, 7, "You are `/root`, the primary agent in a team", "t3"),
            prompt(AUG12_0500_UTC + 2_000, 8, "third", "t3"),
        ]});
        let input = input_for(NY, AUG12_0500_UTC + 5_000, true);
        let (report, state) = apply_codex_environment_context(&mut body, &input);
        let items = body["input"].as_array().unwrap();
        let texts = texts(&body);
        assert_eq!(report.blocks_removed, 1, "{texts:#?}");
        assert_eq!(report.blocks_inserted, 1, "{texts:#?}");
        assert_eq!(items.len(), 10);
        // Turn 2: the redundant diff is gone; skills → prompt.
        assert!(texts[3].starts_with("<skills_instructions>"));
        assert_eq!(texts[4], "second");
        // Turn 3: skills → synthesized diff → team developer message → prompt.
        assert!(texts[6].starts_with("<skills_instructions>"));
        assert_eq!(
            texts[7],
            diff_block("2026-08-12", "America/New_York"),
            "synthesized diff copies the client's last diff shape"
        );
        assert!(texts[8].starts_with("You are `/root`"));
        assert_eq!(texts[9], "third");
        let synthesized = &items[7];
        assert_eq!(synthesized["type"], "message");
        assert_eq!(synthesized["role"], "user");
        assert_eq!(
            synthesized["internal_chat_message_metadata_passthrough"],
            json!({"turn_id": "t3"})
        );
        assert_eq!(
            synthesized.as_object().unwrap().keys().collect::<Vec<_>>(),
            vec![
                "type",
                "id",
                "role",
                "content",
                "internal_chat_message_metadata_passthrough"
            ]
        );
        let id = synthesized["id"].as_str().unwrap();
        assert!(id.starts_with("msg_"));
        // Same batch as the preceding skills message: same millisecond, sorts after it.
        assert_eq!(id_unix_millis(id), Some(AUG12_0500_UTC));
        assert!(id > items[6]["id"].as_str().unwrap());
        assert!(
            id < items[8]["id"].as_str().unwrap()
                || id_unix_millis(items[8]["id"].as_str().unwrap()) == Some(AUG12_0500_UTC)
        );
        let state = state.unwrap();
        assert_eq!(state.date.as_deref(), Some("2026-08-12"));
        assert_eq!(
            state.last_diff_shape,
            Some(vec![
                Scalar::CurrentDate,
                Scalar::Timezone,
                Scalar::Filesystem
            ])
        );
    }

    #[test]
    fn agents_md_user_message_is_a_section_item_and_keeps_the_client_diff_in_place() {
        // Mirrors sample input[383..=388]: AGENTS.md (user, no wrapper tag) →
        // skills → client diff → team developer messages → prompt, all in one
        // UUIDv7 batch. The client's own diff must stay exactly where it is.
        const AGENTS_MD: &str = "# AGENTS.md instructions for D:\\workspace\\new-api\n\n<INSTRUCTIONS>\nbe nice\n</INSTRUCTIONS>";
        let t0 = AUG12_0300_UTC - 20 * 3_600_000;
        // 08-17 06:15 UTC = 08-17 02:15 New York; Shanghai is 08-17 too.
        let t2 = AUG12_0300_UTC + 5 * 86_400_000 + 3 * 3_600_000 + 15 * 60_000;
        let history = |with_client_diff: bool| {
            let mut items = vec![
                env_message(t0, 1, &full_block("2026-08-11", "Asia/Shanghai"), "t1"),
                prompt(t0 + 100, 2, "first", "t1"),
                json!({"type": "message", "id": "msg_03d98a", "role": "assistant", "content": [{"type": "output_text", "text": "hi"}]}),
                env_message(t2, 3, AGENTS_MD, "t2"),
                developer(
                    t2,
                    4,
                    "<skills_instructions>\n## Skills\n</skills_instructions>",
                    "t2",
                ),
            ];
            if with_client_diff {
                items.push(env_message(
                    t2,
                    5,
                    &diff_block("2026-08-17", "Asia/Shanghai"),
                    "t2",
                ));
            }
            items.extend([
                developer(t2, 6, "You are `/root`, the primary agent in a team", "t2"),
                developer(
                    t2,
                    7,
                    "<multi_agent_mode>\nmulti\n</multi_agent_mode>",
                    "t2",
                ),
                prompt(t2 + 1_800, 8, "second", "t2"),
            ]);
            json!({"input": items})
        };

        let mut body = history(true);
        let input = input_for(NY, t2 + 5_000, true);
        let (report, _) = apply_codex_environment_context(&mut body, &input);
        let seen = texts(&body);
        assert_eq!(report.blocks_removed, 0, "{seen:#?}");
        assert_eq!(report.blocks_inserted, 0, "{seen:#?}");
        assert_eq!(report.date_rewritten, 0, "{seen:#?}");
        assert_eq!(seen.len(), 9);
        assert!(seen[3].starts_with("# AGENTS.md instructions for"));
        assert!(seen[4].starts_with("<skills_instructions>"));
        assert_eq!(seen[5], diff_block("2026-08-17", "America/New_York"));
        assert!(seen[6].starts_with("You are `/root`"));
        assert!(seen[7].starts_with("<multi_agent_mode>"));
        assert_eq!(seen[8], "second");

        // Same turn without the client's diff (e.g. the client sat in a zone
        // that had not rolled over): the synthesized block lands after the
        // last section that precedes Environments, i.e. after skills.
        let mut body = history(false);
        let (report, _) = apply_codex_environment_context(&mut body, &input);
        let seen = texts(&body);
        assert_eq!(report.blocks_inserted, 1, "{seen:#?}");
        assert_eq!(seen.len(), 9);
        assert!(seen[3].starts_with("# AGENTS.md instructions for"));
        assert!(seen[4].starts_with("<skills_instructions>"));
        assert_eq!(seen[5], diff_block("2026-08-17", "America/New_York"));
        assert!(seen[6].starts_with("You are `/root`"));
        let items = body["input"].as_array().unwrap();
        let id = items[5]["id"].as_str().unwrap();
        assert_eq!(id_unix_millis(id), Some(t2));
        assert!(id > items[4]["id"].as_str().unwrap());
        assert_eq!(
            items[5]["internal_chat_message_metadata_passthrough"],
            json!({"turn_id": "t2"})
        );
    }

    #[test]
    fn client_day_change_that_survives_the_rewrite_is_kept_and_not_duplicated() {
        // Amsterdam target; client in Shanghai. 2026-08-12 00:30 Amsterdam =
        // 2026-08-11 22:30 UTC = 08-12 06:30 Shanghai (client diff already 08-12).
        let full_at = AUG12_0300_UTC - 10 * 3_600_000; // 08-11 17:00 UTC → 08-11 19:00 Amsterdam
        let diff_at = AUG12_0300_UTC - 4 * 3_600_000 - 1_800_000; // 08-11 22:30 UTC → 08-12 00:30 Amsterdam
        let mut body = json!({"input": [
            env_message(full_at, 1, &full_block("2026-08-12", "Asia/Shanghai"), "t1"),
            prompt(full_at + 50, 2, "first", "t1"),
            env_message(diff_at, 3, &diff_block("2026-08-12", "Asia/Shanghai"), "t2"),
            prompt(diff_at + 40, 4, "second", "t2"),
        ]});
        let input = input_for(Tz::Europe__Amsterdam, diff_at + 100, true);
        let (report, _) = apply_codex_environment_context(&mut body, &input);
        let texts = texts(&body);
        assert_eq!(texts.len(), 4);
        assert_eq!(texts[0], full_block("2026-08-11", "Europe/Amsterdam"));
        assert_eq!(texts[2], diff_block("2026-08-12", "Europe/Amsterdam"));
        assert_eq!(report.blocks_removed, 0);
        assert_eq!(report.blocks_inserted, 0);
        assert_eq!(report.date_rewritten, 1);
        assert_eq!(report.timezone_rewritten, 2);
    }

    #[test]
    fn mid_turn_day_change_goes_after_the_last_tool_output_before_the_next_sample() {
        let ny_midnight = 1_786_507_200_000u64; // 2026-08-12 00:00 New York
        let mut body = json!({"input": [
            env_message(ny_midnight - 3_600_000, 1, &full_block("2026-08-12", "Asia/Shanghai"), "t1"),
            prompt(ny_midnight - 3_600_000 + 10, 2, "go", "t1"),
            function_call(1),
            tool_output(ny_midnight - 1_000, 3),
            function_call(2),
            tool_output(ny_midnight + 1_000, 4),
            tool_output(ny_midnight + 1_500, 5),
            function_call(3),
            tool_output(ny_midnight + 60_000, 6),
        ]});
        let input = input_for(NY, ny_midnight + 61_000, true);
        let (report, state) = apply_codex_environment_context(&mut body, &input);
        let items = body["input"].as_array().unwrap();
        let types: Vec<&str> = items
            .iter()
            .map(|item| item["type"].as_str().unwrap())
            .collect();
        assert_eq!(
            types,
            vec![
                "message",
                "message",
                "function_call",
                "function_call_output",
                "function_call",
                "function_call_output",
                "function_call_output",
                "message",
                "function_call",
                "function_call_output",
            ],
            "{:#?}",
            texts(&body)
        );
        assert_eq!(report.blocks_inserted, 1);
        assert_eq!(report.blocks_appended, 0);
        let synthesized = &items[7];
        assert_eq!(synthesized["role"], "user");
        assert_eq!(
            synthesized["internal_chat_message_metadata_passthrough"]["turn_id"],
            "t1"
        );
        let text = synthesized["content"][0]["text"].as_str().unwrap();
        assert_eq!(text, diff_block("2026-08-12", "America/New_York"));
        let id_ms = id_unix_millis(synthesized["id"].as_str().unwrap()).unwrap();
        assert!((ny_midnight + 1_501..=ny_midnight + 1_564).contains(&id_ms));
        assert_eq!(state.unwrap().date.as_deref(), Some("2026-08-12"));
    }

    #[test]
    fn tail_append_only_when_allowed_and_only_when_the_day_changed() {
        let ny_midnight = 1_786_507_200_000u64;
        let history = |tail_ms: u64| {
            json!({"input": [
                env_message(ny_midnight - 3_600_000, 1, &full_block("2026-08-12", "Asia/Shanghai"), "t1"),
                prompt(ny_midnight - 3_600_000 + 10, 2, "go", "t1"),
                function_call(1),
                tool_output(tail_ms, 3),
            ]})
        };
        let mut body = history(ny_midnight + 5);
        let (report, _) =
            apply_codex_environment_context(&mut body, &input_for(NY, ny_midnight + 10, true));
        assert_eq!(report.blocks_appended, 1);
        assert_eq!(body["input"].as_array().unwrap().len(), 5);
        assert_eq!(body["input"][4]["role"], "user");

        let mut body = history(ny_midnight + 5);
        let (report, _) =
            apply_codex_environment_context(&mut body, &input_for(NY, ny_midnight + 10, false));
        assert_eq!(report.blocks_appended, 0, "compaction never appends");
        assert_eq!(body["input"].as_array().unwrap().len(), 4);

        let mut body = history(ny_midnight - 5_000);
        let (report, _) =
            apply_codex_environment_context(&mut body, &input_for(NY, ny_midnight - 4_000, true));
        assert_eq!(report.blocks_appended, 0, "same day: nothing to say");
        assert_eq!(body["input"].as_array().unwrap().len(), 4);
    }

    #[test]
    fn output_for_a_history_prefix_is_a_prefix_of_the_next_request_output() {
        let ny_midnight = 1_786_507_200_000u64;
        let mut items = vec![
            env_message(
                ny_midnight - 7_200_000,
                1,
                &full_block("2026-08-12", "Asia/Shanghai"),
                "t1",
            ),
            prompt(ny_midnight - 7_200_000 + 10, 2, "one", "t1"),
            function_call(1),
            tool_output(ny_midnight + 100, 3),
        ];
        let mut first = json!({"input": items.clone()});
        let (_, _) =
            apply_codex_environment_context(&mut first, &input_for(NY, ny_midnight + 200, true));
        items.push(json!({"type": "message", "id": "msg_03d9", "role": "assistant", "content": [{"type": "output_text", "text": "done"}]}));
        items.push(developer(
            ny_midnight + 900_000,
            4,
            "<skills_instructions>x</skills_instructions>",
            "t2",
        ));
        items.push(prompt(ny_midnight + 900_000 + 20, 5, "two", "t2"));
        let mut second = json!({"input": items});
        let (report, _) = apply_codex_environment_context(
            &mut second,
            &input_for(NY, ny_midnight + 901_000, true),
        );
        let first_items = first["input"].as_array().unwrap();
        let second_items = second["input"].as_array().unwrap();
        assert_eq!(first_items.len(), 5, "tail appended in the first request");
        assert_eq!(
            &second_items[..5],
            &first_items[..],
            "same block, same id, same position"
        );
        assert_eq!(report.blocks_inserted, 1);
        assert_eq!(report.blocks_appended, 0);
        assert_eq!(
            second_items.len(),
            8,
            "no second day-change block for turn 2"
        );
    }

    #[test]
    fn legacy_date_only_diffs_merge_and_synthesize_in_their_own_shape() {
        let ny_midnight = 1_786_507_200_000u64;
        let legacy_full = "<environment_context>\n  <cwd>/home/u/repo</cwd>\n  <shell>bash</shell>\n  <current_date>2026-08-12</current_date>\n  <timezone>Asia/Shanghai</timezone>\n</environment_context>";
        let mut body = json!({"input": [
            env_message(ny_midnight - 3_600_000, 1, legacy_full, "t1"),
            prompt(ny_midnight - 3_600_000 + 10, 2, "one", "t1"),
            env_message(ny_midnight - 1_800_000, 3, "<environment_context>\n  <current_date>2026-08-12</current_date>\n</environment_context>", "t2"),
            prompt(ny_midnight - 1_800_000 + 10, 4, "two", "t2"),
            prompt(ny_midnight + 3_600_000, 5, "three", "t3"),
        ]});
        let (report, state) = apply_codex_environment_context(
            &mut body,
            &input_for(NY, ny_midnight + 3_600_100, true),
        );
        let texts = texts(&body);
        assert_eq!(report.blocks_removed, 1, "{texts:#?}");
        assert_eq!(report.blocks_inserted, 1, "{texts:#?}");
        assert_eq!(
            texts[0],
            legacy_full
                .replace("Asia/Shanghai", "America/New_York")
                .replace("2026-08-12", "2026-08-11")
        );
        assert_eq!(texts[1], "one");
        assert_eq!(texts[2], "two");
        assert_eq!(
            texts[3],
            "<environment_context>\n  <current_date>2026-08-12</current_date>\n</environment_context>"
        );
        assert_eq!(texts[4], "three");
        let state = state.unwrap();
        assert_eq!(
            state.timezone.as_deref(),
            Some("America/New_York"),
            "a date-only diff never clears the timezone"
        );
        assert_eq!(state.last_diff_shape, Some(vec![Scalar::CurrentDate]));
    }

    #[test]
    fn blocks_without_current_date_never_trigger_synthesis_and_unknown_tags_are_kept() {
        let ny_midnight = 1_786_507_200_000u64;
        let mut body = json!({"input": [
            env_message(ny_midnight - 3_600_000, 1, "<environment_context>\n  <cwd>/x</cwd>\n  <shell>zsh</shell>\n</environment_context>", "t1"),
            prompt(ny_midnight - 3_600_000 + 10, 2, "one", "t1"),
            prompt(ny_midnight + 10, 3, "two", "t2"),
        ]});
        let original = body.clone();
        let (report, state) =
            apply_codex_environment_context(&mut body, &input_for(NY, ny_midnight + 20, true));
        assert_eq!(body, original);
        assert_eq!(report.blocks_seen, 1);
        assert!(!state.unwrap().carries_current_date);

        let mut body = json!({"input": [
            env_message(ny_midnight - 3_600_000, 1, &full_block("2026-08-12", "Asia/Shanghai"), "t1"),
            prompt(ny_midnight - 3_600_000 + 10, 2, "one", "t1"),
            env_message(ny_midnight - 1_000, 3, "<environment_context>\n  <current_date>2026-08-12</current_date>\n  <timezone>Asia/Shanghai</timezone>\n  <locale>zh-CN</locale>\n</environment_context>", "t2"),
            prompt(ny_midnight - 990, 4, "two", "t2"),
        ]});
        let (report, _) =
            apply_codex_environment_context(&mut body, &input_for(NY, ny_midnight, true));
        assert_eq!(report.blocks_removed, 0);
        assert_eq!(report.unknown_child_tags, 1);
        assert_eq!(
            texts(&body)[2],
            "<environment_context>\n  <current_date>2026-08-11</current_date>\n  <timezone>America/New_York</timezone>\n  <locale>zh-CN</locale>\n</environment_context>"
        );
    }

    #[test]
    fn removal_keeps_content_item_kinds_aligned_and_drops_empty_items() {
        let t0 = AUG12_0300_UTC - 20 * 3_600_000;
        let mut combined = env_message(
            AUG12_0300_UTC,
            4,
            &diff_block("2026-08-12", "Asia/Shanghai"),
            "t2",
        );
        combined["content"] = json!([
            {"type": "input_text", "text": "<user_instructions>agents</user_instructions>"},
            {"type": "input_text", "text": diff_block("2026-08-12", "Asia/Shanghai")},
        ]);
        combined["internal_chat_message_metadata_passthrough"] = json!({
            "turn_id": "t2",
            "content_item_kinds": ["agents_md.instructions", "environments.environment_context"],
        });
        let mut string_content = env_message(AUG12_0300_UTC + 1, 5, "", "t2");
        string_content["content"] = json!(diff_block("2026-08-12", "Asia/Shanghai"));
        let mut body = json!({"input": [
            env_message(t0, 1, &full_block("2026-08-11", "Asia/Shanghai"), "t1"),
            prompt(t0 + 100, 2, "first", "t1"),
            combined,
            string_content,
            prompt(AUG12_0300_UTC + 3_300, 6, "second", "t2"),
            prompt(AUG12_0500_UTC + 2_000, 7, "third", "t3"),
        ]});
        let (report, _) = apply_codex_environment_context(
            &mut body,
            &input_for(NY, AUG12_0500_UTC + 5_000, true),
        );
        let items = body["input"].as_array().unwrap();
        assert_eq!(report.blocks_removed, 2);
        assert_eq!(items.len(), 6, "{:#?}", texts(&body));
        assert_eq!(items[2]["content"].as_array().unwrap().len(), 1);
        assert_eq!(
            items[2]["content"][0]["text"],
            "<user_instructions>agents</user_instructions>"
        );
        assert_eq!(
            items[2]["internal_chat_message_metadata_passthrough"]["content_item_kinds"],
            json!(["agents_md.instructions"])
        );
        assert_eq!(items[3]["content"][0]["text"], "second");
        // Turn 3 synthesized block inherits content_item_kinds because the history used it.
        assert_eq!(
            items[4]["internal_chat_message_metadata_passthrough"],
            json!({"turn_id": "t3", "content_item_kinds": ["environments.environment_context"]})
        );
        assert_eq!(
            items[4]["content"][0]["text"],
            diff_block("2026-08-12", "America/New_York")
        );
        assert_eq!(items[5]["content"][0]["text"], "third");
    }

    #[test]
    fn synthesized_ids_are_deterministic_per_thread_and_anchor() {
        let ny_midnight = 1_786_507_200_000u64;
        let history = json!({"input": [
            env_message(ny_midnight - 3_600_000, 1, &full_block("2026-08-12", "Asia/Shanghai"), "t1"),
            prompt(ny_midnight - 3_600_000 + 10, 2, "one", "t1"),
            prompt(ny_midnight + 1_000, 3, "two", "t2"),
        ]});
        let run = |thread: &str, now: u64| {
            let mut body = history.clone();
            let input = EnvironmentContextRewriteInput {
                outbound_thread_id: thread,
                ..input_for(NY, now, true)
            };
            let (report, _) = apply_codex_environment_context(&mut body, &input);
            assert_eq!(report.blocks_inserted, 1, "{:#?}", texts(&body));
            assert_eq!(body["input"][3]["content"][0]["text"], "two");
            body["input"][2]["id"].as_str().unwrap().to_string()
        };
        assert_eq!(
            run("a", ny_midnight + 1_100),
            run("a", ny_midnight + 99_999)
        );
        assert_ne!(run("a", ny_midnight + 1_100), run("b", ny_midnight + 1_100));
        let id = run("a", ny_midnight + 1_100);
        let ms = id_unix_millis(&id).unwrap();
        assert!((ny_midnight + 1_000 - 64..ny_midnight + 1_000).contains(&ms));
        // Structural shape: version 7, RFC variant, ContextV7 gap bits clear.
        let uuid = Uuid::parse_str(uuid_tail(&id)).unwrap();
        assert_eq!(uuid.get_version_num(), 7);
        assert!(matches!(uuid.as_bytes()[7] & 0x0C, 0));
    }

    #[test]
    fn same_batch_bump_keeps_high_bits_and_sorts_after_the_previous_id() {
        let ms = AUG12_0500_UTC;
        let prev = format!("msg_{}", uuid_at(ms, 0x5a));
        let digest = sha256(&[b"x"]);
        let next = synthesize_uuid(ms, &digest, Some(&prev));
        assert_eq!(id_unix_millis(&next), Some(ms));
        assert_eq!(
            &next[..24],
            &uuid_tail(&prev)[..24],
            "time + rand_a + variant nibble preserved"
        );
        assert!(next.as_str() > uuid_tail(&prev));
        // A previous id from another millisecond is ignored.
        let other = format!("msg_{}", uuid_at(ms - 1, 0x5a));
        assert_eq!(
            synthesize_uuid(ms, &digest, Some(&other)),
            synthesize_uuid(ms, &digest, None)
        );
    }

    #[test]
    fn incremental_step_uses_prior_state_and_only_appends() {
        let ny_midnight = 1_786_507_200_000u64;
        let mut first = json!({"input": [
            env_message(ny_midnight - 3_600_000, 1, &full_block("2026-08-12", "Asia/Shanghai"), "t1"),
            prompt(ny_midnight - 3_600_000 + 10, 2, "one", "t1"),
        ]});
        let (_, prior) = apply_codex_environment_context(
            &mut first,
            &input_for(NY, ny_midnight - 3_599_000, true),
        );
        let prior = prior.unwrap();
        assert_eq!(prior.date.as_deref(), Some("2026-08-11"));

        let mut step = json!({
            "previous_response_id": "resp_1",
            "input": [tool_output(ny_midnight + 5, 3)],
        });
        let input = EnvironmentContextRewriteInput {
            prior_state: Some(&prior),
            turn_id: Some("t-outbound"),
            ..input_for(NY, ny_midnight + 10, true)
        };
        let (report, next) = apply_codex_environment_context(&mut step, &input);
        assert_eq!(report.blocks_appended, 1);
        assert_eq!(step["input"].as_array().unwrap().len(), 2);
        assert_eq!(
            step["input"][1]["content"][0]["text"],
            diff_block("2026-08-12", "America/New_York")
        );
        // No item of the step carries a passthrough: the outbound turn id
        // given by the caller is what the next full replay would find.
        assert_eq!(
            step["input"][1]["internal_chat_message_metadata_passthrough"]["turn_id"],
            "t-outbound"
        );
        assert_eq!(next.unwrap().date.as_deref(), Some("2026-08-12"));

        // Without prior state an incremental step is left alone.
        let mut step =
            json!({"previous_response_id": "resp_1", "input": [tool_output(ny_midnight + 5, 3)]});
        let (report, _) =
            apply_codex_environment_context(&mut step, &input_for(NY, ny_midnight + 10, true));
        assert!(report.is_noop());
        assert_eq!(step["input"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn full_block_after_compaction_resets_state_and_is_kept() {
        let ny_midnight = 1_786_507_200_000u64;
        let mut body = json!({"input": [
            env_message(ny_midnight - 7_200_000, 1, &full_block("2026-08-12", "Asia/Shanghai"), "t1"),
            prompt(ny_midnight - 7_200_000 + 10, 2, "one", "t1"),
            json!({"type": "message", "role": "user", "content": [{"type": "input_text", "text": format!("{}summary", crate::codex_runtime_identity::COMPACT_SUMMARY_PREFIX)}]}),
            env_message(ny_midnight + 600_000, 3, &full_block("2026-08-12", "Asia/Shanghai"), "t2"),
            prompt(ny_midnight + 600_000 + 10, 4, "two", "t2"),
        ]});
        let (report, state) =
            apply_codex_environment_context(&mut body, &input_for(NY, ny_midnight + 601_000, true));
        assert_eq!(report.blocks_seen, 2);
        assert_eq!(report.blocks_removed, 0);
        assert_eq!(report.blocks_inserted, 0);
        assert_eq!(body["input"].as_array().unwrap().len(), 5);
        let texts = texts(&body);
        assert_eq!(texts[0], full_block("2026-08-11", "America/New_York"));
        assert_eq!(texts[3], full_block("2026-08-12", "America/New_York"));
        assert_eq!(state.unwrap().date.as_deref(), Some("2026-08-12"));
    }

    #[test]
    fn legacy_items_without_ids_fall_back_to_the_request_instant_and_the_heuristic() {
        let mut body = json!({"input": [
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": diff_block("2026-08-12", "Asia/Shanghai")}]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "one"}]},
            {"type": "message", "role": "assistant", "content": [{"type": "output_text", "text": "a"}]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": diff_block("2026-08-13", "Asia/Shanghai")}]},
            {"type": "message", "role": "user", "content": [{"type": "input_text", "text": "two"}]},
        ]});
        let input = EnvironmentContextRewriteInput {
            turn_started_at_unix_ms: Some(AUG12_0500_UTC),
            ..input_for(NY, AUG12_0500_UTC, true)
        };
        let (report, _) = apply_codex_environment_context(&mut body, &input);
        let texts = texts(&body);
        // First block: nothing stamped and a model sample follows → noon
        // heuristic (08-12 12:00 Shanghai = 08-12 00:00 New York).
        assert_eq!(report.instant_sources[InstantSource::Heuristic as usize], 1);
        // Second block belongs to the turn being submitted → request instant.
        assert_eq!(report.instant_sources[InstantSource::TurnStart as usize], 1);
        assert_eq!(texts[0], diff_block("2026-08-12", "America/New_York"));
        // The client's 08-12 → 08-13 (Shanghai) day change is 08-12 → 08-12
        // in New York: redundant, removed.
        assert_eq!(report.blocks_removed, 1, "{texts:#?}");
        assert_eq!(
            texts,
            vec![
                diff_block("2026-08-12", "America/New_York"),
                "one".to_string(),
                "a".to_string(),
                "two".to_string()
            ]
        );
        assert_eq!(report.blocks_inserted, 0);
    }

    #[test]
    fn string_input_gets_the_literal_rewrite() {
        let mut body = json!({"input": diff_block("2026-08-12", "Asia/Shanghai")});
        let (report, state) =
            apply_codex_environment_context(&mut body, &input_for(NY, AUG12_0300_UTC, true));
        assert_eq!(body["input"], diff_block("2026-08-11", "America/New_York"));
        assert_eq!(report.blocks_seen, 1);
        assert!(state.is_none());
    }

    #[test]
    fn web_search_user_location_is_stripped() {
        let mut body = json!({
            "input": [],
            "tools": [
                {"type": "web_search", "user_location": {"type": "approximate", "timezone": "Asia/Shanghai"}},
                {"type": "web_search_preview", "user_location": {"type": "approximate"}},
                {"type": "function", "name": "shell", "user_location": "not a web search"},
            ]
        });
        let (report, _) =
            apply_codex_environment_context(&mut body, &input_for(NY, AUG12_0300_UTC, true));
        assert_eq!(report.user_location_removed, 2);
        assert!(body["tools"][0].get("user_location").is_none());
        assert!(body["tools"][1].get("user_location").is_none());
        assert_eq!(body["tools"][2]["user_location"], "not a web search");
    }

    #[test]
    fn bodies_without_environment_blocks_are_untouched() {
        let mut body = json!({"input": [
            prompt(AUG12_0300_UTC, 1, "hello", "t1"),
            {"type": "message", "id": "msg_03d98a", "role": "assistant", "content": [{"type": "output_text", "text": "hi"}]},
        ]});
        let original = body.clone();
        let (report, state) =
            apply_codex_environment_context(&mut body, &input_for(NY, AUG12_0300_UTC + 5, true));
        assert_eq!(body, original);
        assert!(report.is_noop());
        assert_eq!(state, Some(EnvironmentEffectiveState::default()));
    }

    /// Eyeball run against a captured production request:
    /// `cargo test -p aether-gateway environment_context_sample -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn environment_context_sample_eyeball() {
        let path = std::env::var("AETHER_ENV_CONTEXT_SAMPLE").unwrap_or_else(|_| {
            "/var/tmp/9142aa36-7d3f-416b-92bf-a338cd8f5e49.request.json".into()
        });
        let Ok(raw) = std::fs::read_to_string(&path) else {
            eprintln!("sample {path} not readable; skipping");
            return;
        };
        let mut body: Value = serde_json::from_str(&raw).expect("sample json");
        let before = body["input"].as_array().unwrap().len();
        let now = crate::codex_runtime_identity::unix_millis(std::time::SystemTime::now());
        let (report, state) = apply_codex_environment_context(&mut body, &input_for(NY, now, true));
        eprintln!(
            "items {before} -> {}",
            body["input"].as_array().unwrap().len()
        );
        eprintln!("{report:#?}");
        eprintln!("{state:#?}");
        let items = body["input"].as_array().unwrap();
        for (index, item) in items.iter().enumerate() {
            let positions = environment_block_positions(item);
            if positions.is_empty() {
                continue;
            }
            let neighbor = |offset: isize| -> String {
                let at = index as isize + offset;
                if at < 0 || at as usize >= items.len() {
                    return "-".into();
                }
                let n = &items[at as usize];
                format!(
                    "{} {} {:?} {:?}",
                    n["type"].as_str().unwrap_or(""),
                    n["role"].as_str().unwrap_or(""),
                    n["id"].as_str().unwrap_or(""),
                    n.get("content")
                        .and_then(message_text)
                        .map(|t| t.chars().take(60).collect::<String>())
                )
            };
            eprintln!(
                "--- input[{index}] id={:?} turn={:?}",
                item_id(item),
                turn_id_of(item)
            );
            eprintln!("    prev: {}", neighbor(-1));
            for position in positions {
                eprintln!("    {}", block_text(item, position).unwrap_or(""));
            }
            eprintln!("    next: {}", neighbor(1));
        }
    }

    #[test]
    fn prefix_cache_item_ids_follow_the_outbound_thread() {
        let inbound_thread = "01a07550-6ae3-72c2-aeca-79892a6f0c61";
        let instructions = "You are Codex, an agent based on GPT-6.";
        let tools = json!([{"type": "function", "name": "exec_command"}]);
        let client_ns = Uuid::new_v5(&Uuid::NAMESPACE_OID, inbound_thread.as_bytes());
        let client_at = format!(
            "at_{}",
            Uuid::new_v5(
                &client_ns,
                serde_json::to_string(&tools).unwrap().as_bytes()
            )
        );
        let client_msg = format!("msg_{}", Uuid::new_v5(&client_ns, instructions.as_bytes()));

        let build = || {
            json!({"input": [
                {"type": "additional_tools", "id": client_at, "role": "developer", "tools": tools},
                {"type": "message", "id": client_msg, "role": "developer",
                 "content": [{"type": "input_text", "text": instructions}]},
                {"type": "message", "id": format!("msg_{}", uuid_at(AUG12_0500_UTC, 7)),
                 "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
            ]})
        };

        let run = |thread: &str| {
            let mut body = build();
            let input = EnvironmentContextRewriteInput {
                outbound_thread_id: thread,
                ..input_for(NY, AUG12_0500_UTC, true)
            };
            apply_codex_environment_context(&mut body, &input);
            body
        };

        let outbound = "01a08a94-bc76-7270-a2e0-91c745342d13";
        let body = run(outbound);
        let ns = Uuid::new_v5(&Uuid::NAMESPACE_OID, outbound.as_bytes());
        for (index, payload) in [
            (0usize, serde_json::to_string(&tools).unwrap()),
            (1, instructions.to_string()),
        ] {
            let prefix = if index == 0 { "at" } else { "msg" };
            assert_eq!(
                body["input"][index]["id"].as_str().unwrap(),
                format!("{prefix}_{}", Uuid::new_v5(&ns, payload.as_bytes())),
                "re-derived from the outbound thread"
            );
        }
        // A UUIDv7 replay id is the client's own and stays untouched.
        assert_eq!(
            body["input"][2]["id"].as_str().unwrap(),
            format!("msg_{}", uuid_at(AUG12_0500_UTC, 7))
        );

        // Deterministic per thread, and a different thread yields a different
        // pair — the leak the pass exists to close.
        assert_eq!(run(outbound), body);
        assert_ne!(run("other-thread"), body);
    }

    #[test]
    fn prefix_cache_rewrite_leaves_items_it_cannot_derive_alone() {
        let client_ns = Uuid::new_v5(&Uuid::NAMESPACE_OID, b"inbound");
        let at = format!(
            "at_{}",
            Uuid::new_v5(&client_ns, br#"[{"type":"function"}]"#)
        );
        let mut body = json!({"input": [
            // v5 tool id but no tools payload to hash.
            {"type": "additional_tools", "id": at, "role": "developer"},
            // server-minted and UUIDv7 ids are never touched
            {"type": "function_call", "id": "fc_0123", "call_id": "c1", "name": "n", "arguments": "{}"},
            {"type": "message", "id": format!("msg_{}", uuid_at(AUG12_0500_UTC, 9)),
             "role": "user", "content": [{"type": "input_text", "text": "hi"}]},
        ]});
        let before = body.clone();
        let input = EnvironmentContextRewriteInput {
            outbound_thread_id: "thread-outbound",
            ..input_for(NY, AUG12_0500_UTC, true)
        };
        apply_codex_environment_context(&mut body, &input);
        assert_eq!(body["input"][0]["id"], before["input"][0]["id"]);
        assert_eq!(body["input"][1]["id"], before["input"][1]["id"]);
        assert_eq!(body["input"][2]["id"], before["input"][2]["id"]);
    }
}
