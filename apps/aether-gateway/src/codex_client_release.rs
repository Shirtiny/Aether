//! Codex pool client-release follow.
//!
//! A pool account advertises one frozen `user-agent` (the account's concrete
//! profile). The upstream build it names keeps moving: `codex-rs` ships a
//! stable minor every 2-4 days with several point patches, and the request
//! bodies a client sends follow that build. A 0.153.4 user-agent paired with
//! the metadata a 0.151 client emits is a shape no single build produces.
//!
//! Freezing the version forever has the opposite failure: the pool drifts
//! further behind with every release until the account looks like one nobody
//! has updated in weeks. This module lets the frozen user-agent follow the
//! releases real inbound clients are already running, one account at a time.
//!
//! Rules:
//!
//! - Every stable version inbound clients are seen running is tracked, per
//!   originator family (`codex-tui/`, `Codex Desktop/`, `codex_vscode/`,
//!   ...). Pre-release builds (`-alpha.N`, `-beta.N`) never enter the
//!   registry, and non-Codex products (`curl`, `Go-http-client`) are ignored.
//! - An account adopts the newest version that has been out for at least
//!   `lag` seconds, drawn per account from its selection fingerprint, so
//!   accounts do not all upgrade on the same request. The lag is measured
//!   from a version's first-seen second, while its last-seen second decides
//!   whether it stays in the registry at all.
//! - The adopted version is monotonic: it never goes below the frozen
//!   version and never returns to an older one. Registry/Redis loss falls
//!   back to the frozen user-agent.
//! - Only the version token is swapped, in both places the client keeps it
//!   (the product token and the build suffix). The OS, arch and terminal the
//!   account claims stay frozen, as do originator and every other identity
//!   plane.
//!
//! The observable result is one developer who updates Codex every few days,
//! with the pool's accounts spread across the last few builds - the shape a
//! group of real users has. See
//! `docs/architecture/codex-pool-runtime-identity-synthesis-plan-2026-09-03.md`
//! section 18.21.

use std::collections::BTreeMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Redis key prefix of the per-family client release registry. The version
/// high-water mark and its first-seen stamp live in one key per family.
const RELEASE_KEY_DOMAIN: &str = "aether:codex:client_release:v1";
/// A version observed longer ago than this is dropped from the registry:
/// nothing still adopting a build older than 30 days looks like an active
/// developer, and the key stays bounded.
const RELEASE_MAX_AGE_SECS: u64 = 30 * 86_400;
/// Key lifetime: refreshed on every observation, so a family that stops
/// sending traffic expires instead of pinning an ancient version forever.
const RELEASE_TTL_SECS: u64 = 45 * 86_400;
/// How many distinct versions one family tracks. Real traffic carries a
/// handful; the cap keeps a noisy scanner from growing the key.
const RELEASE_MAX_VERSIONS: usize = 24;
/// Longest per-account adoption delay. A 0.154.0 released today reaches an
/// account at the far end of this window at most that many days later, so
/// the pool is never more than a few releases behind.
const ADOPTION_MAX_LAG_SECS: u64 = 4 * 86_400;
/// Sampling window for the in-process observation throttle: one identical
/// observation per family+version per window reaches Redis. Every other
/// request is already invisible to the registry because its version is
/// recorded.
const OBSERVATION_THROTTLE_SECS: u64 = 300;

/// A parsed stable `major.minor.patch` version. Pre-release builds do not
/// enter the registry, so no pre-release fields are carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct ClientVersion {
    pub(crate) major: u64,
    pub(crate) minor: u64,
    pub(crate) patch: u64,
}

impl ClientVersion {
    /// Parses a bare stable version (`0.154.0`). A pre-release suffix
    /// (`-alpha.2`) or an unparsable token makes this `None`, which keeps
    /// pre-release builds out of the registry.
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim();
        if raw.is_empty() || raw.contains('-') || raw.contains('+') {
            return None;
        }
        let mut parts = raw.split('.');
        let major = parts.next()?.parse().ok()?;
        let minor = parts.next()?.parse().ok()?;
        let patch = parts.next().unwrap_or("0").parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
        })
    }

    pub(crate) fn to_string(self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// One payload of the per-family release registry: a stable version observed
/// inbound on this gateway, the unix second it was first seen, and the unix
/// second it was last seen. Two stamps because the two questions differ: an
/// account may adopt a build only after it has been out for a while
/// (first-seen), while a build real clients have stopped sending should leave
/// the registry (last-seen).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ClientReleaseRecord {
    pub(crate) version: ClientVersion,
    pub(crate) first_seen_unix_secs: u64,
    pub(crate) last_seen_unix_secs: u64,
}

