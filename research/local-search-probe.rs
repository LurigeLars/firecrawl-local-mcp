//! Research-only adapter for the existing SearXNG language proxy.
//! No public route, new dependency, cloud fallback or production configuration.
use firecrawl_local::guard::{check_url, NetworkPolicy};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::HashSet, io::{self, Read}, time::Duration};
use url::Url;

const ENDPOINT: &str = "http://searxng-proxy:8081/search";
const MAX_BODY: usize = 2 * 1024 * 1024;
const MAX_REQUEST: u64 = 32 * 1024;
fn default_limit() -> usize { 10 }
fn default_lang() -> String { "en".into() }
fn default_timeout() -> u64 { 60_000 }

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    query: String,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default = "default_lang")]
    lang: String,
    #[serde(default = "default_timeout")]
    timeout: u64,
    #[serde(default)]
    safe: bool,
    #[serde(default)]
    sources: Option<Vec<String>>,
    #[serde(default)]
    include_domains: Vec<String>,
    #[serde(default)]
    exclude_domains: Vec<String>,
    #[serde(default)]
    origin: Option<String>,
    #[serde(default)]
    domain_tools: bool,
    #[serde(default)]
    tool_detail: Option<String>,
}

fn valid_domain(s: &str) -> bool {
    s.len() <= 253 && s.contains('.') &&
        matches!(url::Host::parse(s), Ok(url::Host::Domain(_))) &&
        s.split('.').all(|part| !part.is_empty() && part.len() <= 63 &&
            !part.starts_with('-') && !part.ends_with('-') &&
            part.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-'))
}

impl Request {
    pub(crate) fn limit(&self)->usize{self.limit}
    pub(crate) fn timeout_ms(&self)->u64{self.timeout}

    pub(crate) fn validate(&self) -> Result<(), &'static str> {
        if self.query.trim().is_empty() || self.query.len() > 4096 ||
            self.query.chars().any(char::is_control) ||
            !(1..=100).contains(&self.limit) ||
            !(1..=60_000).contains(&self.timeout) ||
            self.lang.len() > 32 || self.lang.is_empty() ||
            !self.lang.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-') {
            return Err("invalid search parameters");
        }
        // The installed MCP client sends web + alexandria by default.
        // Accept that exact mixed request, but mark catalogue coverage unavailable.
        // Never contact a paid/remote provider and never claim catalogue results.
        if self.sources.as_ref().is_some_and(|v| {
            v.as_slice() != ["web"] && v.as_slice() != ["web","alexandria"]
        }) {
            return Err("only web or the default mixed MCP search is supported");
        }
        if self.origin.as_ref().is_some_and(|s| s.is_empty() || s.len()>256 || s.chars().any(char::is_control)) ||
           self.tool_detail.as_deref().is_some_and(|s| !matches!(s,"compact"|"summary"|"full")) {
            return Err("invalid MCP metadata");
        }
        if !self.include_domains.is_empty() && !self.exclude_domains.is_empty() {
            return Err("includeDomains and excludeDomains are mutually exclusive");
        }
        if self.include_domains.len() + self.exclude_domains.len() > 20 ||
            self.include_domains.iter().chain(&self.exclude_domains).any(|s| !valid_domain(s)) {
            return Err("invalid domain filter");
        }
        Ok(())
    }

    fn catalogue_requested(&self) -> bool {
        self.domain_tools || self.sources.as_ref().is_some_and(|v|v.iter().any(|s|s=="alexandria"))
    }

    fn proxy_query(&self) -> String {
        // Reuse the actual language proxy's site-operator and engine fallback
        // behavior rather than inventing another search implementation.
        let include = self.include_domains.iter().map(|d| format!("site:{}", d.to_ascii_lowercase()))
            .collect::<Vec<_>>().join(" OR ");
        let exclude = self.exclude_domains.iter().map(|d| format!("-site:{}", d.to_ascii_lowercase()))
            .collect::<Vec<_>>().join(" ");
        [self.query.clone(), include, exclude].into_iter()
            .filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" ")
    }
}

#[derive(Deserialize)]
struct RawHit { url: String, title: Option<String>, content: Option<String> }

#[derive(Deserialize)]
struct Page {
    results: Vec<RawHit>,
}

