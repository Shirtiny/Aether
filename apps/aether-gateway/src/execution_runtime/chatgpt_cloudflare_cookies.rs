//! Gateway-side replay of the Cloudflare cookies `chatgpt.com` hands out.
//!
//! codex-rs ≥ 0.143.0 (`http-client/src/chatgpt_cloudflare_cookies.rs`) keeps
//! a process-global cookie jar that stores only Cloudflare's own cookies
//! (`__cf_bm`, `_cfuvid`, `__cflb`, …) for `https` requests to the ChatGPT
//! hosts and replays them on every following HTTP request. A direct-login
//! 0.154.0 capture shows `cookie: _cfuvid=…; __cf_bm=…; __cflb=…` on each
//! `POST /backend-api/codex/responses`. Aether used to answer every request
//! cookie-less, which is what a fresh process looks like on every turn.
//!
//! One jar per (account, host): a pool account is one login, so two accounts
//! never share a `__cf_bm`. Jars are process memory only, bounded, and never
//! consulted for WebSocket handshakes (the official jar is not either).

use std::collections::{BTreeMap, HashMap};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, NaiveDateTime};
use url::Url;

const MAX_JARS: usize = 4096;
const MAX_COOKIES_PER_JAR: usize = 32;
/// Session cookies (no `Max-Age` / `Expires`) live until the process exits in
/// the official client; here they also lapse after a day without traffic so
/// an idle account does not carry a stale edge affinity forever.
const SESSION_COOKIE_IDLE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Mirrors `http-client/src/chatgpt_hosts.rs::is_allowed_chatgpt_host`.
pub(crate) fn is_allowed_chatgpt_host(host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    matches!(
        host.as_str(),
        "chatgpt.com" | "chat.openai.com" | "chatgpt-staging.com"
    ) || host.ends_with(".chatgpt.com")
        || host.ends_with(".chatgpt-staging.com")
}

/// Mirrors the official allowlist: only Cloudflare's own cookies are kept.
fn is_allowed_cookie_name(name: &str) -> bool {
    matches!(
        name,
        "__cf_bm"
            | "__cflb"
            | "__cfruid"
            | "__cfseq"
            | "__cfwaitingroom"
            | "_cfuvid"
            | "cf_clearance"
            | "cf_ob_info"
            | "cf_use_ob"
    ) || name.starts_with("cf_chl_")
}

/// `https` + allowed host, like the official `is_chatgpt_cookie_url`.
fn chatgpt_cookie_url(url: &str) -> Option<(Url, String)> {
    let url = Url::parse(url).ok()?;
    if url.scheme() != "https" {
        return None;
    }
    let host = url.host_str()?.trim_end_matches('.').to_ascii_lowercase();
    if !is_allowed_chatgpt_host(&host) {
        return None;
    }
    Some((url, host))
}

#[derive(Debug, Clone)]
struct StoredCookie {
    value: String,
    path: String,
    /// `None` = session cookie.
    expires_at: Option<SystemTime>,
    created_at: SystemTime,
    /// Per-jar insertion counter that breaks `created_at` ties: cookies set
    /// by one response all carry the same clock reading, and RFC 6265 §5.4
    /// still wants them replayed in the order they were created. Kept across
    /// refreshes together with `created_at` (§5.3 step 11).
    sequence: u64,
    last_seen: SystemTime,
}

impl StoredCookie {
    fn is_expired(&self, now: SystemTime) -> bool {
        match self.expires_at {
            Some(expires_at) => expires_at <= now,
            None => now
                .duration_since(self.last_seen)
                .map(|idle| idle >= SESSION_COOKIE_IDLE_TTL)
                .unwrap_or(false),
        }
    }

    fn matches_path(&self, request_path: &str) -> bool {
        // RFC 6265 §5.1.4 path-match.
        let cookie_path = self.path.as_str();
        request_path == cookie_path
            || (request_path.starts_with(cookie_path)
                && (cookie_path.ends_with('/')
                    || request_path[cookie_path.len()..].starts_with('/')))
    }
}

