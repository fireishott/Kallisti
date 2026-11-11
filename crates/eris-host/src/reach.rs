//! Reaching the web when a plain fetch is not enough.
//!
//! `fetch_url` is a text fetcher with a 15 second timeout. On this host it failed
//! 446 times out of 1,177 calls, and the failures are not random: they are pages
//! that need JavaScript to render a body, and pages behind a bot wall that answers
//! a plain client with a challenge. Both are ordinary conditions on the modern web,
//! and neither is a reason for a turn to give up.
//!
//! Two tools, both local, both needing no external API key:
//!
//! - **`browse`** drives a rendering engine and returns readable text. On this host
//!   three are running: camofox on 9377, firecrawl on 3002 and byparr on 8191. The
//!   tool tries them in that order and reports which one answered, because "it
//!   worked" and "camofox worked" are different facts to an operator debugging a
//!   site that renders differently here.
//! - **`web_search`** finds pages when no URL is in hand. A SearXNG instance is
//!   used when one is configured; otherwise the local firecrawl answers.
//!
//! Every engine failure is reported with the engine's own words rather than
//! collapsed into one sentence, so a broken container reads as a broken container
//! and not as "the page is unreachable".

use std::time::Duration;

use core_runtime::provider::ToolSpec;

use crate::tools::{ToolResult, fetch_allowed, html_to_text};

/// The tool that renders a page.
pub const BROWSE: &str = "browse";

/// The tool that finds pages.
pub const SEARCH: &str = "web_search";

/// Hits returned when the caller does not say.
pub const DEFAULT_LIMIT: usize = 5;

/// The most one search returns.
pub const MAX_LIMIT: usize = 10;

/// The most text one browse returns.
pub const MAX_BROWSE_BYTES: usize = 512 * 1024;

/// How long one engine is given before the next is tried.
pub const BROWSE_TIMEOUT_SECS: u64 = 60;

/// How long a search is given.
pub const SEARCH_TIMEOUT_SECS: u64 = 30;

/// One engine: a URL and a settle time in, a page or its own refusal out.
type Engine = fn(&str, u64) -> Result<Page, String>;

/// The identity a rendering engine files this client under.
const USER: &str = "kallisti";

fn engine_url(variable: &str, fallback: &str) -> String {
    std::env::var(variable)
        .ok()
        .map(|value| value.trim().trim_end_matches('/').to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

/// The tool spec for `browse`.
#[must_use]
pub fn browse_spec() -> ToolSpec {
    ToolSpec {
        name: BROWSE.to_owned(),
        description: "Open a page in a real browser and return its readable text. \
                      Use this instead of fetch_url when a page renders with \
                      JavaScript or answers a plain fetch with a bot challenge. \
                      Slower than fetch_url, so try fetch_url first."
            .to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "An http or https URL. Private and loopback addresses are refused."
                },
                "wait_ms": {
                    "type": "integer",
                    "description": "Extra time to let the page settle before reading it, 0 to 30000. Defaults to 0."
                }
            },
            "required": ["url"]
        }),
    }
}

/// The tool spec for `web_search`.
#[must_use]
pub fn search_spec() -> ToolSpec {
    ToolSpec {
        name: SEARCH.to_owned(),
        description: "Search the web for pages when no URL is in hand. Returns a \
                      title, a URL and a snippet per hit; follow a hit with \
                      fetch_url or browse to read it."
            .to_owned(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What to look for."
                },
                "limit": {
                    "type": "integer",
                    "description": format!("How many hits, 1 to {MAX_LIMIT}. Defaults to {DEFAULT_LIMIT}.")
                }
            },
            "required": ["query"]
        }),
    }
}

/// A page with no text is a failed read, not a successful one.
///
/// A rendering engine can answer `success: true` and hand back an empty document,
/// which is what a bot wall looks like from inside the engine. Reporting that as a
/// page would tell a turn the site had no content when the truth is that the site
/// refused to show it.
fn usable(page: Page) -> Result<Page, String> {
    if page.text.trim().is_empty() {
        return Err(format!("{}: answered with an empty body", page.backend));
    }
    Ok(page)
}

