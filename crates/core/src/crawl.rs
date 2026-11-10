//! Website link crawler. Breadth-first over HTML pages, bounded in depth,
//! pages, links and bytes, honouring robots.txt. It only collects candidate
//! file links for review; it never starts downloads by itself.
use crate::{
    desktop::{CrawlInput, CrawlLink, CrawlResult},
    error::{EngineError, EngineResult},
    transport::Transport,
    types::RemoteIdentity,
};
use futures::StreamExt;
use std::collections::{HashSet, VecDeque};
use url::Url;

const MAX_PAGE: usize = 2 * 1024 * 1024;
const MAX_LINKS: usize = 2000;
const MAX_QUEUE: usize = 5000;
const MAX_RULES: usize = 512;
const PAGE_TYPES: &[&str] = &["", "htm", "html", "php", "asp", "aspx", "jsp", "shtml"];
/// Pause between pages; a longer robots.txt `Crawl-delay` is honoured up to the cap.
const PAUSE_SECS: f64 = 0.25;
const MAX_PAUSE_SECS: f64 = 5.0;

fn invalid(message: &str) -> EngineError {
    EngineError::Config(format!("crawl: {message}"))
}

fn extension(url: &Url) -> String {
    url.path_segments()
        .and_then(|mut s| s.next_back())
        .and_then(|name| {
            name.rsplit_once('.')
                .map(|(_, ext)| ext.to_ascii_lowercase())
        })
        .unwrap_or_default()
}

fn decode(value: &str) -> String {
    value
        .replace("&amp;", "&")
        .replace("&#38;", "&")
        .replace("&#x26;", "&")
}

/// Collect href/src/srcset attribute values, including their `data-` lazy-load
/// forms. A scanner, not a parser: enough for links in real pages, and it
/// never executes or interprets markup.
pub fn attribute_links(html: &str) -> Vec<String> {
    let lower = html.to_ascii_lowercase();
    let mut found = Vec::new();
    for attribute in ["href", "src", "srcset"] {
        let mut from = 0;
        while let Some(at) = lower[from..].find(attribute) {
            let start = from + at + attribute.len();
            from = start;
            // Must be a whole attribute name followed by optional spaces and '='.
            let before = &lower[..start - attribute.len()];
            if !before.ends_with("data-")
                && before
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                continue;
            }
            let rest = html[start..].trim_start();
            let Some(rest) = rest.strip_prefix('=') else {
                continue;
            };
            let rest = rest.trim_start();
            let value = match rest.chars().next() {
                Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or_default(),
                Some(_) => rest
                    .split(|c: char| c.is_whitespace() || c == '>')
                    .next()
                    .unwrap_or_default(),
                None => "",
            };
            if value.is_empty() || value.len() > 8192 {
                continue;
            }
            if attribute == "srcset" {
                // "url 1x, url 2x": each candidate is a URL and a descriptor.
                found.extend(
                    value
                        .split(',')
                        .filter_map(|candidate| candidate.split_whitespace().next())
                        .map(decode),
                );
            } else {
                found.push(decode(value));
            }
        }
    }
    found
}

/// The document's `<base href>`, which relative links resolve against.
fn base_href(html: &str) -> Option<String> {
    let lower = html.to_ascii_lowercase();
    let at = lower.find("<base")?;
    let tag = &html[at..at + lower[at..].find('>')?];
    attribute_links(tag).into_iter().next()
}

/// Rules for user-agent `*` (the crawler identifies generically).
#[derive(Default)]
struct Robots {
    // (allow, path pattern), at most `MAX_RULES`.
    rules: Vec<(bool, String)>,
    delay_secs: Option<f64>,
}

fn robots_rules(text: &str) -> Robots {
    let mut robots = Robots::default();
    let (mut applies, mut in_agents) = (false, false);
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let (key, value) = (key.trim().to_ascii_lowercase(), value.trim());
        if key == "user-agent" {
            // Consecutive user-agent lines name one group.
            applies = (in_agents && applies) || value == "*";
            in_agents = true;
            continue;
        }
        in_agents = false;
        if !applies {
            continue;
        }
        match key.as_str() {
            "allow" | "disallow" if !value.is_empty() && robots.rules.len() < MAX_RULES => {
                robots.rules.push((key == "allow", value.to_owned()))
            }
            "crawl-delay" => robots.delay_secs = value.parse().ok().filter(|d: &f64| d.is_finite()),
            _ => {}
        }
    }
    robots
}