#[derive(Serialize)]
struct Hit { url: String, title: String, description: String, source: &'static str, position: usize }

fn host_matches(host: &str, domain: &str) -> bool {
    let domain = domain.to_ascii_lowercase();
    host == domain || host.strip_suffix(&domain).is_some_and(|prefix| prefix.ends_with('.'))
}

fn accept_hit(raw: RawHit, req: &Request) -> Option<Hit> {
    if raw.url.len() > 8192 { return None; }
    let url = Url::parse(&raw.url).ok()?;
    // Search results are inert text, not authorized fetch targets. The existing
    // DNS/redirect policy is still mandatory for every later scrape.
    check_url(&url, NetworkPolicy::PublicOnly).ok()?;
    let host = url.host_str()?.trim_end_matches('.');
    if (!req.include_domains.is_empty() &&
        !req.include_domains.iter().any(|d| host_matches(host, d))) ||
        req.exclude_domains.iter().any(|d| host_matches(host, d)) {
        return None;
    }
    let title = raw.title.unwrap_or_default();
    let description = raw.content.unwrap_or_default();
    if title.len() > 4096 || description.len() > 16384 { return None; }
    Some(Hit { url: url.into(), title, description, source:"web", position:0 })
}

pub(crate) async fn search_at(endpoint: &str, req: Request) -> Result<Value, &'static str> {
    req.validate()?;
    let client = reqwest::Client::builder()
        .no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_millis(req.timeout))
        .build().map_err(|_| "search client unavailable")?;
    let budget = Duration::from_millis(req.timeout);
    tokio::time::timeout(budget, async {
        let mut web = Vec::new();
        let mut seen = HashSet::new();
        let mut omitted = 0usize;
        // Same 20-result page budget as the legacy SearXNG adapter.
        // No automatic extra pages or retries for low-result queries.
        for page in 1..=req.limit.div_ceil(20) {
            let mut params = vec![
                ("q", req.proxy_query()), ("language", req.lang.clone()),
                ("format", "json".to_string()), ("pageno", page.to_string()),
            ];
            if req.safe { params.push(("safesearch", "2".into())); }
            let mut url = Url::parse(endpoint).map_err(|_| "invalid fixed search endpoint")?;
            url.query_pairs_mut().extend_pairs(params.iter().map(|(k, v)| (*k, v.as_str())));
            let mut response = client.get(url).send().await
                .map_err(|_| "search proxy transport failed")?;
            if !response.status().is_success() { return Err("search proxy returned non-success"); }
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| "search proxy body failed")? {
                if chunk.len() > MAX_BODY - body.len() { return Err("search proxy body too large"); }
                body.extend_from_slice(&chunk);
            }
            let results: Page = serde_json::from_slice(&body).map_err(|_| "invalid search proxy response")?;
            if results.results.is_empty() { break; }
            for raw in results.results {
                if let Some(mut hit) = accept_hit(raw, &req) {
                    if seen.insert(hit.url.clone()) {
                        hit.position=web.len()+1;
                        web.push(hit);
                    }
                } else { omitted += 1; }
                if web.len() >= req.limit { break; }
            }
            if web.len() >= req.limit { break; }
        }
        // Research metadata is explicit. Final public API envelope and
        // credits/id semantics still need end-to-end compatibility testing.
        let catalogue_requested=req.catalogue_requested();
        let mut value=json!({"success": true, "data": {"web": web}, "research": {
            "requested": req.limit, "returned": web.len(), "omitted": omitted,
            "source": "local-searxng", "catalogue_requested":catalogue_requested,
            "catalogue_available":false
        }});
        if catalogue_requested {
            value["warning"]=json!("Web results only: Alexandria catalogue and domain tool discovery are unavailable on this local candidate.");
        }
        Ok(value)
    }).await.map_err(|_| "search deadline exceeded")?
}