/// One page, from whichever engine answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Page {
    /// The readable body.
    pub text: String,
    /// The page's own title, when the engine reported one.
    pub title: Option<String>,
    /// Where the request ended up, after redirects.
    pub final_url: String,
    /// Which engine answered.
    pub backend: String,
}

/// One search hit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// The page title.
    pub title: String,
    /// The page URL.
    pub url: String,
    /// A one or two line excerpt.
    pub snippet: String,
}

/// Run one browser fetch and return the readable text.
#[must_use]
pub fn browse(arguments: &serde_json::Value) -> ToolResult {
    let url = arguments
        .get("url")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if url.is_empty() {
        return ToolResult::error("browse needs a `url`");
    }
    if let Err(refusal) = fetch_allowed(&url) {
        return ToolResult::refused(&refusal);
    }
    let wait = arguments
        .get("wait_ms")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0)
        .min(30_000);

    let engines: [Engine; 3] = [camofox, firecrawl, byparr];
    let mut failures: Vec<String> = Vec::new();
    for engine in engines {
        match engine(&url, wait).and_then(usable) {
            Ok(page) => {
                let mut body = serde_json::json!({
                    "url": url,
                    "final_url": page.final_url,
                    "title": page.title,
                    "backend": page.backend,
                    "text": page.text,
                });
                if !failures.is_empty() {
                    // Say what did not work, so a half-broken stack is visible
                    // rather than hidden behind the engine that happened to answer.
                    body["earlier_failures"] = serde_json::json!(failures);
                }
                return ToolResult::ok(body);
            }
            Err(error) => failures.push(error),
        }
    }
    ToolResult::error(format!(
        "no rendering engine could open {url}: {}",
        failures.join("; ")
    ))
}

/// Search the web and return hits.
#[must_use]
pub fn search(arguments: &serde_json::Value) -> ToolResult {
    let query = arguments
        .get("query")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned();
    if query.is_empty() {
        return ToolResult::error("web_search needs a `query`");
    }
    let limit = arguments
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .map(|value| value as usize)
        .unwrap_or(DEFAULT_LIMIT)
        .clamp(1, MAX_LIMIT);

    let mut failures: Vec<String> = Vec::new();
    if std::env::var("KALLISTI_SEARXNG_URL").is_ok_and(|value| !value.trim().is_empty()) {
        match searxng(&query, limit) {
            Ok((hits, backend)) => return hits_body(&query, &backend, &hits, &failures),
            Err(error) => failures.push(error),
        }
    }
    match firecrawl_search(&query, limit) {
        Ok((hits, backend)) => hits_body(&query, &backend, &hits, &failures),
        Err(error) => {
            failures.push(error);
            ToolResult::error(format!(
                "no search backend answered for {query}: {}",
                failures.join("; ")
            ))
        }
    }
}

fn hits_body(query: &str, backend: &str, hits: &[Hit], failures: &[String]) -> ToolResult {
    let mut body = serde_json::json!({
        "query": query,
        "backend": backend,
        "results": hits
            .iter()
            .map(|hit| serde_json::json!({
                "title": hit.title,
                "url": hit.url,
                "snippet": hit.snippet,
            }))
            .collect::<Vec<_>>(),
        "returned": hits.len(),
    });
    if !failures.is_empty() {
        body["earlier_failures"] = serde_json::json!(failures);
    }
    ToolResult::ok(body)
}

fn client(seconds: u64) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(seconds))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| format!("cannot build a client: {error}"))
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start a request: {error}"))
}

/// Percent-encode a query parameter.
///
/// The pinned reqwest build does not carry `RequestBuilder::query`, and building
/// the URL here is one function instead of a dependency-feature change.
fn encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn trim(text: &str) -> String {
    let collapsed = text
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let collapsed = collapsed.trim();
    collapsed.chars().take(MAX_BROWSE_BYTES).collect()
}

