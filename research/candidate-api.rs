//! Isolated, opt-in candidate using the upstream server's existing auth gate.
//! This is not the full Firecrawl v2 contract. Unsupported requests fail closed.
use super::{AppState, Cloud, gate, from_a_web_page, error_response};
use crate::{cloud::{self, CloudMode}, guard::NetworkPolicy,
    scrape::{BrowserMode, Engine, EngineConfig, ScrapeOptions}};
use axum::{body::Bytes, extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode}, middleware, response::{IntoResponse, Response},
    routing::{get, post}, Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::{Arc,Mutex}, time::Duration};
use tokio::sync::Semaphore;

#[path = "local-search-probe.rs"]
mod search;
#[path = "local-render-probe.rs"]
mod render;
#[path = "candidate-batch.rs"]
mod batch;

const MAX_OUTPUT: usize = 2 * 1024 * 1024;
const SEARCH: &str = "http://searxng-proxy:8081/search";
const RENDER: &str = "http://playwright-router:3000/scrape";

#[derive(Clone)]
struct CandidateState {
    auth: Arc<AppState>,
    slots: Arc<Semaphore>,
    search_endpoint: String,
    render_endpoint: String,
    batch: Option<batch::BatchState>,
}

#[derive(Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct StaticRequest {
    url: String,
    formats: Vec<String>,
    only_main_content: bool,
    max_age: u64,
    store_in_cache: bool,
    timeout: u64,
}

fn refusal(status: StatusCode, code: &str) -> Response {
    error_response(status,code,"Request outside the supported research contract.")
}

fn decode<T: for<'de> Deserialize<'de>>(headers: &HeaderMap,body:&Bytes) -> Result<T,Response> {
    if from_a_web_page(headers) {
        return Err(refusal(StatusCode::FORBIDDEN,"WEB_PAGE_DENIED"));
    }
    let raw=super::json_body(headers,body)?;
    serde_json::from_value(raw).map_err(|_|refusal(StatusCode::BAD_REQUEST,"UNSUPPORTED_OPTIONS"))
}

fn bounded(value: Value) -> Response {
    match serde_json::to_vec(&value) {
        Ok(bytes) if bytes.len() <= MAX_OUTPUT => Json(value).into_response(),
        _ => refusal(StatusCode::PAYLOAD_TOO_LARGE,"RESULT_TOO_LARGE"),
    }
}

fn valid_static(req:&StaticRequest) -> bool {
    req.formats == ["markdown"] && req.only_main_content && req.max_age==0 &&
        !req.store_in_cache && (1..=45000).contains(&req.timeout)
}

fn static_target(raw:&str)->Result<url::Url,StatusCode>{
    let target=render::public_url(raw).map_err(|_|StatusCode::FORBIDDEN)?;
    if target.scheme()!="https" || target.query().is_some() || target.fragment().is_some() || target.port().is_some(){
        return Err(StatusCode::BAD_REQUEST);
    }
    Ok(target)
}

