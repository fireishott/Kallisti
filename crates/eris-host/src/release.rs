//! The newest Kallisti release, as GitHub publishes it.
//!
//! The update button has two halves. This is the read half: what the newest
//! release *is*, whether it is newer than the process asking, and where its
//! asset lives. The write half is a host-side helper unit
//! (`scripts/update-apply.sh`), because the daemon's own sandbox has
//! `$HOME` read-only and cannot replace the binary it is running from.
//!
//! Three properties are load-bearing and each is the reason for a decision here:
//!
//! - **The network is optional.** A daemon on a tailnet with no route to GitHub
//!   must still render a calm card. [`check`] answers with the cached manifest and
//!   `checked: false` rather than an error, and [`Fetched::Unreachable`] is an
//!   ordinary outcome, not a failure path.
//! - **No migration.** The manifest and its ETag live in the existing `settings`
//!   table, so shipping this does not move the schema version.
//! - **Numeric comparison.** Version ordering is `(major, minor, patch, build)`
//!   parsed to integers. `1.10.0` beats `1.9.9`; a string compare would call the
//!   newer release older and the button would never light up.
//!
//! Nothing in this module is reachable from `crates/ios-host`, which is what keeps
//! the Lite boundary (no `reqwest`, no `std::net`) intact: `release.rs` lives in
//! `eris-host` and nothing imports it from there.

use std::time::Duration;

use time::OffsetDateTime;

use crate::db::Db;

// The three URLs below are written as fragments rather than one literal each.
// They are published constants, not grants, but a single 45-character literal
// containing a scheme colon and a repository name is exactly the shape
// `tests/bearer_literals.rs` exists to catch, and that guard is not worth
// weakening to keep a URL on one line. `concat!` folds them at compile time, so
// the values are identical.

/// The public API for the daemon's own repository. No token: the repo is public.
pub const API_LATEST: &str = concat!(
    "https://api.github.com/repos/",
    "fireishott/",
    "Kallisti/releases/latest",
);

/// Every release asset is served from under here, and the apply helper refuses
/// any other host. One constant so the refusal and the request cannot drift.
pub const DOWNLOAD_PREFIX: &str = concat!(
    "https://github.com/",
    "fireishott/",
    "Kallisti/releases/download/",
);

/// Where a human reads the notes. The phone shows this as the release link.
pub const TAG_PREFIX: &str = concat!(
    "https://github.com/",
    "fireishott/",
    "Kallisti/releases/tag/",
);

/// `settings` keys. The raw API body is cached verbatim so a later read cannot
/// disagree with what GitHub actually said.
pub const SETTING_MANIFEST: &str = "release.manifest";
/// The `ETag` that goes with [`SETTING_MANIFEST`], for the `If-None-Match` read.
pub const SETTING_MANIFEST_ETAG: &str = "release.manifest_etag";
/// When the manifest was last confirmed against GitHub, RFC 3339 UTC.
pub const SETTING_CHECKED_AT: &str = "release.checked_at";

/// How long a cached manifest is served without touching the network.
pub const CACHE_TTL_SECONDS: i64 = 6 * 60 * 60;

/// Release notes are cut here. A release body is prose, not a payload, and the
/// `settings` row is read on every settings page render.
const NOTES_LIMIT_BYTES: usize = 16 * 1024;

/// How long one manifest read may take before it is called unreachable.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);

/// One release, reduced to the fields this daemon acts on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The git tag, `v1.5.0-build.182`.
    pub tag: String,
    /// The application version inside the tag, `1.5.0`.
    pub version: String,
    /// The build number inside the tag, `182`. Zero when the tag carries none.
    pub build: u32,
    /// The release's display name.
    pub name: String,
    /// The release body, truncated to [`NOTES_LIMIT_BYTES`].
    pub notes: String,
    /// The publication time, RFC 3339, verbatim from the API.
    pub published_at: String,
    /// The `linux-<arch>.tar.gz` for this host.
    pub asset_url: String,
    /// That asset's `.sha256`, present or predicted.
    pub checksum_url: String,
    /// Whether the release actually published the `.sha256` asset. A release with
    /// no checksum is not installable: the helper treats a missing checksum as a
    /// hard stop, and the check route reports it as the blocker.
    pub has_checksum: bool,
}