#[derive(Debug, Default)]
struct Jar {
    cookies: BTreeMap<String, StoredCookie>,
    next_sequence: u64,
    last_used: Option<SystemTime>,
}

type JarKey = (String, String);

static JARS: LazyLock<Mutex<HashMap<JarKey, Jar>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn jar_key(scope: &str, host: &str) -> JarKey {
    (scope.to_string(), host.to_string())
}

/// `cookie` header value for `url` from the jar of `scope`, or `None` when
/// the URL is not a ChatGPT `https` URL or the jar has nothing to replay.
pub(crate) fn request_cookie_header(scope: &str, url: &str, now: SystemTime) -> Option<String> {
    let (url, host) = chatgpt_cookie_url(url)?;
    let request_path = default_request_path(&url);
    let mut jars = JARS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let jar = jars.get_mut(&jar_key(scope, &host))?;
    jar.cookies.retain(|_, cookie| !cookie.is_expired(now));
    let mut matching = jar
        .cookies
        .iter()
        .filter(|(_, cookie)| cookie.matches_path(request_path))
        .collect::<Vec<_>>();
    if matching.is_empty() {
        return None;
    }
    // RFC 6265 §5.4: longer paths first, then earlier creation first; the
    // insertion sequence keeps that order deterministic when several cookies
    // were set by the same response.
    matching.sort_by(|(_, a), (_, b)| {
        b.path
            .len()
            .cmp(&a.path.len())
            .then_with(|| a.created_at.cmp(&b.created_at))
            .then_with(|| a.sequence.cmp(&b.sequence))
    });
    jar.last_used = Some(now);
    Some(
        matching
            .into_iter()
            .map(|(name, cookie)| format!("{name}={}", cookie.value))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// Stores the allowlisted cookies of a response to `url` into the jar of
/// `scope`. Returns how many cookies were stored or refreshed.
pub(crate) fn ingest_set_cookie_headers<'a>(
    scope: &str,
    url: &str,
    set_cookie_values: impl IntoIterator<Item = &'a str>,
    now: SystemTime,
) -> usize {
    let Some((url, host)) = chatgpt_cookie_url(url) else {
        return 0;
    };
    let parsed = set_cookie_values
        .into_iter()
        .filter_map(|value| parse_set_cookie(value, &host, &url, now))
        .collect::<Vec<_>>();
    if parsed.is_empty() {
        return 0;
    }
    let mut jars = JARS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = jar_key(scope, &host);
    if !jars.contains_key(&key) && jars.len() >= MAX_JARS {
        evict_least_recently_used_jar(&mut jars);
    }
    let jar = jars.entry(key).or_default();
    jar.last_used = Some(now);
    let mut stored = 0;
    for cookie in parsed {
        match cookie.disposition {
            Disposition::Remove => {
                jar.cookies.remove(&cookie.name);
            }
            Disposition::Store {
                value,
                path,
                expires_at,
            } => {
                let (created_at, sequence) = match jar.cookies.get(&cookie.name) {
                    Some(existing) => (existing.created_at, existing.sequence),
                    None => {
                        if jar.cookies.len() >= MAX_COOKIES_PER_JAR {
                            evict_oldest_cookie(jar);
                        }
                        let sequence = jar.next_sequence;
                        jar.next_sequence += 1;
                        (now, sequence)
                    }
                };
                jar.cookies.insert(
                    cookie.name,
                    StoredCookie {
                        value,
                        path,
                        expires_at,
                        created_at,
                        sequence,
                        last_seen: now,
                    },
                );
                stored += 1;
            }
        }
    }
    jar.cookies.retain(|_, cookie| !cookie.is_expired(now));
    stored
}

fn evict_least_recently_used_jar(jars: &mut HashMap<JarKey, Jar>) {
    let victim = jars
        .iter()
        .min_by_key(|(_, jar)| jar.last_used)
        .map(|(key, _)| key.clone());
    if let Some(victim) = victim {
        jars.remove(&victim);
    }
}

fn evict_oldest_cookie(jar: &mut Jar) {
    let victim = jar
        .cookies
        .iter()
        .min_by_key(|(_, cookie)| (cookie.created_at, cookie.sequence))
        .map(|(name, _)| name.clone());
    if let Some(victim) = victim {
        jar.cookies.remove(&victim);
    }
}

enum Disposition {
    Store {
        value: String,
        path: String,
        expires_at: Option<SystemTime>,
    },
    Remove,
}

struct ParsedCookie {
    name: String,
    disposition: Disposition,
}

/// RFC 6265 §5.2 parsing of one `set-cookie` value, restricted to the
/// allowlisted names, the request host (`Domain` must cover it) and `https`.
fn parse_set_cookie(
    raw: &str,
    request_host: &str,
    request_url: &Url,
    now: SystemTime,
) -> Option<ParsedCookie> {
    let mut segments = raw.split(';');
    let name_value = segments.next()?.trim();
    let (name, value) = name_value.split_once('=')?;
    let name = name.trim();
    let value = value.trim();
    if name.is_empty() || !is_allowed_cookie_name(name) {
        return None;
    }
    let mut path: Option<String> = None;
    let mut max_age: Option<i64> = None;
    let mut expires: Option<SystemTime> = None;
    for attribute in segments {
        let attribute = attribute.trim();
        if attribute.is_empty() {
            continue;
        }
        let (attribute_name, attribute_value) = match attribute.split_once('=') {
            Some((name, value)) => (name.trim(), Some(value.trim())),
            None => (attribute, None),
        };
        match attribute_name.to_ascii_lowercase().as_str() {
            "domain" => {
                let domain = attribute_value?
                    .trim_start_matches('.')
                    .trim_end_matches('.')
                    .to_ascii_lowercase();
                if domain.is_empty() {
                    return None;
                }
                let covers_host = request_host == domain
                    || request_host
                        .strip_suffix(domain.as_str())
                        .is_some_and(|prefix| prefix.ends_with('.'));
                if !covers_host || !is_allowed_chatgpt_host(&domain) {
                    return None;
                }
            }
            "path" => {
                if let Some(value) = attribute_value.filter(|value| value.starts_with('/')) {
                    path = Some(value.to_string());
                }
            }
            "max-age" => {
                if let Some(value) = attribute_value.and_then(|value| value.parse::<i64>().ok()) {
                    max_age = Some(value);
                }
            }
            "expires" => {
                if let Some(value) = attribute_value.and_then(parse_http_date) {
                    expires = Some(value);
                }
            }
            _ => {}
        }
    }
    let expires_at = match max_age {
        // Max-Age wins over Expires (RFC 6265 §5.3 step 3).
        Some(seconds) if seconds <= 0 => return remove(name),
        Some(seconds) => Some(now + Duration::from_secs(seconds as u64)),
        None => expires,
    };
    if expires_at.is_some_and(|expires_at| expires_at <= now) {
        return remove(name);
    }
    Some(ParsedCookie {
        name: name.to_string(),
        disposition: Disposition::Store {
            value: value.to_string(),
            path: path.unwrap_or_else(|| default_request_path(request_url).to_string()),
            expires_at,
        },
    })
}

fn remove(name: &str) -> Option<ParsedCookie> {
    Some(ParsedCookie {
        name: name.to_string(),
        disposition: Disposition::Remove,
    })
}

/// RFC 6265 §5.1.4 default-path.
fn default_request_path(url: &Url) -> &str {
    let path = url.path();
    if !path.starts_with('/') {
        return "/";
    }
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(index) => &path[..index],
    }
}

