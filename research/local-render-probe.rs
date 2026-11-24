//! Research-only bridge to the existing one-shot Playwright router.
//! Camofox remains an independent gateway tool. No browser sessions or keys.
use firecrawl_local::{guard::{check_url, check_ip, NetworkPolicy}, scrape::{process_html, Format, ScrapeOptions}};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{io::{self, Read}, time::Duration};
use url::Url;

const ENDPOINT: &str = "http://playwright-router:3000/scrape";
const MAX_BODY: usize = 16 * 1024 * 1024;
const MAX_INPUT: u64 = 16 * 1024;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    url: String,
    #[serde(default)]
    wait_for: u64,
    only_main_content: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Rendered {
    content: String,
    page_status_code: u16,
    page_error: Option<String>,
    content_type: Option<String>,
    url: Option<String>,
}

pub(crate) fn public_url(raw: &str) -> Result<Url, &'static str> {
    if raw.len() > 8192 { return Err("URL too long"); }
    let url = Url::parse(raw).map_err(|_| "invalid URL")?;
    check_url(&url, NetworkPolicy::PublicOnly).map_err(|_| "destination denied")?;
    Ok(url)
}

// Match the existing gateway's rule: reject the whole answer set if any
// address is private. The browser must ALSO enforce this at connection time;
// this preflight by itself cannot protect against DNS rebinding.
pub(crate) async fn check_dns(url: &Url) -> Result<(), &'static str> {
    let host = url.host_str().ok_or("missing host")?.trim_matches(['[', ']']);
    let port = url.port_or_known_default().ok_or("missing port")?;
    let mut resolved = tokio::net::lookup_host((host, port)).await.map_err(|_| "DNS failed")?.peekable();
    if resolved.peek().is_none() || resolved.any(|a| check_ip(a.ip(), NetworkPolicy::PublicOnly).is_err()) {
        return Err("destination denied");
    }
    Ok(())
}

pub(crate) async fn render_at(endpoint: &str, req: Request, budget: Duration) -> Result<Value, &'static str> {
    let target = public_url(&req.url)?;
    if req.wait_for > 10_000 { return Err("waitFor exceeds research budget"); }
    let client = reqwest::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none()).timeout(budget)
        .build().map_err(|_| "browser client unavailable")?;
    let (rendered, landed, route) = tokio::time::timeout(budget, async {
        check_dns(&target).await?;
        let mut response = client.post(endpoint).json(&json!({
            "url": target.as_str(),
            "wait_after_load": req.wait_for,
            "timeout": 30_000,
            "skip_tls_verification": false
        })).send().await.map_err(|_| "browser transport failed")?;
        if !response.status().is_success() { return Err("browser service failed"); }
        let route = match response.headers().get("x-firecrawl-public-proxy").and_then(|x| x.to_str().ok()) {
            Some("direct") => "direct",
            Some("used") => "proxy",
            _ => "unknown",
        };
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "browser body failed")? {
            if chunk.len() > MAX_BODY - body.len() { return Err("browser body too large"); }
            body.extend_from_slice(&chunk);
        }
        let rendered: Rendered = serde_json::from_slice(&body).map_err(|_| "invalid browser response")?;
        if rendered.page_error.as_deref().is_some_and(|e| e.starts_with("Blocked insecure target URL ")) {
            return Err("destination denied");
        }
        if !(200..300).contains(&rendered.page_status_code) || rendered.page_error.is_some() {
            return Err("target response failed");
        }
        let mime = rendered.content_type.as_deref().unwrap_or("").split(';').next().unwrap_or("").trim();
        if !mime.eq_ignore_ascii_case("text/html") || rendered.content.trim().is_empty() {
            return Err("non-HTML or empty target response");
        }
        let landed = public_url(rendered.url.as_deref().ok_or("missing landed URL")?)?;
        check_dns(&landed).await?;
        Ok((rendered, landed, route))
    }).await.map_err(|_| "browser deadline exceeded")??;
    let opts = ScrapeOptions { only_main_content: req.only_main_content, ..Default::default() };
    // shortcut: conversion is synchronous and separately resource-bounded in
    // research; production must include it in the end-to-end worker deadline.
    let (data, _, _) = process_html(rendered.content, &landed, &opts, &[Format::Markdown, Format::Links])
        .map_err(|_| "HTML conversion failed")?;
    Ok(json!({"data": data, "research": {
        "landed_url": landed.as_str(), "target_status": rendered.page_status_code,
        "router_path": route, "source_verified":false
    }}))
}