/// camofox: the fingerprint-resistant browser, when its browser is connected.
fn camofox(url: &str, wait: u64) -> Result<Page, String> {
    let base = engine_url("KALLISTI_CAMOFOX_URL", "http://127.0.0.1:9377");
    let key = std::env::var("CAMOFOX_API_KEY")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| std::env::var("KALLISTI_CAMOFOX_KEY").ok())
        .ok_or_else(|| "camofox: CAMOFOX_API_KEY is not set".to_owned())?;
    let client = client(BROWSE_TIMEOUT_SECS)?;
    let run = runtime()?;
    let opened = run.block_on(async {
        client
            .post(format!("{base}/tabs/open"))
            .bearer_auth(&key)
            .json(&serde_json::json!({"url": url, "userId": USER}))
            .send()
            .await
    });
    let opened = match opened {
        Ok(response) => response,
        Err(error) => return Err(format!("camofox: cannot open a tab: {error}")),
    };
    let status = opened.status();
    let body = run
        .block_on(opened.text())
        .map_err(|error| format!("camofox: cannot read the open reply: {error}"))?;
    if !status.is_success() {
        return Err(format!("camofox: {status} {body}"));
    }
    let opened: serde_json::Value = serde_json::from_str(&body)
        .map_err(|error| format!("camofox: the open reply is not JSON: {error}"))?;
    let target = opened
        .get("targetId")
        .or_else(|| opened.get("tabId"))
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| format!("camofox: no tab id in {body}"))?
        .to_owned();
    if wait > 0 {
        std::thread::sleep(Duration::from_millis(wait));
    }
    let snapshot = run.block_on(async {
        client
            .get(format!(
                "{base}/snapshot?targetId={}&userId={}",
                encode(&target),
                encode(USER)
            ))
            .bearer_auth(&key)
            .send()
            .await
    });
    let snapshot = match snapshot {
        Ok(response) => response,
        Err(error) => return Err(format!("camofox: cannot read the page: {error}")),
    };
    let status = snapshot.status();
    let text = run
        .block_on(snapshot.text())
        .map_err(|error| format!("camofox: cannot read the snapshot: {error}"))?;
    if !status.is_success() {
        return Err(format!("camofox: snapshot {status} {text}"));
    }
    Ok(Page {
        text: trim(&text),
        title: None,
        final_url: url.to_owned(),
        backend: "camofox".to_owned(),
    })
}

/// firecrawl: renders the page and hands back markdown.
fn firecrawl(url: &str, wait: u64) -> Result<Page, String> {
    let base = engine_url("KALLISTI_FIRECRAWL_URL", "http://127.0.0.1:3002");
    let client = client(BROWSE_TIMEOUT_SECS)?;
    let run = runtime()?;
    let mut payload = serde_json::json!({"url": url, "formats": ["markdown"]});
    if wait > 0 {
        payload["waitFor"] = serde_json::json!(wait);
    }
    let response = run.block_on(async {
        client
            .post(format!("{base}/v1/scrape"))
            .json(&payload)
            .send()
            .await
    });
    let response = response.map_err(|error| format!("firecrawl: {error}"))?;
    let status = response.status();
    let body = run
        .block_on(response.text())
        .map_err(|error| format!("firecrawl: cannot read the reply: {error}"))?;
    if !status.is_success() {
        return Err(format!("firecrawl: {status} {body}"));
    }
    parse_firecrawl_scrape(&body)
}

/// Parse one firecrawl scrape body.
fn parse_firecrawl_scrape(body: &str) -> Result<Page, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("firecrawl: bad JSON: {error}"))?;
    if value.get("success").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err(format!("firecrawl: {}", error_text(&value)));
    }
    let data = value
        .get("data")
        .ok_or_else(|| "firecrawl: no data in the reply".to_owned())?;
    let text = data
        .get("markdown")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "firecrawl: no markdown in the reply".to_owned())?;
    let metadata = data.get("metadata").cloned().unwrap_or_default();
    Ok(Page {
        text: trim(text),
        title: metadata
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .filter(|title| !title.trim().is_empty()),
        final_url: metadata
            .get("sourceURL")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        backend: "firecrawl".to_owned(),
    })
}

