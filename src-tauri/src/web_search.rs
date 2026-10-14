use crate::llm::{ChatMessage, LlmClient, LlmError, StructuredRequest, TaskKind};
use reqwest::{header, Client, Url};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::{BTreeMap, BTreeSet}, fmt, net::{IpAddr, SocketAddr, ToSocketAddrs}, sync::Arc, time::Duration};
use unicode_normalization::UnicodeNormalization;
pub const TAVILY_ENDPOINT: &str = "https://api.tavily.com/search";
pub const TAVILY_EXTRACT_ENDPOINT: &str = "https://api.tavily.com/extract";
pub const TARGET_DOMAINS: &[&str] = &[
    "qidian.com", "fanqienovel.com", "jjwxc.net", "17k.com", "zongheng.com", "readnovel.com",
];

const MAX_RESULTS: usize = 5;
const MAX_CANDIDATES: usize = 4;
// Fetch, enrichment, and selection share one visible per-page character window.
const MAX_PROMPT_CHARS_PER_CANDIDATE: usize = 20_000;
const MAX_EVIDENCE_CHARS: usize = 280;
const MAX_HTTP_RESPONSE_BYTES: usize = 1_500_000;
const MAX_EDITION_PARAGRAPH_CHARS: usize = 700;
const FIRST_CHAPTER_MIN_CHARS: usize = 12;

/// Provider budget supplied by the runtime. Web compaction is skipped before
/// calling the model when its complete request cannot fit this usable context.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebOutlineBudget {
    pub context_limit: usize,
    pub output_reserve: usize,
}

impl WebOutlineBudget {
    pub const fn new(context_limit: usize, output_reserve: usize) -> Self { Self { context_limit, output_reserve } }
}


/// Local-only lookup data. The bounded first paragraph of the first
/// substantive chapter goes solely to the configured LLM for edition proof.
#[derive(Debug, Clone)]
pub struct LocalOutlineChapter {
    pub index: usize,
    pub heading: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct OutlineLookup {
    pub book_title: String,
    /// Optional only because imported metadata may not include an author.
    pub author: Option<String>,
    pub chapters: Vec<LocalOutlineChapter>,
}

/// Small, serializable provenance only. It is safe to persist or expose: the
/// quote is capped evidence, never a fetched page body.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebOutlineProvenance {
    pub url: String,
    pub title: String,
    pub evidence: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum EditionProofMethod { AiParagraph, TextSimilarity }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EditionProof {
    pub chapter_index: usize,
    pub method: EditionProofMethod,
    pub provenance: WebOutlineProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifiedChapterOutline {
    pub index: usize,
    pub heading: String,
    pub summary: String,
    pub provenance: WebOutlineProvenance,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebCharacterCard {
    pub name: String,
    pub baseline: String,
    /// Each index was independently located on the cited source page and
    /// contains this card's short evidence quote.
    pub source_chapters: Vec<usize>,
    pub provenance: WebOutlineProvenance,
}

/// The public web result contains conclusions and short attributable evidence,
/// never search `raw_content` or extracted page text.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifiedWebOutline {
    pub source: WebOutlineProvenance,
    pub edition_proof: EditionProof,
    pub plot_summary: String,
    pub plot_chapters: Vec<usize>,
    pub plot_provenance: WebOutlineProvenance,
    pub characters: Vec<WebCharacterCard>,
    pub chapters: Vec<VerifiedChapterOutline>,
}

impl VerifiedWebOutline {
    pub fn provenance(&self) -> Vec<WebOutlineProvenance> {
        let mut unique = BTreeMap::new();
        for item in std::iter::once(&self.source)
            .chain(std::iter::once(&self.plot_provenance))
            .chain(self.characters.iter().map(|card| &card.provenance))
            .chain(self.chapters.iter().map(|chapter| &chapter.provenance))
        {
            unique.entry((item.url.clone(), item.title.clone(), item.evidence.clone())).or_insert_with(|| item.clone());
        }
        unique.into_values().collect()
    }
}

/// Fixed diagnostic codes only: no upstream error, page text, title, URL, or key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebOutlineRejection {
    InvalidLookup,
    SearchUnavailable,
    RateLimited,
    EditionTextUnavailable,
    EditionUnverified,
    EvidenceInsufficient,
    AmbiguousSources,
    ContextInsufficient,
    CompactionUnavailable,
    InvalidOutline,
}

impl WebOutlineRejection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidLookup => "invalid_lookup",
            Self::SearchUnavailable => "search_unavailable",
            Self::RateLimited => "rate_limited",
            Self::EditionTextUnavailable => "edition_text_unavailable",
            Self::EditionUnverified => "edition_unverified",
            Self::EvidenceInsufficient => "evidence_insufficient",
            Self::AmbiguousSources => "ambiguous_sources",
            Self::ContextInsufficient => "context_insufficient",
            Self::CompactionUnavailable => "compaction_unavailable",
            Self::InvalidOutline => "invalid_outline",
        }
    }
}

#[derive(Debug)]
pub enum WebOutlineError {
    Cancelled,
    Unavailable,
    Rejected(WebOutlineRejection),
}

impl fmt::Display for WebOutlineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("请求已取消"),
            Self::Unavailable => f.write_str("联网概要不可用"),
            Self::Rejected(reason) => f.write_str(reason.as_str()),
        }
    }
}
impl std::error::Error for WebOutlineError {}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SearchResult {
    pub title: String,
    pub url: String,
    pub content: String,
    #[serde(default)]
    raw_content: Option<String>,
    #[serde(default)]
    pub score: Option<f64>,
}
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SearchResponse { pub results: Vec<SearchResult> }

/// Fixed, redacted errors: upstream bodies, URLs and request details never escape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DdgError { RateLimited, Unavailable }

impl fmt::Display for DdgError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::RateLimited => "DuckDuckGo 搜索暂时受限",
            Self::Unavailable => "DuckDuckGo 搜索不可用",
        })
    }
}
impl std::error::Error for DdgError {}

type DdgResolver = Arc<dyn Fn(&str, u16) -> Result<Vec<SocketAddr>, DdgError> + Send + Sync>;
pub struct DuckDuckGoClient { http: Client, endpoint: String, resolver: DdgResolver, #[cfg(test)] transport_address: Option<SocketAddr> }

impl DuckDuckGoClient {
    fn http_builder() -> reqwest::ClientBuilder {
        // Every GET must pass the public URL guard before any request is sent.
        // Search uses a direct endpoint too; redirects fail closed for both.
        Client::builder().connect_timeout(Duration::from_secs(8))
            .timeout(Duration::from_secs(20)).redirect(reqwest::redirect::Policy::none())
    }

    pub fn new() -> Result<Self, DdgError> {
        let http = Self::http_builder().build().map_err(|_| DdgError::Unavailable)?;
        Ok(Self { http, endpoint: "https://html.duckduckgo.com/html/".into(), resolver: Arc::new(|host, port| resolve_public_addresses(host, port)), #[cfg(test)] transport_address: None })
    }

    #[cfg(test)]
    pub fn with_endpoint(endpoint: String) -> Result<Self, DdgError> {
        let mut client = Self::new()?;
        client.endpoint = endpoint;
        Ok(client)
    }

    pub async fn search(&self, query: &str, domains: Option<&[&str]>) -> Result<SearchResponse, DdgError> {
        let query = match domains.filter(|domains| !domains.is_empty()) {
            Some(domains) => {
                let mut scoped = String::new();
                for (index, domain) in domains.iter().enumerate() {
                    if index > 0 { scoped.push_str(" OR "); }
                    scoped.push_str("site:");
                    scoped.push_str(domain);
                }
                scoped.push(' ');
                scoped.push_str(query);
                std::borrow::Cow::Owned(scoped)
            }
            None => std::borrow::Cow::Borrowed(query),
        };
        // reqwest's optional `form` feature is not enabled. URL query pairs use
        // the same application/x-www-form-urlencoded serializer.
        let mut form = Url::parse("https://duckduckgo.com/").map_err(|_| DdgError::Unavailable)?;
        form.query_pairs_mut().append_pair("q", &query).append_pair("kp", "-2");
        let mut response = self.http.post(&self.endpoint)
            .header(header::USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form.query().unwrap_or_default().to_owned())
            .send().await.map_err(|_| DdgError::Unavailable)?;
        let status = response.status();
        if matches!(status.as_u16(), 202 | 403) { return Err(DdgError::RateLimited); }
        if !status.is_success() { return Err(DdgError::Unavailable); }
        if response.content_length().is_some_and(|size| size > MAX_HTTP_RESPONSE_BYTES as u64) { return Err(DdgError::Unavailable); }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| DdgError::Unavailable)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_HTTP_RESPONSE_BYTES { return Err(DdgError::Unavailable); }
            bytes.extend_from_slice(&chunk);
        }
        let html = std::str::from_utf8(&bytes).map_err(|_| DdgError::Unavailable)?;
        Ok(SearchResponse { results: parse_ddg_html(html)? })
    }

    /// Fetch only a bounded public candidate page; errors never expose its URL or body.
    pub async fn fetch_page_text(&self, url: &str) -> Result<String, DdgError> {
        if !is_public_http_url(url) { return Err(DdgError::Unavailable); }
        let parsed = Url::parse(url).map_err(|_| DdgError::Unavailable)?;
        let host = parsed.host_str().ok_or(DdgError::Unavailable)?;
        let port = parsed.port_or_known_default().ok_or(DdgError::Unavailable)?;
        let addresses = self.resolve_fetch_addresses(host, port).await?;
        let connect_host = host.trim_start_matches('[').trim_end_matches(']');
        let pinned = {
            #[cfg(test)] { self.transport_address.unwrap_or(addresses[0]) }
            #[cfg(not(test))] { addresses[0] }
        };
        let client = DuckDuckGoClient::http_builder().no_proxy().resolve(connect_host, pinned).build().map_err(|_| DdgError::Unavailable)?;
        let mut response = client.get(parsed)
            .timeout(Duration::from_secs(10))
            .header(header::USER_AGENT, "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
            .send().await.map_err(|_| DdgError::Unavailable)?;
        if !response.status().is_success() { return Err(DdgError::Unavailable); }
        if response.content_length().is_some_and(|size| size > MAX_HTTP_RESPONSE_BYTES as u64) { return Err(DdgError::Unavailable); }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| DdgError::Unavailable)? {
            if bytes.len().saturating_add(chunk.len()) > MAX_HTTP_RESPONSE_BYTES { return Err(DdgError::Unavailable); }
            bytes.extend_from_slice(&chunk);
        }
        let html = std::str::from_utf8(&bytes).map_err(|_| DdgError::Unavailable)?;
        Ok(clip(html_to_text(html), MAX_PROMPT_CHARS_PER_CANDIDATE))
    }
}
impl DuckDuckGoClient {
    async fn resolve_fetch_addresses(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, DdgError> {
        let host = host.trim_start_matches('[').trim_end_matches(']').to_owned();
        let resolver = Arc::clone(&self.resolver);
        tokio::time::timeout(Duration::from_secs(5), tokio::task::spawn_blocking(move || {
            let addresses = (resolver)(&host, port)?;
            validate_public_addresses(&addresses)
        })).await.map_err(|_| DdgError::Unavailable)?.map_err(|_| DdgError::Unavailable)?
    }
}

fn resolve_public_addresses(host: &str, port: u16) -> Result<Vec<SocketAddr>, DdgError> {
    if let Ok(ip) = host.parse::<IpAddr>() { return validate_public_addresses(&[SocketAddr::new(ip, port)]); }
    let addresses: Vec<SocketAddr> = (host, port).to_socket_addrs().map_err(|_| DdgError::Unavailable)?.collect();
    validate_public_addresses(&addresses)
}
fn validate_public_addresses(addresses: &[SocketAddr]) -> Result<Vec<SocketAddr>, DdgError> {
    if addresses.is_empty() || addresses.iter().any(|address| !is_public_ip(address.ip())) { return Err(DdgError::Unavailable); }
    Ok(addresses.to_vec())
}

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => !(ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast()),
        IpAddr::V6(ip) => !(ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local() || ip.is_unicast_link_local() || ip.to_ipv4_mapped().is_some_and(|ip| ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified() || ip.is_broadcast())),
    }
}

fn ddg_result_url(href: &str) -> Option<String> {
    let href = href.replace("&amp;", "&");
    let absolute = if href.starts_with("//") { std::borrow::Cow::Owned(format!("https:{href}")) }
        else { std::borrow::Cow::Borrowed(href.as_str()) };
    let parsed = Url::parse(&absolute).ok()?;
    let destination = if parsed.host_str() == Some("duckduckgo.com") && parsed.path() == "/l/" {
        parsed.query_pairs().find(|(key, _)| key == "uddg")?.1.into_owned()
    } else { href };
    is_public_http_url(&destination).then_some(destination)
}

fn ddg_plain_text(html: &str, max: usize) -> String {
    static TAGS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?s)<[^>]*>").unwrap());
    let text = TAGS.replace_all(html, "");
    clip(text.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"")
        .replace("&#39;", "'").replace("&nbsp;", " ").replace("&amp;", "&").trim(), max)
}

/// Minimal HTML conversion for chapter matching, not a DOM or layout parser.
fn html_to_text(html: &str) -> String {
    static HIDDEN: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(concat!(
        r"(?is)<!--.*?(?:-->|$)|",
        r#"<script\b(?:"[^"]*"|'[^']*'|[^'">])*>.*?(?:</script\s*>|$)|"#,
        r#"<style\b(?:"[^"]*"|'[^']*'|[^'">])*>.*?(?:</style\s*>|$)|"#,
        r#"<noscript\b(?:"[^"]*"|'[^']*'|[^'">])*>.*?(?:</noscript\s*>|$)"#,
    )).unwrap());
    static TAGS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(
        r#"(?is)<\s*/?\s*([a-z][a-z0-9]*)\b(?:"[^"]*"|'[^']*'|[^'">])*>|<[^>]*>"#
    ).unwrap());
    static ENTITIES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(
        r"&(?:amp|lt|gt|quot|apos|nbsp|#[0-9]+|#[xX][0-9a-fA-F]+);"
    ).unwrap());
    const BLOCK_TAGS: &[&str] = &[
        "address", "article", "aside", "blockquote", "br", "caption", "dd", "details", "div", "dl", "dt",
        "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "header",
        "hr", "li", "main", "nav", "ol", "p", "pre", "section", "summary", "table", "tbody", "tfoot", "thead", "tr", "ul",
    ];
    let visible = HIDDEN.replace_all(html, "");
    let stripped = TAGS.replace_all(&visible, |tag: &regex::Captures<'_>| {
        if tag.get(1).is_some_and(|name| BLOCK_TAGS.iter().any(|block| name.as_str().eq_ignore_ascii_case(block))) { "\n" }
        else { "" }
    });

    // Decode once, after stripping markup, and normalize directly into the
    // output without allocating a string per entity or per line. A newline
    // wins over adjacent spaces; deferred whitespace also trims both ends.
    let mut text = String::with_capacity(stripped.len());
    let mut whitespace = None;
    let mut push = |character: char| {
        if character.is_whitespace() {
            if matches!(character, '\r' | '\n') { whitespace = Some('\n'); }
            else if whitespace != Some('\n') { whitespace = Some(' '); }
        } else {
            if let Some(separator) = whitespace.take() {
                if !text.is_empty() { text.push(separator); }
            }
            text.push(character);
        }
    };
    let mut offset = 0;
    for entity in ENTITIES.find_iter(&stripped) {
        for character in stripped[offset..entity.start()].chars() { push(character); }
        let encoded = &entity.as_str()[1..entity.len() - 1];
        let decoded = match encoded {
            "amp" => Some('&'), "lt" => Some('<'), "gt" => Some('>'), "quot" => Some('"'), "nbsp" => Some(' '),
            "apos" => Some('\''),
            _ => {
                let number = encoded.strip_prefix('#').unwrap();
                let (digits, radix) = match number.strip_prefix('x').or_else(|| number.strip_prefix('X')) {
                    Some(digits) => (digits, 16), None => (number, 10),
                };
                u32::from_str_radix(digits, radix).ok().and_then(char::from_u32)
            }
        };
        if let Some(character) = decoded { push(character); }
        else { for character in entity.as_str().chars() { push(character); } }
        offset = entity.end();
    }
    for character in stripped[offset..].chars() { push(character); }
    text
}