impl ClientReleaseRecord {
    /// `version:first_seen:last_seen`.
    fn parse(raw: &str) -> Option<Self> {
        let mut parts = raw.split(':');
        let version = ClientVersion::parse(parts.next()?)?;
        let first_seen_unix_secs = parts.next()?.parse().ok()?;
        let last_seen_unix_secs = parts.next()?.parse().ok()?;
        if parts.next().is_some() {
            return None;
        }
        Some(Self {
            version,
            first_seen_unix_secs,
            last_seen_unix_secs,
        })
    }

    fn serialize(&self) -> String {
        format!(
            "{}:{}:{}",
            self.version.to_string(),
            self.first_seen_unix_secs,
            self.last_seen_unix_secs
        )
    }
}

/// Redis key of one originator family's release registry.
pub(crate) fn codex_client_release_key(provider_id: &str, family: &str) -> String {
    format!("{RELEASE_KEY_DOMAIN}:{provider_id}:{family}")
}

/// Originator family of a user-agent: the product name before the first
/// version slash, lowercased (`codex-tui/0.154.0 (...)` -> `codex-tui`,
/// `Codex Desktop/0.153.4 (...)` -> `codex desktop`). The multi-word desktop
/// product stays one family; the client rotates nothing here, only the
/// version after the slash moves.
///
/// Only products that name Codex take part: `curl`, `Go-http-client` and the
/// like have a product/version shape too, but their "version" is a protocol
/// revision, and tracking it would grow a registry entry per relay build for
/// a family no account advertises.
pub(crate) fn codex_client_family_from_user_agent(user_agent: &str) -> Option<String> {
    let product = user_agent.split('(').next().unwrap_or(user_agent);
    let (family, _) = product.split_once('/')?;
    let family = family.trim().to_ascii_lowercase();
    if family.is_empty() || family.len() > 64 {
        return None;
    }
    if !family.contains("codex") {
        return None;
    }
    // Spaces are allowed: the desktop client names itself with two words,
    // and the family is only ever a key segment, never a header value.
    if !family
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || "-_. ".contains(character))
    {
        return None;
    }
    Some(family)
}

/// Version of a user-agent, only when it is a stable release. The version
/// token is whatever follows the first slash of the product segment, so a
/// two-word product name (`Codex Desktop/0.153.4`) parses too.
///
/// The family gate is applied here as well as in
/// [`codex_client_family_from_user_agent`], because callers ask the two
/// questions separately: `Go-http-client/2.0` has a product/version shape but
/// its version is a protocol revision, not a build of a client the pool
/// advertises, and `2.0.0` must never reach the registry.
pub(crate) fn codex_client_stable_version(user_agent: &str) -> Option<ClientVersion> {
    codex_client_family_from_user_agent(user_agent)?;
    let product = user_agent.split('(').next().unwrap_or(user_agent);
    let (_, version) = product.split_once('/')?;
    let version = version.split_whitespace().next()?;
    ClientVersion::parse(version)
}

/// Per-account adoption delay, drawn from the selection fingerprint. The
/// same account always has the same delay, so an account's version never
/// jumps back and forth as clocks move.
pub(crate) fn codex_client_adoption_lag_secs(selection_fp: &str) -> u64 {
    let digest = crate::codex_runtime_identity::sha256(&[
        b"aether:codex:client_release:lag:v1",
        selection_fp.as_bytes(),
    ]);
    u64::from_be_bytes(digest[..8].try_into().expect("8 bytes")) % ADOPTION_MAX_LAG_SECS
}

/// Whether an observed version has been out long enough for this account to
/// adopt it. The build's first-seen stamp, not its last-seen one, is what the
/// lag is measured against: a busy family re-records every version it carries,
/// and measuring against last-seen would push adoption out forever.
pub(crate) fn codex_client_version_is_eligible(
    record: &ClientReleaseRecord,
    lag_secs: u64,
    now_unix_secs: u64,
) -> bool {
    now_unix_secs.saturating_sub(record.first_seen_unix_secs) >= lag_secs
}

