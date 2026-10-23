//! Asking a JEV provider which folder a post belongs in.
//!
//! ## Why this module exists at all
//!
//! The project's rule is that Rust at the boundary never opens an HTTP
//! connection (PRD §7.4): the browser captures, Rust only reads its own
//! database. Classification is the one exception, and it is deliberately
//! narrow — one endpoint, chosen by the user, carrying the user's own key, and
//! it never sees an X host. Nothing here can reach `x.com` or `pbs.twimg.com`.
//!
//! ## Provider agnostic by construction
//!
//! JEV is a model, not a vendor. TypeSafe and OpenCode Zen both serve it behind
//! the same `/v1/systemone` shape, so the only per-vendor data is a base URL, a
//! model name and a key. That data is configuration ([`ProviderConfig`]), not
//! code: adding a third host is a preset, not a branch.
//!
//! ## Structured in, structured out
//!
//! There is no conversation here. One request carries the post as `state`, a
//! `choice` question whose criteria are the folders, and a `score` question for
//! how central the post is to whichever folder it lands in. The reply is a
//! distribution, and the confidence it carries is stored alongside the verdict
//! so a future UI can show "unsure" rather than pretending every filing is
//! certain.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub mod client;
pub mod run;

pub use client::Client;
// `run` is both the module and its entry point; the entry point is aliased so
// call sites read as `inference::classify(...)` rather than `inference::run::run`.
pub use run::{run as classify, status, Progress, RunOptions, RunReport, Status};

/// The question name JEV answers under. Internal, but fixed: the response is
/// keyed by it.
pub const CATEGORY_QUESTION: &str = "category";
/// The second question: how central the post is to the chosen folder, as an
/// ordered rubric. Stored in `post_categories.score` as the expected value.
pub const FIT_QUESTION: &str = "fit";
pub const FIT_LEVELS: [&str; 3] = [
    "Mentions it in passing, or is only loosely related",
    "Clearly about it, but not the main subject",
    "Its main subject",
];

/// A selectable provider. The three fields are the whole contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Preset {
    pub id: &'static str,
    pub label: &'static str,
    pub base_url: &'static str,
    pub model: &'static str,
    /// What the free tier will actually accept, per minute. BeatAPI's free
    /// model allows one successful request a minute until the account is
    /// topped up, and firing sixteen at it is how a run looks broken.
    pub requests_per_minute: u32,
    /// One line about the pace this provider actually enforces, shown beside
    /// the field so the number is not a guess.
    pub note: &'static str,
}

/// Known providers, in the order they are offered.
pub const PRESETS: &[Preset] = &[
    Preset {
        // The free model is the only provider here that costs nothing at all
        // and still reads images, so it is offered first and is the default
        // for a fresh install.
        id: "beatapi",
        label: "BeatAPI",
        base_url: "https://api.beatapi.io/v1",
        model: "jev-1.14-free",
        requests_per_minute: 1,
        note: "Free model: 1 request a minute; 10 a minute after a top-up.",
    },
    Preset {
        id: "typesafe",
        label: "TypeSafe",
        base_url: "https://api.typesafe.ai",
        model: "jev-latest",
        requests_per_minute: 200,
        note: "Billed per input token; 200 a minute is well inside the limit.",
    },
    Preset {
        id: "opencode",
        label: "OpenCode Zen",
        base_url: "https://opencode.ai/zen",
        model: "jev-1.13-free",
        requests_per_minute: 200,
        note: "Free tier is limited; lower this if you see 429s.",
    },
    Preset {
        id: "custom",
        label: "Custom",
        base_url: "",
        model: "",
        requests_per_minute: 60,
        note: "Whatever your host allows.",
    },
];

/// What the user configured. Serializable because it lives in `settings.json`
/// beside the library; the *key* never does — it lives in the OS credential
/// store (see the app crate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConfig {
    pub id: String,
    pub base_url: String,
    pub model: String,
    /// The pace this account is allowed. A setting rather than a constant
    /// because the same model is one request a minute on a free balance and ten
    /// after a top-up, and no code can know which of those an account is.
    #[serde(default = "default_rpm")]
    pub requests_per_minute: u32,
}

/// Zero means "the file predates this field": the preset for that provider id
/// then decides the pace, so an existing BeatAPI config is not left at a speed
/// its free tier will refuse.
pub fn default_rpm() -> u32 {
    0
}

