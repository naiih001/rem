use std::time::Duration;

use rig::tool::{Tool, ToolContext};
use serde::Deserialize;
use serde_json::json;

use super::ToolError;

const TIMEOUT_SECS: u64 = 30;
const DEFAULT_MAX_BYTES: usize = 8000;
const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 rem-agent/0.1";

fn http_client() -> Result<reqwest::Client, ToolError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(TIMEOUT_SECS))
        .user_agent(UA)
        .build()
        .map_err(|e| ToolError(format!("web: http client: {e}")))
}

fn check_url(url: &str) -> Result<(), ToolError> {
    if url.starts_with("http://") || url.starts_with("https://") {
        Ok(())
    } else {
        Err(ToolError(format!(
            "web_fetch: only http:// and https:// URLs are allowed, got {url:?}"
        )))
    }
}

// ---------------------------------------------------------------------------
// web_fetch
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct WebFetchArgs {
    pub url: String,
    pub max_bytes: Option<usize>,
}

#[derive(Debug)]
pub struct WebFetchTool;

impl Tool for WebFetchTool {
    const NAME: &'static str = "web_fetch";
    type Args = WebFetchArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "Fetch a URL over HTTP(S) and return its content as readable text. HTML pages are converted to plain text (scripts/styles stripped). 30s timeout. Optional `max_bytes` caps output (default 8000). Only http:// and https:// are allowed.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "url": { "type": "string", "description": "Full http(s) URL to fetch" },
                "max_bytes": { "type": "number", "description": "Max bytes of text to return. Omit for 8000." }
            },
            "required": ["url"]
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        check_url(&args.url)?;
        let client = http_client()?;
        let resp = client
            .get(&args.url)
            .send()
            .await
            .map_err(|e| ToolError(format!("web_fetch {}: request failed: {e}", args.url)))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(ToolError(format!(
                "web_fetch {}: HTTP {}",
                args.url,
                status.as_u16()
            )));
        }
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| ToolError(format!("web_fetch {}: read body: {e}", args.url)))?;
        let raw = String::from_utf8_lossy(&bytes).into_owned();
        let text = if content_type.contains("html") || looks_like_html(&raw) {
            html_to_text(&raw)
        } else {
            raw
        };
        let cap = args.max_bytes.unwrap_or(DEFAULT_MAX_BYTES).max(1);
        let mut note = String::new();
        let mut out = text;
        if out.len() > cap {
            out.truncate(cap);
            note = format!(" [truncated to {cap} bytes]");
        }
        Ok(format!(
            "HTTP {}{note}\n{out}",
            status.as_u16()
        ))
    }
}

fn looks_like_html(s: &str) -> bool {
    let head: String = s.chars().take(512).collect();
    let low = head.to_lowercase();
    low.contains("<html") || low.contains("<body") || low.contains("<!doctype html")
}

/// Case-insensitive removal of `<tag ...>...</tag>` blocks (script/style).
fn strip_tag_blocks(html: &str, tag: &str) -> String {
    let lower = html.to_lowercase();
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut rest_lower = lower.as_str();
    loop {
        let Some(start) = rest_lower.find(open_pat.as_str()) else {
            out.push_str(rest);
            break;
        };
        // Guard: `<tags...` prefix (e.g. `<stylex`) must not match `<style`.
        let after = rest_lower[start + open_pat.len()..].chars().next();
        let valid = matches!(after, None | Some('>' | '/' | ' ' | '\t' | '\n' | '\r'));
        if !valid {
            out.push_str(&rest[..start + 1]);
            rest = &rest[start + 1..];
            rest_lower = &rest_lower[start + 1..];
            continue;
        }
        out.push_str(&rest[..start]);
        let after_open = &rest_lower[start..];
        let Some(gt) = after_open.find('>') else { break; };
        let content_start = start + gt + 1;
        match rest_lower[content_start..].find(close_pat.as_str()) {
            Some(end) => {
                let skip = content_start + end + close_pat.len();
                rest = &rest[skip..];
                rest_lower = &rest_lower[skip..];
            }
            None => break,
        }
    }
    out
}

/// Case-insensitive substring replace (ASCII patterns only).
fn replace_ci(haystack: &str, needle: &str, replacement: &str) -> String {
    let lower = haystack.to_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut rest = haystack;
    let mut rest_lower = lower.as_str();
    loop {
        let Some(i) = rest_lower.find(needle) else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        out.push_str(replacement);
        rest = &rest[i + needle.len()..];
        rest_lower = &rest_lower[i + needle.len()..];
    }
    out
}

fn html_to_text(html: &str) -> String {
    let no_script = strip_tag_blocks(html, "script");
    let no_style = strip_tag_blocks(&no_script, "style");
    // Block-level tags become line breaks before stripping.
    let mut s = no_style;
    for tag in ["<br", "</p>", "</div>", "</h1>", "</h2>", "</h3>", "</h4>", "</h5>", "</h6>", "</li>", "</tr>", "</table>", "</blockquote>", "<p", "<div", "<li", "<h1", "<h2", "<h3", "<tr"] {
        s = replace_ci(&s, tag, "\n");
    }
    // Strip remaining tags.
    let mut plain = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => plain.push(c),
            _ => {}
        }
    }
    let decoded = html_escape::decode_html_entities(&plain).into_owned();
    decoded
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