#[tokio::main]
async fn main() -> Result<(), &'static str> {
    let mut bytes = Vec::new();
    io::stdin().take(MAX_REQUEST + 1).read_to_end(&mut bytes).map_err(|_| "read failed")?;
    if bytes.len() as u64 > MAX_REQUEST { return Err("search request too large"); }
    let request: Request = serde_json::from_slice(&bytes).map_err(|_| "unsupported or invalid search request")?;
    println!("{}", search_at(ENDPOINT, request).await?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{extract::Query, routing::get, Router, Json, response::IntoResponse, http::StatusCode};
    use std::collections::HashMap;

    fn req(v: Value) -> Request { serde_json::from_value(v).unwrap() }

    #[test]
    fn rejects_unsupported_options_and_invalid_domains() {
        for v in [
            json!({"query":"x","scrapeOptions":{}}),
            json!({"query":"x","country":"se"}),
            json!({"query":"x","endpoint":"http://localhost"}),
            json!({"query":"x","tbs":"qdr:d"}),
        ] { assert!(serde_json::from_value::<Request>(v).is_err()); }
        for v in [
            json!({"query":"x","sources":["news"]}),
            json!({"query":"x","limit":101}),
            json!({"query":"x","timeout":0}),
            json!({"query":"x","includeDomains":["eia.gov -site:sec.gov"]}),
            json!({"query":"x","includeDomains":["127.0.0.1"]}),
            json!({"query":"x","includeDomains":["eia.gov"],"excludeDomains":["sec.gov"]}),
        ] { assert!(req(v).validate().is_err()); }
    }

    async fn spawn(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/search", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        (url, task)
    }

    #[tokio::test]
    async fn proxy_parameters_pagination_filtering_and_dedup() {
        let router = Router::new().route("/search", get(|Query(p): Query<HashMap<String,String>>| async move {
            assert_eq!(p["q"], "oil & gas site:eia.gov");
            assert_eq!(p["language"], "sv-SE");
            assert_eq!(p["format"], "json");
            assert_eq!(p["safesearch"], "2");
            assert!(!p.contains_key("engines")); // Existing proxy chooses engines.
            let page = &p["pageno"];
            let results = if page == "1" {
                json!([
                    {"url":"https://www.eia.gov/a","title":"A","content":"first"},
                    {"url":"https://eia.gov.example.org/b","title":"wrong"},
                    {"url":"http://127.0.0.1/","title":"private"},
                    {"url":"https://user:pass@eia.gov/b","title":"credentials"}
                ])
            } else {
                assert_eq!(page, "2");
                json!([
                    {"url":"https://www.eia.gov/a","title":"duplicate"},
                    {"url":"https://eia.gov/b","title":"B","content":"second"}
                ])
            };
            Json(json!({"results":results}))
        }));
        let (url, task) = spawn(router).await;
        let result = search_at(&url, req(json!({"query":"oil & gas","limit":21,
            "lang":"sv-SE","safe":true,"includeDomains":["eia.gov"]}))).await.unwrap();
        task.abort();
        assert_eq!(result["data"]["web"].as_array().unwrap().len(), 2);
        assert_eq!(result["data"]["web"][0]["description"], "first");
        assert_eq!(result["data"]["web"][1]["url"], "https://eia.gov/b");
        assert_eq!(result["research"]["omitted"], 3);
    }

    #[tokio::test]
    async fn rejects_redirect_invalid_json_and_large_response() {
        for (router, expected) in [
            (Router::new().route("/search", get(|| async {
                (StatusCode::FOUND, [("location", "http://127.0.0.1:9/")]).into_response()
            })), "search proxy returned non-success"),
            (Router::new().route("/search", get(|| async { "not json" })), "invalid search proxy response"),
            (Router::new().route("/search", get(|| async { "x".repeat(MAX_BODY + 1) })), "search proxy body too large"),
        ] {
            let (url, task) = spawn(router).await;
            let result = search_at(&url, req(json!({"query":"test"}))).await;
            task.abort();
            assert_eq!(result.unwrap_err(), expected);
        }
    }

    #[tokio::test]
    async fn total_deadline_is_bounded() {
        let router = Router::new().route("/search", get(|| async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            Json(json!({"results":[]}))
        }));
        let (url, task) = spawn(router).await;
        let result = search_at(&url, req(json!({"query":"test","timeout":50}))).await;
        task.abort();
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn actual_mcp_default_search_reports_catalogue_unavailable() {
        let router=Router::new().route("/search",get(||async{
            Json(json!({"results":[{"url":"https://example.org/","title":"Fixture"}]}))
        }));
        let (url,task)=spawn(router).await;
        let result=search_at(&url,req(json!({"query":"synthetic fixture","limit":2,
            "sources":["web","alexandria"],"domainTools":true,"toolDetail":"compact",
            "origin":"mcp-candidate-contract@3.40.0"}))).await.unwrap();
        assert_eq!(result["data"]["web"][0]["source"],"web");
        assert_eq!(result["data"]["web"][0]["position"],1);
        assert_eq!(result["research"]["catalogue_available"],false);
        assert!(result["warning"].as_str().unwrap().contains("Web results only"));
        assert!(result["data"].get("alexandria").is_none());
        assert!(req(json!({"query":"x","sources":["alexandria"]})).validate().is_err());
        assert!(req(json!({"query":"x","sources":["web","news"]})).validate().is_err());
        task.abort();
    }

    #[test]
    fn exclusion_matches_domain_boundary() {
        let r = req(json!({"query":"test","excludeDomains":["eia.gov"]}));
        let hit = |url: &str| RawHit {url:url.into(), title:None, content:None};
        assert!(accept_hit(hit("https://www.eia.gov/"), &r).is_none());
        assert!(accept_hit(hit("https://not-eia.gov/"), &r).is_some());
    }
}