fn parse_ddg_html(html: &str) -> Result<Vec<SearchResult>, DdgError> {
    if html.contains("anomaly") && html.contains("duckduckgo.com") { return Err(DdgError::RateLimited); }
    // Scan opening tags rather than whole elements so surrounding result divs
    // cannot swallow title anchors. Snippets may use any element, without href.
    static TAGS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"(?is)<(/?)([a-z][a-z0-9]*)\b([^>]*)>").unwrap());
    static ATTRIBUTES: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r#"(?is)\b(class|href)\s*=\s*(?:"([^"]*)"|'([^']*)')"#).unwrap());
    let mut results: Vec<SearchResult> = Vec::new();
    let mut current = None;
    struct Capture<'a> { element: &'a str, start: usize, depth: usize, index: usize, is_title: bool }
    let mut active: Option<Capture<'_>> = None;
    let finish = |capture: Capture<'_>, end: usize, results: &mut Vec<SearchResult>| {
        let text = &html[capture.start..end];
        if capture.is_title { results[capture.index].title = ddg_plain_text(text, 300); }
        else { results[capture.index].content = ddg_plain_text(text, MAX_PROMPT_CHARS_PER_CANDIDATE); }
    };
    for tag in TAGS.captures_iter(html) {
        let bounds = tag.get(0).unwrap();
        if !tag[1].is_empty() {
            if let Some(capture) = active.as_mut() {
                if capture.element.eq_ignore_ascii_case(&tag[2]) {
                    capture.depth -= 1;
                    if capture.depth == 0 { finish(active.take().unwrap(), bounds.start(), &mut results); }
                }
            }
            continue;
        }
        let mut class = "";
        let mut href = "";
        for attribute in ATTRIBUTES.captures_iter(&tag[3]) {
            let value = attribute.get(2).or_else(|| attribute.get(3)).unwrap().as_str();
            if attribute[1].eq_ignore_ascii_case("class") { class = value; }
            else { href = value; }
        }
        let is_title = tag[2].eq_ignore_ascii_case("a") && class.split_ascii_whitespace().any(|class| class == "result__a");
        let is_snippet = class.split_ascii_whitespace().any(|class| class == "result__snippet");
        if !is_title && !is_snippet {
            if let Some(capture) = active.as_mut() {
                if capture.element.eq_ignore_ascii_case(&tag[2]) && !tag[3].trim_end().ends_with('/') {
                    capture.depth += 1;
                }
            }
            continue;
        }
        // A new result field also terminates an unclosed previous field. Text
        // ranges never overlap, so malformed HTML cannot cause quadratic scans.
        if let Some(capture) = active.take() { finish(capture, bounds.start(), &mut results); }
        if is_title {
            if results.len() == MAX_RESULTS { break; }
            current = None;
        }
        if is_title {
            if let Some(url) = ddg_result_url(href) {
                current = Some(results.len());
                results.push(SearchResult { title: String::new(), url,
                    content: String::new(), raw_content: None, score: None });
            }
        }
        if let Some(index) = current {
            active = Some(Capture { element: tag.get(2).unwrap().as_str(), start: bounds.end(), depth: 1, index, is_title });
        }
    }
    if let Some(capture) = active { finish(capture, html.len(), &mut results); }
    Ok(results)
}

#[derive(Debug, Deserialize)]
struct ExtractResponse { #[serde(default)] results: Vec<ExtractResult> }
#[derive(Debug, Deserialize)]
struct ExtractResult { url: String, #[serde(default)] raw_content: String }

pub struct TavilyClient { http: Client, api_key: String, endpoint: String, extract_endpoint: String }
impl TavilyClient {
    pub fn new(api_key: String) -> Result<Self, String> {
        if api_key.trim().is_empty() { return Err("Tavily 凭据不可用".into()); }
        let http = Client::builder().connect_timeout(Duration::from_secs(8)).timeout(Duration::from_secs(20)).build().map_err(|_| "Tavily 客户端初始化失败".to_string())?;
        Ok(Self { http, api_key, endpoint: TAVILY_ENDPOINT.into(), extract_endpoint: TAVILY_EXTRACT_ENDPOINT.into() })
    }
    #[cfg(test)]
    pub fn with_endpoint(api_key: String, endpoint: String) -> Result<Self, String> {
        let mut client = Self::new(api_key)?;
        client.extract_endpoint = endpoint.replace("/search", "/extract");
        client.endpoint = endpoint;
        Ok(client)
    }

    /// Searches only metadata supplied by the caller. The API key is sent as
    /// bearer auth, never included in JSON or in an error string.
    pub async fn search(&self, query: &str, domains: Option<&[&str]>) -> Result<SearchResponse, String> {
        if query.trim().is_empty() { return Err("搜索查询不能为空".into()); }
        let mut body = json!({
            "query": clip(query, 500), "search_depth": "basic", "chunks_per_source": 1,
            "max_results": MAX_RESULTS, "include_answer": false, "include_raw_content": "text",
            "topic": "general", "safe_search": false
        });
        if let Some(domains) = domains { body["include_domains"] = json!(domains); body["include_domains_mode"] = json!("restrict"); }
        self.post_json(&self.endpoint, body).await
    }

    async fn extract(&self, urls: &[String]) -> Result<Vec<ExtractResult>, String> {
        if urls.is_empty() { return Ok(Vec::new()); }
        let body = json!({
            "urls": urls,
            "extract_depth": "basic", "format": "text", "timeout": 10.0,
            "include_images": false, "include_favicon": false
        });
        let response: ExtractResponse = self.post_json(&self.extract_endpoint, body).await?;
        Ok(response.results)
    }

    async fn post_json<T: for<'de> Deserialize<'de>>(&self, endpoint: &str, body: Value) -> Result<T, String> {
        let mut response = self.http.post(endpoint)
            .header(header::AUTHORIZATION, format!("Bearer {}", self.api_key))
            .json(&body).send().await.map_err(|_| "Tavily 请求失败".to_owned())?;
        let status = response.status();
        if !status.is_success() { return Err(format!("Tavily 返回 HTTP {}", status.as_u16())); }
        if response.content_length().is_some_and(|size| size > MAX_HTTP_RESPONSE_BYTES as u64) { return Err("Tavily 响应过大".into()); }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "Tavily 响应读取失败".to_owned())? {
            if bytes.len().saturating_add(chunk.len()) > MAX_HTTP_RESPONSE_BYTES { return Err("Tavily 响应过大".into()); }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice(&bytes).map_err(|_| "Tavily 响应格式无效".to_owned())
    }
}

#[derive(Clone)]
pub(crate) struct RawCandidate { title: String, url: String, text: String }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebOutlineStage {
    TargetedSearch,
    BroadSearch,
    Match,
    Outline,
    DdgSearch,
    DdgSearchBroad,
    DdgMatch,
    DdgOutline,
}
impl WebOutlineStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TargetedSearch => "web_search",
            Self::BroadSearch => "web_search_broad",
            Self::Match => "web_match",
            Self::Outline => "web_outline",
            Self::DdgSearch => "ddg_search",
            Self::DdgSearchBroad => "ddg_search_broad",
            Self::DdgMatch => "ddg_match",
            Self::DdgOutline => "ddg_outline",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SearchPhase { TargetedSearch, BroadSearch, Match, Outline }

#[async_trait::async_trait]
pub(crate) trait SearchEngine: Send + Sync {
    async fn search_engine(&self, query: &str, domains: Option<&[&str]>) -> Result<SearchResponse, WebOutlineRejection>;
    async fn enrich_engine(&self, response: SearchResponse) -> Result<Vec<RawCandidate>, String>;
    fn stage(&self, phase: SearchPhase) -> WebOutlineStage;
}
#[async_trait::async_trait]
impl SearchEngine for TavilyClient {
    async fn search_engine(&self, query: &str, domains: Option<&[&str]>) -> Result<SearchResponse, WebOutlineRejection> { self.search(query, domains).await.map_err(|_| WebOutlineRejection::SearchUnavailable) }
    async fn enrich_engine(&self, response: SearchResponse) -> Result<Vec<RawCandidate>, String> { enrich_candidates(self, response).await }
    fn stage(&self, phase: SearchPhase) -> WebOutlineStage {
        match phase { SearchPhase::TargetedSearch => WebOutlineStage::TargetedSearch, SearchPhase::BroadSearch => WebOutlineStage::BroadSearch, SearchPhase::Match => WebOutlineStage::Match, SearchPhase::Outline => WebOutlineStage::Outline }
    }
}

#[async_trait::async_trait]
impl SearchEngine for DuckDuckGoClient {
    async fn search_engine(&self, query: &str, domains: Option<&[&str]>) -> Result<SearchResponse, WebOutlineRejection> {
        self.search(query, domains).await.map_err(|error| match error { DdgError::RateLimited => WebOutlineRejection::RateLimited, DdgError::Unavailable => WebOutlineRejection::SearchUnavailable })
    }
    async fn enrich_engine(&self, response: SearchResponse) -> Result<Vec<RawCandidate>, String> {
        let mut candidates = response.results.into_iter().filter_map(|result| {
            is_public_http_url(&result.url).then(|| RawCandidate { title: clip(&result.title, 300), url: result.url, text: result.content })
        }).take(MAX_CANDIDATES).collect::<Vec<_>>();
        for candidate in &mut candidates {
            let snippet = candidate.text.clone();
            if let Ok(text) = self.fetch_page_text(&candidate.url).await { candidate.text = text; }
            else { candidate.text = snippet; }
            candidate.text = clip(&candidate.text, MAX_PROMPT_CHARS_PER_CANDIDATE);
        }
        Ok(candidates)
    }
    fn stage(&self, phase: SearchPhase) -> WebOutlineStage {
        match phase { SearchPhase::TargetedSearch => WebOutlineStage::DdgSearch, SearchPhase::BroadSearch => WebOutlineStage::DdgSearchBroad, SearchPhase::Match => WebOutlineStage::DdgMatch, SearchPhase::Outline => WebOutlineStage::DdgOutline }
    }
}

/// Find a usable web outline. Fixed `Rejected` diagnostics require local
/// fallback; cancellation is terminal and is never hidden. The optional result
/// is retained for the caller's existing lookup interface.
pub(crate) async fn find_outline<E: SearchEngine>(
    engine: &E,
    llm: &dyn LlmClient,
    lookup: &OutlineLookup,
    budget: WebOutlineBudget,
) -> Result<Option<VerifiedWebOutline>, WebOutlineError> {
    find_outline_with_progress(engine, llm, lookup, budget, &|_| {}).await
}

/// Same lookup, with synchronous lifecycle notifications suitable for a job
/// updater. Notifications contain no local text, fetched content, hashes, or URLs.
pub(crate) async fn find_outline_with_progress<E: SearchEngine>(
    engine: &E,
    llm: &dyn LlmClient,
    lookup: &OutlineLookup,
    budget: WebOutlineBudget,
    on_stage: &(dyn Fn(WebOutlineStage) + Sync),
) -> Result<Option<VerifiedWebOutline>, WebOutlineError> {
    validate_lookup(lookup)?;
    check_cancelled(llm)?;
    let query = search_query(lookup);
    on_stage(engine.stage(SearchPhase::TargetedSearch));
    let targeted = engine.search_engine(&query, Some(TARGET_DOMAINS)).await;
    check_cancelled(llm)?;
    on_stage(engine.stage(SearchPhase::Match));
    let targeted = match targeted {
        Ok(response) => prepare_candidate_set(engine, lookup, Some(response)).await,
        Err(reason) => Err(reason),
    };
    let selected = match targeted {
        Ok(set) => set,
        Err(WebOutlineRejection::AmbiguousSources) => return Err(WebOutlineError::Rejected(WebOutlineRejection::AmbiguousSources)),
        Err(WebOutlineRejection::RateLimited) => return Err(WebOutlineError::Rejected(WebOutlineRejection::RateLimited)),
        Err(_) => {
            check_cancelled(llm)?;
            on_stage(engine.stage(SearchPhase::BroadSearch));
            let broad = engine.search_engine(&query, None).await;
            check_cancelled(llm)?;
            on_stage(engine.stage(SearchPhase::Match));
            let broad = broad.map_err(WebOutlineError::Rejected)?;
            prepare_candidate_set(engine, lookup, Some(broad)).await.map_err(WebOutlineError::Rejected)?
        }
    };
    check_cancelled(llm)?;
    let verdict = call_edition_llm(llm, lookup, &selected, budget).await?;
    check_cancelled(llm)?;
    let proof = validate_edition_verdict(&selected, verdict)
        .ok_or(WebOutlineError::Rejected(WebOutlineRejection::EditionUnverified))?;
    on_stage(engine.stage(SearchPhase::Outline));
    let value = call_outline_llm(llm, lookup, &selected.candidates, budget).await?;
    check_cancelled(llm)?;
    validate_llm_outline(lookup, &selected.candidates, &selected.primary, proof, value)
        .map(Some).ok_or(WebOutlineError::Rejected(WebOutlineRejection::InvalidOutline))
}

struct CandidateSet { candidates: Vec<RawCandidate>, primary: RawCandidate, proof_paragraph: String, proof_chapter_index: usize }
async fn prepare_candidate_set<E: SearchEngine>(
    engine: &E, lookup: &OutlineLookup, response: Option<SearchResponse>,
) -> Result<CandidateSet, WebOutlineRejection> {
    let response = response.ok_or(WebOutlineRejection::SearchUnavailable)?;
    let candidates = engine.enrich_engine(response).await
        .map_err(|_| WebOutlineRejection::SearchUnavailable)?;
    select_candidate_set(lookup, candidates)
}

/// Match bibliographic candidates by registrable-domain grouping keys, handling
/// common public suffixes explicitly and preserving IP/single-label identities.
/// A group supplies every chapter, while its first proof-bearing page alone
/// anchors the unchanged AI edition comparison; retain all pages for provenance.
/// Shared chapter bodies must agree after normalizing their first 200 characters
/// (excluding the heading prefix); identical mirrors and disjoint pages coexist.
fn select_candidate_set(lookup: &OutlineLookup, candidates: Vec<RawCandidate>) -> Result<CandidateSet, WebOutlineRejection> {
    let mut groups: BTreeMap<String, Vec<RawCandidate>> = BTreeMap::new();
    let mut has_text = false;
    let mut insufficient = false;
    for mut candidate in candidates {
        let original = candidate.text;
        candidate.text = clip(&original, MAX_PROMPT_CHARS_PER_CANDIDATE);
        has_text |= !candidate.text.trim().is_empty();
        if !bibliographic_identity_matches(lookup, &candidate) { continue; }
        if original.len() > candidate.text.len()
            && source_first_paragraph(lookup, &candidate).is_none() {
            insufficient |= source_first_paragraph(lookup, &RawCandidate { text: original, ..candidate.clone() }).is_some();
        }
        let Ok(url) = Url::parse(&candidate.url) else { continue; };
        let Some(host) = url.host_str() else { continue; };
        let group_key = registrable_domain_key(host);
        groups.entry(group_key.to_owned()).or_default().push(candidate);
    }
    if !has_text { return Err(WebOutlineRejection::EditionTextUnavailable); }
    let mut complete = None;
    for group in groups.into_values() {
        let Some((primary_index, proof_paragraph)) = group.iter().enumerate().find_map(|(index, candidate)| {
            source_first_paragraph(lookup, candidate).map(|paragraph| (index, paragraph))
        }) else { continue; };
        if !lookup.chapters.iter().all(|chapter| group.iter().any(|candidate| {
            chapter_section(chapter, &lookup.chapters, &candidate.text)
                .is_some_and(|section| section.trim().chars().count() >= FIRST_CHAPTER_MIN_CHARS)
        })) {
            insufficient = true;
            continue;
        }
        if group.len() > 1 {
            for chapter in &lookup.chapters {
                let mut shared_prefix = None;
                for candidate in &group {
                    let Some(section) = chapter_section(chapter, &lookup.chapters, &candidate.text) else { continue; };
                    let Some(body) = chapter_body(chapter, section) else { continue; };
                    let end = body.char_indices().nth(200).map_or(body.len(), |(offset, _)| offset);
                    let prefix = normalize(&body[..end]);
                    // Equality with the first located section proves pairwise
                    // equality without normalizing each page for every pair.
                    if let Some(expected) = &shared_prefix {
                        if expected != &prefix { return Err(WebOutlineRejection::AmbiguousSources); }
                    } else {
                        shared_prefix = Some(prefix);
                    }
                }
            }
        }
        if complete.is_some() { return Err(WebOutlineRejection::AmbiguousSources); }
        complete = Some(CandidateSet {
            primary: group[primary_index].clone(), candidates: group, proof_paragraph,
            proof_chapter_index: proof_chapter(lookup).ok_or(WebOutlineRejection::InvalidLookup)?.index,
        });
    }
    complete.ok_or(if insufficient { WebOutlineRejection::EvidenceInsufficient } else { WebOutlineRejection::EditionUnverified })
}

/// Bounded public-suffix handling, not a complete Public Suffix List. Unknown
/// suffixes retain the last-two-label fallback. No extra Unicode/punycode
/// normalization is performed here; distinct representations stay separate.
fn registrable_domain_key(host: &str) -> &str {
    const COMMON_PUBLIC_SUFFIXES: &[&str] = &[
        "co.uk", "org.uk", "ac.uk", "gov.uk", "com.cn", "net.cn", "org.cn", "gov.cn",
        "com.tw", "com.hk", "com.sg", "com.my", "co.jp", "co.kr", "com.au", "net.au",
        "github.io", "gitlab.io", "vercel.app", "netlify.app", "pages.dev",
        "com.br", "com.mx", "com.ar", "co.in", "co.za",
    ];
    let host = host.strip_suffix('.').unwrap_or(host);
    if host.trim_matches(['[', ']']).parse::<IpAddr>().is_ok() { return host; }
    for &suffix in COMMON_PUBLIC_SUFFIXES {
        if host == suffix { return host; }
        let Some(prefix) = host.strip_suffix(suffix).and_then(|prefix| prefix.strip_suffix('.')) else { continue; };
        let label = prefix.rsplit('.').next().unwrap_or(prefix);
        return &host[prefix.len() - label.len()..];
    }
    let mut labels = host.rsplitn(3, '.');
    let last = labels.next().unwrap_or(host);
    match labels.next() {
        Some(previous) => &host[host.len() - previous.len() - last.len() - 1..],
        None => host,
    }
}

fn first_paragraph(text: &str, max_chars: usize) -> Option<String> {
    let text = text.trim_start();
    let mut end = 0;
    let mut chars = 0;
    for line in text.split_inclusive('\n') {
        if line.trim().is_empty() { break; }
        end += line.len();
        chars += line.chars().count();
        if chars >= max_chars { break; }
    }
    let paragraph = clip(text.get(..end)?.trim_end(), max_chars);
    (paragraph.chars().count() >= FIRST_CHAPTER_MIN_CHARS).then_some(paragraph)
}

fn source_first_paragraph(lookup: &OutlineLookup, candidate: &RawCandidate) -> Option<String> {
    let chapter = proof_chapter(lookup)?;
    let section = chapter_section(chapter, &lookup.chapters, &candidate.text)?;
    first_paragraph(chapter_body(chapter, section)?, MAX_EDITION_PARAGRAPH_CHARS)
}

/// Consume only the heading prefix, preserving prose on the same line as well
/// as prose following a standalone heading for both proof and conflict checks.
fn chapter_body<'a>(chapter: &LocalOutlineChapter, section: &'a str) -> Option<&'a str> {
    let (heading_line, rest) = section.split_once('\n').unwrap_or((section, ""));
    let heading_line = heading_line.trim_start();
    let wanted = normalize(&chapter.heading);
    let after_heading = heading_line.char_indices().take(160).find_map(|(offset, _)| {
        (normalize(&heading_line[..offset]) == wanted).then_some(&heading_line[offset..])
    }).or_else(|| (normalize(heading_line) == wanted).then_some(""))?;
    let body = if after_heading.trim().is_empty() { rest } else { &section[heading_line.len() - after_heading.len()..] };
    Some(body.trim_start())
}

