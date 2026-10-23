//! The one outbound connection the backend makes.
//!
//! Two behaviours here are not obvious and both cost time when missing:
//!
//! * **A browser User-Agent.** Both gateways sit behind Cloudflare, which
//!   answers a bare `ureq` signature with HTML `403 error code: 1010` — a bot
//!   ban that looks exactly like an auth failure until you read the body.
//! * **Retry with a wait, not a spin.** 429 and 5xx are normal on a shared free
//!   tier. Retrying immediately just burns the quota that is about to reset, so
//!   `Retry-After` is honoured when present and the backoff is otherwise
//!   exponential and capped.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use crate::error::{Error, Result};

use super::{build_body, parse_answer, Outcome, Plan, ProviderConfig};

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                          (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";

/// How many times a single post is attempted before it is counted as failed.
const MAX_ATTEMPTS: u32 = 4;
/// A wait longer than this is not worth holding a worker for; the run will pick
/// the post up next time. A rate-limit window can be a minute, though, so this
/// has to outlast a `Retry-After` that is worth obeying.
const MAX_WAIT: Duration = Duration::from_secs(60);
/// The gap between request *starts*, shared by every worker.
///
/// Four workers with no gate put ~200 requests into the first twenty seconds of
/// a run; the free tier's per-minute window closes, every retry lands inside it,
/// and the run looks like it died. 320 ms is about 190 a minute — under the
/// window, and still faster than reading.
const MIN_INTERVAL: Duration = Duration::from_millis(320);

/// The longest a single post waits inside the client for a rate limit to
/// clear. The run's own pause handles the real window.
const RATE_WAIT: Duration = Duration::from_secs(3);

pub struct Client {
    agent: ureq::Agent,
    key: String,
    config: ProviderConfig,
    /// Start time of the last request, shared across the run's workers.
    gate: std::sync::Mutex<Option<Instant>>,
}

/// Why a call did not produce an answer, split by whether trying again could
/// help.
enum Failure {
    Retry { message: String, wait: Option<Duration> },
    RateLimited { message: String, wait: Option<Duration> },
    Fatal(String),
}

impl Client {
    pub fn new(config: ProviderConfig, key: String) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            // Image posts carry a few hundred kilobytes of base64; the free
            // tier can take its time thinking about them.
            .timeout(Duration::from_secs(120))
            .user_agent(USER_AGENT)
            .build();
        Self { agent, key, config, gate: std::sync::Mutex::new(None) }
    }

    pub fn config(&self) -> &ProviderConfig {
        &self.config
    }

    /// Hold every worker to one request per [`MIN_INTERVAL`].
    ///
    /// The lock is held across the sleep on purpose: this is a queue of one,
    /// and letting four workers each compute the same wait from the same start
    /// time would send four requests anyway.
    fn pace(&self) {
        let mut last = self.gate.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(previous) = *last {
            let wait = MIN_INTERVAL.saturating_sub(previous.elapsed());
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
        }
        *last = Some(Instant::now());
    }

    /// Classify one post.
    ///
    /// `image` is `(format, bytes)` with the format already mapped to one of
    /// JEV's (`png`, `jpg`, `jpeg`); anything else is the caller's cue to keep
    /// the post text-only rather than send bytes the endpoint will reject.
    pub fn classify(
        &self,
        plan: &Plan,
        criteria: &BTreeMap<String, String>,
        image: Option<(&str, &[u8])>,
    ) -> Result<Outcome> {
        let encoded = image.map(|(format, bytes)| (format, crate::media::base64_encode(bytes)));
        let asset = match (&encoded, plan) {
            (Some((format, data)), Plan::Image { .. }) => Some((*format, data.as_str())),
            _ => None,
        };
        let body = build_body(&self.config.model, plan, criteria, asset);
        let payload = serde_json::to_string(&body)?;

        let mut attempt = 0;
        loop {
            attempt += 1;
            match self.post(&payload, asset.is_some()) {
                Ok(value) => return parse_answer(&value, criteria),
                Err(Failure::Retry { message, wait }) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(Error::Network(format!(
                            "{message} (gave up after {attempt} attempts)"
                        )));
                    }
                    let pause = wait.unwrap_or_else(|| Duration::from_millis(800 << attempt)).min(MAX_WAIT);
                    std::thread::sleep(pause);
                }
                Err(Failure::RateLimited { message, wait }) => {
                    // One short retry, then hand it up: the run pauses at the
                    // wave boundary, where a pause costs one wave instead of
                    // one per post.
                    if attempt >= 2 {
                        return Err(Error::RateLimited(message));
                    }
                    let pause = wait.unwrap_or(Duration::from_millis(900));
                    std::thread::sleep(pause.min(RATE_WAIT));
                }
                Err(Failure::Fatal(message)) => return Err(Error::Network(message)),
            }
        }
    }

    /// One cheap round-trip that proves the base URL, the model name and the key
    /// all work, without touching a post.
    ///
    /// Uses `noul` — the question type with the plainest answer — so a failure
    /// here is about access, not about parsing a distribution.
    pub fn test(&self) -> Result<TestResult> {
        let body = serde_json::json!({
            "model": self.config.model,
            "state": "This is a test message.",
            "questions": {
                "test": {
                    "type": "noul",
                    "instructions": "Is this a test message?"
                }
            }
        });
        let payload = serde_json::to_string(&body)?;
        let started = Instant::now();
        let mut attempt = 0;
        let value = loop {
            attempt += 1;
            match self.post(&payload, false) {
                Ok(value) => break value,
                Err(Failure::Retry { message, wait }) => {
                    if attempt >= MAX_ATTEMPTS {
                        return Err(Error::Network(format!(
                            "{message} (gave up after {attempt} attempts)"
                        )));
                    }
                    let pause = wait.unwrap_or_else(|| Duration::from_millis(800 << attempt)).min(MAX_WAIT);
                    std::thread::sleep(pause);
                }
                Err(Failure::RateLimited { message, wait }) => {
                    // The probe is already the "is this working at all"
                    // question; waiting a minute to ask it again is not.
                    if attempt >= 2 {
                        return Err(Error::RateLimited(message));
                    }
                    std::thread::sleep(wait.unwrap_or(Duration::from_millis(900)).min(RATE_WAIT));
                }
                Err(Failure::Fatal(message)) => return Err(Error::Network(message)),
            }
        };
        let yes = value
            .get("answers")
            .and_then(|a| a.get("test"))
            .and_then(|a| a.get("noul"))
            .and_then(|v| v.as_f64());
        let model = value
            .get("model")
            .and_then(|m| m.as_str())
            .unwrap_or(&self.config.model)
            .to_string();
        Ok(TestResult {
            model,
            yes,
            latency_ms: started.elapsed().as_millis() as u64,
        })
    }

    fn post(&self, payload: &str, has_image: bool) -> std::result::Result<serde_json::Value, Failure> {
        let mut request = self
            .agent
            .post(&self.config.endpoint())
            .set("Authorization", &format!("Bearer {}", self.key))
            .set("Content-Type", "application/json")
            .set("Accept", "application/json");
        if has_image {
            // Without this header the endpoint refuses assets outright; it is
            // the provider's explicit opt-in for image input.
            request = request.set("TypeSafe-Danger-Zone-Images", "true");
        }

        self.pace();
        let response = match request.send_string(payload) {
            Ok(response) => response,
            Err(ureq::Error::Status(code, response)) => {
                let retry_after = response
                    .header("retry-after")
                    .and_then(|s| s.trim().parse::<u64>().ok())
                    .map(Duration::from_secs);
                let text = response.into_string().unwrap_or_default();
                let message = describe_status(code, &text, &self.config.base_url);
                return Err(if code == 429 {
                    Failure::RateLimited { message, wait: retry_after }
                } else if retryable(code) {
                    Failure::Retry { message, wait: retry_after }
                } else {
                    Failure::Fatal(message)
                });
            }
            Err(ureq::Error::Transport(transport)) => {
                return Err(Failure::Retry {
                    message: format!("could not reach {}: {transport}", self.config.base_url),
                    wait: None,
                });
            }
        };

        let text = response
            .into_string()
            .map_err(|e| Failure::Fatal(format!("could not read the reply: {e}")))?;
        serde_json::from_str(&text)
            .map_err(|e| Failure::Fatal(format!("the provider replied with something that is not JSON: {e}")))
    }
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub model: String,
    /// The model's probability that the probe text is a test message. Not an
    /// assertion about correctness — only that a number came back.
    pub yes: Option<f64>,
    pub latency_ms: u64,
}

