use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use http::{HeaderMap, HeaderName, HeaderValue};
use reqwest::{Client, Method};
use tokio::sync::Mutex;

use crate::runtime::{BenchmarkRuntimeSampler, BenchmarkRuntimeSnapshot};

#[derive(Debug, Clone, Copy, Default, serde::Serialize, PartialEq, Eq)]
pub enum HttpLoadProbeResponseMode {
    #[default]
    HeadersOnly,
    FullBody,
}

#[derive(Debug, Clone)]
pub struct HttpLoadProbeConfig {
    pub url: String,
    pub method: Method,
    pub headers: BTreeMap<String, String>,
    pub body: Option<Vec<u8>>,
    pub total_requests: usize,
    pub concurrency: usize,
    pub timeout: Duration,
    pub response_mode: HttpLoadProbeResponseMode,
}

impl Default for HttpLoadProbeConfig {
    fn default() -> Self {
        Self {
            url: String::new(),
            method: Method::GET,
            headers: BTreeMap::new(),
            body: None,
            total_requests: 100,
            concurrency: 10,
            timeout: Duration::from_secs(30),
            response_mode: HttpLoadProbeResponseMode::HeadersOnly,
        }
    }
}

impl HttpLoadProbeConfig {
    pub fn validate(&self) -> Result<(), String> {
        if self.url.trim().is_empty() {
            return Err("load probe url cannot be empty".to_string());
        }
        if self.total_requests == 0 {
            return Err("load probe total_requests must be positive".to_string());
        }
        if self.concurrency == 0 {
            return Err("load probe concurrency must be positive".to_string());
        }
        if self.timeout.is_zero() {
            return Err("load probe timeout must be positive".to_string());
        }
        Ok(())
    }
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct HttpLoadProbeResult {
    pub url: String,
    pub method: String,
    pub response_mode: HttpLoadProbeResponseMode,
    pub total_requests: usize,
    pub concurrency: usize,
    pub duration_ms: u64,
    pub throughput_rps: u64,
    pub p99_ms: u64,
    pub completed_requests: usize,
    pub failed_requests: usize,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
    pub mean_ms: u64,
    pub runtime: BenchmarkRuntimeSnapshot,
    pub status_counts: BTreeMap<u16, usize>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct MultiUrlHttpLoadProbeResult {
    pub target_urls: Vec<String>,
    pub target_request_counts: BTreeMap<String, usize>,
    pub method: String,
    pub response_mode: HttpLoadProbeResponseMode,
    pub total_requests: usize,
    pub concurrency: usize,
    pub duration_ms: u64,
    pub throughput_rps: u64,
    pub p99_ms: u64,
    pub completed_requests: usize,
    pub failed_requests: usize,
    pub p50_ms: u64,
    pub p95_ms: u64,
    pub max_ms: u64,
    pub mean_ms: u64,
    pub runtime: BenchmarkRuntimeSnapshot,
    pub status_counts: BTreeMap<u16, usize>,
}

pub async fn run_http_load_probe(
    config: &HttpLoadProbeConfig,
) -> Result<HttpLoadProbeResult, String> {
    config.validate()?;
    run_http_load_probe_against_urls(config, std::slice::from_ref(&config.url))
        .await
        .map(|result| HttpLoadProbeResult {
            url: result
                .target_urls
                .into_iter()
                .next()
                .unwrap_or_else(|| config.url.clone()),
            method: result.method,
            response_mode: result.response_mode,
            total_requests: result.total_requests,
            concurrency: result.concurrency,
            duration_ms: result.duration_ms,
            throughput_rps: result.throughput_rps,
            p99_ms: result.p99_ms,
            completed_requests: result.completed_requests,
            failed_requests: result.failed_requests,
            p50_ms: result.p50_ms,
            p95_ms: result.p95_ms,
            max_ms: result.max_ms,
            mean_ms: result.mean_ms,
            runtime: result.runtime,
            status_counts: result.status_counts,
        })
}

pub async fn run_multi_url_http_load_probe(
    config: &HttpLoadProbeConfig,
    urls: &[String],
) -> Result<MultiUrlHttpLoadProbeResult, String> {
    config.validate()?;
    if urls.is_empty() {
        return Err("multi-url load probe requires at least one target url".to_string());
    }
    run_http_load_probe_against_urls(config, urls).await
}

async fn run_http_load_probe_against_urls(
    config: &HttpLoadProbeConfig,
    urls: &[String],
) -> Result<MultiUrlHttpLoadProbeResult, String> {
    let client = Client::builder()
        .timeout(config.timeout)
        .build()
        .map_err(|err| format!("failed to build load probe http client: {err}"))?;
    let total_requests = config.total_requests;
    let request_headers = build_headers(&config.headers)?;
    let request_body = config.body.clone().map(Arc::new);
    let response_mode = config.response_mode;
    let mut runtime_sampler = BenchmarkRuntimeSampler::new();
    let started_at = Instant::now();

    let next_request = Arc::new(AtomicUsize::new(0));
    let latencies_ms = Arc::new(Mutex::new(Vec::with_capacity(config.total_requests)));
    let status_counts = Arc::new(Mutex::new(BTreeMap::<u16, usize>::new()));
    let target_request_counts = Arc::new(Mutex::new(BTreeMap::<String, usize>::new()));
    let failed_requests = Arc::new(AtomicUsize::new(0));
    let completed_requests = Arc::new(AtomicUsize::new(0));

    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..config.concurrency {
        let client = client.clone();
        let next_request = Arc::clone(&next_request);
        let latencies_ms = Arc::clone(&latencies_ms);
        let status_counts = Arc::clone(&status_counts);
        let target_request_counts = Arc::clone(&target_request_counts);
        let failed_requests = Arc::clone(&failed_requests);
        let completed_requests = Arc::clone(&completed_requests);
        let method = config.method.clone();
        let urls = urls.to_vec();
        let request_headers = request_headers.clone();
        let request_body = request_body.clone();

        workers.spawn(async move {
            loop {
                let current = next_request.fetch_add(1, Ordering::AcqRel);
                if current >= total_requests {
                    break;
                }

                let started_at = Instant::now();
                let url = urls[current % urls.len()].clone();
                let mut request = client.request(method.clone(), &url);
                for (name, value) in request_headers.iter() {
                    request = request.header(name, value);
                }
                if let Some(body) = request_body.as_ref() {
                    request = request.body(body.as_ref().clone());
                }
                match request.send().await {
                    Ok(response) => {
                        let status = response.status().as_u16();
                        let body_result = match response_mode {
                            HttpLoadProbeResponseMode::HeadersOnly => Ok(()),
                            HttpLoadProbeResponseMode::FullBody => {
                                response.bytes().await.map(|_| ()).map_err(|_| ())
                            }
                        };
                        if body_result.is_ok() {
                            let mut counts = status_counts.lock().await;
                            *counts.entry(status).or_insert(0) += 1;
                            drop(counts);
                            let mut target_counts = target_request_counts.lock().await;
                            *target_counts.entry(url).or_insert(0) += 1;
                        } else {
                            failed_requests.fetch_add(1, Ordering::AcqRel);
                        }
                        let latency_ms = started_at.elapsed().as_millis() as u64;
                        latencies_ms.lock().await.push(latency_ms);
                        completed_requests.fetch_add(1, Ordering::AcqRel);
                    }
                    Err(_) => {
                        let latency_ms = started_at.elapsed().as_millis() as u64;
                        latencies_ms.lock().await.push(latency_ms);
                        failed_requests.fetch_add(1, Ordering::AcqRel);
                        completed_requests.fetch_add(1, Ordering::AcqRel);
                    }
                }
            }
        });
    }

    while let Some(result) = workers.join_next().await {
        result.map_err(|err| format!("load probe worker task failed: {err}"))?;
    }

    let status_counts = status_counts.lock().await.clone();
    let target_request_counts = target_request_counts.lock().await.clone();
    let mut latencies = latencies_ms.lock().await.clone();
    latencies.sort_unstable();
    let (p50_ms, p95_ms, p99_ms, max_ms, mean_ms) = summarize_latencies(&latencies);
    let duration_ms = started_at.elapsed().as_millis() as u64;
    let throughput_rps = if duration_ms == 0 {
        completed_requests.load(Ordering::Acquire) as u64
    } else {
        ((completed_requests.load(Ordering::Acquire) as u64) * 1_000) / duration_ms.max(1)
    };

    Ok(MultiUrlHttpLoadProbeResult {
        target_urls: urls.to_vec(),
        target_request_counts,
        method: config.method.as_str().to_string(),
        response_mode: config.response_mode,
        total_requests: config.total_requests,
        concurrency: config.concurrency,
        duration_ms,
        throughput_rps,
        p99_ms,
        completed_requests: completed_requests.load(Ordering::Acquire),
        failed_requests: failed_requests.load(Ordering::Acquire),
        p50_ms,
        p95_ms,
        max_ms,
        mean_ms,
        runtime: runtime_sampler.snapshot(),
        status_counts,
    })
}

fn build_headers(headers: &BTreeMap<String, String>) -> Result<HeaderMap, String> {
    let mut result = HeaderMap::new();
    for (name, value) in headers {
        let name = HeaderName::try_from(name.as_str())
            .map_err(|err| format!("invalid load probe header name `{name}`: {err}"))?;
        let value = HeaderValue::from_str(value)
            .map_err(|err| format!("invalid load probe header value for `{name}`: {err}"))?;
        result.insert(name, value);
    }
    Ok(result)
}

fn summarize_latencies(latencies: &[u64]) -> (u64, u64, u64, u64, u64) {
    if latencies.is_empty() {
        return (0, 0, 0, 0, 0);
    }

    let max_ms = *latencies.last().unwrap_or(&0);
    let mean_ms = latencies.iter().sum::<u64>() / latencies.len() as u64;
    let p50_ms = percentile(latencies, 50);
    let p95_ms = percentile(latencies, 95);
    let p99_ms = percentile(latencies, 99);
    (p50_ms, p95_ms, p99_ms, max_ms, mean_ms)
}

fn percentile(latencies: &[u64], percentile: u8) -> u64 {
    if latencies.is_empty() {
        return 0;
    }
    let last_index = latencies.len() - 1;
    let rank = ((last_index as f64) * (percentile as f64 / 100.0)).round() as usize;
    latencies[rank.min(last_index)]
}

#[cfg(test)]
mod tests {
    use super::{
        build_headers, summarize_latencies, HttpLoadProbeConfig, HttpLoadProbeResponseMode,
    };
    use reqwest::Method;
    use std::collections::BTreeMap;
    use std::time::Duration;

    #[test]
    fn validates_probe_config() {
        assert!(HttpLoadProbeConfig {
            url: String::new(),
            ..HttpLoadProbeConfig::default()
        }
        .validate()
        .is_err());
        assert!(HttpLoadProbeConfig {
            total_requests: 0,
            ..HttpLoadProbeConfig::default()
        }
        .validate()
        .is_err());
        assert!(HttpLoadProbeConfig {
            concurrency: 0,
            ..HttpLoadProbeConfig::default()
        }
        .validate()
        .is_err());
        assert!(HttpLoadProbeConfig {
            timeout: Duration::ZERO,
            ..HttpLoadProbeConfig::default()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn summarizes_latency_distribution() {
        let (p50_ms, p95_ms, p99_ms, max_ms, mean_ms) =
            summarize_latencies(&[10, 20, 30, 40, 50, 60, 70, 80, 90, 100]);
        assert_eq!(p50_ms, 60);
        assert_eq!(p95_ms, 100);
        assert_eq!(p99_ms, 100);
        assert_eq!(max_ms, 100);
        assert_eq!(mean_ms, 55);
    }

    #[test]
    fn default_probe_config_is_reasonable() {
        let config = HttpLoadProbeConfig::default();
        assert_eq!(config.method, Method::GET);
        assert!(config.headers.is_empty());
        assert!(config.body.is_none());
        assert_eq!(config.total_requests, 100);
        assert_eq!(config.concurrency, 10);
        assert_eq!(config.timeout, Duration::from_secs(30));
        assert_eq!(config.response_mode, HttpLoadProbeResponseMode::HeadersOnly);
    }

    #[test]
    fn validates_probe_headers() {
        let mut headers = BTreeMap::new();
        headers.insert("x-aether-test".to_string(), "ok".to_string());
        let built = build_headers(&headers).expect("headers should build");
        assert_eq!(
            built
                .get("x-aether-test")
                .and_then(|value| value.to_str().ok()),
            Some("ok")
        );
        let invalid = BTreeMap::from([("bad header".to_string(), "ok".to_string())]);
        assert!(build_headers(&invalid).is_err());
    }
}