/// The version an account should advertise: the higher of the frozen
/// version and the newest eligible observed version. `None` when the frozen
/// user-agent already carries the newest eligible version (or newer).
pub(crate) fn select_codex_client_version(
    frozen: ClientVersion,
    records: &[ClientReleaseRecord],
    lag_secs: u64,
    now_unix_secs: u64,
) -> Option<ClientVersion> {
    let target = records
        .iter()
        .filter(|record| codex_client_version_is_eligible(record, lag_secs, now_unix_secs))
        .map(|record| record.version)
        .max()?;
    (target > frozen).then_some(target)
}

/// Swaps the version token of a user-agent's product segment, leaving OS,
/// arch and terminal untouched. The frozen user-agent always parses (it is
/// one Aether wrote), but a malformed one is returned unchanged rather than
/// half-rewritten.
///
/// The frozen profile writes the version twice, as the product token and
/// again in the build suffix (`codex-tui/0.153.4 (...) (codex-tui; 0.153.4)`),
/// and a real client keeps the two in lockstep. Rewriting only the first
/// would hand an inspector a client whose build suffix contradicts its own
/// product token, so both move together. A trailing field that is _not_ a
/// repeat of the version (`... (VS Code; 26.901.22334)`) names something
/// else, and is left alone.
pub(crate) fn compose_codex_client_user_agent(
    frozen_user_agent: &str,
    version: ClientVersion,
) -> String {
    let Some((product, rest)) = frozen_user_agent.split_once('(') else {
        return frozen_user_agent.to_string();
    };
    // The product segment carries the separator space before `(`; the rebuild
    // below re-adds exactly one, so drop it here or the result gains a
    // double space the frozen profile never had.
    let product = product.trim_end();
    let Some(slash) = product.find('/') else {
        return frozen_user_agent.to_string();
    };
    let version_start = slash + 1;
    let version_end = product[version_start..]
        .find(char::is_whitespace)
        .map(|offset| version_start + offset)
        .unwrap_or(product.len());
    let Some(frozen_version) = ClientVersion::parse(&product[version_start..version_end]) else {
        return frozen_user_agent.to_string();
    };
    let rendered = version.to_string();
    // The parenthetical group ends with `(<product>; <version>)` when the
    // client keeps the version there at all, and the trailing field holds
    // something else when it does not (`... (VS Code; 26.901.22334)`, a
    // build number that also parses as three numbers). Only a field that
    // repeats the frozen version is the second copy of the version token;
    // anything else is left alone rather than overwritten with a build the
    // client never shipped.
    let rest = match rest.rsplit_once(';') {
        Some((head, tail)) => {
            let trailing = tail.trim_end_matches(')');
            let suffix = trailing.trim();
            if ClientVersion::parse(suffix) == Some(frozen_version) {
                format!("{head}; {rendered}{}", &tail[trailing.len()..])
            } else {
                rest.to_string()
            }
        }
        None => rest.to_string(),
    };
    format!(
        "{}{}{} ({rest}",
        &product[..version_start],
        rendered,
        &product[version_end..]
    )
}

/// In-process throttle for registry observations: the same family and
/// version is written to Redis at most once per window, so a busy family
/// costs one write per five minutes instead of one per request.
static OBSERVATION_THROTTLE: LazyLock<Mutex<BTreeMap<String, u64>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

/// Whether an observation of `family`/`version` should reach the registry at
/// `now_unix_secs`, marking it seen so later requests in the window do not
/// repeat the write.
pub(crate) fn note_codex_client_observation(
    family: &str,
    version: ClientVersion,
    now_unix_secs: u64,
) -> bool {
    let key = format!("{family}:{}", version.to_string());
    let Ok(mut throttle) = OBSERVATION_THROTTLE.lock() else {
        return true;
    };
    if let Some(seen) = throttle.get(&key) {
        if now_unix_secs.saturating_sub(*seen) < OBSERVATION_THROTTLE_SECS {
            return false;
        }
    }
    if throttle.len() > 256 {
        let cutoff = now_unix_secs.saturating_sub(OBSERVATION_THROTTLE_SECS);
        throttle.retain(|_, seen| *seen >= cutoff);
    }
    throttle.insert(key, now_unix_secs);
    true
}