async fn enrich_candidates(tavily: &TavilyClient, response: SearchResponse) -> Result<Vec<RawCandidate>, String> {
    let mut candidates: Vec<RawCandidate> = response.results.into_iter().filter_map(|result| {
        is_public_http_url(&result.url).then(|| RawCandidate { title: clip(&result.title, 300), url: result.url, text: result.raw_content.unwrap_or_default() })
    }).take(MAX_CANDIDATES).collect();
    let missing: Vec<String> = candidates.iter()
        .filter(|candidate| candidate.text.trim().is_empty())
        .map(|candidate| candidate.url.clone()).collect();
    if !missing.is_empty() {
        let extracted = tavily.extract(&missing).await?;
        for result in extracted {
            if let Some(candidate) = candidates.iter_mut().find(|candidate| candidate.url == result.url) { candidate.text = result.raw_content; }
        }
    }
    for candidate in &mut candidates { candidate.text = clip(&candidate.text, MAX_PROMPT_CHARS_PER_CANDIDATE); }
    Ok(candidates)
}

async fn call_edition_llm(
    llm: &dyn LlmClient, lookup: &OutlineLookup, selected: &CandidateSet, budget: WebOutlineBudget,
) -> Result<Value, WebOutlineError> {
    let chapter = proof_chapter(lookup).ok_or(WebOutlineError::Rejected(WebOutlineRejection::InvalidLookup))?;
    let local_paragraph = first_paragraph(&chapter.text, MAX_EDITION_PARAGRAPH_CHARS)
        .ok_or(WebOutlineError::Rejected(WebOutlineRejection::EditionTextUnavailable))?;
    let schema = json!({"type":"object","additionalProperties":false,
        "required":["verdict","source_url"],"properties":{
            "verdict":{"type":"string","enum":["same","different","uncertain"]},
            "source_url":{"type":"string","minLength":1}
        }});
    let prompt = json!({"book_title":lookup.book_title,"author":lookup.author,
        "chapter_index":chapter.index,"chapter_heading":chapter.heading,
        "local_paragraph":local_paragraph,
        "sources":[{"url":selected.primary.url,"title":selected.primary.title,
            "paragraph":selected.proof_paragraph}]}).to_string();
    let messages = vec![
        ChatMessage { role:"system".into(), content:"Compare ONLY the supplied first substantive chapter paragraphs of the local book and the public source. Return same only if they are the same underlying edition despite punctuation, spacing or minor editorial drift. Return different for another story or edition, uncertain if evidence cannot establish sameness. Use the exact supplied source URL; never invent one. Do not echo any local prose.".into() },
        ChatMessage { role:"user".into(), content:prompt },
    ];
    if !web_outline_request_fits(&messages, &schema, budget) {
        return Err(WebOutlineError::Rejected(WebOutlineRejection::ContextInsufficient));
    }
    llm.structured_with_usage(StructuredRequest {
        task:TaskKind::Extract, messages, schema:&schema, schema_name:"web_edition_paragraph_v1",
        temperature:0.0, max_tokens:budget.output_reserve.min(u32::MAX as usize) as u32,
    }).await.map(|response| response.value).map_err(|error| match error {
        LlmError::Cancelled => WebOutlineError::Cancelled,
        _ => WebOutlineError::Rejected(WebOutlineRejection::EditionUnverified),
    })
}

fn validate_edition_verdict(selected: &CandidateSet, value: Value) -> Option<EditionProof> {
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Verdict { verdict: String, source_url: String }
    let verdict: Verdict = serde_json::from_value(value).ok()?;
    if verdict.verdict != "same" || !is_public_http_url(&verdict.source_url)
        || verdict.source_url != selected.primary.url { return None; }
    let evidence = clip(&selected.proof_paragraph, MAX_EVIDENCE_CHARS);
    Some(EditionProof { chapter_index: selected.proof_chapter_index, method:EditionProofMethod::AiParagraph,
        provenance:WebOutlineProvenance { url:verdict.source_url, title:selected.primary.title.clone(), evidence } })
}

async fn call_outline_llm(
    llm: &dyn LlmClient, lookup: &OutlineLookup, candidates: &[RawCandidate], budget: WebOutlineBudget,
) -> Result<Value, WebOutlineError> {
    let sources: Vec<Value> = candidates.iter().map(|candidate| json!({
        "title": candidate.title, "url": candidate.url, "text": candidate.text,
    })).collect();
    let chapters: Vec<Value> = lookup.chapters.iter().map(|chapter| json!({"index": chapter.index, "heading": chapter.heading})).collect();
    let prompt = json!({"book_title": lookup.book_title, "author": lookup.author, "proof_chapter_index": proof_chapter(lookup).map(|chapter| chapter.index), "chapters": chapters, "sources": sources}).to_string();
    let schema = outline_schema();
    let messages = vec![
        ChatMessage { role: "system".into(), content: "Build a chapter-by-chapter outline only from the supplied online sources. Return every requested chapter exactly once, preserving its index and heading. Each summary, plot, and character claim needs a short literal evidence quote from its cited URL. For plot_chapter_indices and every character chapter_indices, list only source chapters on that same cited page whose heading and section contain that exact quote. Do not invent a URL, quote, chapter, or fact. Local chapter prose is unavailable and must not be inferred.".into() },
        ChatMessage { role: "user".into(), content: prompt },
    ];
    if !web_outline_request_fits(&messages, &schema, budget) { return Err(WebOutlineError::Rejected(WebOutlineRejection::ContextInsufficient)); }
    // Exactly one compact structured LLM call for this bounded candidate set.
    llm.structured_with_usage(StructuredRequest {
        task: TaskKind::Extract, messages, schema: &schema, schema_name: "verified_web_outline_v1",
        temperature: 0.0, max_tokens: budget.output_reserve.min(u32::MAX as usize) as u32,
    }).await.map(|response| response.value).map_err(|error| match error {
        LlmError::Cancelled => WebOutlineError::Cancelled,
        _ => WebOutlineError::Rejected(WebOutlineRejection::CompactionUnavailable),
    })
}

fn web_outline_request_fits(messages: &[ChatMessage], schema: &Value, budget: WebOutlineBudget) -> bool {
    let messages_size = messages.iter().fold(0usize, |sum, message| sum.saturating_add(message.content.len()).saturating_add(message.role.len()).saturating_add(16));
    let required = messages_size.saturating_add(schema.to_string().len()).saturating_add(256).saturating_add(budget.output_reserve);
    budget.output_reserve > 0 && required <= budget.context_limit
}

fn outline_schema() -> Value { json!({
    "type":"object", "additionalProperties":false,
    "required":["plot_summary","plot_source_url","plot_evidence","plot_chapter_indices","characters","chapters"],
    "properties": {
      "plot_summary":{"type":"string","minLength":1,"maxLength":1200},
      "plot_source_url":{"type":"string","minLength":1}, "plot_evidence":{"type":"string","minLength":12,"maxLength":280},
      "plot_chapter_indices":{"type":"array","minItems":1,"maxItems":200,"items":{"type":"integer","minimum":0}},
      "characters":{"type":"array","maxItems":24,"items":{"type":"object","additionalProperties":false,"required":["name","baseline","source_url","evidence","chapter_indices"],"properties":{"name":{"type":"string","minLength":1,"maxLength":120},"baseline":{"type":"string","minLength":1,"maxLength":800},"source_url":{"type":"string","minLength":1},"evidence":{"type":"string","minLength":12,"maxLength":280},"chapter_indices":{"type":"array","minItems":1,"maxItems":200,"items":{"type":"integer","minimum":0}}}}},
      "chapters":{"type":"array","maxItems":200,"items":{"type":"object","additionalProperties":false,"required":["index","heading","summary","source_url","evidence"],"properties":{"index":{"type":"integer","minimum":0},"heading":{"type":"string","minLength":1,"maxLength":500},"summary":{"type":"string","minLength":1,"maxLength":1200},"source_url":{"type":"string","minLength":1},"evidence":{"type":"string","minLength":12,"maxLength":280}}}}
    }
}) }

fn validate_llm_outline(
    lookup: &OutlineLookup, candidates: &[RawCandidate], primary: &RawCandidate, proof: EditionProof, value: Value,
) -> Option<VerifiedWebOutline> {
    let output: LlmOutline = serde_json::from_value(value).ok()?;
    let plot_chapters = validated_source_chapters(lookup, candidates, &output.plot_source_url, &output.plot_evidence, &output.plot_chapter_indices)?;
    let plot_provenance = provenance_for_chapter(lookup, candidates, lookup.chapters.iter().find(|chapter| chapter.index == plot_chapters[0])?, output.plot_source_url, output.plot_evidence)?;
    let mut characters = Vec::with_capacity(output.characters.len());
    for character in output.characters {
        if character.name.trim().is_empty() || character.baseline.trim().is_empty() { return None; }
        let source_chapters = validated_source_chapters(lookup, candidates, &character.source_url, &character.evidence, &character.chapter_indices)?;
        let chapter = lookup.chapters.iter().find(|chapter| chapter.index == source_chapters[0])?;
        characters.push(WebCharacterCard {
            name: clip(&character.name, 120), baseline: clip(&character.baseline, 800), source_chapters,
            provenance: provenance_for_chapter(lookup, candidates, chapter, character.source_url, character.evidence)?,
        });
    }
    let expected: BTreeMap<usize, &LocalOutlineChapter> = lookup.chapters.iter().map(|chapter| (chapter.index, chapter)).collect();
    if expected.len() != lookup.chapters.len() || output.chapters.len() != expected.len() { return None; }
    let mut chapters = Vec::with_capacity(output.chapters.len());
    let mut seen = BTreeSet::new();
    for chapter in output.chapters {
        let local = expected.get(&chapter.index)?;
        if !seen.insert(chapter.index) || !headings_equivalent(&chapter.heading, &local.heading) || chapter.summary.trim().is_empty() { return None; }
        chapters.push(VerifiedChapterOutline {
            index: chapter.index, heading: local.heading.clone(), summary: clip(&chapter.summary, 1200),
            provenance: provenance_for_chapter(lookup, candidates, local, chapter.source_url, chapter.evidence)?,
        });
    }
    chapters.sort_by_key(|chapter| chapter.index);
    if !chapters.iter().zip(&lookup.chapters).all(|(mapped, local)| mapped.index == local.index && headings_equivalent(&mapped.heading, &local.heading)) { return None; }
    let source = proof.provenance.clone();
    if source.url != primary.url || proof.chapter_index != proof_chapter(lookup)?.index { return None; }
    Some(VerifiedWebOutline {
        edition_proof: proof,
        source, plot_summary: clip(&output.plot_summary, 1200), plot_chapters, plot_provenance, characters, chapters,
    })
}

fn validated_source_chapters(
    lookup: &OutlineLookup, candidates: &[RawCandidate], url: &str, evidence: &str, indices: &[usize],
) -> Option<Vec<usize>> {
    if indices.is_empty() || indices.len() > lookup.chapters.len() { return None; }
    let mut unique = BTreeSet::new(); let mut validated = Vec::with_capacity(indices.len());
    for index in indices { 
        if !unique.insert(*index) { return None; }
        let chapter = lookup.chapters.iter().find(|chapter| chapter.index == *index)?;
        provenance_for_chapter(lookup, candidates, chapter, url.to_owned(), evidence.to_owned())?;
        validated.push(*index);
    }
    validated.sort_unstable(); Some(validated)
}

fn provenance_for_chapter(
    lookup: &OutlineLookup, candidates: &[RawCandidate], chapter: &LocalOutlineChapter, url: String, evidence: String,
) -> Option<WebOutlineProvenance> {
    let candidate = candidates.iter().find(|candidate| candidate.url == url)?;
    let evidence = evidence.trim();
    if evidence.chars().count() < 12 || evidence.chars().count() > MAX_EVIDENCE_CHARS { return None; }
    let section = chapter_section(chapter, &lookup.chapters, &candidate.text)?;
    if !section.contains(evidence) { return None; }
    Some(WebOutlineProvenance { url, title: candidate.title.clone(), evidence: evidence.to_owned() })
}

#[derive(Deserialize)]
struct LlmOutline { plot_summary: String, plot_source_url: String, plot_evidence: String, plot_chapter_indices: Vec<usize>, characters: Vec<LlmCharacter>, chapters: Vec<LlmChapter> }
#[derive(Deserialize)]
struct LlmCharacter { name: String, baseline: String, source_url: String, evidence: String, chapter_indices: Vec<usize> }
#[derive(Deserialize)]
struct LlmChapter { index: usize, heading: String, summary: String, source_url: String, evidence: String }


fn normalize(value: &str) -> String {
    canonicalize_chapter_numbers(value).nfkc().flat_map(char::to_lowercase).filter(|char| char.is_alphanumeric()).collect()
}

fn canonicalize_chapter_numbers(value: &str) -> String {
    let chars: Vec<char> = value.chars().collect(); let mut output = String::new(); let mut index = 0;
    while index < chars.len() {
        if chars[index] == '第' {
            if let Some(end) = ((index + 1)..(index + 10).min(chars.len())).find(|&end| chars[end] == '章') {
                let number: String = chars[(index + 1)..end].iter().collect();
                if let Some(number) = parse_chapter_number(&number) { output.push_str(&format!("chapter{number}chapter")); index = end + 1; continue; }
            }
        }
        output.push(chars[index]); index += 1;
    }
    output
}