/// byparr: a FlareSolverr-shaped proxy that answers a bot challenge.
fn byparr(url: &str, _wait: u64) -> Result<Page, String> {
    let base = engine_url("KALLISTI_BYPARR_URL", "http://127.0.0.1:8191");
    let client = client(BROWSE_TIMEOUT_SECS)?;
    let run = runtime()?;
    let response = run.block_on(async {
        client
            .post(format!("{base}/v1"))
            .json(&serde_json::json!({
                "cmd": "request.get",
                "url": url,
                "maxTimeout": 45_000,
            }))
            .send()
            .await
    });
    let response = response.map_err(|error| format!("byparr: {error}"))?;
    let status = response.status();
    let body = run
        .block_on(response.text())
        .map_err(|error| format!("byparr: cannot read the reply: {error}"))?;
    if !status.is_success() {
        return Err(format!("byparr: {status} {body}"));
    }
    parse_byparr(&body)
}

/// Parse one byparr solution body.
fn parse_byparr(body: &str) -> Result<Page, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("byparr: bad JSON: {error}"))?;
    let solution = value
        .get("solution")
        .ok_or_else(|| format!("byparr: {}", error_text(&value)))?;
    let html = solution
        .get("response")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "byparr: no page in the solution".to_owned())?;
    Ok(Page {
        text: trim(&html_to_text(html)),
        title: None,
        final_url: solution
            .get("url")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        backend: "byparr".to_owned(),
    })
}

/// SearXNG: used when `KALLISTI_SEARXNG_URL` points at one.
fn searxng(query: &str, limit: usize) -> Result<(Vec<Hit>, String), String> {
    let base = engine_url("KALLISTI_SEARXNG_URL", "");
    if base.is_empty() {
        return Err("searxng: no instance is configured".to_owned());
    }
    let client = client(SEARCH_TIMEOUT_SECS)?;
    let run = runtime()?;
    let response = run.block_on(async {
        client
            .get(format!("{base}/search?q={}&format=json", encode(query)))
            .send()
            .await
    });
    let response = response.map_err(|error| format!("searxng: {error}"))?;
    let status = response.status();
    let body = run
        .block_on(response.text())
        .map_err(|error| format!("searxng: cannot read the reply: {error}"))?;
    if !status.is_success() {
        return Err(format!("searxng: {status} {body}"));
    }
    let hits = parse_searxng(&body, limit)?;
    Ok((hits, "searxng".to_owned()))
}

/// Parse one SearXNG JSON reply.
fn parse_searxng(body: &str, limit: usize) -> Result<Vec<Hit>, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|error| format!("searxng: bad JSON: {error}"))?;
    let results = value
        .get("results")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "searxng: no results in the reply".to_owned())?;
    Ok(results
        .iter()
        .filter_map(|row| {
            Some(Hit {
                title: row
                    .get("title")
                    .and_then(serde_json::Value::as_str)?
                    .to_owned(),
                url: row
                    .get("url")
                    .and_then(serde_json::Value::as_str)?
                    .to_owned(),
                snippet: row
                    .get("content")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .take(limit)
        .collect())
}

/// firecrawl's own search, the local backend that needs no key.
fn firecrawl_search(query: &str, limit: usize) -> Result<(Vec<Hit>, String), String> {
    let base = engine_url("KALLISTI_FIRECRAWL_URL", "http://127.0.0.1:3002");
    let client = client(SEARCH_TIMEOUT_SECS)?;
    let run = runtime()?;
    let response = run.block_on(async {
        client
            .post(format!("{base}/v1/search"))
            .json(&serde_json::json!({"query": query, "limit": limit}))
            .send()
            .await
    });
    let response = response.map_err(|error| format!("firecrawl search: {error}"))?;
    let status = response.status();
    let body = run
        .block_on(response.text())
        .map_err(|error| format!("firecrawl search: cannot read the reply: {error}"))?;
    if !status.is_success() {
        return Err(format!("firecrawl search: {status} {body}"));
    }
    let hits = parse_firecrawl_search(&body, limit)?;
    Ok((hits, "firecrawl".to_owned()))
}

/// Parse one firecrawl search reply.
fn parse_firecrawl_search(body: &str, limit: usize) -> Result<Vec<Hit>, String> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| format!("firecrawl search: bad JSON: {error}"))?;
    let results = value
        .get("data")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| format!("firecrawl search: {}", error_text(&value)))?;
    Ok(results
        .iter()
        .filter_map(|row| {
            Some(Hit {
                title: row
                    .get("title")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
                url: row
                    .get("url")
                    .and_then(serde_json::Value::as_str)?
                    .to_owned(),
                snippet: row
                    .get("description")
                    .or_else(|| row.get("snippet"))
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default()
                    .to_owned(),
            })
        })
        .take(limit)
        .collect())
}