/// Read-path resolution of the version one account should advertise.
///
/// `frozen_user_agent` is the account's concrete profile user-agent; it is
/// returned unchanged whenever the registry cannot answer, when no observed
/// version is eligible yet, or when the frozen version is already the
/// newest. Every failure mode degrades to the frozen UA.
pub(crate) async fn resolve_codex_client_user_agent(
    store: &ClientReleaseStore<'_>,
    frozen_user_agent: &str,
    selection_fp: &str,
    now_unix_secs: u64,
) -> String {
    let Some(family) = codex_client_family_from_user_agent(frozen_user_agent) else {
        return frozen_user_agent.to_string();
    };
    let Some(frozen) = codex_client_stable_version(frozen_user_agent) else {
        return frozen_user_agent.to_string();
    };
    let records = match store.release_records(&family).await {
        Ok(records) => records,
        Err(_) => return frozen_user_agent.to_string(),
    };
    let lag_secs = codex_client_adoption_lag_secs(selection_fp);
    match select_codex_client_version(frozen, &records, lag_secs, now_unix_secs) {
        Some(version) => compose_codex_client_user_agent(frozen_user_agent, version),
        None => frozen_user_agent.to_string(),
    }
}

/// Records the stable client version an inbound request came from, so the
/// registry (and every account on it) can follow the build real clients run.
/// Pre-release builds and unknown families are ignored.
pub(crate) async fn observe_codex_client_release(
    store: &ClientReleaseStore<'_>,
    user_agent: Option<&str>,
    now_unix_secs: u64,
) {
    let Some(user_agent) = user_agent else {
        return;
    };
    let Some(family) = codex_client_family_from_user_agent(user_agent) else {
        return;
    };
    let Some(version) = codex_client_stable_version(user_agent) else {
        return;
    };
    if !note_codex_client_observation(&family, version, now_unix_secs) {
        return;
    }
    let _ = store.record_release(&family, version, now_unix_secs).await;
}

pub(crate) fn unix_now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

pub(crate) const CLIENT_RELEASE_TTL: Duration = Duration::from_secs(RELEASE_TTL_SECS);
pub(crate) const CLIENT_RELEASE_MAX_AGE_SECS: u64 = RELEASE_MAX_AGE_SECS;
pub(crate) const CLIENT_RELEASE_MAX_VERSIONS: usize = RELEASE_MAX_VERSIONS;

// ---------------------------------------------------------------------------
// Registry storage
// ---------------------------------------------------------------------------

/// Reads and writes the per-family client release registry through the shared
/// runtime state. Everything is best-effort: a Redis outage degrades to the
/// frozen user-agent, never to an error a caller has to handle.
pub(crate) struct ClientReleaseStore<'a> {
    runtime: &'a aether_runtime_state::RuntimeState,
    /// Registry namespace; the pool provider id, so two Codex providers on
    /// one gateway do not share a version timeline.
    provider_id: String,
    #[cfg(test)]
    unavailable: bool,
}