fn parse_http_date(value: &str) -> Option<SystemTime> {
    let value = value.trim();
    let naive = NaiveDateTime::parse_from_str(value, "%a, %d %b %Y %H:%M:%S GMT")
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%A, %d-%b-%y %H:%M:%S GMT"))
        .or_else(|_| NaiveDateTime::parse_from_str(value, "%a %b %e %H:%M:%S %Y"))
        .ok()
        .or_else(|| {
            DateTime::parse_from_rfc2822(value)
                .ok()
                .map(|parsed| parsed.naive_utc())
        })?;
    let unix = naive.and_utc().timestamp();
    if unix < 0 {
        return Some(SystemTime::UNIX_EPOCH);
    }
    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix as u64))
}

#[cfg(test)]
pub(crate) fn clear_for_tests() {
    JARS.lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
}

#[cfg(test)]
mod tests {
    use super::*;

    const URL: &str = "https://chatgpt.com/backend-api/codex/responses";

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(secs)
    }

    fn scope(name: &str) -> String {
        format!("test-{name}-{}", std::process::id())
    }

    #[test]
    fn allowed_hosts_mirror_the_official_list() {
        for host in [
            "chatgpt.com",
            "CHATGPT.com",
            "chat.openai.com",
            "chatgpt-staging.com",
            "ab.chatgpt.com",
            "x.chatgpt-staging.com",
        ] {
            assert!(is_allowed_chatgpt_host(host), "{host}");
        }
        for host in [
            "openai.com",
            "api.openai.com",
            "notchatgpt.com",
            "chatgpt.com.evil.test",
            "example.test",
        ] {
            assert!(!is_allowed_chatgpt_host(host), "{host}");
        }
    }

    #[test]
    fn only_cloudflare_cookies_are_stored_and_replayed_in_rfc_order() {
        let scope = scope("allowlist");
        let stored = ingest_set_cookie_headers(
            &scope,
            URL,
            [
                "__cf_bm=bm-value; path=/; expires=Thu, 01 Jan 2099 00:00:00 GMT; domain=.chatgpt.com; HttpOnly; Secure; SameSite=None",
                "_cfuvid=uvid-value; path=/; domain=.chatgpt.com; HttpOnly; Secure; SameSite=None",
                "__cflb=lb-value; SameSite=None; Secure; path=/; expires=Thu, 01 Jan 2099 00:00:00 GMT; HttpOnly",
                "oai-did=device-value; Path=/; Expires=Thu, 01 Jan 2099 00:00:00 GMT; Secure; SameSite=Lax",
                "__Secure-next-auth.session-token=secret; Path=/; HttpOnly; Secure",
                "cf_chl_2=challenge; Path=/backend-api; Secure",
            ],
            at(1_000),
        );
        assert_eq!(stored, 4);
        let header = request_cookie_header(&scope, URL, at(1_001)).unwrap();
        // Longest path first, then creation order.
        assert_eq!(
            header,
            "cf_chl_2=challenge; __cf_bm=bm-value; _cfuvid=uvid-value; __cflb=lb-value"
        );
        assert!(!header.contains("oai-did"));
        assert!(!header.contains("session-token"));
        // A shallower path does not receive the deeper cookie.
        let header = request_cookie_header(&scope, "https://chatgpt.com/api/x", at(1_001)).unwrap();
        assert_eq!(
            header,
            "__cf_bm=bm-value; _cfuvid=uvid-value; __cflb=lb-value"
        );
    }

    #[test]
    fn jars_are_scoped_per_account_and_host() {
        let a = scope("scope-a");
        let b = scope("scope-b");
        ingest_set_cookie_headers(&a, URL, ["__cf_bm=a; Path=/"], at(10));
        assert_eq!(
            request_cookie_header(&a, URL, at(11)).as_deref(),
            Some("__cf_bm=a")
        );
        assert!(request_cookie_header(&b, URL, at(11)).is_none());
        assert!(request_cookie_header(&a, "https://chat.openai.com/x", at(11)).is_none());
        // Non-ChatGPT and non-https URLs never touch the jar.
        assert_eq!(
            ingest_set_cookie_headers(&a, "https://api.openai.com/v1", ["__cf_bm=x"], at(10)),
            0
        );
        assert_eq!(
            ingest_set_cookie_headers(&a, "http://chatgpt.com/x", ["__cf_bm=x"], at(10)),
            0
        );
        assert!(request_cookie_header(&a, "http://chatgpt.com/x", at(11)).is_none());
    }

    #[test]
    fn expiry_and_deletion_follow_rfc_6265() {
        let scope = scope("expiry");
        ingest_set_cookie_headers(
            &scope,
            URL,
            [
                "__cf_bm=short; Path=/; Max-Age=1800; Expires=Thu, 01 Jan 2099 00:00:00 GMT",
                "_cfuvid=session; Path=/",
                "__cflb=dated; Path=/; Expires=Thu, 01 Jan 1970 00:10:00 GMT",
            ],
            at(100),
        );
        assert_eq!(
            request_cookie_header(&scope, URL, at(101)).as_deref(),
            Some("__cf_bm=short; _cfuvid=session; __cflb=dated")
        );
        // Dated cookie lapses at its Expires instant.
        assert_eq!(
            request_cookie_header(&scope, URL, at(601)).as_deref(),
            Some("__cf_bm=short; _cfuvid=session")
        );
        // Max-Age wins over the far Expires: gone after 1800 s.
        assert_eq!(
            request_cookie_header(&scope, URL, at(100 + 1800)).as_deref(),
            Some("_cfuvid=session")
        );
        // A server-side deletion (Max-Age=0) removes the cookie.
        ingest_set_cookie_headers(&scope, URL, ["_cfuvid=; Path=/; Max-Age=0"], at(700));
        assert!(request_cookie_header(&scope, URL, at(701)).is_none());
        // Session cookies lapse after a day idle.
        ingest_set_cookie_headers(&scope, URL, ["_cfuvid=session; Path=/"], at(800));
        assert!(request_cookie_header(&scope, URL, at(800 + 24 * 3600)).is_none());
    }

    #[test]
    fn domain_attribute_must_cover_the_request_host() {
        let scope = scope("domain");
        assert_eq!(
            ingest_set_cookie_headers(
                &scope,
                URL,
                [
                    "__cf_bm=foreign; Domain=openai.com; Path=/",
                    "_cfuvid=sub; Domain=ab.chatgpt.com; Path=/",
                    "__cflb=ok; Domain=chatgpt.com; Path=/",
                ],
                at(1),
            ),
            1
        );
        assert_eq!(
            request_cookie_header(&scope, URL, at(2)).as_deref(),
            Some("__cflb=ok")
        );
    }

    #[test]
    fn refresh_keeps_creation_order_and_updates_value() {
        let scope = scope("refresh");
        ingest_set_cookie_headers(&scope, URL, ["_cfuvid=v1; Path=/"], at(1));
        ingest_set_cookie_headers(&scope, URL, ["__cf_bm=b1; Path=/"], at(2));
        ingest_set_cookie_headers(&scope, URL, ["_cfuvid=v2; Path=/"], at(3));
        assert_eq!(
            request_cookie_header(&scope, URL, at(4)).as_deref(),
            Some("_cfuvid=v2; __cf_bm=b1")
        );
    }

    #[test]
    fn http_dates_parse_in_the_usual_formats() {
        let expected = at(1_445_412_480);
        for value in [
            "Wed, 21 Oct 2015 07:28:00 GMT",
            "Wednesday, 21-Oct-15 07:28:00 GMT",
            "Wed Oct 21 07:28:00 2015",
            "Wed, 21 Oct 2015 07:28:00 +0000",
        ] {
            assert_eq!(parse_http_date(value), Some(expected), "{value}");
        }
        assert!(parse_http_date("never").is_none());
    }

    #[test]
    fn default_path_follows_the_request_path() {
        let url = Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap();
        assert_eq!(default_request_path(&url), "/backend-api/codex");
        let url = Url::parse("https://chatgpt.com/x").unwrap();
        assert_eq!(default_request_path(&url), "/");
        let url = Url::parse("https://chatgpt.com").unwrap();
        assert_eq!(default_request_path(&url), "/");
    }
}