fn parse_chapter_number(value: &str) -> Option<u32> {
    if let Ok(number) = value.parse::<u32>() { return (number > 0).then_some(number); }
    let digit = |ch| match ch { '零' | '〇' => Some(0), '一' => Some(1), '二' | '两' => Some(2), '三' => Some(3), '四' => Some(4), '五' => Some(5), '六' => Some(6), '七' => Some(7), '八' => Some(8), '九' => Some(9), _ => None };
    let mut total = 0; let mut current = 0;
    for ch in value.chars() {
        match ch { '十' => { total += current.max(1) * 10; current = 0; }, '百' => { total += current.max(1) * 100; current = 0; }, '千' => { total += current.max(1) * 1_000; current = 0; }, _ => current = digit(ch)? }
    }
    let number = total + current; (number > 0).then_some(number)
}

fn headings_equivalent(left: &str, right: &str) -> bool { normalize(left) == normalize(right) }

fn find_heading_offset(heading: &str, text: &str) -> Option<usize> {
    let wanted = normalize(heading);
    if wanted.chars().count() < 2 { return None; }
    let mut offset = 0;
    // Only a source line's heading prefix can establish a chapter boundary.
    // An inline reference to another chapter remains in its original section.
    text.split_inclusive('\n').find_map(|line| {
        let start = offset;
        offset += line.len();
        let trimmed = line.trim_start();
        let prefix: String = trimmed.chars().take(160).collect();
        normalize(&prefix).starts_with(&wanted).then_some(start + line.len() - trimmed.len())
    })
}

fn chapter_section<'a>(chapter: &LocalOutlineChapter, all: &[LocalOutlineChapter], text: &'a str) -> Option<&'a str> {
    let start = find_heading_offset(&chapter.heading, text)?;
    let end = all.iter().filter_map(|other| find_heading_offset(&other.heading, text)).filter(|offset| *offset > start).min().unwrap_or(text.len());
    text.get(start..end)
}

fn bibliographic_identity_matches(lookup: &OutlineLookup, candidate: &RawCandidate) -> bool {
    let haystack = normalize(&format!("{} {}", candidate.title, candidate.text));
    let title = normalize(&lookup.book_title);
    if title.chars().count() < 2 || !haystack.contains(&title) { return false; }
    match lookup.author.as_deref().map(normalize).filter(|author| !author.is_empty()) {
        Some(author) => ![(&candidate.title, true), (&candidate.text, false)].into_iter().any(|(text, is_title)| {
            let mut consumed_until = 0;
            text.match_indices("作者").any(|(offset, _)| {
                if offset < consumed_until { return false; }
                let suffix = text[offset + "作者".len()..].trim_start();
                let name = if let Some(name) = suffix.strip_prefix([':', '：']) {
                    name.trim_start()
                } else if is_title {
                    suffix
                } else {
                    return false;
                };
                let end = name.find(|character: char| !character.is_alphanumeric() && character != '·').unwrap_or(name.len());
                consumed_until = text.len() - name.len() + end;
                let found = normalize(&name[..end]);
                !found.is_empty() && found != author
            })
        }), None => true,
    }
}
fn is_public_http_url(value: &str) -> bool {
    let Ok(url) = Url::parse(value) else { return false; };
    if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() || url.password().is_some() { return false; }
    let Some(host) = url.host_str() else { return false; };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    if host.eq_ignore_ascii_case("localhost") || host.ends_with(".localhost") { return false; }
    match host.parse::<IpAddr>() {
        Ok(ip) => is_public_ip(ip),
        Err(_) => true,
    }
}
fn clip(value: impl AsRef<str>, max: usize) -> String { value.as_ref().chars().take(max).collect() }
fn proof_chapter(lookup: &OutlineLookup) -> Option<&LocalOutlineChapter> {
    lookup.chapters.iter().find(|chapter| !matches!(normalize(&chapter.heading).as_str(),
        "前言" | "序言" | "引言" | "作者序" | "译者序" | "自序" | "代序" | "preface" | "foreword" | "introduction"
    ))
}