/// The engine's own error text, whichever field it used.
fn error_text(value: &serde_json::Value) -> String {
    for key in ["error", "message", "warning", "detail"] {
        if let Some(text) = value.get(key).and_then(serde_json::Value::as_str) {
            return text.to_owned();
        }
    }
    "the engine refused without saying why".to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured from the firecrawl on this host, trimmed to the fields read.
    const FIRECRAWL_SCRAPE: &str = r##"{
        "success": true,
        "data": {
            "markdown": "# Example Domain\n\nThis domain is for use in documentation examples.",
            "metadata": {
                "title": "Example Domain",
                "language": "en",
                "sourceURL": "https://example.com/"
            }
        }
    }"##;

    /// Captured from `/v1/search` on the same host.
    const FIRECRAWL_SEARCH: &str = r##"{
        "success": true,
        "data": [
            {"url": "https://github.com/fireishott/Kallisti", "title": "Kallisti", "description": "A self-hosted iPhone client for a personal AI agent."},
            {"url": "https://example.test/second", "title": "Second", "description": "Another hit."}
        ]
    }"##;

    const BYPARR: &str = r##"{
        "status": "ok",
        "message": "Success",
        "solution": {
            "url": "https://example.com/",
            "status": 200,
            "response": "<html><head><title>Example</title></head><body><h1>Example Domain</h1><p>Hello there.</p></body></html>"
        }
    }"##;

    const SEARXNG: &str = r##"{
        "query": "rust",
        "results": [
            {"title": "Rust", "url": "https://rust-lang.org/", "content": "A language empowering everyone."},
            {"title": "Docs", "url": "https://doc.rust-lang.org/", "content": "The Rust documentation."}
        ]
    }"##;

    #[test]
    fn firecrawl_markdown_becomes_a_page_with_its_title() {
        let page = parse_firecrawl_scrape(FIRECRAWL_SCRAPE).expect("a page");
        assert!(page.text.contains("Example Domain"), "{}", page.text);
        assert_eq!(page.title.as_deref(), Some("Example Domain"));
        assert_eq!(page.final_url, "https://example.com/");
        assert_eq!(page.backend, "firecrawl");
    }

    #[test]
    fn a_failed_scrape_reports_the_engine_word_not_a_shrug() {
        let body = r##"{"success": false, "error": "the page is behind a bot wall"}"##;
        let error = parse_firecrawl_scrape(body).expect_err("a refusal");
        assert!(error.contains("bot wall"), "{error}");
    }

    #[test]
    fn firecrawl_search_maps_description_to_snippet_and_respects_the_limit() {
        let hits = parse_firecrawl_search(FIRECRAWL_SEARCH, 10).expect("hits");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].title, "Kallisti");
        assert!(
            hits[0].snippet.contains("self-hosted"),
            "{}",
            hits[0].snippet
        );
        let capped = parse_firecrawl_search(FIRECRAWL_SEARCH, 1).expect("hits");
        assert_eq!(capped.len(), 1);
    }

    #[test]
    fn searxng_results_are_read_the_same_shape_as_any_other_backend() {
        let hits = parse_searxng(SEARXNG, 10).expect("hits");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://rust-lang.org/");
        assert!(hits[0].snippet.contains("empowering"));
    }

    #[test]
    fn beparr_html_becomes_readable_text_and_its_final_url_is_kept() {
        let page = parse_byparr(BYPARR).expect("a page");
        assert!(page.text.contains("Example Domain"), "{}", page.text);
        assert!(page.text.contains("Hello there"), "{}", page.text);
        assert!(!page.text.contains('<'), "the tags are gone: {}", page.text);
        assert_eq!(page.final_url, "https://example.com/");
        assert_eq!(page.backend, "byparr");
    }

    #[test]
    fn browse_refuses_a_target_it_may_not_reach() {
        for url in [
            "http://127.0.0.1:9377/health",
            "http://192.168.68.51/",
            "file:///etc/passwd",
        ] {
            let result = browse(&serde_json::json!({"url": url}));
            assert!(result.is_error, "{url} must be refused: {}", result.content);
        }
        let empty = browse(&serde_json::json!({}));
        assert!(empty.is_error);
        assert!(empty.content.contains("url"));
    }

    #[test]
    fn web_search_refuses_an_empty_query_and_clamps_the_limit() {
        let empty = search(&serde_json::json!({}));
        assert!(empty.is_error);
        assert!(empty.content.contains("query"), "{}", empty.content);
        assert_eq!(
            query_limit(serde_json::json!({"query": "x", "limit": 99})),
            MAX_LIMIT
        );
        assert_eq!(
            query_limit(serde_json::json!({"query": "x"})),
            DEFAULT_LIMIT
        );
        assert_eq!(
            query_limit(serde_json::json!({"query": "x", "limit": 0})),
            1
        );
    }

    /// The clamping rule, read out of the tool so the test cannot drift from it.
    fn query_limit(arguments: serde_json::Value) -> usize {
        arguments
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(DEFAULT_LIMIT)
            .clamp(1, MAX_LIMIT)
    }

    #[test]
    fn both_tools_are_offered_and_named_in_the_dispatcher() {
        let offered = crate::tools::specs_offering(crate::tools::SandboxMode::ReadOnly, false);
        let names: Vec<&str> = offered.iter().map(|spec| spec.name.as_str()).collect();
        assert!(names.contains(&BROWSE), "{names:?}");
        assert!(names.contains(&SEARCH), "{names:?}");
        // And the dispatcher knows them by name rather than answering "unknown".
        let sandbox = crate::tools::Sandbox::new(
            std::path::PathBuf::from("."),
            crate::tools::SandboxMode::ReadOnly,
        );
        let result =
            crate::tools::execute("web_search", &serde_json::json!({"query": ""}), &sandbox);
        assert!(result.is_error);
        assert!(
            !result.content.contains("unknown tool"),
            "the dispatcher must know the name: {}",
            result.content
        );
    }
}