#[tokio::main]
async fn main() -> Result<(), &'static str> {
    let mut bytes = Vec::new();
    io::stdin().take(MAX_INPUT + 1).read_to_end(&mut bytes).map_err(|_| "read failed")?;
    if bytes.len() as u64 > MAX_INPUT { return Err("request too large"); }
    let req = serde_json::from_slice(&bytes).map_err(|_| "invalid browser request")?;
    println!("{}", render_at(ENDPOINT, req, Duration::from_secs(60)).await?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{routing::post, Router, Json, response::IntoResponse, http::StatusCode};

    const TARGET: &str = "https://93.184.216.34/start";
    fn req() -> Request {
        Request { url: TARGET.into(), wait_for: 50, only_main_content: false }
    }

    async fn spawn(router: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/scrape", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap(); });
        (url, task)
    }
    #[test]
    fn public_only_and_no_sensitive_options() {
        for url in ["http://127.0.0.1/", "http://169.254.169.254/", "https://user:pw@example.com/"] {
            assert!(public_url(url).is_err());
        }
        for v in [
            json!({"url":TARGET,"onlyMainContent":true,"headers":{"Cookie":"x"}}),
            json!({"url":TARGET,"onlyMainContent":true,"skipTlsVerification":true}),
            json!({"url":TARGET,"onlyMainContent":true,"endpoint":"http://localhost"}),
        ] { assert!(serde_json::from_value::<Request>(v).is_err()); }
    }

    #[tokio::test]
    async fn uses_rendered_html_and_landed_url() {
        let router = Router::new().route("/scrape", post(|Json(body): Json<Value>| async move {
            assert_eq!(body["url"], TARGET);
            assert_eq!(body["wait_after_load"], 50);
            assert_eq!(body["skip_tls_verification"], false);
            assert!(body.get("headers").is_none());
            ([("x-firecrawl-public-proxy","direct")], Json(json!({
                "content":"<html><body><h1>Stock report</h1><p>421.5 million barrels</p><a href=\"next\">Next</a></body></html>",
                "pageStatusCode":200, "contentType":"text/html; charset=utf-8",
                "url":"https://93.184.216.34/reports/weekly"
            })))
        }));
        let (url, task) = spawn(router).await;
        let out = render_at(&url, req(), Duration::from_secs(3)).await.unwrap();
        task.abort();
        assert!(out["data"]["markdown"].as_str().unwrap().contains("421.5 million barrels"));
        assert!(out["data"]["links"].to_string().contains("https://93.184.216.34/reports/next"));
        assert_eq!(out["research"]["router_path"], "direct");
        assert_eq!(out["research"]["source_verified"], false);
    }

    #[tokio::test]
    async fn rejects_denials_redirects_missing_metadata_and_non_html() {
        for (body, error) in [
            (json!({"content":"","pageStatusCode":403,"pageError":"Blocked insecure target URL \"http://127.0.0.1/\": resolves to a private/internal address"}), "destination denied"),
            (json!({"content":"blocked","pageStatusCode":429}), "target response failed"),
            (json!({"content":"<p>x</p>","pageStatusCode":200,"contentType":"text/html","url":"http://127.0.0.1/"}), "destination denied"),
            (json!({"content":"<p>x</p>","pageStatusCode":200,"contentType":"text/html"}), "missing landed URL"),
            (json!({"content":"{}","pageStatusCode":200,"contentType":"application/json","url":TARGET}), "non-HTML or empty target response"),
        ] {
            let router = Router::new().route("/scrape", post(move || {
                let body = body.clone();
                async move { Json(body) }
            }));
            let (url, task) = spawn(router).await;
            let result = render_at(&url, req(), Duration::from_secs(3)).await;
            task.abort();
            assert_eq!(result.unwrap_err(), error);
        }
        let router = Router::new().route("/scrape", post(|| async {
            (StatusCode::FOUND, [("location","http://127.0.0.1:9/")]).into_response()
        }));
        let (url, task) = spawn(router).await;
        let result = render_at(&url, req(), Duration::from_secs(3)).await;
        task.abort();
        assert_eq!(result.unwrap_err(), "browser service failed");
    }
}