/// robots.txt path matching: `*` matches any run of characters and a final
/// `$` anchors the end; otherwise the pattern is a prefix.
fn pattern_matches(pattern: &str, path: &str) -> bool {
    let (pattern, anchored) = match pattern.strip_suffix('$') {
        Some(pattern) => (pattern, true),
        None => (pattern, false),
    };
    let mut parts = pattern.split('*');
    let Some(mut rest) = path.strip_prefix(parts.next().unwrap_or_default()) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    for (index, part) in parts.iter().enumerate() {
        if anchored && index + 1 == parts.len() {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    !anchored || rest.is_empty()
}

impl Robots {
    /// The longest matching rule decides; `Allow` wins a tie.
    fn allows(&self, path: &str) -> bool {
        self.rules
            .iter()
            .filter(|(_, pattern)| pattern_matches(pattern, path))
            .max_by_key(|(allow, pattern)| (pattern.len(), *allow))
            .is_none_or(|(allow, _)| *allow)
    }
}

async fn get_text(transport: &dyn Transport, url: &Url) -> Option<String> {
    let response = transport
        .fetch(url, &[], None, &RemoteIdentity::empty())
        .await
        .ok()?;
    let mut body = response.body;
    let mut bytes = Vec::new();
    while let Some(chunk) = body.next().await {
        let chunk = chunk.ok()?;
        if bytes.len() + chunk.len() > MAX_PAGE {
            // Oversized pages are scanned up to the limit only.
            bytes.extend_from_slice(&chunk[..MAX_PAGE - bytes.len()]);
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

pub async fn crawl(transport: &dyn Transport, input: CrawlInput) -> EngineResult<CrawlResult> {
    let start =
        Url::parse(input.url.trim()).map_err(|_| invalid("enter a full http(s) address"))?;
    if !matches!(start.scheme(), "http" | "https")
        || !start.username().is_empty()
        || start.password().is_some()
    {
        return Err(invalid(
            "enter an http(s) address without a username or password",
        ));
    }
    if input.depth > 3 || !(1..=200).contains(&input.max_pages) || input.extensions.len() > 64 {
        return Err(invalid("use depth 0–3 and 1–200 pages"));
    }
    let wanted: HashSet<String> = input
        .extensions
        .iter()
        .map(|e| e.trim().trim_start_matches('.').to_ascii_lowercase())
        .filter(|e| !e.is_empty())
        .collect();
    let host = start.host_str().unwrap_or_default().to_owned();
    let robots = match Url::parse(&start.origin().ascii_serialization())
        .ok()
        .and_then(|origin| origin.join("/robots.txt").ok())
    {
        Some(url) => robots_rules(&get_text(transport, &url).await.unwrap_or_default()),
        None => Robots::default(),
    };
    let blocked = |url: &Url| {
        let path = match url.query() {
            Some(query) => format!("{}?{query}", url.path()),
            None => url.path().to_owned(),
        };
        url.host_str() == Some(host.as_str()) && !robots.allows(&path)
    };
    let pause = std::time::Duration::from_secs_f64(
        robots
            .delay_secs
            .unwrap_or(PAUSE_SECS)
            .clamp(PAUSE_SECS, MAX_PAUSE_SECS),
    );
    let mut queue = VecDeque::from([(start.clone(), 0u32)]);
    // Pages already queued, so one page is fetched once and the queue stays bounded.
    let mut visited = HashSet::from([start.clone()]);
    let mut seen = HashSet::new();
    let mut links = Vec::new();
    let (mut pages, mut skipped, mut truncated) = (0u32, 0u32, false);
    while let Some((page, depth)) = queue.pop_front() {
        if blocked(&page) {
            skipped += 1;
            continue;
        }
        if pages >= input.max_pages {
            truncated = true;
            break;
        }
        if pages > 0 {
            // Be a polite client: one page at a time with a pause between.
            tokio::time::sleep(pause).await;
        }
        pages += 1;
        let Some(html) = get_text(transport, &page).await else {
            continue;
        };
        let base = base_href(&html)
            .and_then(|href| page.join(&href).ok())
            .filter(|base| matches!(base.scheme(), "http" | "https"))
            .unwrap_or_else(|| page.clone());
        for raw in attribute_links(&html) {
            let Ok(mut url) = base.join(&raw) else {
                continue;
            };
            url.set_fragment(None);
            if !matches!(url.scheme(), "http" | "https") || !url.username().is_empty() {
                continue;
            }
            let same_host = url.host_str() == Some(host.as_str());
            let ext = extension(&url);
            if PAGE_TYPES.contains(&ext.as_str()) {
                if same_host
                    && depth < input.depth
                    && queue.len() < MAX_QUEUE
                    && visited.insert(url.clone())
                {
                    queue.push_back((url, depth + 1));
                }
                continue;
            }
            if (input.same_host && !same_host)
                || (!wanted.is_empty() && !wanted.contains(&ext))
                || !seen.insert(url.clone())
            {
                continue;
            }
            if links.len() >= MAX_LINKS {
                truncated = true;
                break;
            }
            links.push(CrawlLink {
                url: url.into(),
                page: page.to_string(),
                extension: ext,
            });
        }
    }
    Ok(CrawlResult {
        links,
        pages,
        skipped_by_robots: skipped,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn robots_longest_match_wildcards_and_agent_groups() {
        let robots = robots_rules(
            "User-agent: other\nDisallow: /\n\nUser-agent: *\nUser-agent: also\nDisallow: /private/\nAllow: /private/open/\nDisallow: /*.cgi$\nDisallow: /search?q=\nCrawl-delay: 2\n",
        );
        assert_eq!(robots.delay_secs, Some(2.0));
        for (path, allowed) in [
            ("/", true),
            ("/private/a.html", false),
            ("/private/open/a.html", true),
            ("/bin/run.cgi", false),
            ("/bin/run.cgi?x=1", true),
            ("/search?q=files", false),
        ] {
            assert_eq!(robots.allows(path), allowed, "{path}");
        }
        assert!(robots_rules("User-agent: other\nDisallow: /\n").allows("/any"));
    }
    #[test]
    fn links_include_srcset_lazy_attributes_and_base() {
        let html = r#"<base href="/files/"><a href="a.zip?x=1&amp;y=2">a</a>
            <img data-src=b.png srcset="c.png 1x, d.png 2x"><i hreflang="en"></i>"#;
        let links = attribute_links(html);
        for link in ["a.zip?x=1&y=2", "b.png", "c.png", "d.png"] {
            assert!(links.iter().any(|found| found == link), "{link}");
        }
        assert!(!links.iter().any(|found| found == "en"));
        assert_eq!(base_href(html).as_deref(), Some("/files/"));
    }
}