// ---------------------------------------------------------------------------
// web_search (DuckDuckGo lite, no API key)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct WebSearchArgs {
    pub query: String,
    pub count: Option<u8>,
}

#[derive(Debug)]
pub struct WebSearchTool;

const MAX_RESULTS: u8 = 10;

impl Tool for WebSearchTool {
    const NAME: &'static str = "web_search";
    type Args = WebSearchArgs;
    type Output = String;
    type Error = ToolError;

    fn description(&self) -> String {
        "Search the web via DuckDuckGo (no API key needed). Returns up to `count` results (default 5, max 10) as numbered `title — url` lines with snippets. Use `web_fetch` to read a result page.".to_string()
    }

    fn parameters(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "query": { "type": "string", "description": "Search query" },
                "count": { "type": "number", "description": "Max results to return (default 5, max 10)" }
            },
            "required": ["query"]
        })
    }

    async fn call(&self, _ctx: &mut ToolContext, args: Self::Args) -> Result<String, ToolError> {
        if args.query.trim().is_empty() {
            return Err(ToolError("web_search: query must not be empty".to_string()));
        }
        let want = args.count.unwrap_or(5).clamp(1, MAX_RESULTS) as usize;
        let client = http_client()?;
        let resp = client
            .get("https://lite.duckduckgo.com/lite/")
            .query(&[("q", args.query.as_str())])
            .send()
            .await
            .map_err(|e| ToolError(format!("web_search: request failed: {e}")))?;
        if !resp.status().is_success() {
            return Err(ToolError(format!(
                "web_search: HTTP {}",
                resp.status().as_u16()
            )));
        }
        let html = resp
            .text()
            .await
            .map_err(|e| ToolError(format!("web_search: read body: {e}")))?;
        let results = parse_ddg_lite(&html, want);
        if results.is_empty() {
            Ok(format!("no results for {:?}", args.query))
        } else {
            Ok(results
                .iter()
                .enumerate()
                .map(|(i, (title, url, snippet))| {
                    if snippet.is_empty() {
                        format!("{}. {title}\n   {url}", i + 1)
                    } else {
                        format!("{}. {title}\n   {url}\n   {snippet}", i + 1)
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
    }
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(bytes[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Parse `lite.duckduckgo.com` result blocks: `<a ... href="..." class='result-link'>TITLE</a>`
/// followed by a `<td class='result-snippet'>SNIPPET</td>`.
fn parse_ddg_lite(html: &str, want: usize) -> Vec<(String, String, String)> {
    let mut results = Vec::new();
    let mut rest = html;
    while results.len() < want {
        let Some(link_pos) = rest.find("class='result-link'") else {
            break;
        };
        let anchor_start = rest[..link_pos].rfind("<a ").unwrap_or(0);
        let anchor = &rest[anchor_start..];
        // href="..."
        let href = anchor
            .find("href=\"")
            .and_then(|i| {
                let after = &anchor[i + 6..];
                after.find('"').map(|j| &after[..j])
            })
            .unwrap_or("");
        // >TITLE</a>
        let title = anchor
            .find('>')
            .and_then(|i| {
                let after = &anchor[i + 1..];
                after.find("</a>").map(|j| after[..j].trim().to_string())
            })
            .unwrap_or_default();
        // real URL lives in the uddg= redirect param
        let url = if let Some(u) = href.split("uddg=").nth(1) {
            let enc = u.split('&').next().unwrap_or("");
            percent_decode(enc)
        } else if href.starts_with("//") {
            format!("https:{href}")
        } else {
            href.to_string()
        };
        // snippet after the anchor
        let after_anchor = &anchor[anchor.find("</a>").map(|i| i + 4).unwrap_or(0)..];
        let snippet = after_anchor
            .find("class='result-snippet'>")
            .and_then(|i| {
                let after = &after_anchor[i + 22..];
                after.find("</td>").map(|j| {
                    let raw = &after[..j];
                    // strip <b> etc, decode entities, collapse
                    let mut plain = String::with_capacity(raw.len());
                    let mut in_tag = false;
                    for c in raw.chars() {
                        match c {
                            '<' => in_tag = true,
                            '>' => in_tag = false,
                            _ if !in_tag => plain.push(c),
                            _ => {}
                        }
                    }
                    let decoded = html_escape::decode_html_entities(&plain).into_owned();
                    let collapsed: String = decoded.split_whitespace().collect::<Vec<_>>().join(" ");
                    collapsed.chars().take(300).collect()
                })
            })
            .unwrap_or_default();
        let clean_title: String = html_escape::decode_html_entities(title.trim())
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if !clean_title.is_empty() && !url.is_empty() {
            results.push((clean_title, url, snippet));
        }
        rest = &rest[link_pos + 20..];
    }
    results
}