impl<'a> ClientReleaseStore<'a> {
    pub(crate) fn new(runtime: &'a aether_runtime_state::RuntimeState, provider_id: &str) -> Self {
        Self {
            runtime,
            provider_id: provider_id.to_string(),
            #[cfg(test)]
            unavailable: false,
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable(
        runtime: &'a aether_runtime_state::RuntimeState,
        provider_id: &str,
    ) -> Self {
        Self {
            runtime,
            provider_id: provider_id.to_string(),
            unavailable: true,
        }
    }

    fn key(&self, family: &str) -> String {
        codex_client_release_key(&self.provider_id, family)
    }

    fn check(&self) -> Result<(), String> {
        #[cfg(test)]
        if self.unavailable {
            return Err("runtime state unavailable (test)".to_string());
        }
        Ok(())
    }

    /// Every tracked version of one family, newest first.
    pub(crate) async fn release_records(
        &self,
        family: &str,
    ) -> Result<Vec<ClientReleaseRecord>, String> {
        self.check()?;
        let key = self.key(family);
        let members = self
            .runtime
            .score_range_by_min(&key, 0.0)
            .await
            .map_err(|error| error.to_string())?;
        let mut records = members
            .into_iter()
            .filter_map(|member| ClientReleaseRecord::parse(&member))
            .collect::<Vec<_>>();
        records.sort_by(|left, right| right.version.cmp(&left.version));
        Ok(records)
    }

    /// Every originator family with a registry key under this provider,
    /// sorted. Scans the provider's key prefix; the key layout is
    /// `{domain}:{provider_id}:{family}`.
    pub(crate) async fn families(&self) -> Result<Vec<String>, String> {
        self.check()?;
        let prefix = format!("{RELEASE_KEY_DOMAIN}:{}:", self.provider_id);
        let keys = self
            .runtime
            .scan_keys(&format!("{prefix}*"), 100)
            .await
            .map_err(|error| error.to_string())?;
        let mut families = keys
            .iter()
            .filter_map(|key| key.strip_prefix(prefix.as_str()))
            .filter(|family| !family.is_empty())
            .map(str::to_owned)
            .collect::<Vec<_>>();
        families.sort();
        families.dedup();
        Ok(families)
    }

    /// Records `version` as seen at `observed_at`. The member carries the
    /// version's first-seen stamp (kept from its first observation, because
    /// the adoption lag is measured from it) and its last-seen stamp (moved
    /// forward on every observation, so a version real clients have all moved
    /// past ages out instead of being pinned by its own history). The score
    /// is the last-seen second, so rank 0 is the least-current version and
    /// the cap below drops that first. Stale versions are pruned, the key's
    /// TTL slides on every observation.
    pub(crate) async fn record_release(
        &self,
        family: &str,
        version: ClientVersion,
        observed_at: u64,
    ) -> Result<(), String> {
        self.check()?;
        let key = self.key(family);
        let member = version.to_string();
        let first_seen = self
            .runtime
            .score_range_by_min(&key, 0.0)
            .await
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter_map(|raw| ClientReleaseRecord::parse(&raw))
            .find(|record| record.version == version)
            .map(|record| record.first_seen_unix_secs)
            .unwrap_or(observed_at);
        let record = ClientReleaseRecord {
            version,
            first_seen_unix_secs: first_seen,
            last_seen_unix_secs: observed_at,
        };
        self.runtime
            .score_set(&key, &record.serialize(), observed_at as f64)
            .await
            .map_err(|error| error.to_string())?;
        // The member string changes as its last-seen stamp moves, so the
        // previous payload of the same version would otherwise linger: drop
        // any other record that still names this version.
        let stale = self
            .runtime
            .score_range_by_min(&key, 0.0)
            .await
            .unwrap_or_default()
            .into_iter()
            .filter(|raw| {
                raw != &record.serialize()
                    && ClientReleaseRecord::parse(raw)
                        .is_some_and(|existing| existing.version == version)
            })
            .collect::<Vec<_>>();
        for raw in stale {
            let _ = self.runtime.score_remove(&key, &raw).await;
        }
        // Score = last-seen second, so members score in recency order and
        // rank 0 is the version real clients stopped sending longest ago: the
        // cap drops the least-current versions first.
        let cutoff = observed_at
            .saturating_sub(CLIENT_RELEASE_MAX_AGE_SECS)
            .saturating_sub(1);
        let _ = self
            .runtime
            .score_remove_by_score(&key, cutoff as f64)
            .await;
        if let Ok(len) = self.runtime.score_len(&key).await {
            let excess = len.saturating_sub(CLIENT_RELEASE_MAX_VERSIONS);
            if excess > 0 {
                let _ = self
                    .runtime
                    .score_remove_by_rank(&key, 0, excess as i64 - 1)
                    .await;
            }
        }
        let _ = self.runtime.key_expire(&key, CLIENT_RELEASE_TTL).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_runtime() -> aether_runtime_state::RuntimeState {
        aether_runtime_state::RuntimeState::memory(
            aether_runtime_state::MemoryRuntimeStateConfig::default(),
        )
    }

    #[test]
    fn stable_versions_parse_and_pre_releases_do_not() {
        assert_eq!(
            ClientVersion::parse("0.154.0"),
            Some(ClientVersion {
                major: 0,
                minor: 154,
                patch: 0
            })
        );
        assert_eq!(
            ClientVersion::parse("1.2.3"),
            Some(ClientVersion {
                major: 1,
                minor: 2,
                patch: 3
            })
        );
        for rejected in [
            "",
            "0.154.0-alpha.2",
            "0.153.0-beta",
            "1.2.3.4",
            "abc",
            "0.154.0+build",
        ] {
            assert_eq!(ClientVersion::parse(rejected), None, "{rejected}");
        }
        // A missing patch is not a pre-release; the UA always carries three
        // segments, and a two-segment token still orders correctly.
        assert_eq!(
            ClientVersion::parse("0.154"),
            Some(ClientVersion {
                major: 0,
                minor: 154,
                patch: 0
            })
        );
    }

    #[test]
    fn family_and_version_come_from_the_product_segment() {
        for (user_agent, family, version) in [
            (
                "codex-tui/0.154.0 (Mac OS 15.7.7; x86_64) iTerm.app/3.7.0 (codex-tui; 0.154.0)",
                Some("codex-tui"),
                Some("0.154.0"),
            ),
            (
                "Codex Desktop/0.153.4 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.901)",
                Some("codex desktop"),
                Some("0.153.4"),
            ),
            (
                "codex_cli_rs/0.154.0-alpha.6 (Mac OS 26.5.1; arm64) ghostty/1.3.1",
                Some("codex_cli_rs"),
                None,
            ),
            // A non-Codex client is not a family at all: its version is a
            // protocol revision and no account advertises it.
            ("Go-http-client/2.0", None, None),
        ] {
            assert_eq!(
                codex_client_family_from_user_agent(user_agent).as_deref(),
                family,
                "{user_agent}"
            );
            assert_eq!(
                codex_client_stable_version(user_agent).map(ClientVersion::to_string),
                version.map(str::to_string),
                "{user_agent}"
            );
        }
        assert_eq!(codex_client_family_from_user_agent("no-slash-here"), None);
        // The product has to name Codex: `curl` and `Go-http-client` are
        // excluded even though they carry a slash.
        assert_eq!(codex_client_family_from_user_agent("curl/8.7.1"), None);
    }

    #[test]
    fn only_the_version_token_is_swapped() {
        // Both places a real client keeps the version move together: the
        // product token and the build suffix after the last `;`.
        assert_eq!(
            compose_codex_client_user_agent(
                "codex-tui/0.153.4 (Mac OS 26.5.1; arm64) iTerm.app/3.7.0 (codex-tui; 0.153.4)",
                ClientVersion::parse("0.154.0").unwrap()
            ),
            "codex-tui/0.154.0 (Mac OS 26.5.1; arm64) iTerm.app/3.7.0 (codex-tui; 0.154.0)"
        );
        // The client segment is untouched when its trailing field is a build
        // number rather than a version.
        assert_eq!(
            compose_codex_client_user_agent(
                "codex_vscode/0.153.0 (Windows 10.0.26200; x86_64) unknown (VS Code; 26.901)",
                ClientVersion::parse("0.154.0").unwrap()
            ),
            "codex_vscode/0.154.0 (Windows 10.0.26200; x86_64) unknown (VS Code; 26.901)"
        );
        // A two-word product name composes with only its version token
        // replaced.
        assert_eq!(
            compose_codex_client_user_agent(
                "Codex Desktop/0.153.4 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.901)",
                ClientVersion::parse("0.154.0").unwrap()
            ),
            "Codex Desktop/0.154.0 (Windows 10.0.26200; x86_64) unknown (Codex Desktop; 26.901)"
        );
        // Unparsable shapes are returned unchanged, never half-rewritten.
        assert_eq!(
            compose_codex_client_user_agent(
                "Go-http-client/2.0",
                ClientVersion::parse("0.154.0").unwrap()
            ),
            "Go-http-client/2.0"
        );
        assert_eq!(
            compose_codex_client_user_agent(
                "no-parens-codex/1.2.3",
                ClientVersion::parse("0.154.0").unwrap()
            ),
            "no-parens-codex/1.2.3"
        );
        // The separator between the product segment and the parenthetical
        // group is exactly one space: the frozen profile has one, and a
        // rewrite that both moves the version and keeps the suffix must not
        // introduce a second.
        let rewritten = compose_codex_client_user_agent(
            "codex-tui/0.153.0 (Mac OS 26.5.1; arm64) iTerm.app/3.7.0 (codex-tui; 0.153.0)",
            ClientVersion::parse("0.154.0").unwrap(),
        );
        assert_eq!(
            rewritten,
            "codex-tui/0.154.0 (Mac OS 26.5.1; arm64) iTerm.app/3.7.0 (codex-tui; 0.154.0)"
        );
        assert!(!rewritten.contains("  "), "{rewritten}");
        // A version only ever gets longer, so the frozen UA must also survive
        // a no-op rewrite byte-for-byte.
        assert_eq!(
            compose_codex_client_user_agent(
                "codex-tui/0.154.0 (Mac OS 26.5.1; arm64) iTerm.app/3.7.0 (codex-tui; 0.154.0)",
                ClientVersion::parse("0.154.0").unwrap(),
            ),
            "codex-tui/0.154.0 (Mac OS 26.5.1; arm64) iTerm.app/3.7.0 (codex-tui; 0.154.0)"
        );
    }

    #[test]
    fn adoption_lag_is_per_account_and_bounded() {
        let a = codex_client_adoption_lag_secs("fp-a");
        let b = codex_client_adoption_lag_secs("fp-b");
        assert_eq!(a, codex_client_adoption_lag_secs("fp-a"));
        assert!(a < ADOPTION_MAX_LAG_SECS);
        assert!(b < ADOPTION_MAX_LAG_SECS);
        assert_ne!(a, b);
    }

    #[test]
    fn version_selection_is_lagged_monotonic_and_never_older() {
        let fresh = ClientReleaseRecord {
            version: ClientVersion::parse("0.154.0").unwrap(),
            first_seen_unix_secs: 1_000_000,
            last_seen_unix_secs: 1_000_000,
        };
        let old = ClientReleaseRecord {
            version: ClientVersion::parse("0.153.4").unwrap(),
            first_seen_unix_secs: 900_000,
            last_seen_unix_secs: 900_000,
        };
        let frozen = ClientVersion::parse("0.153.4").unwrap();
        let records = vec![fresh.clone(), old.clone()];

        // Inside the lag window nothing is adopted.
        assert_eq!(
            select_codex_client_version(frozen, &records, 86_400, 1_000_000),
            None
        );
        // Once the newest version has aged past the lag it is adopted.
        assert_eq!(
            select_codex_client_version(frozen, &records, 86_400, 1_086_400),
            Some(ClientVersion::parse("0.154.0").unwrap())
        );
        // A version already at or below the frozen one is never adopted.
        let frozen_newer = ClientVersion::parse("0.155.0").unwrap();
        assert_eq!(
            select_codex_client_version(frozen_newer, &records, 0, 2_000_000),
            None
        );
        // An account that already advertises the newest build keeps it: the
        // registry catching up to its own version is not a downgrade.
        assert_eq!(
            select_codex_client_version(
                ClientVersion::parse("0.154.0").unwrap(),
                &records,
                0,
                2_000_000
            ),
            None
        );
    }

    #[test]
    fn record_round_trips() {
        let record = ClientReleaseRecord {
            version: ClientVersion::parse("0.154.0").unwrap(),
            first_seen_unix_secs: 1_789_094_091,
            last_seen_unix_secs: 1_789_100_000,
        };
        assert_eq!(
            ClientReleaseRecord::parse(&record.serialize()),
            Some(record.clone())
        );
        assert_eq!(ClientReleaseRecord::parse("garbage"), None);
        assert_eq!(ClientReleaseRecord::parse("0.154.0-alpha.1:5"), None);
        // Both stamps are required; a truncated payload is not a record.
        assert_eq!(ClientReleaseRecord::parse("0.154.0:5"), None);
        assert_eq!(ClientReleaseRecord::parse("0.154.0:5:6:7"), None);
    }

    #[test]
    fn observation_throttle_collapses_repeats_within_a_window() {
        let version = ClientVersion::parse("0.154.0").unwrap();
        let family = "codex-tui-test-throttle";
        assert!(note_codex_client_observation(family, version, 10_000));
        assert!(!note_codex_client_observation(family, version, 10_001));
        assert!(!note_codex_client_observation(family, version, 10_299));
        assert!(note_codex_client_observation(family, version, 10_300));
        // A different version is its own observation.
        assert!(note_codex_client_observation(
            family,
            ClientVersion::parse("0.155.0").unwrap(),
            10_300
        ));
    }

    #[tokio::test]
    async fn registry_tracks_recency_and_resolves_the_effective_user_agent() {
        let runtime = test_runtime();
        let store = ClientReleaseStore::new(&runtime, "provider-1");
        store
            .record_release(
                "codex-tui",
                ClientVersion::parse("0.152.0").unwrap(),
                900_000,
            )
            .await
            .expect("record superseded");
        store
            .record_release(
                "codex-tui",
                ClientVersion::parse("0.153.4").unwrap(),
                1_500_000,
            )
            .await
            .expect("record newest");
        let records = store.release_records("codex-tui").await.expect("records");
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].version.to_string(), "0.153.4");
        assert_eq!(records[0].last_seen_unix_secs, 1_500_000);

        let frozen =
            "codex-tui/0.153.0 (Mac OS 26.5.1; arm64) iTerm.app/3.7.0 (codex-tui; 0.153.0)";
        let selection_fp = "account-fingerprint";
        let lag = codex_client_adoption_lag_secs(selection_fp);
        // Before the lag elapses the frozen user-agent stands.
        assert_eq!(
            resolve_codex_client_user_agent(&store, frozen, selection_fp, 1_500_000 + lag - 1)
                .await,
            frozen
        );
        // After it, both version tokens move and nothing else does.
        assert_eq!(
            resolve_codex_client_user_agent(&store, frozen, selection_fp, 1_500_000 + lag).await,
            "codex-tui/0.153.4 (Mac OS 26.5.1; arm64) iTerm.app/3.7.0 (codex-tui; 0.153.4)"
        );
    }

    #[tokio::test]
    async fn registry_never_moves_a_frozen_user_agent_backwards() {
        let runtime = test_runtime();
        let store = ClientReleaseStore::new(&runtime, "provider-1");
        store
            .record_release(
                "codex-tui",
                ClientVersion::parse("0.153.0").unwrap(),
                1_000_000,
            )
            .await
            .expect("record");
        let frozen =
            "codex-tui/0.154.0 (Mac OS 26.5.1; arm64) iTerm.app/3.7.0 (codex-tui; 0.154.0)";
        assert_eq!(
            resolve_codex_client_user_agent(&store, frozen, "fp", 9_999_999).await,
            frozen
        );
    }

    #[tokio::test]
    async fn an_unavailable_registry_degrades_to_the_frozen_user_agent() {
        let runtime = test_runtime();
        let store = ClientReleaseStore::unavailable(&runtime, "provider-1");
        let frozen =
            "codex-tui/0.153.4 (Debian 13.0.0; x86_64) xterm-256color (codex-tui; 0.153.4)";
        assert_eq!(
            resolve_codex_client_user_agent(&store, frozen, "fp", 9_999_999).await,
            frozen
        );
        // Observing must not panic or propagate when the registry is down.
        observe_codex_client_release(&store, Some(frozen), 9_999_999).await;
    }

    #[tokio::test]
    async fn observations_record_only_stable_versions() {
        let runtime = test_runtime();
        let store = ClientReleaseStore::new(&runtime, "provider-1");
        observe_codex_client_release(
            &store,
            Some("codex_cli_rs/0.155.0-alpha.6 (Mac OS 26.5.1; arm64) ghostty/1.3.1"),
            1_000,
        )
        .await;
        assert!(store
            .release_records("codex_cli_rs")
            .await
            .expect("records")
            .is_empty());
        observe_codex_client_release(
            &store,
            Some("codex_cli_rs/0.154.0 (Mac OS 26.5.1; arm64) ghostty/1.3.1"),
            2_000,
        )
        .await;
        let records = store
            .release_records("codex_cli_rs")
            .await
            .expect("records");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].version.to_string(), "0.154.0");
        assert_eq!(records[0].first_seen_unix_secs, 2_000);
    }
}