fn retryable(code: u16) -> bool {
    code == 408 || code == 425 || (500..600).contains(&code)
}

/// Turn an HTTP failure into a sentence a person can act on.
fn describe_status(code: u16, body: &str, base_url: &str) -> String {
    // The gateways wrap errors as `{"message": ...}` or `{"detail": ...}`, and
    // an HTML body means Cloudflare, not the API.
    let detail = serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|v| {
            // Three shapes seen in the wild: `{message}`, `{detail}` and, from
            // the Zen gateway, `{error: {message}}`.
            v.get("message")
                .or_else(|| v.get("detail"))
                .or_else(|| v.get("error").and_then(|e| e.get("message")))
                .and_then(|m| m.as_str().map(str::to_string))
        })
        .or_else(|| {
            let trimmed = body.trim();
            if trimmed.is_empty() {
                None
            } else if trimmed.starts_with('<') {
                Some("the host answered with an HTML page (a proxy block, not the API)".into())
            } else {
                Some(trimmed.chars().take(300).collect())
            }
        })
        .unwrap_or_default();

    let advice = match code {
        401 | 403 => " — the key was rejected; check it in Settings",
        404 => " — check the base URL and model name",
        402 => " — the provider wants payment for this model",
        429 => " — rate limited; try again in a minute",
        _ => "",
    };
    let host = base_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(base_url);
    format!("{host} returned HTTP {code}{advice}{}{}", if detail.is_empty() { "" } else { ": " }, detail)
}