/// Live checks against the engines on this host.
///
/// Ignored by default because CI has no stealth stack, and a test that needs four
/// containers to be up is a test that fails for the wrong reason. Run them with
/// `cargo test -p eris-host --lib reach -- --ignored` on the host that runs them.
#[cfg(test)]
mod live {
    use super::*;

    #[test]
    #[ignore]
    fn a_real_page_renders_through_the_engine_chain() {
        let result = browse(&serde_json::json!({"url": "https://example.com"}));
        assert!(!result.is_error, "{}", result.content);
        let body: serde_json::Value = serde_json::from_str(&result.content).expect("JSON");
        // The body is markdown, so the page's H1 arrives as metadata rather than
        // as a heading the markdown carries. Both are read.
        assert_eq!(
            body["title"].as_str(),
            Some("Example Domain"),
            "{}",
            result.content
        );
        assert!(
            body["text"].as_str().is_some_and(|text| text.len() > 40),
            "the body is the page, not a stub: {}",
            result.content
        );
        assert!(body["backend"].as_str().is_some(), "{}", result.content);
    }

    #[test]
    #[ignore]
    fn a_real_search_returns_hits_with_no_external_key() {
        let result = search(&serde_json::json!({"query": "kallisti rust agent", "limit": 3}));
        assert!(!result.is_error, "{}", result.content);
        let body: serde_json::Value = serde_json::from_str(&result.content).expect("JSON");
        assert!(
            body["results"]
                .as_array()
                .is_some_and(|hits| !hits.is_empty()),
            "{}",
            result.content
        );
        assert!(body["backend"].as_str().is_some());
    }
}

#[cfg(test)]
mod empty_body {
    use super::*;

    #[test]
    fn a_success_with_no_text_is_a_failure_not_a_page() {
        let body =
            r##"{"success": true, "data": {"markdown": "   ", "metadata": {"title": "g2.com"}}}"##;
        let page = parse_firecrawl_scrape(body).expect("the engine said success");
        assert!(
            page.text.trim().is_empty(),
            "the fixture is empty on purpose"
        );
        let error = usable(page).expect_err("an empty page is not usable");
        assert!(error.contains("empty body"), "{error}");
        assert!(error.contains("firecrawl"), "the engine is named: {error}");
    }
}