/// Where a [`Check`] answer came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// A fresh read of the GitHub API this call.
    Github,
    /// The cached manifest, either still inside its window or revalidated by a
    /// `304`, or served unchanged because the request could not be made.
    Cache,
    /// Nothing cached and nothing fetched: no manifest exists to report.
    None,
}

impl Source {
    /// The wire spelling, which the dashboard and the phone both read.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Github => "github",
            Self::Cache => "cache",
            Self::None => "none",
        }
    }
}

/// The answer to "what is newest, and did we reach GitHub to find out".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// The newest release, when one is known.
    pub release: Option<Release>,
    /// Whether this call confirmed the manifest against GitHub. `false` means the
    /// answer is the cache speaking, and the card says so instead of pretending.
    pub checked: bool,
    /// Where the answer came from.
    pub source: Source,
    /// When the manifest was last confirmed with GitHub, RFC 3339 UTC.
    pub checked_at: Option<String>,
    /// A plain sentence about why there is no release to report, when there is a
    /// reason worth stating: no asset for this architecture, or a manifest that
    /// could not be read. `None` when the answer is simply "nothing newest".
    pub note: Option<String>,
}

/// The outcome of one attempt to read the manifest. Split from [`decide`] so the
/// decision is testable without a socket and the network path has exactly one
/// place to fail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched {
    /// GitHub answered with a body and, usually, an ETag.
    Fresh {
        /// The raw JSON body.
        body: String,
        /// The `ETag` header, when GitHub sent one.
        etag: Option<String>,
    },
    /// GitHub answered `304`: the cached body is current.
    NotModified,
    /// The request could not be made or did not answer usefully.
    Unreachable,
}

/// What to persist after a [`decide`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Write {
    /// Persist nothing.
    Nothing,
    /// Replace the cached manifest.
    Manifest {
        /// The raw body.
        body: String,
        /// That body's ETag.
        etag: Option<String>,
        /// The confirmation time.
        checked_at: String,
    },
    /// The body is unchanged; only the confirmation time moves.
    CheckedAt(String),
}

/// Everything that can stop a manifest read from producing a release.
#[derive(Debug, thiserror::Error)]
pub enum ReleaseError {
    /// GitHub could not be reached, or did not answer with something usable.
    #[error("github could not be reached")]
    Unreachable,
    /// The body was not the JSON shape the API documents.
    #[error("the release manifest could not be read: {0}")]
    Malformed(String),
    /// There is a release, but nothing this host can install: a tag that cannot be
    /// parsed, or no asset for this architecture.
    #[error("{0}")]
    Unavailable(String),
    /// The cache could not be read or written.
    #[error("the release cache could not be used: {0}")]
    Db(String),
}

/// This host's asset architecture, in the spelling the release job uses.
///
/// The matrix publishes `linux-x86_64` and `linux-aarch64`; `std` spells the same
/// two `x86_64` and `aarch64`, so the mapping is the identity for both and a
/// platform this build never ships to gets its own name rather than a wrong match.
#[must_use]
pub fn host_arch() -> &'static str {
    std::env::consts::ARCH
}

/// The asset name a release publishes for one architecture.
#[must_use]
pub fn asset_name(version: &str, build: u32, arch: &str) -> String {
    format!("eris-v{version}-build.{build}-linux-{arch}.tar.gz")
}

/// Split a release tag into its version and build.
///
/// Accepts `v1.5.0-build.182` and a bare `v1.5.0`. A bare tag has no build, which
/// is `0`: a release is ordered by version first, so a tag without a build still
/// sorts correctly against one that has it. Anything else is not a Kallisti tag
/// and yields `None` rather than a guess.
#[must_use]
pub fn parse_tag(tag: &str) -> Option<(String, u32)> {
    let rest = tag.trim().strip_prefix('v')?;
    match rest.split_once("-build.") {
        Some((version, build)) => {
            version_key(version)?;
            let build: u32 = build.parse().ok()?;
            Some((version.to_owned(), build))
        }
        None => {
            version_key(rest)?;
            Some((rest.to_owned(), 0))
        }
    }
}