fn search_query(lookup: &OutlineLookup) -> String {
    format!("{} {} {} 小说 章节 剧情 人物", lookup.book_title.trim(), lookup.author.as_deref().unwrap_or("").trim(), proof_chapter(lookup).map(|chapter| chapter.heading.trim()).unwrap_or(""))
}
fn validate_lookup(lookup: &OutlineLookup) -> Result<(), WebOutlineError> {
    if lookup.book_title.trim().is_empty() || lookup.chapters.is_empty() || lookup.chapters.len() > 200 || lookup.chapters.iter().any(|chapter| chapter.heading.trim().is_empty()) || proof_chapter(lookup).is_none_or(|chapter| chapter.text.trim().is_empty()) { return Err(WebOutlineError::Rejected(WebOutlineRejection::InvalidLookup)); }
    let mut indices = BTreeSet::new(); let mut headings = BTreeSet::new();
    if !lookup.chapters.iter().all(|chapter| indices.insert(chapter.index) && headings.insert(normalize(&chapter.heading))) { return Err(WebOutlineError::Rejected(WebOutlineRejection::InvalidLookup)); }
    Ok(())
}
fn check_cancelled(llm: &dyn LlmClient) -> Result<(), WebOutlineError> { llm.check_cancelled().map_err(map_llm_error) }
fn map_llm_error(error: LlmError) -> WebOutlineError { match error { LlmError::Cancelled => WebOutlineError::Cancelled, _ => WebOutlineError::Unavailable } }

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::{matchers::{header, method, path}, Mock, MockServer, ResponseTemplate};
    use crate::llm::{StructuredResponse, TextRequest};
    use parking_lot::Mutex;
    use std::sync::{atomic::{AtomicUsize, Ordering}, Arc};
    use wiremock::{Match, Request};


    #[test]
    fn html_to_text_strips_scripts_styles_and_noscript_content() {
        let html = "<STYLE>.x{content:'<p>hidden</p>'}</STYLE><script>var a=\"<p>hidden\";</script><p>第一章&nbsp;雨夜</p><noscript><p>hidden fallback</p></noscript><p>林昭推门。</p>";
        assert_eq!(html_to_text(html), "第一章 雨夜\n林昭推门。");
    }

    #[test]
    fn html_to_text_hidden_openings_respect_quoted_attribute_boundaries() {
        for tag in ["script", "style", "noscript"] {
            for quote in ['\'', '"'] {
                let html = format!("<{tag} data-note={quote}> </{tag}>{quote}>HIDDEN_BODY</{tag}><p>可见正文</p>");
                assert_eq!(html_to_text(&html), "可见正文");
            }
        }
    }

    #[test]
    fn html_to_text_decodes_named_and_numeric_entities_after_tags() {
        assert_eq!(html_to_text("<p>&amp; &lt;p&gt; &quot; &#39; &nbsp; &#65; &#x4E2D; &#X1F600;</p>"), "& <p> \" ' A 中 😀");
        assert_eq!(html_to_text("&amp;lt; &unknown; &#xD800; &#99999999; &#oops;"), "&lt; &unknown; &#xD800; &#99999999; &#oops;");
    }

    #[test]
    fn html_to_text_decodes_named_apostrophe_entity() {
        assert_eq!(html_to_text("<p>it&apos;s</p>"), "it's");
    }

    #[test]
    fn html_to_text_removes_comments_including_embedded_tags() {
        assert_eq!(html_to_text("<p>雨<!-- secret > <div>hidden</div> -->夜</p><!-- trailing -->"), "雨夜");
    }

    #[test]
    fn html_to_text_collapses_spaces_and_blank_lines() {
        assert_eq!(html_to_text(" \t 第一章 \t  雨夜 \r\n \n\n 林昭\u{a0}  推门。 \n "), "第一章 雨夜\n林昭 推门。");
        assert_eq!(html_to_text("<p> \t </p><div>\n</div>"), "");
    }

    #[test]
    fn html_to_text_preserves_block_boundaries_and_inline_text() {
        let html = "<div><h1>第一章</h1><p>雨<b>夜</b><br/>林昭</p><ul><li>推门</li><li>取信</li></ul><table><tr><td>钟楼</td><td>子夜</td></tr><tr><td>黎明</td></tr></table></div>";
        assert_eq!(html_to_text(html), "第一章\n雨夜\n林昭\n推门\n取信\n钟楼子夜\n黎明");
    }

    // Keep production URL validation active: a public-looking host resolves
    // only inside this client's test transport to the loopback mock server.
    fn ddg_page_client(server: &MockServer) -> DuckDuckGoClient {
        let transport = *server.address();
        DuckDuckGoClient {
            http: DuckDuckGoClient::http_builder().no_proxy().resolve("public.example", transport).build().unwrap(),
            endpoint: format!("{}/html/", server.uri()),
            resolver: Arc::new(move |_, port| Ok(vec![SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::new(93, 184, 216, 34)), port)])),
            transport_address: Some(transport),
        }
    }
    fn injected_ddg(resolver:DdgResolver, server:&MockServer) -> DuckDuckGoClient {
        let transport=*server.address();
        DuckDuckGoClient { http:DuckDuckGoClient::http_builder().no_proxy().resolve("public.example",transport).build().unwrap(), endpoint:format!("{}/html/",server.uri()), resolver, transport_address:Some(transport) }
    }
    #[tokio::test]
    async fn injected_private_or_mixed_dns_results_are_rejected_before_connection() {
        for addresses in [vec!["127.0.0.1:80".parse().unwrap()], vec!["93.184.216.34:80".parse().unwrap(),"10.0.0.1:80".parse().unwrap()]] {
            let server=MockServer::start().await;
            Mock::given(method("GET")).and(path("/book")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&server).await;
            let client=injected_ddg(Arc::new(move |_,_|Ok(addresses.clone())),&server);
            assert_eq!(client.fetch_page_text(&ddg_page_url(&server)).await,Err(DdgError::Unavailable));
            assert!(server.received_requests().await.unwrap().is_empty());
        }
    }
    #[tokio::test]
    async fn injected_all_public_dns_result_is_accepted_and_ipv6_brackets_are_normalized() {
        let server=MockServer::start().await;
        let seen=Arc::new(parking_lot::Mutex::new(String::new()));let capture=Arc::clone(&seen);
        let client=injected_ddg(Arc::new(move |host,port|{*capture.lock()=host.to_owned();Ok(vec![SocketAddr::new(IpAddr::V4(std::net::Ipv4Addr::new(93,184,216,34)),port)])}),&server);
        assert!(client.resolve_fetch_addresses("[2001:db8::42]",80).await.is_ok());
        assert_eq!(seen.lock().as_str(),"2001:db8::42");
    }
    #[tokio::test]
    async fn injected_dns_resolution_timeout_is_rejected() {
        let server=MockServer::start().await;
        let client=injected_ddg(Arc::new(|_,_|{std::thread::sleep(Duration::from_secs(6));Ok(vec!["93.184.216.34:80".parse().unwrap()])}),&server);
        assert_eq!(client.resolve_fetch_addresses("public.example",80).await,Err(DdgError::Unavailable));
    }


    fn ddg_page_url(server: &MockServer) -> String {
        format!("http://public.example:{}/book", server.address().port())
    }

    #[tokio::test]
    async fn ddg_fetch_returns_page_text_without_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/book"))
            .respond_with(ResponseTemplate::new(200).set_body_string("<style>hidden</style><h1>第一章&nbsp;雨夜</h1><p>林昭<b>推门</b>。&#x4E2D;</p>"))
            .expect(1).mount(&server).await;
        let client = ddg_page_client(&server);
        assert_eq!(client.fetch_page_text(&ddg_page_url(&server)).await.unwrap(), "第一章 雨夜\n林昭推门。中");
        let requests = server.received_requests().await.unwrap();
        assert!(requests[0].body.is_empty());
        assert!(!requests[0].headers.contains_key("authorization"));
        assert!(!requests[0].headers.contains_key("cookie"));
        assert!(requests[0].headers.get("user-agent").unwrap().to_str().unwrap().contains("Mozilla/5.0"));
    }

    #[tokio::test]
    async fn ddg_fetch_clips_text_by_characters() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/book"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!("<p>{}</p>", "章".repeat(MAX_PROMPT_CHARS_PER_CANDIDATE + 1))))
            .expect(1).mount(&server).await;
        assert_eq!(ddg_page_client(&server).fetch_page_text(&ddg_page_url(&server)).await.unwrap(), "章".repeat(MAX_PROMPT_CHARS_PER_CANDIDATE));
    }

    #[tokio::test]
    async fn ddg_fetch_refuses_redirects_before_contacting_private_destination() {
        let public = MockServer::start().await;
        let private = MockServer::start().await;
        Mock::given(method("GET")).and(path("/book"))
            .respond_with(ResponseTemplate::new(302).insert_header("Location", format!("{}/private", private.uri())))
            .expect(1).mount(&public).await;
        Mock::given(method("GET")).and(path("/private"))
            .respond_with(ResponseTemplate::new(200).set_body_string("PRIVATE_PAGE_SECRET"))
            .mount(&private).await;
        let result = ddg_page_client(&public).fetch_page_text(&ddg_page_url(&public)).await;
        assert_eq!(result, Err(DdgError::Unavailable));
        assert!(private.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ddg_fetch_refuses_private_and_invalid_urls_before_request() {
        let server = MockServer::start().await;
        let client = ddg_page_client(&server);
        for url in [server.uri(), "https://127.0.0.1/".into(), "http://10.2.3.4/book".into(),
            "http://192.168.1.2/book".into(), "http://localhost/book".into(), "/relative".into(),
            "not a URL".into(), "file:///book".into(), "https://user:secret@public.example/book".into()] {
            assert_eq!(client.fetch_page_text(&url).await, Err(DdgError::Unavailable));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ddg_fetch_refuses_private_ipv6_literals_before_request() {
        let listener = std::net::TcpListener::bind("[::1]:0").unwrap();
        let server = MockServer::builder().listener(listener).start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_string("IPV6_PRIVATE_SECRET"))
            .mount(&server).await;
        let client = ddg_page_client(&server);
        assert_eq!(client.fetch_page_text(&server.uri()).await, Err(DdgError::Unavailable));
        for url in ["http://[::]/", "http://[fc00::1]/", "http://[fe80::1]/", "http://[::ffff:127.0.0.1]/"] {
            assert_eq!(client.fetch_page_text(url).await, Err(DdgError::Unavailable));
        }
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn ddg_fetch_rejects_oversized_body_and_accepts_exact_limit() {
        for size in [MAX_HTTP_RESPONSE_BYTES, MAX_HTTP_RESPONSE_BYTES + 1] {
            let server = MockServer::start().await;
            Mock::given(method("GET")).and(path("/book"))
                .respond_with(ResponseTemplate::new(200).set_body_raw("x".repeat(size), "text/html"))
                .expect(1).mount(&server).await;
            let result = ddg_page_client(&server).fetch_page_text(&ddg_page_url(&server)).await;
            if size == MAX_HTTP_RESPONSE_BYTES { assert_eq!(result.unwrap(), "x".repeat(MAX_PROMPT_CHARS_PER_CANDIDATE)); }
            else { assert_eq!(result, Err(DdgError::Unavailable)); }
        }
    }

    #[tokio::test]
    async fn ddg_fetch_non_success_status_is_unavailable_and_redacted() {
        for status in [400, 403, 404, 500] {
            let server = MockServer::start().await;
            Mock::given(method("GET")).and(path("/book"))
                .respond_with(ResponseTemplate::new(status).set_body_string("SECRET_PAGE_BODY"))
                .expect(1).mount(&server).await;
            let error = ddg_page_client(&server).fetch_page_text(&ddg_page_url(&server)).await.unwrap_err();
            assert_eq!(error, DdgError::Unavailable);
            assert!(!error.to_string().contains("SECRET_PAGE_BODY"));
            assert!(!error.to_string().contains("public.example"));
        }
    }

    #[tokio::test]
    async fn ddg_fetch_invalid_utf8_is_unavailable() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/book"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(vec![0xff, 0xfe], "text/html"))
            .expect(1).mount(&server).await;
        assert_eq!(ddg_page_client(&server).fetch_page_text(&ddg_page_url(&server)).await, Err(DdgError::Unavailable));
    }

    #[tokio::test]
    async fn ddg_fetch_times_out_after_ten_seconds() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/book"))
            .respond_with(ResponseTemplate::new(200).set_body_string("late").set_delay(Duration::from_secs(11)))
            .expect(1).mount(&server).await;
        assert_eq!(ddg_page_client(&server).fetch_page_text(&ddg_page_url(&server)).await, Err(DdgError::Unavailable));
    }
    #[test]
    fn ddg_parse_unwraps_redirect_and_strips_snippet_tags() {
        let html = r#"<a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fpublic.example%2Fbook%3Fa%3D1%26b%3D2&amp;rut=ignored" class="result__a">书名 &amp; <b>章节</b></a>
            <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fpublic.example%2Fbook%3Fa%3D1%26b%3D2">第一章 <b>雨夜</b> &amp; 剧情</a>"#;
        let results = parse_ddg_html(html).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://public.example/book?a=1&b=2");
        assert_eq!(results[0].title, "书名 & 章节");
        assert_eq!(results[0].content, "第一章 雨夜 & 剧情");
        assert_eq!(results[0].raw_content, None);
        assert_eq!(results[0].score, None);
    }

    #[test]
    fn ddg_parse_unwraps_absolute_redirect_and_guards_destination() {
        let html = r#"<a class="result__a" href="https://duckduckgo.com/l/?uddg=https%3A%2F%2Fpublic.example%2Fbook%3Fa%3D1%26b%3D2&amp;rut=ignored">书</a>
            <a class="result__a" href="https://duckduckgo.com/l/?uddg=http%3A%2F%2F127.0.0.1%2Fbook">私网</a>
            <a class="result__a" href="https://duckduckgo.com/l/?uddg=%2Frelative">相对</a>
            <a class="result__a" href="https://duckduckgo.com/l/?rut=missing">无目标</a>"#;
        let results = parse_ddg_html(html).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://public.example/book?a=1&b=2");
    }

    #[test]
    fn ddg_parse_associates_non_anchor_and_unresolvable_snippets_with_previous_title() {
        let html = r#"<div class="result"><a class="result__a" href="https://public.example/one">第一本</a>
            <div class="result__snippet">第一章 <b>雨夜</b> &amp; 剧情</div></div>
            <a class="result__a" href="https://public.example/two">第二本</a>
            <a class="result__snippet">第二章 <span>黎明</span></a>
            <a class="result__a" href="https://public.example/three">第三本</a>
            <a class="result__snippet" href="/unresolvable">第三章 <b>旅途</b></a>
            <a class="result__a" href="https://public.example/four">第四本</a>
            <span class="result__snippet">第四章 <i>归来</i></span>
            <a class="result__a" href="/invalid">无效标题</a>
            <div class="result__snippet">不得关联第四本</div>"#;
        let results = parse_ddg_html(html).unwrap();
        assert_eq!(results.len(), 4);
        assert_eq!(results[0].content, "第一章 雨夜 & 剧情");
        assert_eq!(results[1].content, "第二章 黎明");
        assert_eq!(results[2].content, "第三章 旅途");
        assert_eq!(results[3].content, "第四章 归来");
    }

    #[test]
    fn public_url_guard_rejects_credentials_and_non_http_schemes() {
        assert!(!is_public_http_url("https://user:secret@public.example/book"));
        assert!(!is_public_http_url("ftp://public.example/book"));
    }

    #[test]
    fn ddg_parse_unclosed_snippets_terminate_with_linear_work() {
        let html = format!("<a class='result__a' href='https://public.example/book'>书</a>{}",
            "<span class='result__snippet'>text".repeat(30_000));
        assert!(html.len() < MAX_HTTP_RESPONSE_BYTES);
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || { let _ = sender.send(parse_ddg_html(&html)); });
        let results = receiver.recv_timeout(Duration::from_secs(5))
            .expect("malformed snippets must not trigger repeated scans of the remaining HTML").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "text");
    }
    #[tokio::test]
    async fn ddg_fetch_rejects_unresolvable_hostname_before_request() {
        let server = MockServer::start().await;
        let client = ddg_page_client(&server);
        assert_eq!(client.fetch_page_text("https://this-host-does-not-exist.invalid/book").await, Err(DdgError::Unavailable));
        assert!(server.received_requests().await.unwrap().is_empty());
    }


    #[test]
    fn public_ipv6_literal_passes_address_guard() {
        assert!(is_public_ip("2001:db8::42".parse().unwrap()));
        assert!(!is_public_ip("fc00::42".parse().unwrap()));
        assert!(!is_public_ip("::1".parse().unwrap()));
    }

    #[test]
    fn ddg_parse_discards_relative_private_and_invalid_redirect_links() {
        let html = r#"<a class="result__a" href="/relative">相对</a>
            <a class="result__snippet" href="/relative">错误摘要</a>
            <a class="result__a" href="http://127.0.0.1/book">私网</a>
            <a class="result__a" href="//duckduckgo.com/l/?rut=missing">无目标</a>
            <a class="result__a" href="//duckduckgo.com/l/?uddg=%2Frelative">相对目标</a>
            <a class="result__a" href="javascript:alert(1)">脚本</a>
            <a class="result__a" href="https://public.example/book">有效</a>"#;
        let results = parse_ddg_html(html).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].url, "https://public.example/book");
        assert!(results[0].content.is_empty());
    }

    #[test]
    fn ddg_parse_detects_anomaly_and_accepts_empty_results() {
        assert_eq!(parse_ddg_html("<form action='https://duckduckgo.com/anomaly.js'>SECRET</form>"), Err(DdgError::RateLimited));
        assert_eq!(parse_ddg_html("<html>没有结果</html>").unwrap(), vec![]);
        assert_eq!(parse_ddg_html("anomaly without provider marker").unwrap(), vec![]);
    }

    #[test]
    fn ddg_parse_bounds_titles_snippets_and_result_count() {
        let html = (0..MAX_RESULTS + 2).map(|index| format!(
            "<a class='other result__a' href='https://public.example/{index}'>{}</a><a href='https://public.example/{index}' class='result__snippet'>{}</a>",
            "书".repeat(301), "章".repeat(MAX_PROMPT_CHARS_PER_CANDIDATE + 1)
        )).collect::<String>();
        let results = parse_ddg_html(&html).unwrap();
        assert_eq!(results.len(), MAX_RESULTS);
        for result in results {
            assert_eq!(result.title.chars().count(), 300);
            assert_eq!(result.content.chars().count(), MAX_PROMPT_CHARS_PER_CANDIDATE);
        }
    }

    #[tokio::test]
    async fn ddg_request_posts_form_domains_and_no_credentials() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/html/"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"<a class="result__a" href="https://public.example/book">书</a>"#))
            .expect(3).mount(&server).await;
        let client = DuckDuckGoClient::with_endpoint(format!("{}/html/", server.uri())).unwrap();
        for domains in [Some(&["qidian.com", "jjwxc.net"][..]), None, Some(&[][..])] {
            assert_eq!(client.search("书名 & 第一章", domains).await.unwrap().results.len(), 1);
        }
        let requests = server.received_requests().await.unwrap();
        for (index, request) in requests.iter().enumerate() {
            let parsed = Url::parse(&format!("https://public.example/?{}", String::from_utf8_lossy(&request.body))).unwrap();
            let fields = parsed.query_pairs().collect::<BTreeMap<_, _>>();
            assert_eq!(fields.len(), 2);
            assert_eq!(fields.get("kp").unwrap(), "-2");
            assert_eq!(fields.get("q").unwrap(), if index == 0 { "site:qidian.com OR site:jjwxc.net 书名 & 第一章" } else { "书名 & 第一章" });
            assert!(!request.headers.contains_key("authorization"));
            assert!(!request.headers.contains_key("cookie"));
            assert!(request.headers.get("user-agent").unwrap().to_str().unwrap().contains("Mozilla/5.0"));
        }
    }
    #[tokio::test]
    async fn ddg_enrichment_fetches_full_page_even_when_snippet_exists() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/book")).respond_with(ResponseTemplate::new(200).set_body_string("<h1>第一章 雨夜</h1><p>完整正文来自候选页。</p>")).expect(1).mount(&server).await;
        let client = ddg_page_client(&server);
        let candidates = client.enrich_engine(SearchResponse { results: vec![SearchResult { title:"钟楼书".into(), url:ddg_page_url(&server), content:"摘要不含章节完整正文".into(), raw_content:None, score:None }] }).await.unwrap();
        assert_eq!(candidates[0].text, "第一章 雨夜\n完整正文来自候选页。");
    }

    fn page_with_chapter_at_fifteen_thousand_characters() -> String {
        let mut text = good_raw().split("\n第二章").next().unwrap().to_owned();
        text.push_str(&"填".repeat(15_000 - text.chars().count()));
        text.push_str("\n第二章 钟楼，林昭在钟楼发现钥匙。");
        text
    }

    #[tokio::test]
    async fn ddg_enrichment_retains_chapter_at_fifteen_thousand_characters() {
        let server = MockServer::start().await;
        Mock::given(method("GET")).and(path("/book"))
            .respond_with(ResponseTemplate::new(200).set_body_string(format!("<pre>{}</pre>", page_with_chapter_at_fifteen_thousand_characters())))
            .expect(1).mount(&server).await;
        let candidates = ddg_page_client(&server).enrich_engine(SearchResponse { results: vec![SearchResult {
            title: "钟楼书".into(), url: ddg_page_url(&server), content: "摘要".into(), raw_content: None, score: None,
        }] }).await.unwrap();
        let lookup = lookup_fixture();
        let selected = select_candidate_set(&lookup, candidates).unwrap();
        assert!(find_heading_offset(&lookup.chapters[1].heading, &selected.candidates[0].text).is_some());
        assert!(provenance_for_chapter(&lookup, &selected.candidates, &lookup.chapters[1], ddg_page_url(&server), "第二章 钟楼，林昭在钟楼发现钥匙".into()).is_some());
    }

    #[tokio::test]
    async fn tavily_enrichment_retains_chapter_at_fifteen_thousand_characters() {
        let server = MockServer::start().await;
        let client = TavilyClient::with_endpoint("tvly-test".into(), format!("{}/search", server.uri())).unwrap();
        let url = "https://public.example/book";
        Mock::given(method("POST")).and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{
                "title":"钟楼书", "url":url, "content":"摘要", "raw_content":page_with_chapter_at_fifteen_thousand_characters()
            }]}))).expect(1).mount(&server).await;
        let candidates = client.search("钟楼书", None).await.unwrap();
        let candidates = enrich_candidates(&client, candidates).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].url.path(), "/search");
        let lookup = lookup_fixture();
        let selected = select_candidate_set(&lookup, candidates).unwrap();
        assert!(find_heading_offset(&lookup.chapters[1].heading, &selected.candidates[0].text).is_some());
        assert!(provenance_for_chapter(&lookup, &selected.candidates, &lookup.chapters[1], url.into(), "第二章 钟楼，林昭在钟楼发现钥匙".into()).is_some());
    }

    #[tokio::test]
    async fn ddg_rate_limit_does_not_retry_broad_search() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/html/")).respond_with(ResponseTemplate::new(202)).expect(1).mount(&server).await;
        let ddg = DuckDuckGoClient::with_endpoint(format!("{}/html/", server.uri())).unwrap();
        let llm = StubLlm { value: good_outline(), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        assert!(matches!(find_outline(&ddg, &llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await, Err(WebOutlineError::Rejected(WebOutlineRejection::RateLimited))));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }


    #[tokio::test]
    async fn ddg_request_status_errors_are_fixed_and_redacted() {
        for (status, expected) in [(202, DdgError::RateLimited), (403, DdgError::RateLimited), (500, DdgError::Unavailable)] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/html/"))
                .respond_with(ResponseTemplate::new(status).set_body_string("SECRET_SERVER_BODY"))
                .expect(1).mount(&server).await;
            let client = DuckDuckGoClient::with_endpoint(format!("{}/html/", server.uri())).unwrap();
            let error = client.search("书名", None).await.unwrap_err();
            assert_eq!(error, expected);
            let message = error.to_string();
            assert!(!message.contains("SECRET_SERVER_BODY"));
            assert!(!message.contains(&server.uri()));
            assert_eq!(message, match expected { DdgError::RateLimited => "DuckDuckGo 搜索暂时受限", DdgError::Unavailable => "DuckDuckGo 搜索不可用" });
        }
    }

    #[tokio::test]
    async fn ddg_request_rejects_oversized_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/html/"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("x".repeat(MAX_HTTP_RESPONSE_BYTES + 1), "text/html"))
            .expect(1).mount(&server).await;
        let client = DuckDuckGoClient::with_endpoint(format!("{}/html/", server.uri())).unwrap();
        assert_eq!(client.search("书名", None).await.unwrap_err(), DdgError::Unavailable);
    }

    struct NoDomainFilter;
    impl Match for NoDomainFilter {
        fn matches(&self, request: &Request) -> bool {
            !String::from_utf8_lossy(&request.body).contains("include_domains")
        }
    }

    struct StubLlm { value: Value, calls: AtomicUsize, prompt: Mutex<String>, cancelled: bool }
    #[async_trait::async_trait]
    impl LlmClient for StubLlm {
        async fn structured_with_usage(&self, request: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let prompt = request.messages.last().unwrap().content.clone();
            let value = if request.schema_name == "web_edition_paragraph_v1" {
                let input: Value = serde_json::from_str(&prompt).unwrap();
                assert!(input["local_paragraph"].is_string());
                assert!(request.schema["additionalProperties"] == false);
                json!({"verdict":"same", "source_url":input["sources"][0]["url"]})
            } else {
                *self.prompt.lock() = prompt;
                self.value.clone()
            };
            Ok(StructuredResponse { value, usage: None })
        }
        fn check_cancelled(&self) -> Result<(), LlmError> {
            if self.cancelled { Err(LlmError::Cancelled) } else { Ok(()) }
        }
        async fn stream_text(&self, _: TextRequest, _: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>) -> Result<String, LlmError> { Err(LlmError::Config("unused".into())) }
    }

    struct RefusingLlm { calls: AtomicUsize }
    #[async_trait::async_trait]
    impl LlmClient for RefusingLlm {
        async fn structured_with_usage(&self, _: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(LlmError::Config("SECRET_LLM_PROVIDER_BODY".into()))
        }
        fn check_cancelled(&self) -> Result<(), LlmError> { Ok(()) }
        async fn stream_text(&self, _: TextRequest, _: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>) -> Result<String, LlmError> { Err(LlmError::Config("unused".into())) }
    }

    struct RecordingLlm { verdict: Value, requests: Mutex<Vec<(String, String)>> }
    #[async_trait::async_trait]
    impl LlmClient for RecordingLlm {
        async fn structured_with_usage(&self, request: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError> {
            self.requests.lock().push((request.schema_name.into(), request.messages.last().unwrap().content.clone()));
            let value = if request.schema_name == "web_edition_paragraph_v1" { self.verdict.clone() } else { good_outline() };
            Ok(StructuredResponse { value, usage: None })
        }
        async fn stream_text(&self, _: TextRequest, _: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>) -> Result<String, LlmError> { Err(LlmError::Config("unused".into())) }
    }

    fn lookup_fixture() -> OutlineLookup {
        OutlineLookup { book_title: "钟楼书".into(), author: None, chapters: vec![
            LocalOutlineChapter { index: 1, heading: "第一章 雨夜".into(), text: "林昭推开旧宅木门看见没有署名的信城南钟楼将在子夜敲响这是足够长的第一章正文LOCAL_ONLY_SECRET_123夜雨落在青石台阶上而林昭握紧信封向钟楼走去她在钟声里想起失踪多年的姐姐于是决定独自查明真相后来她沿着河岸穿过寂静的长街反复确认信封上的蜡印并记录每一处可疑痕迹直到东方泛白她仍在思考钟楼管理员留下的谜语她决定第二天带着钥匙回到旧宅继续调查".into() },
            LocalOutlineChapter { index: 2, heading: "第二章 钟楼".into(), text: String::new() },
        ] }
    }

    fn good_raw() -> &'static str { "钟楼书\n第一章 雨夜 林昭推开旧宅木门看见没有署名的信城南钟楼将在子夜敲响这是足够长的第一章正文夜雨落在青石台阶上而林昭握紧信封向钟楼走去她在钟声里想起失踪多年的姐姐于是决定独自查明真相后来她沿着河岸穿过寂静的长街反复确认信封上的蜡印并记录每一处可疑痕迹直到东方泛白她仍在思考钟楼管理员留下的谜语她决定第二天带着钥匙回到旧宅继续调查。\n第二章 钟楼，林昭在钟楼发现钥匙。补录内容仅供来源页内部核验不应作为返回数据补录内容仅供来源页内部核验不应作为返回数据补录内容仅供来源页内部核验不应作为返回数据补录内容仅供来源页内部核验不应作为返回数据补录内容仅供来源页内部核验不应作为返回数据补录内容仅供来源页内部核验不应作为返回数据RAW_PAGE_ONLY_TRAILER" }
    fn good_outline() -> Value { json!({"plot_summary":"林昭循信件调查钟楼。", "plot_source_url":"https://public.example/chapter-1", "plot_evidence":"第二章 钟楼，林昭在钟楼发现钥匙", "plot_chapter_indices":[2], "characters":[{"name":"林昭","baseline":"收到信件后调查钟楼。","source_url":"https://public.example/chapter-1","evidence":"林昭推开旧宅木门看见没有署名的信","chapter_indices":[1]}], "chapters":[{"index":1,"heading":"第一章 雨夜","summary":"林昭发现信件。","source_url":"https://public.example/chapter-1","evidence":"第一章 雨夜 林昭推开旧宅木门"},{"index":2,"heading":"第二章 钟楼","summary":"林昭在钟楼发现钥匙。","source_url":"https://public.example/chapter-1","evidence":"第二章 钟楼，林昭在钟楼发现钥匙"}]}) }
    struct CancelBeforeBroad { checks: AtomicUsize }
    #[async_trait::async_trait]
    impl LlmClient for CancelBeforeBroad {
        async fn structured_with_usage(&self, _: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError> { unreachable!("no model request expected") }
        fn check_cancelled(&self) -> Result<(), LlmError> { if self.checks.fetch_add(1, Ordering::SeqCst) >= 2 { Err(LlmError::Cancelled) } else { Ok(()) } }
        async fn stream_text(&self, _: TextRequest, _: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>) -> Result<String, LlmError> { unreachable!("no stream request expected") }
    }

    #[tokio::test]
    async fn cancellation_after_targeted_enrichment_prevents_broad_search() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[]}))).mount(&server).await;
        let tavily = TavilyClient::with_endpoint("tvly-test".into(), format!("{}/search", server.uri())).unwrap();
        let llm = CancelBeforeBroad { checks: AtomicUsize::new(0) };
        assert!(matches!(find_outline(&tavily, &llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await, Err(WebOutlineError::Cancelled)));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }


    #[tokio::test]
    async fn ddg_ambiguous_candidates_are_terminal_without_broad_retry() {
        let server = MockServer::start().await;
        let body = format!("<a class='result__a' href='https://public.example/a'>钟楼书</a><a class='result__snippet'>{}</a><a class='result__a' href='https://other.example/b'>钟楼书</a><a class='result__snippet'>{}</a>", good_raw(), good_raw());
        Mock::given(method("POST")).and(path("/html/")).respond_with(ResponseTemplate::new(200).set_body_string(body)).expect(1).mount(&server).await;
        let ddg = DuckDuckGoClient::with_endpoint(format!("{}/html/", server.uri())).unwrap();
        let llm = StubLlm { value: good_outline(), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        assert!(matches!(find_outline(&ddg, &llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await, Err(WebOutlineError::Rejected(WebOutlineRejection::AmbiguousSources))));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }

    fn preface_lookup_and_source() -> (OutlineLookup, RawCandidate, Value) {
        let body = lookup_fixture().chapters.remove(0).text.replace("LOCAL_ONLY_SECRET_123", "");
        let mut chapters = vec![LocalOutlineChapter {
            index: 1, heading: "前言".into(), text: format!("PREFACE_LOCAL_ONLY_SECRET{}", "本地前言原文不可外传".repeat(700)),
        }];
        let preface = "前言 这份导读介绍本书的结构与阅读方式并非原版前言正文";
        let mut raw = format!("钟楼书\n{preface}\n第02章\n{body}\n");
        let url = "https://public.example/preface-edition";
        let mut mapped = vec![json!({"index":1,"heading":"前言","summary":"导读介绍阅读方式。","source_url":url,"evidence":preface})];
        for index in 2..=20 {
            let heading = format!("第{index:02}章");
            chapters.push(LocalOutlineChapter { index, heading: heading.clone(), text: if index == 2 { body.clone() } else { String::new() } });
            let evidence = if index == 2 { "林昭推开旧宅木门看见没有署名的信".to_owned() } else { format!("{heading} 这段章节导读说明调查继续展开并提供本章的事实依据") };
            if index > 2 { raw.push_str(&format!("{evidence}\n")); }
            mapped.push(json!({"index":index,"heading":heading,"summary":"调查继续展开。","source_url":url,"evidence":evidence}));
        }
        let output = json!({"plot_summary":"调查继续展开。","plot_source_url":url,"plot_evidence":"林昭推开旧宅木门看见没有署名的信","plot_chapter_indices":[2],"characters":[],"chapters":mapped});
        (OutlineLookup { book_title: "钟楼书".into(), author: None, chapters }, RawCandidate { title: "钟楼书".into(), url: url.into(), text: raw }, output)
    }

    #[test]
    fn paragraph_comparison_is_bounded_and_heading_scoped() {
        let local = format!("第一段。{}\r\n  \r\n第二段秘密不得发送", "本地原文".repeat(500));
        let paragraph = first_paragraph(&local, MAX_EDITION_PARAGRAPH_CHARS).unwrap();
        assert!(paragraph.chars().count() <= MAX_EDITION_PARAGRAPH_CHARS);
        assert!(!paragraph.contains("第二段秘密"));
        let wrapped = "第一章正文首行\n续行确实仍在第一段\n   \n第二段秘密不得发送";
        assert_eq!(first_paragraph(wrapped, 700).unwrap(), "第一章正文首行\n续行确实仍在第一段");
        assert!(first_paragraph("", 700).is_none());
        assert_eq!(first_paragraph("没有换行的首段足够长，内容完整。", 700).unwrap(), "没有换行的首段足够长，内容完整。");
        assert_eq!(first_paragraph("本章先在钟楼留下十分重要的线索。\r\n \r\n另一段秘密", 700).unwrap(), "本章先在钟楼留下十分重要的线索。");
        let lookup = lookup_fixture();
        let wrong = RawCandidate { title:"钟楼书".into(), url:"https://public.example/wrong".into(), text:"钟楼书\n第二章 钟楼，林昭在钟楼发现钥匙。\n第一章 雨夜\n".into() };
        assert!(source_first_paragraph(&lookup, &wrong).is_none());
    }

    #[test]
    fn compare_verdict_only_accepts_explicit_same_for_exact_public_source() {
        let lookup = lookup_fixture();
        let selected = select_candidate_set(&lookup, vec![RawCandidate { title:"钟楼书".into(), url:"https://public.example/chapter-1".into(), text:good_raw().into() }]).unwrap();
        let url = "https://public.example/chapter-1";
        assert!(validate_edition_verdict(&selected, json!({"verdict":"same", "source_url":url})).is_some());
        for verdict in ["different", "uncertain", "SAME", ""] {
            assert!(validate_edition_verdict(&selected, json!({"verdict":verdict, "source_url":url})).is_none());
        }
        for invalid in ["", "https://other.example/wrong", "http://localhost/private", "file:///secret"] {
            assert!(validate_edition_verdict(&selected, json!({"verdict":"same", "source_url":invalid})).is_none());
        }
        assert!(validate_edition_verdict(&selected, json!({"verdict":"same"})).is_none());
    }

    fn multi_page_fixture(domain: &str) -> (OutlineLookup, Vec<RawCandidate>) {
        let chapters: Vec<_> = (1..=6).map(|index| LocalOutlineChapter {
            index, heading: format!("第{index}章"), text: "林昭沿着河岸寻找失踪姐姐留下的线索并记录钟楼的钟声。".into(),
        }).collect();
        let candidates = [(1, 3), (4, 6)].into_iter().map(|(start, end)| RawCandidate {
            title: "钟楼书".into(), url: format!("https://{domain}/chapters-{start}-{end}"),
            text: (start..=end).map(|index| format!("第{index}章\n林昭沿着河岸寻找失踪姐姐留下的线索并记录钟楼的钟声。\n")).collect(),
        }).collect();
        (OutlineLookup { book_title: "钟楼书".into(), author: None, chapters }, candidates)
    }

    #[test]
    fn multi_page_same_domain_combines_chapter_coverage() {
        let (lookup, mut candidates) = multi_page_fixture("read.public.example");
        candidates[1].url = "https://archive.public.example/chapters-4-6".into();
        candidates.reverse();
        let selected = select_candidate_set(&lookup, candidates.clone()).unwrap();
        assert_eq!(selected.candidates.len(), 2);
        assert_eq!(selected.candidates[0].url, candidates[0].url);
        assert_eq!(selected.primary.url, candidates[1].url);
        assert_eq!(selected.proof_chapter_index, 1);
        assert_eq!(selected.proof_paragraph, source_first_paragraph(&lookup, &candidates[1]).unwrap());
        assert!(validate_edition_verdict(&selected, json!({"verdict":"same", "source_url":candidates[0].url})).is_none());
    }

    #[test]
    fn cross_domain_pages_do_not_combine() {
        let (lookup, mut candidates) = multi_page_fixture("public.example");
        candidates[1].url = "https://other.example/chapters-4-6".into();
        assert!(matches!(select_candidate_set(&lookup, candidates), Err(WebOutlineRejection::EvidenceInsufficient)));
    }

    #[test]
    fn public_suffix_sites_do_not_combine() {
        for suffix in [
            "co.uk", "org.uk", "ac.uk", "gov.uk", "com.cn", "net.cn", "org.cn", "gov.cn",
            "com.tw", "com.hk", "com.sg", "com.my", "co.jp", "co.kr", "com.au", "net.au",
            "github.io", "gitlab.io", "vercel.app", "netlify.app", "pages.dev",
            "com.br", "com.mx", "com.ar", "co.in", "co.za",
        ] {
            let (lookup, mut candidates) = multi_page_fixture(&format!("a.{suffix}"));
            candidates[1].url = format!("https://b.{suffix}/chapters-4-6");
            assert!(matches!(select_candidate_set(&lookup, candidates), Err(WebOutlineRejection::EvidenceInsufficient)),
                "distinct sites under {suffix} must not share chapter coverage");
        }
    }

    #[test]
    fn github_io_tenants_do_not_combine() {
        let (lookup, mut candidates) = multi_page_fixture("alice.github.io");
        candidates[1].url = "https://bob.github.io/chapters-4-6".into();
        assert!(matches!(select_candidate_set(&lookup, candidates), Err(WebOutlineRejection::EvidenceInsufficient)));
    }

    #[test]
    fn public_suffix_site_subdomains_combine() {
        for site in ["a.co.uk", "alice.github.io"] {
            let (lookup, mut candidates) = multi_page_fixture(&format!("x.{site}"));
            candidates[1].url = format!("https://y.{site}/chapters-4-6");
            assert_eq!(select_candidate_set(&lookup, candidates).unwrap().candidates.len(), 2);
        }
    }

    #[test]
    fn public_suffix_match_requires_a_label_boundary() {
        let (lookup, mut candidates) = multi_page_fixture("x.aco.uk");
        candidates[1].url = "https://y.aco.uk/chapters-4-6".into();
        assert_eq!(select_candidate_set(&lookup, candidates).unwrap().candidates.len(), 2);
    }

    #[test]
    fn two_complete_groups_remain_ambiguous() {
        let (lookup, mut candidates) = multi_page_fixture("public.example");
        candidates.extend(multi_page_fixture("other.example").1);
        assert!(matches!(select_candidate_set(&lookup, candidates), Err(WebOutlineRejection::AmbiguousSources)));
    }

    #[test]
    fn rooted_dns_hosts_do_not_merge_unrelated_domains() {
        let (lookup, mut candidates) = multi_page_fixture("read.public.example.");
        candidates[1].url = "https://archive.other.example./chapters-4-6".into();
        assert!(matches!(select_candidate_set(&lookup, candidates), Err(WebOutlineRejection::EvidenceInsufficient)));
    }

    #[test]
    fn rooted_dns_hosts_combine_with_their_unrooted_domain() {
        let (lookup, mut candidates) = multi_page_fixture("read.public.example.");
        candidates[1].url = "https://archive.public.example/chapters-4-6".into();
        assert_eq!(select_candidate_set(&lookup, candidates).unwrap().candidates.len(), 2);
    }

    #[test]
    fn ip_addresses_are_not_grouped_by_last_two_octets() {
        let (lookup, mut candidates) = multi_page_fixture("93.184.216.34");
        candidates[1].url = "https://94.184.216.34/chapters-4-6".into();
        assert!(matches!(select_candidate_set(&lookup, candidates), Err(WebOutlineRejection::EvidenceInsufficient)));
    }

    #[test]
    fn single_label_hosts_combine_only_with_the_same_host() {
        let (lookup, candidates) = multi_page_fixture("novels");
        assert_eq!(select_candidate_set(&lookup, candidates.clone()).unwrap().candidates.len(), 2);
        let mut separate = candidates;
        separate[1].url = "https://archive/chapters-4-6".into();
        assert!(matches!(select_candidate_set(&lookup, separate), Err(WebOutlineRejection::EvidenceInsufficient)));
    }

    #[test]
    fn mirror_candidates_with_identical_sections_are_not_ambiguous() {
        let lookup = lookup_fixture();
        let first = RawCandidate { title: "钟楼书".into(), url: "https://public.example/a".into(), text: good_raw().into() };
        let second = RawCandidate { url: "https://public.example/b".into(), ..first.clone() };
        for chapter in &lookup.chapters {
            let section = chapter_section(chapter, &lookup.chapters, &first.text).unwrap();
            assert!(section.lines().next().unwrap().contains("林昭"));
        }
        let selected = select_candidate_set(&lookup, vec![first.clone(), second]).unwrap();
        assert_eq!(selected.candidates.len(), 2);
        assert_eq!(selected.primary.url, first.url);
    }

    #[test]
    fn conflicting_inline_section_text_rejects_group() {
        let lookup = lookup_fixture();
        let first = RawCandidate { title: "钟楼书".into(), url: "https://public.example/a".into(), text: good_raw().into() };
        for (original, changed) in [("林昭推开旧宅木门", "林昭离开旧宅木门"), ("林昭在钟楼发现钥匙", "林昭在旧宅发现钥匙")] {
            let second = RawCandidate {
                url: "https://public.example/b".into(), text: first.text.replace(original, changed), ..first.clone()
            };
            assert!(matches!(select_candidate_set(&lookup, vec![first.clone(), second]), Err(WebOutlineRejection::AmbiguousSources)),
                "inline body conflict must be detected: {original}");
        }
    }

    #[test]
    fn inline_and_separate_heading_mirrors_compare_the_same_body() {
        let lookup = lookup_fixture();
        let first = RawCandidate { title: "钟楼书".into(), url: "https://public.example/a".into(), text: good_raw().into() };
        let second = RawCandidate {
            url: "https://public.example/b".into(),
            text: first.text.replace("雨夜 ", "雨夜\n").replace("钟楼，", "钟楼\n，"), ..first.clone()
        };
        assert_eq!(select_candidate_set(&lookup, vec![first, second]).unwrap().candidates.len(), 2);
    }

    #[test]
    fn conflicting_section_text_rejects_group() {
        let (lookup, mut candidates) = multi_page_fixture("public.example");
        let mirror = RawCandidate { url: "https://public.example/mirror".into(), ..candidates[0].clone() };
        let conflict = RawCandidate {
            url: "https://public.example/conflict".into(),
            text: candidates[1].text.replace("钟楼的钟声", "旧宅的秘密"),
            ..candidates[1].clone()
        };
        candidates.extend([mirror, conflict]);
        assert!(matches!(select_candidate_set(&lookup, candidates), Err(WebOutlineRejection::AmbiguousSources)));
    }

    #[test]
    fn mirror_sections_ignore_heading_lines_and_normalize_body_prefixes() {
        let (lookup, mut candidates) = multi_page_fixture("public.example");
        let mirror = RawCandidate {
            url: "https://public.example/mirror".into(),
            text: candidates[0].text.replace("第1章", "第一章").replace("钟声。", "钟声！"),
            ..candidates[0].clone()
        };
        candidates.push(mirror);
        assert_eq!(select_candidate_set(&lookup, candidates).unwrap().candidates.len(), 3);
    }

    #[test]
    fn mirror_section_comparison_is_limited_to_two_hundred_characters() {
        let (lookup, mut candidates) = multi_page_fixture("public.example");
        let prefix = "正文".repeat(100);
        candidates[0].text = format!("第1章\n{prefix}左侧后文\n{}", candidates[0].text);
        let mirror = RawCandidate {
            url: "https://public.example/mirror".into(),
            text: candidates[0].text.replace("左侧后文", "右侧后文"),
            ..candidates[0].clone()
        };
        candidates.push(mirror);
        assert_eq!(select_candidate_set(&lookup, candidates).unwrap().candidates.len(), 3);
    }

    #[test]
    fn evidence_must_exist_on_its_cited_page() {
        let (lookup, candidates) = multi_page_fixture("public.example");
        let evidence = "林昭沿着河岸寻找失踪姐姐留下的线索并记录钟楼的钟声。";
        assert!(provenance_for_chapter(&lookup, &candidates, &lookup.chapters[3], candidates[1].url.clone(), evidence.into()).is_some());
        assert!(provenance_for_chapter(&lookup, &candidates, &lookup.chapters[3], candidates[0].url.clone(), evidence.into()).is_none());
    }

    #[tokio::test]
    async fn multi_page_outline_prompt_preserves_entire_group_and_budget() {
        let (lookup, candidates) = multi_page_fixture("public.example");
        let selected = select_candidate_set(&lookup, candidates).unwrap();
        let mut llm = StubLlm { value: good_outline(), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        call_outline_llm(&llm, &lookup, &selected.candidates, WebOutlineBudget::new(100_000, 4_096)).await.unwrap();
        let prompt: Value = serde_json::from_str(llm.prompt.get_mut()).unwrap();
        assert_eq!(prompt["sources"].as_array().unwrap().len(), 2);
        for (source, candidate) in prompt["sources"].as_array().unwrap().iter().zip(&selected.candidates) {
            assert_eq!(source["url"], candidate.url);
            assert_eq!(source["text"], candidate.text);
        }
        assert!(matches!(call_outline_llm(&llm, &lookup, &selected.candidates, WebOutlineBudget::new(1, 1)).await,
            Err(WebOutlineError::Rejected(WebOutlineRejection::ContextInsufficient))));
        assert_eq!(llm.calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn ambiguous_complete_candidates_are_not_submitted_for_comparison() {
        let lookup = lookup_fixture();
        let first = RawCandidate { title:"钟楼书".into(), url:"https://public.example/a".into(), text:good_raw().into() };
        let second = RawCandidate { url:"https://other.example/b".into(), ..first.clone() };
        assert!(matches!(select_candidate_set(&lookup, vec![first, second]), Err(WebOutlineRejection::AmbiguousSources)));
    }

    #[tokio::test]
    async fn ambiguous_targeted_candidates_never_retry_into_a_single_web_source() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search")).and(wiremock::matchers::body_string_contains("include_domains"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[
                {"title":"钟楼书","url":"https://public.example/a","content":"","raw_content":good_raw()},
                {"title":"钟楼书","url":"https://other.example/b","content":"","raw_content":good_raw()}
            ]}))).expect(1).mount(&server).await;
        let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
        let llm = StubLlm { value: good_outline(), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        assert!(matches!(find_outline(&tavily, &llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await,
            Err(WebOutlineError::Rejected(WebOutlineRejection::AmbiguousSources))));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn conflicting_targeted_group_is_terminal_without_broad_retry() {
        let (lookup, candidates) = multi_page_fixture("public.example");
        let text = format!("{}{}", candidates[0].text, candidates[1].text);
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search"))
            .and(wiremock::matchers::body_string_contains("include_domains"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[
                {"title":"钟楼书","url":"https://public.example/a","content":"","raw_content":text},
                {"title":"钟楼书","url":"https://public.example/b","content":"","raw_content":text.replace("钟楼的钟声", "旧宅的秘密")}
            ]}))).expect(1).mount(&server).await;
        let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
        let llm = StubLlm { value: good_outline(), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        assert!(matches!(find_outline(&tavily, &llm, &lookup, WebOutlineBudget::new(100_000, 4_096)).await,
            Err(WebOutlineError::Rejected(WebOutlineRejection::AmbiguousSources))));
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn preface_is_not_edition_proof_but_all_twenty_chapters_are_required() {
        let (lookup, candidate, output) = preface_lookup_and_source();
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search"))
            .and(wiremock::matchers::body_string_contains("include_domains"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":candidate.title,"url":candidate.url,"content":"","raw_content":candidate.text}]})))
            .expect(1).mount(&server).await;
        let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
        let llm = StubLlm { value: output, calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        let result = find_outline(&tavily, &llm, &lookup, WebOutlineBudget::new(100_000, 4_096)).await.unwrap().unwrap();
        assert_eq!(result.edition_proof.chapter_index, 2);
        assert_eq!(result.chapters.iter().map(|chapter| chapter.index).collect::<Vec<_>>(), (1..=20).collect::<Vec<_>>());
        assert_eq!(result.chapters[0].heading, "前言");
        assert_eq!(llm.calls.load(Ordering::SeqCst), 2);
        let prompt: Value = serde_json::from_str(&llm.prompt.lock()).unwrap();
        assert_eq!(prompt["proof_chapter_index"], 2);
        let visible = prompt["sources"][0]["text"].as_str().unwrap();
        for chapter in &result.chapters { assert!(visible.contains(&chapter.provenance.evidence)); }
        assert!(visible.contains(&result.edition_proof.provenance.evidence));
        assert!(!llm.prompt.lock().contains("PREFACE_LOCAL_ONLY_SECRET"));
        let serialized = serde_json::to_string(&result).unwrap();
        assert!(!serialized.contains("PREFACE_LOCAL_ONLY_SECRET"));
        assert!(!serialized.contains("LOCAL_ONLY_SECRET_123"));
        assert_eq!(result.edition_proof.method, EditionProofMethod::AiParagraph);
        assert!(!serialized.contains("score_percent"));
        assert!(!serialized.contains("first_chapter_match"));
        assert!(result.provenance().contains(&result.edition_proof.provenance));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let request: Value = serde_json::from_slice(&requests[0].body).unwrap();
        let query = request["query"].as_str().unwrap();
        assert!(query.contains("第02章"));
        assert!(!query.contains("前言"));
        assert!(!String::from_utf8_lossy(&requests[0].body).contains("PREFACE_LOCAL_ONLY_SECRET"));
        assert!(!String::from_utf8_lossy(&requests[0].body).contains("林昭推开"));
    }

    #[test]
    fn incomplete_twenty_chapter_map_rejects_ai_edition_proof() {
        let (lookup, candidate, mut output) = preface_lookup_and_source();
        let selected = select_candidate_set(&lookup, vec![candidate]).unwrap();
        let proof = validate_edition_verdict(&selected, json!({"verdict":"same", "source_url":selected.primary.url})).unwrap();
        output["chapters"].as_array_mut().unwrap().remove(0);
        assert!(validate_llm_outline(&lookup, &selected.candidates, &selected.primary, proof, output).is_none());
    }

    #[test]
    fn chapter_heading_ten_thousand_characters_into_visible_window_is_locatable() {
        let lookup = lookup_fixture();
        let mut raw = good_raw().split("\n第二章").next().unwrap().to_owned();
        raw.push_str(&"填".repeat(10_000 - raw.chars().count()));
        raw.push_str("\n第二章 钟楼，林昭在钟楼发现钥匙。");
        let candidate = RawCandidate { title: "钟楼书".into(), url: "https://public.example/chapter-1".into(), text: raw };
        let selected = select_candidate_set(&lookup, vec![candidate]).unwrap();
        assert!(find_heading_offset(&lookup.chapters[1].heading, &selected.candidates[0].text).is_some());
        assert!(provenance_for_chapter(&lookup, &selected.candidates, &lookup.chapters[1], "https://public.example/chapter-1".into(), "第二章 钟楼，林昭在钟楼发现钥匙".into()).is_some());
    }

    #[test]
    fn two_headings_beyond_prompt_window_cannot_pass_preflight_or_claim_validation() {
        let mut lookup = lookup_fixture();
        lookup.chapters.push(LocalOutlineChapter { index: 3, heading: "第三章 河岸".into(), text: String::new() });
        let mut raw = good_raw().split("\n第二章").next().unwrap().to_owned();
        raw.push_str(&"填".repeat(MAX_PROMPT_CHARS_PER_CANDIDATE + 1 - raw.chars().count()));
        raw.push_str("\n第二章 钟楼，林昭在钟楼发现钥匙。\n第三章 河岸，林昭沿河岸继续调查失踪事件。");
        assert!(find_heading_offset(&lookup.chapters[1].heading, &raw).is_some());
        assert!(find_heading_offset(&lookup.chapters[2].heading, &raw).is_some());
        let candidate = RawCandidate { title: "钟楼书".into(), url: "https://public.example/chapter-1".into(), text: raw };
        assert!(matches!(select_candidate_set(&lookup, vec![candidate.clone()]), Err(WebOutlineRejection::EvidenceInsufficient)));
        let visible = RawCandidate { text: clip(&candidate.text, MAX_PROMPT_CHARS_PER_CANDIDATE), ..candidate };
        assert!(find_heading_offset(&lookup.chapters[1].heading, &visible.text).is_none());
        assert!(find_heading_offset(&lookup.chapters[2].heading, &visible.text).is_none());
        assert!(provenance_for_chapter(&lookup, &[visible], &lookup.chapters[1], "https://public.example/chapter-1".into(), "第二章 钟楼，林昭在钟楼发现钥匙".into()).is_none());
    }

    #[test]
    fn only_explicit_preface_labels_are_skipped_for_proof() {
        for heading in ["前言", "序言", "引言", "作者序", "译者序", "Preface", "Foreword"] {
            let (mut lookup, _, _) = preface_lookup_and_source();
            lookup.chapters[0].heading = heading.into();
            assert_eq!(proof_chapter(&lookup).unwrap().index, 2);
        }
        let (mut lookup, _, _) = preface_lookup_and_source();
        lookup.chapters[0].heading = "序章".into();
        assert_eq!(proof_chapter(&lookup).unwrap().index, 1);
    }

    #[test]
    fn inline_chapter_mentions_cannot_supply_headings_or_evidence() {
        let lookup = lookup_fixture();
        let inline_quote = "第二章 钟楼，林昭在钟楼发现钥匙";
        let mut candidate = RawCandidate { title: "钟楼书".into(), url: "https://public.example/chapter-1".into(),
            text: format!("钟楼书\n第一章 雨夜 {}\n她翻到{inline_quote}并写下读后感。", lookup.chapters[0].text) };
        assert!(find_heading_offset(&lookup.chapters[1].heading, &candidate.text).is_none());
        assert!(matches!(select_candidate_set(&lookup, vec![candidate.clone()]), Err(WebOutlineRejection::EvidenceInsufficient)));
        candidate.text.push_str("\n第二章 钟楼\n本章真正的来源事实与上一章内提到的内容不同。");
        assert!(provenance_for_chapter(&lookup, &[candidate], &lookup.chapters[1], "https://public.example/chapter-1".into(), inline_quote.into()).is_none());
    }

    #[tokio::test]
    async fn missing_selected_body_text_is_invalid_lookup_before_any_request() {
        let (mut lookup, _, _) = preface_lookup_and_source();
        lookup.chapters[1].text.clear();
        let server = MockServer::start().await;
        let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
        let llm = StubLlm { value: json!({}), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        assert!(matches!(find_outline(&tavily, &llm, &lookup, WebOutlineBudget::new(100_000, 4_096)).await,
            Err(WebOutlineError::Rejected(WebOutlineRejection::InvalidLookup))));
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
        assert!(server.received_requests().await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn snippet_only_results_and_http_errors_have_safe_rejections() {
        for status in [None, Some(429), Some(500)] {
            let server = MockServer::start().await;
            let template = if let Some(status) = status { ResponseTemplate::new(status).set_body_string("SECRET_SERVER_BODY") } else {
                ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"钟楼书","url":"https://public.example/secret-url","content":good_raw()}]}))
            };
            Mock::given(method("POST")).and(path("/search")).respond_with(template).expect(2).mount(&server).await;
            Mock::given(method("POST")).and(path("/extract"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[]})))
                .expect(if status.is_some() { 0 } else { 2 }).mount(&server).await;
            let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
            let llm = StubLlm { value: good_outline(), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
            let error = find_outline(&tavily, &llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await.unwrap_err();
            let expected = if status.is_some() { WebOutlineRejection::SearchUnavailable } else { WebOutlineRejection::EditionTextUnavailable };
            assert!(matches!(error, WebOutlineError::Rejected(reason) if reason == expected));
            assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
            for secret in ["SECRET_SERVER_BODY", "tvly-private", "secret-url", "林昭"] { assert!(!error.to_string().contains(secret)); }
        }
    }

    #[tokio::test]
    async fn compare_failure_and_invalid_outline_have_distinct_safe_rejections() {
        for service_failure in [false, true] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/search"))
                .and(wiremock::matchers::body_string_contains("include_domains"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"钟楼书","url":"https://public.example/chapter-1","content":"","raw_content":good_raw()}]})))
                .expect(1).mount(&server).await;
            let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
            let mut incomplete = good_outline();
            incomplete["chapters"].as_array_mut().unwrap().pop();
            let invalid = StubLlm { value: incomplete, calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
            let refusing = RefusingLlm { calls: AtomicUsize::new(0) };
            let llm: &dyn LlmClient = if service_failure { &refusing } else { &invalid };
            let error = find_outline(&tavily, llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await.unwrap_err();
            let expected = if service_failure { WebOutlineRejection::EditionUnverified } else { WebOutlineRejection::InvalidOutline };
            assert!(matches!(error, WebOutlineError::Rejected(reason) if reason == expected));
            assert!(!error.to_string().contains("SECRET_LLM_PROVIDER_BODY"));
            assert_eq!(invalid.calls.load(Ordering::SeqCst) + refusing.calls.load(Ordering::SeqCst), if service_failure { 1 } else { 2 });
            assert_eq!(server.received_requests().await.unwrap().len(), 1);
        }
    }

    #[test]
    fn web_preface_mapping_cannot_be_invented_from_body_proof() {
        let (lookup, mut candidate, _) = preface_lookup_and_source();
        candidate.text = candidate.text.lines().filter(|line| !line.starts_with("前言")).collect::<Vec<_>>().join("\n");
        assert!(source_first_paragraph(&lookup, &candidate).is_some());
        assert!(matches!(select_candidate_set(&lookup, vec![candidate]), Err(WebOutlineRejection::EvidenceInsufficient)));
    }

    #[test]
    fn cross_domain_supplementary_page_cannot_complete_an_incomplete_source() {
        let lookup = lookup_fixture();
        let primary = RawCandidate { title: "钟楼书".into(), url: "https://public.example/chapter-1".into(),
            text: good_raw().split("\n第二章").next().unwrap().into() };
        let supplemental = RawCandidate { title: "钟楼书".into(), url: "https://other.example/chapter-2".into(),
            text: format!("钟楼书\n第二章 钟楼，林昭在钟楼发现钥匙。{}", "没有证明版本相同的来源内容".repeat(10)) };
        assert!(source_first_paragraph(&lookup, &primary).is_some());
        assert!(source_first_paragraph(&lookup, &supplemental).is_none());
        assert!(matches!(select_candidate_set(&lookup, vec![primary, supplemental]), Err(WebOutlineRejection::EvidenceInsufficient)));
    }

    #[test]
    fn proven_pages_with_incompatible_coverage_are_conservatively_insufficient() {
        let lookup = lookup_fixture();
        let primary = RawCandidate { title: "钟楼书".into(), url: "https://public.example/chapter-1".into(), text: good_raw().into() };
        let mut supplemental = primary.clone();
        supplemental.url = "https://other.example/other-coverage".into();
        supplemental.text.extend((0..2_000).map(|offset| char::from_u32(0x5000 + offset).unwrap()));
        assert!(source_first_paragraph(&lookup, &primary).is_some());
        assert!(source_first_paragraph(&lookup, &supplemental).is_some());
        assert!(matches!(select_candidate_set(&lookup, vec![primary, supplemental]), Err(WebOutlineRejection::AmbiguousSources)));
    }

    #[test]
    fn complete_edition_proof_outside_visible_window_is_evidence_insufficient() {
        let body: String = (0..6_500).map(|offset| char::from_u32(0x5000 + offset).unwrap()).collect();
        let lookup = OutlineLookup { book_title: "钟楼书".into(), author: None, chapters: vec![
            LocalOutlineChapter { index: 1, heading: "第一章".into(), text: body.clone() },
        ] };
        let candidate = RawCandidate { title: "钟楼书".into(), url: "https://public.example/long-proof".into(), text: format!("{}\n第一章\n{body}", "填".repeat(MAX_PROMPT_CHARS_PER_CANDIDATE + 1)) };
        assert!(source_first_paragraph(&lookup, &candidate).is_some());
        assert!(source_first_paragraph(&lookup, &RawCandidate { text: clip(&candidate.text, MAX_PROMPT_CHARS_PER_CANDIDATE), ..candidate.clone() }).is_none());
        assert!(matches!(select_candidate_set(&lookup, vec![candidate]), Err(WebOutlineRejection::EvidenceInsufficient)));
    }

    #[tokio::test]
    async fn unusable_targeted_results_fall_back_to_broad_without_leaking_local_prose() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search")).and(wiremock::matchers::body_string_contains("include_domains"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"误匹配","url":"https://public.example/wrong","content":"这是一篇毫不相干的长页面。这里没有相同故事。"}]}))).expect(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/extract"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[],"failed_results":[{"url":"https://public.example/wrong","error":"unavailable"}]}))).expect(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/search")).and(NoDomainFilter)
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"钟楼书第一章","url":"https://public.example/chapter-1","content":"", "raw_content":good_raw()}]}))).expect(1).mount(&server).await;
        let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
        let llm = StubLlm { value: good_outline(), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        let result = find_outline(&tavily, &llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await.unwrap().unwrap();
        assert_eq!(result.chapters.len(), 2);
        assert_eq!(llm.calls.load(Ordering::SeqCst), 2);
        assert!(!serde_json::to_string(&result).unwrap().contains("RAW_PAGE_ONLY_TRAILER"));
        assert!(!llm.prompt.lock().contains("LOCAL_ONLY_SECRET_123"));
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 3);
        assert!(!requests.iter().any(|request| String::from_utf8_lossy(&request.body).contains("LOCAL_ONLY_SECRET_123")));
    }
    #[tokio::test]
    async fn partial_targeted_headings_trigger_broad_search_before_single_compaction() {
        let server = MockServer::start().await;
        let partial = good_raw().split("\n第二章").next().unwrap();
        Mock::given(method("POST")).and(path("/search")).and(wiremock::matchers::body_string_contains("include_domains"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"钟楼书 第一章","url":"https://public.example/partial","content":"", "raw_content":partial}]}))).expect(1).mount(&server).await;
        Mock::given(method("POST")).and(path("/search")).and(NoDomainFilter)
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"钟楼书第一章","url":"https://public.example/chapter-1","content":"", "raw_content":good_raw()}]}))).expect(1).mount(&server).await;
        let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
        let llm = StubLlm { value: good_outline(), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        let result = find_outline(&tavily, &llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await.unwrap();
        assert!(result.is_some());
        assert_eq!(llm.calls.load(Ordering::SeqCst), 2);
        assert_eq!(server.received_requests().await.unwrap().len(), 2);
    }

    #[tokio::test]
    async fn insufficient_provider_budget_returns_local_fallback_without_llm_call() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search")).and(wiremock::matchers::body_string_contains("include_domains"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"钟楼书第一章","url":"https://public.example/chapter-1","content":"", "raw_content":good_raw()}]}))).expect(1).mount(&server).await;
        let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
        let llm = StubLlm { value: good_outline(), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: false };
        assert!(matches!(find_outline(&tavily, &llm, &lookup_fixture(), WebOutlineBudget::new(1, 1)).await,
            Err(WebOutlineError::Rejected(WebOutlineRejection::ContextInsufficient))));
        assert_eq!(llm.calls.load(Ordering::SeqCst), 0);
    }


    #[tokio::test]
    async fn cancellation_is_terminal_before_any_tavily_request() {
        let server = MockServer::start().await;
        let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
        let llm = StubLlm { value: json!({}), calls: AtomicUsize::new(0), prompt: Mutex::new(String::new()), cancelled: true };
        assert!(matches!(find_outline(&tavily, &llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await, Err(WebOutlineError::Cancelled)));
        assert!(server.received_requests().await.unwrap().is_empty());
    }


    #[tokio::test]
    async fn search_uses_bearer_raw_content_and_never_serializes_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search")).and(header("authorization", "Bearer tvly-secret"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[]}))).expect(1).mount(&server).await;
        let client = TavilyClient::with_endpoint("tvly-secret".into(), format!("{}/search", server.uri())).unwrap();
        client.search("书名 第一章", Some(&["qidian.com"])).await.unwrap();
        let requests = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&requests[0].body);
        assert!(body.contains("include_raw_content")); assert!(body.contains("qidian.com"));
        assert!(!body.contains("tvly-secret"));
    }
    #[tokio::test]
    async fn http_failures_are_status_specific_and_redacted() {
        for status in [401, 429, 500] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/search"))
                .respond_with(ResponseTemplate::new(status).set_body_string("tvly-server-echo"))
                .expect(1).mount(&server).await;
            let client = TavilyClient::with_endpoint("tvly-secret".into(), format!("{}/search", server.uri())).unwrap();
            let error = client.search("书名 第一章", None).await.unwrap_err();
            assert_eq!(error, format!("Tavily 返回 HTTP {status}"));
            assert!(!error.contains("tvly-secret"));
            assert!(!error.contains("tvly-server-echo"));
        }
    }

    #[test]
    fn engine_stage_and_rejection_codes_are_redacted_and_stable() {
        assert_eq!(WebOutlineStage::DdgSearch.as_str(), "ddg_search");
        assert_eq!(WebOutlineStage::DdgSearchBroad.as_str(), "ddg_search_broad");
        assert_eq!(WebOutlineStage::DdgMatch.as_str(), "ddg_match");
        assert_eq!(WebOutlineStage::DdgOutline.as_str(), "ddg_outline");
        assert_eq!(WebOutlineRejection::RateLimited.as_str(), "rate_limited");
    }

    #[tokio::test]
    async fn malformed_success_body_is_rejected_without_echoing_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("{definitely-not-json tvly-secret}", "application/json"))
            .expect(1).mount(&server).await;
        let client = TavilyClient::with_endpoint("tvly-secret".into(), format!("{}/search", server.uri())).unwrap();
        assert_eq!(client.search("书名 第一章", None).await.unwrap_err(), "Tavily 响应格式无效");
    }

    #[tokio::test]
    async fn response_size_cap_rejects_oversized_body_before_json_parsing() {
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_raw("x".repeat(MAX_HTTP_RESPONSE_BYTES + 1), "application/json"))
            .expect(1).mount(&server).await;
        let client = TavilyClient::with_endpoint("tvly-secret".into(), format!("{}/search", server.uri())).unwrap();
        assert_eq!(client.search("书名 第一章", None).await.unwrap_err(), "Tavily 响应过大");
    }

    #[tokio::test]
    async fn request_timeout_uses_paused_clock_without_a_real_wait() {
        let server = MockServer::start().await;
        let accepted = Arc::new(tokio::sync::Notify::new());
        let accepted_by_server = Arc::clone(&accepted);
        Mock::given(method("POST")).and(path("/search"))
            .respond_with(move |_: &Request| {
                accepted_by_server.notify_one();
                ResponseTemplate::new(200).set_delay(Duration::from_secs(21)).set_body_json(json!({"results":[]}))
            })
            .expect(1).mount(&server).await;
        let client = Arc::new(TavilyClient::with_endpoint("tvly-secret".into(), format!("{}/search", server.uri())).unwrap());
        let request = tokio::spawn({ let client = Arc::clone(&client); async move { client.search("书名 第一章", None).await } });
        // Real socket readiness is not guaranteed by a fixed number of yields.
        // Pause only after the server confirms receipt, as provider tests do.
        tokio::time::timeout(Duration::from_secs(5), accepted.notified()).await.unwrap();
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(21)).await;
        assert_eq!(request.await.unwrap().unwrap_err(), "Tavily 请求失败");
    }


    #[test]
    fn heading_equivalence_does_not_claim_text_similarity_as_edition_proof() {
        assert!(headings_equivalent("第01章 雨夜", "第一章 雨夜"));
    }

    #[test]
    fn chapter_provenance_requires_heading_scoped_evidence() {
        let lookup = lookup_fixture();
        let candidate = RawCandidate { title: "钟楼书".into(), url: "https://public.example/chapter-1".into(), text: good_raw().into() };
        assert!(provenance_for_chapter(&lookup, &[candidate.clone()], &lookup.chapters[1], candidate.url.clone(), "第二章 钟楼，林昭在钟楼发现钥匙".into()).is_some());
        assert!(provenance_for_chapter(&lookup, &[candidate], &lookup.chapters[1], "https://public.example/chapter-1".into(), "第一章 雨夜 林昭推开旧宅木门".into()).is_none());
    }

    #[test]
    fn known_author_only_rejects_conflicting_bibliographic_identity() {
        let mut lookup = lookup_fixture(); lookup.author = Some("作者甲".into());
        let without_author = RawCandidate { title: "钟楼书".into(), url: "https://public.example/1".into(), text: good_raw().into() };
        let with_author = RawCandidate { title: "钟楼书 作者：作者甲".into(), ..without_author.clone() };
        let conflicting_author = RawCandidate { text: format!("作者：作者乙\n{}", good_raw()), ..without_author.clone() };
        assert!(bibliographic_identity_matches(&lookup, &without_author));
        assert!(bibliographic_identity_matches(&lookup, &with_author));
        assert!(!bibliographic_identity_matches(&lookup, &conflicting_author));
    }
    #[test]
    fn known_single_character_author_only_rejects_conflicting_identity() {
        let mut lookup = lookup_fixture(); lookup.author = Some("甲".into());
        let without_author = RawCandidate { title: "钟楼书".into(), url: "https://public.example/1".into(), text: good_raw().into() };
        let with_author = RawCandidate { title: "钟楼书 作者甲".into(), ..without_author.clone() };
        let conflicting_author = RawCandidate { title: "钟楼书 作者乙".into(), ..without_author.clone() };
        assert!(bibliographic_identity_matches(&lookup, &without_author));
        assert!(bibliographic_identity_matches(&lookup, &with_author));
        assert!(!bibliographic_identity_matches(&lookup, &conflicting_author));
    }


    #[test]
    fn invalid_source_references_or_incomplete_maps_are_rejected() {
        let lookup = OutlineLookup { book_title: "钟楼书".into(), author: None, chapters: vec![
            LocalOutlineChapter { index: 1, heading: "第一章 雨夜".into(), text: "林昭推开旧宅木门看见没有署名的信城南钟楼将在子夜敲响这是足够长的第一章正文用于严格匹配".into() },
            LocalOutlineChapter { index: 2, heading: "第二章 钟楼".into(), text: String::new() },
        ] };
        let raw = "书\n第一章 雨夜 林昭推开旧宅木门看见没有署名的信城南钟楼将在子夜敲响这是足够长的第一章正文用于严格匹配。\n第二章 钟楼，林昭到了钟楼。";
        let primary = RawCandidate { title: "钟楼书".into(), url: "https://public.example/1".into(), text: raw.into() };
        let selected = select_candidate_set(&lookup, vec![primary.clone()]).unwrap();
        let proof = validate_edition_verdict(&selected, json!({"verdict":"same", "source_url":primary.url})).unwrap();
        let incomplete = json!({"plot_summary":"钟楼事件", "plot_source_url":"https://public.example/1", "plot_evidence":"第二章 钟楼，林昭到了钟楼", "plot_chapter_indices":[2], "characters":[], "chapters":[{"index":1,"heading":"第一章 雨夜","summary":"发现信件","source_url":"https://public.example/1","evidence":"第一章 雨夜 林昭推开旧宅木门"}]});
        assert!(validate_llm_outline(&lookup, &[primary.clone()], &primary, proof.clone(), incomplete).is_none());
        let invalid_url = json!({"plot_summary":"钟楼事件", "plot_source_url":"https://evil.example/1", "plot_evidence":"第二章 钟楼，林昭到了钟楼", "plot_chapter_indices":[2], "characters":[], "chapters":[{"index":1,"heading":"第一章 雨夜","summary":"发现信件","source_url":"https://public.example/1","evidence":"第一章 雨夜 林昭推开旧宅木门"},{"index":2,"heading":"第二章 钟楼","summary":"到达钟楼","source_url":"https://public.example/1","evidence":"第二章 钟楼，林昭到了钟楼"}]});
        assert!(validate_llm_outline(&lookup, &[primary.clone()], &primary, proof, invalid_url).is_none());
    }
    #[tokio::test]
    async fn punctuation_drift_same_compares_bounded_paragraph_then_compacts_without_leaking_local_text() {
        let mut lookup = lookup_fixture();
        lookup.chapters[0].text = lookup.chapters[0].text.replace("看见没有署名的信", "看见、没有署名的信");
        lookup.chapters[0].text.push_str(&"本地仅供比较的长段".repeat(200));
        lookup.chapters[0].text.push_str("\n\nNEVER_SEND_SECOND_LOCAL_PARAGRAPH");
        let server = MockServer::start().await;
        Mock::given(method("POST")).and(path("/search"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"钟楼书","url":"https://public.example/chapter-1","content":"", "raw_content":good_raw()}]})))
            .expect(1).mount(&server).await;
        let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
        let llm = RecordingLlm { verdict:json!({"verdict":"same","source_url":"https://public.example/chapter-1"}), requests:Mutex::new(Vec::new()) };
        let result = find_outline(&tavily, &llm, &lookup, WebOutlineBudget::new(100_000, 4_096)).await.unwrap().unwrap();
        assert_eq!(result.edition_proof.method, EditionProofMethod::AiParagraph);
        assert_eq!(result.edition_proof.provenance.url, result.chapters[0].provenance.url);
        let requests = llm.requests.lock();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0].0, "web_edition_paragraph_v1");
        assert_eq!(requests[1].0, "verified_web_outline_v1");
        let compare: Value = serde_json::from_str(&requests[0].1).unwrap();
        assert!(compare["local_paragraph"].as_str().unwrap().contains("LOCAL_ONLY_SECRET_123"));
        assert!(compare["local_paragraph"].as_str().unwrap().chars().count() <= MAX_EDITION_PARAGRAPH_CHARS);
        assert!(compare["sources"][0]["paragraph"].as_str().unwrap().starts_with("林昭推开旧宅木门"));
        assert!(compare["local_paragraph"].as_str().unwrap().contains("看见、没有署名的信"));
        assert!(!compare["sources"][0]["paragraph"].as_str().unwrap().contains("看见、没有署名的信"));
        assert!(!requests[0].1.contains("NEVER_SEND_SECOND_LOCAL_PARAGRAPH"));
        assert!(!requests[1].1.contains("LOCAL_ONLY_SECRET_123"));
        assert!(!serde_json::to_string(&result).unwrap().contains("LOCAL_ONLY_SECRET_123"));
        for request in server.received_requests().await.unwrap() {
            let body = String::from_utf8_lossy(&request.body);
            assert!(!body.contains("LOCAL_ONLY_SECRET_123"));
            assert!(!body.contains("NEVER_SEND_SECOND_LOCAL_PARAGRAPH"));
        }
    }

    #[tokio::test]
    async fn non_same_or_wrong_url_verdicts_never_trigger_outline_compaction() {
        for verdict in [json!({"verdict":"different","source_url":"https://public.example/chapter-1"}),
            json!({"verdict":"uncertain","source_url":"https://public.example/chapter-1"}),
            json!({"verdict":"same","source_url":"https://public.example/unrelated"}),
            json!({"verdict":"same","source_url":""}), json!({"verdict":"same"})] {
            let server = MockServer::start().await;
            Mock::given(method("POST")).and(path("/search"))
                .respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"钟楼书","url":"https://public.example/chapter-1","content":"", "raw_content":good_raw()}]})))
                .expect(1).mount(&server).await;
            let tavily = TavilyClient::with_endpoint("tvly-private".into(), format!("{}/search", server.uri())).unwrap();
            let llm = RecordingLlm { verdict, requests:Mutex::new(Vec::new()) };
            let result = find_outline(&tavily, &llm, &lookup_fixture(), WebOutlineBudget::new(100_000, 4_096)).await;
            assert!(matches!(result, Err(WebOutlineError::Rejected(WebOutlineRejection::EditionUnverified))));
            assert_eq!(llm.requests.lock().len(), 1);
            assert_eq!(llm.requests.lock()[0].0, "web_edition_paragraph_v1");
        }
    }
}