async fn static_scrape(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response {
    let req:StaticRequest=match decode(&headers,&body) { Ok(v)=>v,Err(r)=>return r };
    if !valid_static(&req) { return refusal(StatusCode::BAD_REQUEST,"UNSUPPORTED_OPTIONS"); }
    let target=match static_target(&req.url) {
        Ok(u)=>u,Err(status)=>return refusal(status,"DESTINATION_DENIED")
    };
    let Ok(_slot)=state.slots.try_acquire() else {
        return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY");
    };
    match static_document(&state,target,req.timeout).await {
        Ok(data)=>bounded(json!({"success":true,"data":data})),
        Err(status)=>refusal(status,"STATIC_SCRAPE_FAILED")
    }
}

async fn static_document(state:&CandidateState,target:url::Url,timeout:u64)->Result<Value,StatusCode>{
    let budget=Duration::from_millis(timeout);
    tokio::time::timeout(budget,async {
        render::check_dns(&target).await.map_err(|_|StatusCode::FORBIDDEN)?;
        let opts=ScrapeOptions {timeout:Some(timeout),..Default::default()};
        let mut data=cloud::scrape_with_fallback(&state.auth.engine,&state.auth.cloud,target.as_str(),&opts).await
            .map_err(|e|if e.code=="BLOCKED_TARGET" { StatusCode::FORBIDDEN } else { StatusCode::BAD_GATEWAY })?;
        // Do not promote incomplete, blocked or non-HTML results to success.
        let markdown=data["markdown"].as_str().unwrap_or("");
        if data["source"]!="local" || data["blocked"]==true || !data["warning"].is_null() ||
            !data["error"].is_null() || data["metadata"]["statusCode"]!=200 ||
            markdown.trim().is_empty() || cloud::looks_like_bot_check(markdown) ||
            !data["metadata"]["contentType"].as_str().unwrap_or("").split(';').next()
                .unwrap_or("").trim().eq_ignore_ascii_case("text/html") {
            return Err(StatusCode::BAD_GATEWAY);
        }
        let landed=render::public_url(data["metadata"]["url"].as_str().ok_or(StatusCode::BAD_GATEWAY)?)
            .map_err(|_|StatusCode::FORBIDDEN)?;
        render::check_dns(&landed).await.map_err(|_|StatusCode::FORBIDDEN)?;
        // Provenance, not an assertion of source completeness.
        data["researchSourceVerified"]=json!(false);
        Ok::<_,StatusCode>(data)
    }).await.map_err(|_|StatusCode::GATEWAY_TIMEOUT)?
}

async fn search_route(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response {
    let req:search::Request=match decode(&headers,&body) {Ok(v)=>v,Err(r)=>return r};
    if req.validate().is_err() {return refusal(StatusCode::BAD_REQUEST,"UNSUPPORTED_OPTIONS");}
    let Ok(_slot)=state.slots.try_acquire() else {return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY");};
    match search::search_at(&state.search_endpoint,req).await {
        Ok(value)=>bounded(value),
        Err(_)=>refusal(StatusCode::BAD_GATEWAY,"SEARCH_FAILED"),
    }
}

async fn render_route(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response {
    let req:render::Request=match decode(&headers,&body) {Ok(v)=>v,Err(r)=>return r};
    let Ok(_slot)=state.slots.try_acquire() else {return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY");};
    match render::render_at(&state.render_endpoint,req,Duration::from_secs(60)).await {
        Ok(value)=>bounded(value),
        Err("destination denied")=>refusal(StatusCode::FORBIDDEN,"DESTINATION_DENIED"),
        Err(_)=>refusal(StatusCode::BAD_GATEWAY,"RENDER_FAILED"),
    }
}

fn router(token:String, search_endpoint:String, render_endpoint:String,batch:Option<batch::BatchState>)->Result<(Router,CandidateState),String> {
    if !(32..=256).contains(&token.len()) || token.chars().any(|c|c.is_control()||c.is_whitespace()) {
        return Err("invalid candidate token".into());
    }
    let engine=Arc::new(Engine::new(EngineConfig{policy:NetworkPolicy::PublicOnly,
        user_agent:None,browser:BrowserMode::Never})?);
    // Even an accidental cloud invocation cannot contact a provider.
    let auth=Arc::new(AppState {
        engine,cloud:Cloud::new("http://127.0.0.1:9".into(),None,CloudMode::Off),
        token:Some(token),use_client_key:false,allowed_hosts:vec![],
        jobs:Mutex::new(HashMap::new())
    });
    let state=CandidateState{auth:auth.clone(),slots:Arc::new(Semaphore::new(4)),
        search_endpoint,render_endpoint,batch};
    let app=Router::new()
        .route("/health",get(|State(state):State<CandidateState>|async move{
            let job_store=match &state.batch{None=>"disabled",Some(b) if b.ready()=>"ready",Some(_)=>"degraded"};
            let status=if job_store=="degraded"{StatusCode::SERVICE_UNAVAILABLE}else{StatusCode::OK};
            (status,Json(json!({"status":if status==StatusCode::OK{"ok"}else{"degraded"},"engine":"rust-candidate","research":true,"jobStore":job_store})))
        }))
        .route("/v2/scrape",post(static_scrape))
        .route("/v2/search",post(search_route))
        .route("/research/render",post(render_route))
        .route("/research/batch",post(batch::create))
        .route("/research/batch/{id}",get(batch::status).delete(batch::cancel))
        .layer(DefaultBodyLimit::max(32*1024))
        .layer(middleware::from_fn_with_state(auth,gate))
        .with_state(state.clone());
    Ok((app,state))
}

/// Research binary always binds loopback. Secret file is operator configuration.
pub async fn run_candidate()->Result<(),String> {
    let path=std::env::var("CANDIDATE_TOKEN_FILE").unwrap_or_else(|_|"/run/secrets/firecrawl_candidate_token".into());
    let token=std::fs::read_to_string(path).map_err(|_|"candidate token file unavailable".to_string())?;
    let batch=std::env::var_os("CANDIDATE_JOB_DB_FILE").map(|path|batch::BatchState::open(std::path::Path::new(&path))).transpose()?;
    let (app,state)=router(token.trim().to_string(),SEARCH.into(),RENDER.into(),batch)?;
    let listener=tokio::net::TcpListener::bind("127.0.0.1:3002").await.map_err(|e|e.to_string())?;
    let worker=state.batch.is_some().then(||tokio::spawn(batch::supervise(state)));
    let result=axum::serve(listener,app).with_graceful_shutdown(super::shutdown_signal()).await.map_err(|e|e.to_string());
    if let Some(worker)=worker {worker.abort();let _=worker.await;}
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    const TOKEN:&str="synthetic-test-token-012345678901234567890";
    async fn spawn(app:Router)->(String,tokio::task::JoinHandle<()>) {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base=format!("http://{}",listener.local_addr().unwrap());
        let task=tokio::spawn(async move {axum::serve(listener,app).await.unwrap();});
        (base,task)
    }

    #[tokio::test]
    async fn authenticated_composed_routes_and_denials() {
        let (backend,worker)=spawn(Router::new()
            .route("/search",get(||async{Json(json!({"results":[{"url":"https://93.184.216.34/report","title":"Report"}]}))}))
            .route("/scrape",post(|Json(body):Json<Value>|async move{
                Json(json!({"content":"<html><body><p>Stock 421.5</p></body></html>",
                    "pageStatusCode":200,"contentType":"text/html","url":body["url"]}))
            }))).await;
        let (base,server)=spawn(router(TOKEN.into(),format!("{backend}/search"),format!("{backend}/scrape"),None).unwrap().0).await;
        let c=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        assert_eq!(c.get(format!("{base}/health")).send().await.unwrap().status(),StatusCode::UNAUTHORIZED);
        assert_eq!(c.get(format!("{base}/health")).bearer_auth(TOKEN).header("Host","bad.example").send().await.unwrap().status(),StatusCode::FORBIDDEN);
        let search:Value=c.post(format!("{base}/v2/search")).bearer_auth(TOKEN).json(&json!({"query":"stock"}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        let result:Value=c.post(format!("{base}/research/render")).bearer_auth(TOKEN)
            .json(&json!({"url":search["data"]["web"][0]["url"],"onlyMainContent":false}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert!(result["data"]["markdown"].as_str().unwrap().contains("421.5"));
        let static_body=json!({"url":"https://127.0.0.1/","formats":["markdown"],"onlyMainContent":true,
            "maxAge":0,"storeInCache":false,"timeout":1000});
        assert_eq!(c.post(format!("{base}/v2/scrape")).bearer_auth(TOKEN).json(&static_body)
            .send().await.unwrap().status(),StatusCode::FORBIDDEN);
        assert_eq!(c.post(format!("{base}/v2/scrape")).bearer_auth(TOKEN).header("Origin","https://example.org")
            .json(&static_body).send().await.unwrap().status(),StatusCode::FORBIDDEN);
        let mut unsupported=static_body;
        unsupported["headers"]=json!({});
        assert_eq!(c.post(format!("{base}/v2/scrape")).bearer_auth(TOKEN).json(&unsupported)
            .send().await.unwrap().status(),StatusCode::BAD_REQUEST);
        assert_eq!(c.post(format!("{base}/v2/crawl")).bearer_auth(TOKEN).json(&json!({}))
            .send().await.unwrap().status(),StatusCode::NOT_FOUND);
        server.abort();worker.abort();
    }
}
