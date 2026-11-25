//! Research-only bridge to the existing one-shot Playwright router.
//! Camofox remains an independent gateway tool. No browser sessions or keys.
use firecrawl_local::{browser::{needs_browser,looks_like_nav_only_shell},cloud, guard::{check_url, check_ip, NetworkPolicy}, scrape::{process_html, Format, ScrapeOptions}};
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
    pub(crate) url: String,
    #[serde(default)]
    pub(crate) wait_for: u64,
    pub(crate) only_main_content: bool,
    #[serde(default)] pub(crate) include_tags:Vec<String>,
    #[serde(default)] pub(crate) exclude_tags:Vec<String>,
}

// Deliberately limited CSS subset: tag, .class, #id and their compounds.
// No :has, combinators, wildcards, selector lists or arbitrary expressions.
// Invalid or advanced selectors fail rather than being silently dropped.
pub(crate) fn valid_tag_filters(include:&[String],exclude:&[String])->bool{
    if include.len()+exclude.len()>16{return false;}
    let pattern=regex::Regex::new(r"^(?:[A-Za-z][A-Za-z0-9_-]*)?(?:[.#][A-Za-z_][A-Za-z0-9_-]*)*$").unwrap();
    include.iter().chain(exclude).all(|s|!s.is_empty()&&s.len()<=128&&pattern.is_match(s))
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
    render_formats_at(endpoint,req,budget,vec![Format::Markdown,Format::Links]).await
}

pub(crate) async fn render_formats_at(endpoint:&str,req:Request,budget:Duration,formats:Vec<Format>)->Result<Value,&'static str>{
    render_checked_at(endpoint,req,budget,formats,false).await
}

pub(crate) async fn render_auto_at(endpoint:&str,req:Request,budget:Duration,formats:Vec<Format>)->Result<Value,&'static str>{
    render_checked_at(endpoint,req,budget,formats,true).await
}

async fn render_checked_at(endpoint:&str,req:Request,budget:Duration,mut formats:Vec<Format>,automatic:bool)->Result<Value,&'static str>{
    let target = public_url(&req.url)?;
    if !valid_tag_filters(&req.include_tags,&req.exclude_tags){return Err("unsupported HTML selector");}
    if req.wait_for > 10_000 || budget.is_zero() || budget.as_millis() <= u128::from(req.wait_for) { return Err("waitFor exceeds research budget"); }
    let client = reqwest::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none()).timeout(budget)
        .build().map_err(|_| "browser client unavailable")?;
    let (rendered, landed, route) = tokio::time::timeout(budget, async {
        check_dns(&target).await?;
        let mut response = client.post(endpoint).json(&json!({
            "url": target.as_str(),
            "wait_after_load": req.wait_for,
            "timeout": budget.as_millis().min(30_000) as u64,
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
    let opts = ScrapeOptions { only_main_content: req.only_main_content,
        include_tags:req.include_tags,exclude_tags:req.exclude_tags,..Default::default() };
    // Run conversion off the async executor; caller retains admission until
    // completion. The network deadline does not bound the blocking parser.
    let status=rendered.page_status_code;
    let url=landed.to_string();
    // For automatic fallback, reuse upstream shell detection on the returned
    // HTML. Raw HTML is only kept internally unless the caller requested it.
    let added_raw=automatic&&!formats.contains(&Format::RawHtml);
    if added_raw{formats.push(Format::RawHtml);}
    let (mut data,mut metadata,markdown)=tokio::task::spawn_blocking(move||
        process_html(rendered.content,&landed,&opts,&formats))
        .await.map_err(|_|"HTML conversion failed")?.map_err(|_|"HTML conversion failed")?;
    if markdown.trim().is_empty() || cloud::looks_like_bot_check(&markdown){
        return Err("empty or blocked rendered content");
    }
    if automatic{
        let raw=data.get("rawHtml").and_then(Value::as_str).ok_or("missing rendered HTML")?;
        if needs_browser(raw,&markdown)||looks_like_nav_only_shell(raw,&markdown){
            return Err("rendered content still appears incomplete");
        }
        if added_raw{data.remove("rawHtml");}
    }
    metadata.insert("url".into(),json!(url));
    metadata.insert("sourceURL".into(),json!(target.as_str()));
    metadata.insert("statusCode".into(),json!(status));
    metadata.insert("contentType".into(),json!("text/html"));
    // Legacy engine class, not a claim of direct routing. Keep actual route separately.
    metadata.insert("proxyUsed".into(),json!("basic"));
    data.insert("metadata".into(),Value::Object(metadata));
    data.insert("source".into(),json!("local"));
    data.insert("researchSourceVerified".into(),json!(false));
    let receipt=json!({"landed_url":url,"target_status":status,
        "router_path":route,"source_verified":false,"automatic":automatic});
    data.insert("researchBrowser".into(),receipt.clone());
    Ok(json!({"data":data,"research":receipt}))
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
        Request { url: TARGET.into(), wait_for: 50, only_main_content: false,include_tags:vec![],exclude_tags:vec![] }
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
        assert_eq!(out["data"]["metadata"]["url"],"https://93.184.216.34/reports/weekly");
        assert_eq!(out["data"]["metadata"]["sourceURL"],TARGET);
        assert_eq!(out["data"]["source"],"local");
        assert_eq!(out["research"]["source_verified"], false);
    }

    #[tokio::test]
    async fn rendered_html_respects_the_same_selector_subset(){
        let router=Router::new().route("/scrape",post(||async{Json(json!({
            "content":"<html><body><article><p>Included fixture 42</p><p class=\"ads\">Omit this ad</p></article><aside>Omit sidebar</aside></body></html>",
            "pageStatusCode":200,"contentType":"text/html","url":TARGET
        }))}));
        let (url,task)=spawn(router).await;
        let mut req=req();
        req.include_tags=vec!["article".into()];req.exclude_tags=vec![".ads".into()];
        let doc=render_formats_at(&url,req,Duration::from_secs(3),vec![Format::Markdown]).await.unwrap();
        let md=doc["data"]["markdown"].as_str().unwrap();
        assert!(md.contains("Included fixture 42"));
        assert!(!md.contains("Omit this ad")&&!md.contains("Omit sidebar"));
        task.abort();
    }

    #[tokio::test]
    async fn automatic_render_preserves_formats_and_rejects_remaining_shell() {
        for (html,success) in [
            ("<html><body><p>Stock report 421.5 million barrels</p></body></html>",true),
            ("<html><body><div id=\"root\">Loading...</div><script src=\"app.js\"></script></body></html>",false)
        ]{
            let router=Router::new().route("/scrape",post(move||async move{Json(json!({
                "content":html,"pageStatusCode":200,"contentType":"text/html","url":TARGET
            }))}));
            let (url,task)=spawn(router).await;
            let result=render_auto_at(&url,req(),Duration::from_secs(3),vec![Format::Markdown]).await;
            task.abort();
            if success{
                let doc=result.unwrap();
                assert!(doc["data"]["markdown"].as_str().unwrap().contains("421.5"));
                assert!(doc["data"].get("rawHtml").is_none());
            }else{assert_eq!(result.unwrap_err(),"rendered content still appears incomplete");}
        }
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