impl ProviderConfig {
    /// Fill in a pace that was never written down.
    pub fn resolve_pace(&mut self) {
        if self.requests_per_minute == 0 {
            self.requests_per_minute = Self::preset(self.id.trim())
                .map(|p| p.requests_per_minute)
                .unwrap_or(60);
        }
    }
}

impl ProviderConfig {
    pub fn preset(id: &str) -> Option<Self> {
        PRESETS.iter().find(|p| p.id == id).map(|p| Self {
            id: p.id.to_string(),
            base_url: p.base_url.to_string(),
            model: p.model.to_string(),
            requests_per_minute: p.requests_per_minute,
        })
    }

    /// The default for a fresh install: the free tier, so the first thing a
    /// user sees is a provider that costs nothing to try.
    pub fn default_config() -> Self {
        Self::preset("beatapi").expect("the beatapi preset exists")
    }

    /// Trim, and reject a config that could not possibly work.
    pub fn validated(self) -> Result<Self> {
        let base_url = self.base_url.trim().trim_end_matches('/').to_string();
        let model = self.model.trim().to_string();
        let requests_per_minute = if self.requests_per_minute == 0 {
            Self::preset(self.id.trim()).map(|p| p.requests_per_minute).unwrap_or(60)
        } else {
            self.requests_per_minute
        }
        .clamp(1, 6_000);
        if base_url.is_empty() {
            return Err(Error::Invalid("the provider needs a base URL".into()));
        }
        if !(base_url.starts_with("https://") || base_url.starts_with("http://")) {
            return Err(Error::Invalid(
                "the base URL must start with https:// (or http:// for a local server)".into(),
            ));
        }
        if model.is_empty() {
            return Err(Error::Invalid("the provider needs a model name".into()));
        }
        Ok(Self {
            id: if self.id.trim().is_empty() { "custom".into() } else { self.id.trim().to_string() },
            base_url,
            model,
            requests_per_minute,
        })
    }

    /// Where requests go. A base that already ends in `/v1` (the convention the
    /// TypeSafe docs use) is not given a second one.
    pub fn endpoint(&self) -> String {
        let base = self.base_url.trim_end_matches('/');
        if base.ends_with("/v1") {
            format!("{base}/systemone")
        } else {
            format!("{base}/v1/systemone")
        }
    }
}

/// The folders, as they go to the provider: choice name → description.
///
/// A `BTreeMap` because the order is both the tie-break the model sees and the
/// thing the taxonomy hash is computed over; two runs over the same folders
/// must produce byte-identical requests.
pub fn criteria_of(cats: &[crate::categories::Category]) -> Result<BTreeMap<String, String>> {
    let paths = paths_of(cats);
    let mut map = BTreeMap::new();
    for cat in cats {
        let label = cat.label.trim();
        if label.is_empty() {
            return Err(Error::Invalid("a folder has no name".into()));
        }
        // The description starts with where the folder sits, because a nested
        // name alone ("Crypto") does not say it lives under "Finance". When the
        // user has written a definition, it follows the path; when they have
        // not, the path is all the model gets beyond the name — which the API
        // documents as "interpreted by its name alone".
        let path = paths.get(&cat.id).map(String::as_str).unwrap_or(label);
        let definition = cat.criteria.trim();
        let description = if definition.is_empty() {
            path.to_string()
        } else {
            format!("{path} — {definition}")
        };
        // Two folders with one name would make the returned choice ambiguous,
        // and the answer is a *name*, not an id.
        if map.insert(label.to_string(), description).is_some() {
            return Err(Error::Invalid(format!(
                "two folders are called “{label}” — rename one so the classifier can tell them apart"
            )));
        }
    }
    if map.is_empty() {
        return Err(Error::Invalid("make a folder first".into()));
    }
    if map.len() > 255 {
        return Err(Error::Invalid("JEV accepts at most 255 choices".into()));
    }
    Ok(map)
}

/// "Finance / Crypto" for every folder, from the tree the user built.
///
/// A tab strip cannot show nesting and this map is where that information
/// survives: the classifier is the one consumer that can use it.
fn paths_of(cats: &[crate::categories::Category]) -> BTreeMap<String, String> {
    let by_id: BTreeMap<&str, &crate::categories::Category> =
        cats.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut out = BTreeMap::new();
    for cat in cats {
        let mut parts = vec![cat.label.trim().to_string()];
        let mut parent = cat.parent_id.as_deref();
        // A cycle cannot exist (move_to refuses one), but a corrupt database
        // should not hang a run while it walks upward.
        for _ in 0..64 {
            let Some(id) = parent else { break };
            let Some(p) = by_id.get(id) else { break };
            parts.insert(0, p.label.trim().to_string());
            parent = p.parent_id.as_deref();
        }
        out.insert(cat.id.clone(), parts.join(" / "));
    }
    out
}