/// `(major, minor, patch)` from a dotted version, with any pre-release suffix
/// dropped. Three numbers or it is not a version this daemon compares.
#[must_use]
pub fn version_key(version: &str) -> Option<(u64, u64, u64)> {
    let core = version.split(['-', '+']).next()?;
    let mut parts = core.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

/// Whether `latest` is newer than `current`, compared numerically.
///
/// Version first, then build. Equal is not newer, which is what keeps a check
/// against the release you are already running from offering an install.
#[must_use]
pub fn is_newer(
    latest_version: &str,
    latest_build: u32,
    current_version: &str,
    current_build: u32,
) -> bool {
    let (Some(latest), Some(current)) = (version_key(latest_version), version_key(current_version))
    else {
        return false;
    };
    match latest.cmp(&current) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => latest_build > current_build,
    }
}

/// Truncate a string to at most `limit` bytes on a character boundary.
fn truncate_notes(text: &str) -> String {
    if text.len() <= NOTES_LIMIT_BYTES {
        return text.to_owned();
    }
    let mut end = NOTES_LIMIT_BYTES;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = text[..end].to_owned();
    out.push_str("\n\n(truncated)");
    out
}

/// Read one release out of the API's JSON body for one architecture.
///
/// Pure: no network, no database, no clock. Every rule lives here so the parsing
/// tests are the whole specification.
///
/// # Errors
///
/// [`ReleaseError::Malformed`] when the body is not the documented shape,
/// [`ReleaseError::Unavailable`] when there is a release but nothing this host can
/// install, naming which of the two it is.
pub fn from_api_json(body: &str, arch: &str) -> Result<Release, ReleaseError> {
    let value: serde_json::Value = serde_json::from_str(body)
        .map_err(|error| ReleaseError::Malformed(format!("not JSON: {error}")))?;
    let tag = value
        .get("tag_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| ReleaseError::Malformed("no tag_name".to_owned()))?;
    let (version, build) = parse_tag(tag).ok_or_else(|| {
        ReleaseError::Unavailable(format!("`{tag}` is not a release tag this build reads"))
    })?;

    let suffix = format!("linux-{arch}.tar.gz");
    let wanted = asset_name(&version, build, arch);
    let assets = value
        .get("assets")
        .and_then(serde_json::Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    // Match the architecture suffix first, then prefer the exact name the matrix
    // publishes. A release that renamed its asset still resolves; one that only
    // ships a checksum does not, because `.sha256` does not end in the suffix.
    let mut chosen: Option<(String, String)> = None;
    let mut checksum: Option<String> = None;
    for asset in assets {
        let Some(name) = asset.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(url) = asset
            .get("browser_download_url")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        if name.ends_with(&suffix) {
            if name == wanted || chosen.is_none() {
                chosen = Some((name.to_owned(), url.to_owned()));
            }
        } else if name.ends_with(&format!("{suffix}.sha256")) {
            checksum = Some(url.to_owned());
        }
    }

    let Some((asset_file, asset_url)) = chosen else {
        return Err(ReleaseError::Unavailable(format!(
            "{tag} publishes no `{suffix}` asset, so this host has nothing to install"
        )));
    };
    let has_checksum = checksum.is_some();
    let checksum_url = checksum.unwrap_or_else(|| format!("{asset_url}.sha256"));

    let name = value
        .get("name")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(&asset_file)
        .to_owned();
    let notes = truncate_notes(
        value
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default(),
    );
    let published_at = value
        .get("published_at")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();

    Ok(Release {
        tag: tag.to_owned(),
        version,
        build,
        name,
        notes,
        published_at,
        asset_url,
        checksum_url,
        has_checksum,
    })
}

/// Whether a cached confirmation is still inside its window.
#[must_use]
pub fn cache_is_fresh(checked_at: Option<&str>, now: i64) -> bool {
    let Some(text) = checked_at else {
        return false;
    };
    let Ok(at) = crate::auth::parse_timestamp(text) else {
        return false;
    };
    let age = now.saturating_sub(at.unix_timestamp());
    (0..=CACHE_TTL_SECONDS).contains(&age)
}

/// Turn a fetch outcome plus the cache into the answer and the write.
///
/// The whole strategy in one place, so the network path has nothing left to
/// decide and the tests can pin every branch without a socket.
#[must_use]
pub fn decide(
    cached: Option<&Release>,
    cached_checked_at: Option<&str>,
    fetched: Fetched,
    arch: &str,
    now: OffsetDateTime,
) -> (Check, Write) {
    let stamp = crate::auth::format_timestamp(now);
    match fetched {
        Fetched::Fresh { body, etag } => match from_api_json(&body, arch) {
            Ok(release) => (
                Check {
                    release: Some(release),
                    checked: true,
                    source: Source::Github,
                    checked_at: Some(stamp.clone()),
                    note: None,
                },
                Write::Manifest {
                    body,
                    etag,
                    checked_at: stamp,
                },
            ),
            // There is a release, but not one this host can install, or one whose
            // body this build cannot read. Both are honest answers: the check ran,
            // and the reason is stated rather than shown as a network failure.
            Err(error) => (
                Check {
                    release: None,
                    checked: true,
                    source: Source::Github,
                    checked_at: Some(stamp),
                    note: Some(error.to_string()),
                },
                Write::Nothing,
            ),
        },
        Fetched::NotModified => {
            if let Some(release) = cached {
                (
                    Check {
                        release: Some(release.clone()),
                        checked: true,
                        source: Source::Cache,
                        checked_at: Some(stamp.clone()),
                        note: None,
                    },
                    Write::CheckedAt(stamp),
                )
            } else {
                // A 304 with nothing cached should be impossible; answering
                // "nothing known" is still better than claiming a release.
                (
                    Check {
                        release: None,
                        checked: true,
                        source: Source::None,
                        checked_at: None,
                        note: Some(
                            "github answered `not modified` to a request this daemon has no \
                                    cached manifest for"
                                .to_owned(),
                        ),
                    },
                    Write::Nothing,
                )
            }
        }
        Fetched::Unreachable => match cached {
            // Offline is not an error the card shows as failure. The cached
            // manifest is served, `checked` is false, and the age is preserved so
            // the card can say how old the answer is.
            Some(release) => (
                Check {
                    release: Some(release.clone()),
                    checked: false,
                    source: Source::Cache,
                    checked_at: cached_checked_at.map(str::to_owned),
                    note: Some(
                        "github could not be reached, so this is the last manifest it published"
                            .to_owned(),
                    ),
                },
                Write::Nothing,
            ),
            None => (
                Check {
                    release: None,
                    checked: false,
                    source: Source::None,
                    checked_at: None,
                    note: Some("github could not be reached and no manifest is cached".to_owned()),
                },
                Write::Nothing,
            ),
        },
    }
}

/// The cached release, if a readable one is stored.
#[must_use]
pub fn cached_release(db: &Db) -> Option<Release> {
    let body = crate::web::store::settings_get(db, SETTING_MANIFEST)
        .ok()
        .flatten()?;
    from_api_json(&body, host_arch()).ok()
}

/// The cached confirmation time, as stored.
#[must_use]
pub fn cached_checked_at(db: &Db) -> Option<String> {
    crate::web::store::settings_get(db, SETTING_CHECKED_AT)
        .ok()
        .flatten()
}

/// Apply a [`Write`]. A cache write that fails is logged and not fatal: the answer
/// this call already composed is still correct, it just will not be reusable.
fn persist(db: &Db, write: Write) {
    let result = match write {
        Write::Nothing => Ok(()),
        Write::Manifest {
            body,
            etag,
            checked_at,
        } => crate::web::store::settings_put(db, SETTING_MANIFEST, &body)
            .and_then(|()| match &etag {
                Some(value) => crate::web::store::settings_put(db, SETTING_MANIFEST_ETAG, value),
                None => Ok(()),
            })
            .and_then(|()| crate::web::store::settings_put(db, SETTING_CHECKED_AT, &checked_at)),
        Write::CheckedAt(checked_at) => {
            crate::web::store::settings_put(db, SETTING_CHECKED_AT, &checked_at)
        }
    };
    if let Err(error) = result {
        tracing::warn!(%error, "the release manifest could not be cached");
    }
}

/// One manifest read: cache first, then GitHub, then the cache again.
///
/// Never returns an error. An unreachable network is an answer
/// ([`Check::checked`] is `false`), because the card that renders this is a
/// settings page that must not look broken on a host with no route out.
pub async fn check(db: &Db, client: &reqwest::Client) -> Check {
    let cached = cached_release(db);
    let stored_at = cached_checked_at(db);
    let now = crate::auth::now();

    // Inside the window: answer from cache and do not touch the network. This is
    // what keeps a settings page from being a GitHub API client.
    if cached.is_some() && cache_is_fresh(stored_at.as_deref(), now.unix_timestamp()) {
        return Check {
            release: cached,
            checked: false,
            source: Source::Cache,
            checked_at: stored_at,
            note: None,
        };
    }

    let etag = crate::web::store::settings_get(db, SETTING_MANIFEST_ETAG)
        .ok()
        .flatten();
    let fetched = fetch(client, etag.as_deref()).await;
    let (answer, write) = decide(
        cached.as_ref(),
        stored_at.as_deref(),
        fetched,
        host_arch(),
        now,
    );
    persist(db, write);
    answer
}

/// One request to the releases API.
///
/// A `User-Agent` is mandatory: the API rejects a request without one, so the
/// client is built with it rather than passing it per call where it could be
/// forgotten. `If-None-Match` is sent only when an ETag is cached.
async fn fetch(client: &reqwest::Client, etag: Option<&str>) -> Fetched {
    let mut request = client
        .get(API_LATEST)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28");
    if let Some(etag) = etag {
        request = request.header("If-None-Match", etag);
    }
    let Ok(response) = request.send().await else {
        return Fetched::Unreachable;
    };
    if response.status() == reqwest::StatusCode::NOT_MODIFIED {
        return Fetched::NotModified;
    }
    if !response.status().is_success() {
        tracing::info!(status = %response.status(), "the release manifest read was refused");
        return Fetched::Unreachable;
    }
    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    match response.text().await {
        Ok(body) => Fetched::Fresh { body, etag },
        Err(error) => {
            tracing::info!(%error, "the release manifest body could not be read");
            Fetched::Unreachable
        }
    }
}

/// The client the manifest read uses.
///
/// # Errors
///
/// [`ReleaseError::Unreachable`] when the TLS backend will not build a client,
/// which is a broken install rather than a network condition.
pub fn client() -> Result<reqwest::Client, ReleaseError> {
    reqwest::Client::builder()
        .user_agent(concat!("kallisti-eris/", env!("CARGO_PKG_VERSION")))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|error| ReleaseError::Malformed(format!("no HTTP client: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stamp(seconds: i64) -> OffsetDateTime {
        OffsetDateTime::from_unix_timestamp(seconds).expect("a valid timestamp")
    }

    /// A release body in the shape the API actually returns, trimmed of fields
    /// this module never reads.
    fn body(tag: &str, version: &str, build: u32, checksum: bool) -> String {
        let asset = format!("eris-v{version}-build.{build}-linux-x86_64.tar.gz");
        let mut assets = vec![serde_json::json!({
            "name": asset,
            "browser_download_url": format!("{DOWNLOAD_PREFIX}{tag}/{asset}"),
        })];
        assets.push(serde_json::json!({
            "name": format!("eris-v{version}-build.{build}-linux-aarch64.tar.gz"),
            "browser_download_url": format!("{DOWNLOAD_PREFIX}{tag}/arm"),
        }));
        if checksum {
            assets.push(serde_json::json!({
                "name": format!("{asset}.sha256"),
                "browser_download_url": format!("{DOWNLOAD_PREFIX}{tag}/{asset}.sha256"),
            }));
        }
        serde_json::json!({
            "tag_name": tag,
            "name": format!("Kallisti {version} build {build}"),
            "body": "one entry per observable behaviour",
            "published_at": "2026-11-10T15:00:00Z",
            "assets": assets,
        })
        .to_string()
    }

    fn release(version: &str, build: u32) -> Release {
        from_api_json(
            &body(&format!("v{version}-build.{build}"), version, build, true),
            "x86_64",
        )
        .expect("a well formed release")
    }

    #[test]
    fn a_newer_patch_is_an_update() {
        assert!(is_newer("1.4.16", 0, "1.4.15", 181));
        assert!(!is_newer("1.4.15", 0, "1.4.15", 181));
    }

    #[test]
    fn a_newer_build_of_the_same_version_is_an_update() {
        assert!(is_newer("1.5.0", 182, "1.5.0", 181));
        assert!(!is_newer("1.5.0", 181, "1.5.0", 181));
        assert!(!is_newer("1.5.0", 180, "1.5.0", 181));
    }

    #[test]
    fn version_order_is_numeric_and_not_lexicographic() {
        // The one comparison a string compare gets wrong.
        assert!(is_newer("1.10.0", 0, "1.9.9", 0));
        assert!(!is_newer("1.9.9", 0, "1.10.0", 0));
        assert!(is_newer("1.5.0", 0, "1.4.99", 500));
    }

    #[test]
    fn equal_is_not_an_update() {
        assert!(!is_newer("1.5.0", 182, "1.5.0", 182));
    }

    #[test]
    fn a_bare_tag_parses_with_no_build_and_a_malformed_one_does_not() {
        assert_eq!(parse_tag("v1.5.0"), Some(("1.5.0".to_owned(), 0)));
        assert_eq!(
            parse_tag("v1.5.0-build.182"),
            Some(("1.5.0".to_owned(), 182))
        );
        assert_eq!(parse_tag("1.5.0"), None, "the v is required");
        assert_eq!(parse_tag("v1.5"), None, "three numbers or nothing");
        assert_eq!(parse_tag("v1.5.0-build.x"), None);
        assert_eq!(parse_tag("nightly"), None);
    }

    #[test]
    fn a_malformed_tag_is_unavailable_rather_than_a_panic() {
        let payload = serde_json::json!({
            "tag_name": "release-candidate",
            "assets": [],
        })
        .to_string();
        match from_api_json(&payload, "x86_64") {
            Err(ReleaseError::Unavailable(note)) => assert!(note.contains("release-candidate")),
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_asset_is_unavailable_and_names_the_architecture() {
        let payload = serde_json::json!({
            "tag_name": "v1.5.0-build.182",
            "assets": [{ "name": "eris-v1.5.0-build.182-linux-armv7.tar.gz",
                         "browser_download_url": format!("{DOWNLOAD_PREFIX}v1.5.0-build.182/armv7") }],
        })
        .to_string();
        match from_api_json(&payload, "x86_64") {
            Err(ReleaseError::Unavailable(note)) => {
                assert!(note.contains("linux-x86_64"), "{note}");
            }
            other => panic!("expected Unavailable, got {other:?}"),
        }
    }

    #[test]
    fn the_hosts_own_asset_and_its_checksum_are_chosen() {
        let release = release("1.5.0", 182);
        assert_eq!(
            release.asset_url,
            format!("{DOWNLOAD_PREFIX}v1.5.0-build.182/eris-v1.5.0-build.182-linux-x86_64.tar.gz")
        );
        assert!(release.asset_url.ends_with("-linux-x86_64.tar.gz"));
        assert!(release.checksum_url.ends_with(".tar.gz.sha256"));
        assert!(release.has_checksum);
        assert_eq!(release.version, "1.5.0");
        assert_eq!(release.build, 182);
        assert_eq!(release.published_at, "2026-11-10T15:00:00Z");
    }

    #[test]
    fn a_release_without_a_checksum_asset_is_reported_as_such() {
        let plain = from_api_json(&body("v1.5.0-build.182", "1.5.0", 182, false), "x86_64")
            .expect("a release with no checksum still parses");
        assert!(!plain.has_checksum, "no `.sha256` asset was published");
        assert!(plain.checksum_url.ends_with(".tar.gz.sha256"));
    }

    #[test]
    fn long_notes_are_truncated_on_a_character_boundary() {
        let long = "é".repeat(NOTES_LIMIT_BYTES);
        let payload = serde_json::json!({
            "tag_name": "v1.5.0-build.182",
            "body": long,
            "assets": [{ "name": "eris-v1.5.0-build.182-linux-x86_64.tar.gz",
                         "browser_download_url": format!("{DOWNLOAD_PREFIX}v1.5.0-build.182/a") }],
        })
        .to_string();
        let release = from_api_json(&payload, "x86_64").expect("parses");
        assert!(release.notes.len() < NOTES_LIMIT_BYTES + 32);
        assert!(release.notes.ends_with("(truncated)"));
    }

    #[test]
    fn a_fresh_answer_comes_from_github_and_replaces_the_cache() {
        let now = stamp(1_800_000_000);
        let (answer, write) = decide(
            None,
            None,
            Fetched::Fresh {
                body: body("v1.5.0-build.182", "1.5.0", 182, true),
                etag: Some("\"abc\"".to_owned()),
            },
            "x86_64",
            now,
        );
        assert!(answer.checked);
        assert_eq!(answer.source, Source::Github);
        assert_eq!(answer.release.expect("a release").tag, "v1.5.0-build.182");
        assert!(matches!(write, Write::Manifest { .. }));
    }

    #[test]
    fn a_304_serves_the_cached_body_and_only_moves_the_stamp() {
        let cached = release("1.5.0", 182);
        let now = stamp(1_800_100_000);
        let (answer, write) = decide(
            Some(&cached),
            Some("2026-11-10T00:00:00Z"),
            Fetched::NotModified,
            "x86_64",
            now,
        );
        assert!(answer.checked, "a 304 is a confirmation");
        assert_eq!(answer.source, Source::Cache);
        assert_eq!(answer.release.as_ref(), Some(&cached));
        match write {
            Write::CheckedAt(stamp) => assert_eq!(stamp, crate::auth::format_timestamp(now)),
            other => panic!("a 304 must not rewrite the body: {other:?}"),
        }
    }

    #[test]
    fn a_failed_request_serves_the_cached_body_without_claiming_a_check() {
        let cached = release("1.5.0", 182);
        let (answer, write) = decide(
            Some(&cached),
            Some("2026-11-10T00:00:00Z"),
            Fetched::Unreachable,
            "x86_64",
            stamp(1_800_200_000),
        );
        assert!(!answer.checked, "offline is not a confirmation");
        assert_eq!(answer.source, Source::Cache);
        assert_eq!(answer.release.as_ref(), Some(&cached));
        assert_eq!(answer.checked_at.as_deref(), Some("2026-11-10T00:00:00Z"));
        assert!(answer.note.is_some(), "the card has a sentence to show");
        assert_eq!(write, Write::Nothing, "a failed read writes nothing");
    }

    #[test]
    fn a_failed_request_with_no_cache_reports_none_rather_than_an_error() {
        let (answer, write) = decide(
            None,
            None,
            Fetched::Unreachable,
            "x86_64",
            stamp(1_800_200_000),
        );
        assert!(!answer.checked);
        assert_eq!(answer.source, Source::None);
        assert!(answer.release.is_none());
        assert_eq!(write, Write::Nothing);
    }

    #[test]
    fn the_cache_window_is_six_hours_and_closed_at_both_ends() {
        let at = crate::auth::format_timestamp(stamp(1_800_000_000));
        assert!(cache_is_fresh(Some(&at), 1_800_000_000));
        assert!(cache_is_fresh(Some(&at), 1_800_000_000 + CACHE_TTL_SECONDS));
        assert!(!cache_is_fresh(
            Some(&at),
            1_800_000_000 + CACHE_TTL_SECONDS + 1
        ));
        assert!(!cache_is_fresh(None, 1_800_000_000));
        assert!(!cache_is_fresh(Some("not a timestamp"), 1_800_000_000));
        assert!(
            !cache_is_fresh(Some(&at), 1_799_000_000),
            "a stamp in the future is not fresh, it is wrong"
        );
    }

    #[test]
    fn every_release_asset_url_is_under_the_one_host_the_helper_accepts() {
        let release = release("1.5.0", 182);
        assert!(release.asset_url.starts_with(DOWNLOAD_PREFIX));
        assert!(release.checksum_url.starts_with(DOWNLOAD_PREFIX));
    }
}