/// A stable fingerprint of the folder set: label and definition, by id.
///
/// When it changes, every stored verdict was reached under a different
/// question and is worth re-running.
pub fn taxonomy_hash(cats: &[crate::categories::Category]) -> String {
    use sha2::{Digest, Sha256};
    let mut items: Vec<(&str, &str, &str, &str)> = cats
        .iter()
        .map(|c| {
            (
                c.id.as_str(),
                c.label.trim(),
                c.criteria.trim(),
                c.parent_id.as_deref().unwrap_or(""),
            )
        })
        .collect();
    items.sort();
    let mut hasher = Sha256::new();
    // The parent is part of the fingerprint: moving Crypto under Finance
    // changes the description the model is given, so verdicts reached in the
    // old place are worth re-asking.
    for (id, label, criteria, parent) in items {
        hasher.update(id.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(label.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(criteria.as_bytes());
        hasher.update(b"\x1f");
        hasher.update(parent.as_bytes());
        hasher.update(b"\x1e");
    }
    let hex = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>();
    hex[..16].to_string()
}

/// The post, as the model sees it.
///
/// Author and timestamps are deliberately absent: a folder is about what a post
/// says, not who said it, and including the handle invites the model to file by
/// reputation instead of content.
pub fn compose_state(text: &str, quoted: Option<&str>) -> String {
    let mut state = text.trim().to_string();
    if let Some(q) = quoted.map(str::trim).filter(|q| !q.is_empty()) {
        if !state.is_empty() {
            state.push_str("\n\n");
        }
        state.push_str("Quoted post: ");
        state.push_str(q);
    }
    state
}

/// How a candidate is presented to the provider.
#[derive(Debug, Clone, PartialEq)]
pub enum Plan {
    /// Text only; the image, if any, is incidental.
    Text { state: String },
    /// An image post: the picture *is* the post, so it is sent, and the state
    /// names it with a placeholder the asset map replaces.
    Image { state: String, placeholder: String },
}

/// Decide text vs image for one post.
///
/// The distinction that matters is image-post vs post-with-an-image. A photo
/// with a paragraph around it is a text post that happens to have a picture;
/// a photo with `t.co` and nothing else is a picture, and sending its caption
/// would be sending nothing.
pub fn plan(text: &str, quoted: Option<&str>, has_image: bool, send_images: bool) -> Plan {
    let state = compose_state(text, quoted);
    let bare = strip_links(&state);
    let thin = bare.trim().chars().count() < 60;
    if has_image && send_images && thin {
        let placeholder = "photo".to_string();
        let mut with = state;
        if !with.is_empty() {
            with.push_str("\n\n");
        }
        with.push('[');
        with.push_str(&placeholder);
        with.push(']');
        Plan::Image { state: with, placeholder }
    } else {
        Plan::Text { state }
    }
}

fn strip_links(s: &str) -> String {
    s.split_whitespace()
        .filter(|w| !w.starts_with("https://t.co/") && !w.starts_with("http://t.co/"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// One classification request body.
pub fn build_body(
    model: &str,
    plan: &Plan,
    criteria: &BTreeMap<String, String>,
    image: Option<(&str, &str)>,
) -> serde_json::Value {
    let mut questions = serde_json::Map::new();
    questions.insert(
        CATEGORY_QUESTION.to_string(),
        serde_json::json!({
            "type": "choice",
            "instructions": "Choose the one folder this post belongs in. Judge by each folder's description, not by whether its name appears in the post. Choose “Unsorted” when no other folder's description fits, or when the post is only a link, an image, or a fragment with nothing to judge.",
            "criteria": criteria,
        }),
    );
    questions.insert(
        FIT_QUESTION.to_string(),
        serde_json::json!({
            "type": "score",
            "instructions": "How central is this post to the folder chosen above? A post that merely mentions the topic is low; a post that is about it is high.",
            "criteria": FIT_LEVELS,
        }),
    );

    let state = match plan {
        Plan::Text { state } => state.clone(),
        Plan::Image { state, .. } => state.clone(),
    };

    let mut body = serde_json::json!({
        "model": model,
        "state": state,
        "questions": questions,
    });

    if let (Plan::Image { placeholder, .. }, Some((format, data))) = (plan, image) {
        body["assets"] = serde_json::json!({
            placeholder: { "type": "image", "format": format, "data_base64": data }
        });
    }
    body
}

/// The provider's answer, reduced to what the database stores.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub model: String,
    pub choice: String,
    pub confidence: f64,
    pub score: Option<f64>,
    pub probabilities: BTreeMap<String, f64>,
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// Read `answers.category` out of a response.
///
/// Failure modes are all "the provider said something we cannot store": a
/// missing answer, a choice that is not one of the folders we sent (which would
/// mean filing a post under a name that does not exist), or a confidence that
/// is not a number.
pub fn parse_answer(value: &serde_json::Value, criteria: &BTreeMap<String, String>) -> Result<Outcome> {
    let model = value
        .get("model")
        .and_then(|m| m.as_str())
        .unwrap_or("unknown")
        .to_string();

    let answer = value
        .get("answers")
        .and_then(|a| a.get(CATEGORY_QUESTION))
        .ok_or_else(|| Error::Network("the provider returned no category answer".into()))?;

    let choice = answer
        .get("choice")
        .and_then(|c| c.as_str())
        .ok_or_else(|| Error::Network("the category answer had no choice".into()))?
        .to_string();

    if !criteria.contains_key(&choice) {
        return Err(Error::Network(format!(
            "the provider chose “{choice}”, which is not one of your folders"
        )));
    }

    let confidence = answer
        .get("confidence")
        .and_then(|c| c.as_f64())
        .ok_or_else(|| Error::Network("the category answer had no confidence".into()))?;

    let probabilities = answer
        .get("probabilities")
        .and_then(|p| p.as_object())
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v)))
                .collect()
        })
        .unwrap_or_default();

    let score = value
        .get("answers")
        .and_then(|a| a.get(FIT_QUESTION))
        .and_then(|s| s.get("score"))
        .and_then(|s| s.as_f64());

    let usage = value.get("usage");
    let tokens = |key: &str| -> u64 {
        usage
            .and_then(|u| u.get(key))
            .and_then(|v| v.as_u64())
            .unwrap_or(0)
    };

    Ok(Outcome {
        model,
        choice,
        confidence,
        score,
        probabilities,
        input_tokens: tokens("input_tokens"),
        output_tokens: tokens("output_tokens"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cat(id: &str, label: &str, criteria: &str) -> crate::categories::Category {
        crate::categories::Category {
            id: id.into(),
            parent_id: None,
            label: label.into(),
            criteria: criteria.into(),
            position: 0,
            own: 0,
            total: 0,
        }
    }

    #[test]
    fn two_folders_with_one_name_are_refused_before_anything_is_sent() {
        let cats = vec![cat("c1", "AI", ""), cat("c2", "AI", "")];
        let err = criteria_of(&cats).unwrap_err().to_string();
        assert!(err.contains("two folders are called"), "{err}");
    }

    #[test]
    fn the_taxonomy_hash_tracks_names_and_definitions_not_order() {
        let a = vec![cat("c1", "AI", "models"), cat("c2", "Film", "cinema")];
        let b = vec![cat("c2", "Film", "cinema"), cat("c1", "AI", "models")];
        assert_eq!(taxonomy_hash(&a), taxonomy_hash(&b));

        let renamed = vec![cat("c1", "AI", "models"), cat("c2", "Movies", "cinema")];
        assert_ne!(taxonomy_hash(&a), taxonomy_hash(&renamed));

        let redefined = vec![cat("c1", "AI", "chips"), cat("c2", "Film", "cinema")];
        assert_ne!(taxonomy_hash(&a), taxonomy_hash(&redefined));
    }

    #[test]
    fn a_photo_with_only_a_link_is_sent_as_an_image() {
        let plan = plan("https://t.co/abc", None, true, true);
        assert!(matches!(plan, Plan::Image { .. }));
        if let Plan::Image { state, .. } = plan {
            assert!(state.ends_with("[photo]"), "{state}");
        }
    }

    #[test]
    fn a_long_post_with_a_photo_is_sent_as_text() {
        let text = "A long enough paragraph about a paper, with context, \
                    qualifications, and a link https://t.co/abc";
        assert_eq!(plan(text, None, true, true), Plan::Text { state: text.into() });
    }

    #[test]
    fn images_are_withheld_when_the_user_turns_them_off() {
        assert!(matches!(plan("https://t.co/abc", None, true, false), Plan::Text { .. }));
    }

    #[test]
    fn a_quoted_post_travels_as_context() {
        let state = compose_state("this is wild", Some("the original claim"));
        assert_eq!(state, "this is wild\n\nQuoted post: the original claim");
    }

    #[test]
    fn the_body_asks_two_questions_and_only_carries_an_asset_for_an_image_plan() {
        let criteria = criteria_of(&[cat("c1", "AI", "models")]).unwrap();
        let text = build_body("jev-x", &Plan::Text { state: "hello".into() }, &criteria, None);
        assert_eq!(text["questions"][CATEGORY_QUESTION]["criteria"]["AI"], "AI — models");
        assert!(text["questions"][FIT_QUESTION]["criteria"].as_array().is_some());
        assert!(text.get("assets").is_none());

        let image = build_body(
            "jev-x",
            &Plan::Image { state: "[photo]".into(), placeholder: "photo".into() },
            &criteria,
            Some(("png", "AAAA")),
        );
        assert_eq!(image["assets"]["photo"]["format"], "png");
        assert_eq!(image["state"], "[photo]");
    }

    #[test]
    fn an_answer_becomes_a_choice_with_its_confidence_and_score() {
        let criteria = criteria_of(&[cat("c1", "AI", "models"), cat("c2", "Film", "")]).unwrap();
        let body: serde_json::Value = serde_json::from_str(
            r#"{"model":"jev-1.13",
                "answers":{"category":{"type":"choice","choice":"AI","confidence":0.82,
                           "probabilities":{"AI":0.82,"Film":0.18}},
                           "fit":{"type":"score","score":1.4,"confidence":0.5}},
                "usage":{"input_tokens":41,"output_tokens":9}}"#,
        )
        .unwrap();
        let out = parse_answer(&body, &criteria).unwrap();
        assert_eq!(out.choice, "AI");
        assert_eq!(out.model, "jev-1.13");
        assert!((out.confidence - 0.82).abs() < 1e-9);
        assert_eq!(out.score, Some(1.4));
        assert_eq!(out.input_tokens, 41);
    }

    #[test]
    fn a_choice_that_is_not_a_folder_is_a_failure_not_a_filing() {
        let criteria = criteria_of(&[cat("c1", "AI", "")]).unwrap();
        let body = serde_json::json!({
            "model": "jev-1.13",
            "answers": {"category": {"type": "choice", "choice": "Sports", "confidence": 0.9}},
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let err = parse_answer(&body, &criteria).unwrap_err().to_string();
        assert!(err.contains("not one of your folders"), "{err}");
    }

    #[test]
    fn a_nested_folder_tells_the_model_where_it_sits() {
        // "Crypto" under "Finance" is a different question from "Crypto" at the
        // root, and the choice name alone cannot say which one was asked.
        let mut child = cat("c2", "Crypto", "");
        child.parent_id = Some("c1".into());
        let criteria = criteria_of(&[cat("c1", "Finance", ""), child.clone()]).unwrap();
        assert_eq!(criteria["Crypto"], "Finance / Crypto");

        child.criteria = "chains, tokens, exchanges".into();
        let with_definition = criteria_of(&[cat("c1", "Finance", ""), child]).unwrap();
        assert_eq!(with_definition["Crypto"], "Finance / Crypto — chains, tokens, exchanges");
    }

    #[test]
    fn moving_a_folder_changes_the_taxonomy_hash() {
        let mut child = cat("c2", "Crypto", "chains");
        child.parent_id = Some("c1".into());
        let under_finance = vec![cat("c1", "Finance", ""), child.clone()];
        child.parent_id = Some("c3".into());
        let under_root = vec![cat("c3", "Markets", ""), child];
        assert_ne!(taxonomy_hash(&under_finance), taxonomy_hash(&under_root));
    }

    #[test]
    fn the_endpoint_is_not_doubled_when_the_base_already_says_v1() {
        let cfg = ProviderConfig { id: "x".into(), base_url: "https://api.typesafe.ai/v1/".into(), model: "m".into(), requests_per_minute: 60 };
        assert_eq!(cfg.endpoint(), "https://api.typesafe.ai/v1/systemone");
        let cfg = ProviderConfig { id: "x".into(), base_url: "https://opencode.ai/zen".into(), model: "m".into(), requests_per_minute: 60 };
        assert_eq!(cfg.endpoint(), "https://opencode.ai/zen/v1/systemone");
    }
}
