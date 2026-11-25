//! Isolated, opt-in candidate using the upstream server's existing auth gate.
//! This is not the full Firecrawl v2 contract. Unsupported requests fail closed.
use super::{AppState, Cloud, gate, from_a_web_page, error_response};
use crate::{cloud::{self, CloudMode}, guard::NetworkPolicy,
    scrape::{BrowserMode, Engine, EngineConfig, ScrapeOptions, Format}};
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
#[path = "candidate-crawl.rs"]
mod crawl;
#[path = "candidate-pdf.rs"]
mod pdf;
#[path = "candidate-map.rs"]
mod map;
#[path = "local-model-probe.rs"]
mod model;

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
    crawler: crawl::Crawler,
    pdf_enabled: bool,
    model_name: Option<String>,
    model_slot: Arc<Semaphore>,
    browser_enabled: bool,
}

#[derive(Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct StaticRequest {
    url: String,
    #[serde(default="default_static_formats")]
    formats: Vec<Value>,
    #[serde(default="default_main")]
    only_main_content: bool,
    #[serde(default)]
    max_age: u64,
    #[serde(default)]
    store_in_cache: bool,
    #[serde(default)]
    timeout: Option<u64>,
    #[serde(default)]
    wait_for: u64,
    #[serde(default)]
    parsers: Option<Vec<String>>,
    #[serde(default)]
    origin: Option<String>,
}

fn default_formats()->Vec<String>{vec!["markdown".into()]}
fn default_main()->bool{true}
fn default_static_formats()->Vec<Value>{vec![json!("markdown")]}

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

fn valid_common(req:&StaticRequest)->bool{
    req.max_age==0&&!req.store_in_cache&&
        req.origin.as_ref().is_none_or(|s|!s.is_empty()&&s.len()<=256&&!s.chars().any(char::is_control))
}

fn valid_static(req:&StaticRequest) -> bool {
    valid_common(req)&&req.wait_for==0&&req.parsers.is_none()&&!req.formats.is_empty()&&req.formats.len()<=4&&
        req.formats.iter().all(|s|matches!(s.as_str(),Some("markdown"|"html"|"rawHtml"|"links")))&&
        req.formats.iter().filter_map(Value::as_str).collect::<std::collections::HashSet<_>>().len()==req.formats.len()&&
        (1..=45000).contains(&req.timeout.unwrap_or(30000))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QueryFormat{
    #[serde(rename="type")] kind:String,
    prompt:String,
    mode:String,
}

fn model_format(req:&StaticRequest)->Option<(model::Mode,Option<String>)>{
    if !valid_common(req)||req.wait_for!=0||req.parsers.is_some()||req.formats.len()!=1||!(1..=180000).contains(&req.timeout.unwrap_or(180000)){return None;}
    let f=&req.formats[0];
    if f=="summary"{return Some((model::Mode::Summary,None));}
    let q:QueryFormat=serde_json::from_value(f.clone()).ok()?;
    if q.kind!="query"||q.prompt.trim().is_empty()||q.prompt.len()>2048{return None;}
    let mode=match q.mode.as_str(){"freeform"=>model::Mode::Query,"directQuote"=>model::Mode::DirectQuote,_=>return None};
    Some((mode,Some(q.prompt)))
}

async fn add_model_result(data:&mut Value,mode:model::Mode,prompt:Option<String>,endpoint:&str,
    model_name:&str,budget:Duration)->Result<(),StatusCode>{
    let markdown=data["markdown"].as_str().ok_or(StatusCode::BAD_GATEWAY)?.to_owned();
    let key=if matches!(mode,model::Mode::Summary){"summary"}else{"answer"};
    let direct=matches!(mode,model::Mode::DirectQuote);
    let result=model::extract_at(endpoint,model::Request{mode,markdown,prompt,schema:None},
        model_name,budget).await.map_err(|_|StatusCode::BAD_GATEWAY)?;
    let value=result["candidate"].as_str().ok_or(StatusCode::BAD_GATEWAY)?;
    data[key]=json!(value);
    data["researchModel"]=result["research"].clone();
    data["warning"]=json!(if direct{
        "Selected original Markdown lines; quotation text is source-matched, but relevance and page completeness are unverified."
    }else{
        "Model-generated extraction; source facts and completeness are not independently verified."
    });
    data.as_object_mut().ok_or(StatusCode::BAD_GATEWAY)?.remove("markdown");
    Ok(())
}

fn browser_formats(req:&StaticRequest)->Option<Vec<Format>>{
    if !valid_common(req)||req.parsers.is_some()||!(1..=10000).contains(&req.wait_for)||
        !(1..=60000).contains(&req.timeout.unwrap_or(60000))||
        req.timeout.unwrap_or(60000)<=req.wait_for||req.formats.is_empty()||req.formats.len()>4||
        req.formats.iter().filter_map(Value::as_str).collect::<std::collections::HashSet<_>>().len()!=req.formats.len(){
        return None;
    }
    req.formats.iter().map(|f|match f.as_str()?{
        "markdown"=>Some(Format::Markdown),"html"=>Some(Format::Html),
        "rawHtml"=>Some(Format::RawHtml),"links"=>Some(Format::Links),_=>None
    }).collect()
}

fn valid_pdf_options(req:&StaticRequest)->bool{
    valid_common(req)&&req.wait_for==0&&req.formats==[json!("markdown")]&&
        req.parsers.as_ref().is_none_or(|p|p.as_slice()==["pdf"])&&
        (1..=110000).contains(&req.timeout.unwrap_or(110000))
}
fn pdf_request(req:&StaticRequest,target:&url::Url)->bool{
    valid_pdf_options(req)&&target.path().to_ascii_lowercase().ends_with(".pdf")
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
    let model_req=model_format(&req);
    let render_formats=browser_formats(&req);
    if !valid_static(&req)&&model_req.is_none()&&render_formats.is_none()&&!valid_pdf_options(&req){
        return refusal(StatusCode::BAD_REQUEST,"UNSUPPORTED_OPTIONS");
    }
    let target=match static_target(&req.url) {
        Ok(u)=>u,Err(status)=>return refusal(status,"DESTINATION_DENIED")
    };
    let is_pdf=pdf_request(&req,&target);
    if !valid_static(&req)&&model_req.is_none()&&render_formats.is_none()&&!is_pdf{
        return refusal(StatusCode::BAD_REQUEST,"UNSUPPORTED_OPTIONS");
    }
    if is_pdf{
        if !state.pdf_enabled{return refusal(StatusCode::SERVICE_UNAVAILABLE,"PDF_WORKER_DISABLED");}
        let Ok(_slot)=state.slots.try_acquire()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY")};
        let result=pdf::document(&state,&target,Duration::from_millis(req.timeout.unwrap_or(110000))).await
            .and_then(|receipt|pdf::v2_document(&target,receipt));
        return match result{
            Ok(data)=>bounded(json!({"success":true,"data":data,"research":{"cache_used":false,"cache_supported":false}})),
            Err(status)=>refusal(status,"PDF_EXTRACTION_FAILED")
        };
    }
    if let Some(formats)=render_formats{
        if !state.browser_enabled{return refusal(StatusCode::SERVICE_UNAVAILABLE,"BROWSER_DISABLED");}
        let Ok(slot)=state.slots.clone().try_acquire_owned()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY")};
        let request=render::Request{url:target.to_string(),wait_for:req.wait_for,only_main_content:req.only_main_content};
        let budget=Duration::from_millis(req.timeout.unwrap_or(60000));
        let result=tokio::spawn(async move{
            let _slot=slot;
            render::render_formats_at(&state.render_endpoint,request,budget,formats).await
        }).await;
        return match result{
            Ok(Ok(mut value))=>{
                value["success"]=json!(true);
                value["research"]["cache_used"]=json!(false);
                value["research"]["cache_supported"]=json!(false);
                bounded(value)
            },
            Ok(Err("destination denied"))=>refusal(StatusCode::FORBIDDEN,"DESTINATION_DENIED"),
            _=>refusal(StatusCode::BAD_GATEWAY,"RENDER_FAILED")
        };
    }
    let Ok(_slot)=state.slots.try_acquire() else {
        return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY");
    };
    if let Some((mode,prompt))=model_req{
        let Some(name)=state.model_name.as_deref()else{return refusal(StatusCode::SERVICE_UNAVAILABLE,"MODEL_DISABLED")};
        let Ok(_model_slot)=state.model_slot.try_acquire()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"MODEL_BUSY")};
        let deadline=tokio::time::Instant::now()+Duration::from_millis(req.timeout.unwrap_or(180000));
        let mut data=match static_document_with_options(&state,target,ScrapeOptions{
            timeout:Some(req.timeout.unwrap_or(180000).min(45000)),only_main_content:req.only_main_content,..Default::default()
        }).await{Ok(data)=>data,Err(status)=>return refusal(status,"STATIC_SCRAPE_FAILED")};
        let remaining=deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero(){return refusal(StatusCode::GATEWAY_TIMEOUT,"MODEL_DEADLINE");}
        if let Err(status)=add_model_result(&mut data,mode,prompt,model::ENDPOINT,name,remaining).await{
            return refusal(status,"MODEL_EXTRACTION_FAILED");
        }
        return bounded(json!({"success":true,"data":data,"research":{"cache_used":false,"cache_supported":false}}));
    }
    let mut formats=req.formats.clone();
    // Markdown is used internally for block/empty-content detection even if
    // the client requests only HTML or links. Do not return extra fields.
    let wants_markdown=req.formats.iter().any(|s|s=="markdown");
    if !wants_markdown{formats.push(json!("markdown"));}
    let opts=ScrapeOptions{formats,only_main_content:req.only_main_content,
        timeout:Some(req.timeout.unwrap_or(30000)),..Default::default()};
    match static_document_with_options(&state,target,opts).await {
        Ok(mut data)=>{
            if !wants_markdown{data.as_object_mut().unwrap().remove("markdown");}
            bounded(json!({"success":true,"data":data,"research":{"cache_used":false,"cache_supported":false}}))
        },
        Err(status)=>refusal(status,"STATIC_SCRAPE_FAILED")
    }
}

async fn static_document(state:&CandidateState,target:url::Url,timeout:u64)->Result<Value,StatusCode>{
    static_document_with_options(state,target,ScrapeOptions{timeout:Some(timeout),..Default::default()}).await
}

async fn static_document_with_options(state:&CandidateState,target:url::Url,opts:ScrapeOptions)->Result<Value,StatusCode>{
    let budget=Duration::from_millis(opts.timeout.unwrap_or(30000));
    tokio::time::timeout(budget,async {
        render::check_dns(&target).await.map_err(|_|StatusCode::FORBIDDEN)?;
        let mut data=cloud::scrape_with_fallback(&state.auth.engine,&state.auth.cloud,target.as_str(),&opts).await
            .map_err(|e|match e.code {
                "BLOCKED_TARGET"=>StatusCode::FORBIDDEN,
                "DOCUMENT_ISOLATION_REQUIRED"=>StatusCode::UNSUPPORTED_MEDIA_TYPE,
                _=>StatusCode::BAD_GATEWAY
            })?;
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

fn router(token:String, search_endpoint:String, render_endpoint:String,batch:Option<batch::BatchState>,pdf_enabled:bool,model_name:Option<String>,browser_enabled:bool)->Result<(Router,CandidateState),String> {
    if !(32..=256).contains(&token.len()) || token.chars().any(|c|c.is_control()||c.is_whitespace()) {
        return Err("invalid candidate token".into());
    }
    if model_name.as_ref().is_some_and(|s|s.trim().is_empty()||s.len()>128||s.chars().any(char::is_control)){return Err("invalid operator model name".into());}
    let engine=Arc::new(Engine::new(EngineConfig{policy:NetworkPolicy::PublicOnly,
        user_agent:None,browser:BrowserMode::Never})?.require_html_only());
    // Even an accidental cloud invocation cannot contact a provider.
    let auth=Arc::new(AppState {
        engine,cloud:Cloud::new("http://127.0.0.1:9".into(),None,CloudMode::Off),
        token:Some(token),use_client_key:false,allowed_hosts:vec![],
        jobs:Mutex::new(HashMap::new())
    });
    let state=CandidateState{auth:auth.clone(),slots:Arc::new(Semaphore::new(4)),
        search_endpoint,render_endpoint,batch,crawler:crawl::Crawler::new()?,pdf_enabled,model_name,model_slot:Arc::new(Semaphore::new(1)),browser_enabled};
    let app=Router::new()
        .route("/health",get(|State(state):State<CandidateState>|async move{
            let job_store=match &state.batch{None=>"disabled",Some(b) if b.ready()=>"ready",Some(_)=>"degraded"};
            let status=if job_store=="degraded"{StatusCode::SERVICE_UNAVAILABLE}else{StatusCode::OK};
            (status,Json(json!({"status":if status==StatusCode::OK{"ok"}else{"degraded"},"engine":"rust-candidate","research":true,"jobStore":job_store})))
        }))
        .route("/v2/scrape",post(static_scrape))
        .route("/v2/search",post(search_route))
        .route("/v2/map",post(map::route))
        .route("/v2/crawl",post(batch::create_v2_crawl))
        .route("/v2/crawl/{id}",get(batch::status_v2_crawl).delete(batch::cancel))
        .route("/research/render",post(render_route))
        .route("/research/batch",post(batch::create))
        .route("/research/crawl",post(batch::create_crawl))
        .route("/research/pdf",post(pdf::extract))
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
    let pdf_enabled=std::env::var("CANDIDATE_PDF_ENABLED").as_deref()==Ok("1");
    let model_name=std::env::var("CANDIDATE_MODEL_NAME").ok();
    let browser_enabled=std::env::var("CANDIDATE_BROWSER_ENABLED").as_deref()==Ok("1");
    let (app,state)=router(token.trim().to_string(),SEARCH.into(),RENDER.into(),batch,pdf_enabled,model_name,browser_enabled)?;
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

    #[test]
    fn actual_mcp_scrape_envelope_and_supported_formats(){
        let minimal:StaticRequest=serde_json::from_value(json!({
            "url":"https://example.org/docs/","formats":["markdown"],"origin":"mcp-candidate-contract@3.40.0"
        })).unwrap();
        assert!(valid_static(&minimal));
        assert!(minimal.only_main_content);
        assert_eq!(minimal.timeout.unwrap_or(30000),30000);
        let full:StaticRequest=serde_json::from_value(json!({"url":"https://example.org/",
            "formats":["html","rawHtml","links"],"onlyMainContent":false})).unwrap();
        assert!(valid_static(&full));
        for extra in [
            json!({"formats":["summary"]}),json!({"formats":[]}),json!({"formats":["markdown","markdown"]}),
            json!({"maxAge":1}),json!({"storeInCache":true}),json!({"timeout":45001}),
            json!({"origin":"x\nheader"})
        ]{
            let mut input=json!({"url":"https://example.org/"});
            input.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            assert!(!valid_static(&serde_json::from_value(input).unwrap()));
        }
        assert!(serde_json::from_value::<StaticRequest>(json!({"url":"https://example.org/",
            "headers":{"Cookie":"synthetic"}})).is_err());
    }

    #[tokio::test]
    async fn captured_model_format_and_strict_quote_result(){
        let req:StaticRequest=serde_json::from_value(json!({"url":"https://example.org/",
            "formats":[{"type":"query","prompt":"Quote the value","mode":"directQuote"}],
            "origin":"mcp-candidate-contract@3.40.0"})).unwrap();
        let (mode,prompt)=model_format(&req).unwrap();
        let (base,task)=spawn(Router::new().route("/v1/responses",post(|Json(body):Json<Value>|async move{
            assert_eq!(body["input"][0]["content"][0]["text"],"0: Header\n1: Stock 421.5");
            ([("x-firecrawl-model-contract","strict-v1"),("x-firecrawl-input-trimmed","false"),
                ("x-firecrawl-model-provider","gemini")],
                Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":"[1]"}]}]})))
        }))).await;
        let mut data=json!({"markdown":"Header\nStock 421.5","metadata":{"url":"https://example.org/"}});
        add_model_result(&mut data,mode,prompt,&format!("{base}/v1/responses"),"fixture",Duration::from_secs(3)).await.unwrap();
        assert_eq!(data["answer"],"Stock 421.5");
        assert_eq!(data["researchModel"]["quote_spans_verified"],true);
        assert!(data.get("markdown").is_none());
        assert!(data["warning"].is_string());
        task.abort();
    }

    #[tokio::test]
    async fn static_engine_refuses_documents_before_native_parser() {
        // Only the synthetic loopback test may use AllowPrivate.
        let app=Router::new()
            .route("/page",get(||async{axum::response::Html("<html><body><p>Native HTML fixture 42.</p></body></html>")}))
            .route("/pdf",get(||async{([("content-type","application/pdf")],"%PDF-invalid-fixture")}))
            .route("/mislabelled",get(||async{([("content-type","text/html")],"%PDF-invalid-fixture")}))
            .route("/redirect",get(||async{axum::response::Redirect::temporary("/pdf")}));
        let (base,task)=spawn(app).await;
        let engine=Engine::new(EngineConfig{policy:NetworkPolicy::AllowPrivate,user_agent:None,
            browser:BrowserMode::Never}).unwrap().require_html_only();
        let opts=ScrapeOptions::default();
        let doc=engine.scrape(&format!("{base}/page"),&opts).await.unwrap();
        assert!(doc["markdown"].as_str().unwrap().contains("Native HTML fixture 42."));
        for path in ["/pdf","/mislabelled","/redirect"]{
            let err=engine.scrape(&format!("{base}{path}"),&opts).await.unwrap_err();
            assert_eq!(err.code,"DOCUMENT_ISOLATION_REQUIRED");
            assert_eq!(err.status,415);
        }
        task.abort();
    }

    #[tokio::test]
    async fn authenticated_composed_routes_and_denials() {
        let (backend,worker)=spawn(Router::new()
            .route("/search",get(||async{Json(json!({"results":[{"url":"https://93.184.216.34/report","title":"Report"}]}))}))
            .route("/scrape",post(|Json(body):Json<Value>|async move{
                Json(json!({"content":"<html><body><p>Stock 421.5</p></body></html>",
                    "pageStatusCode":200,"contentType":"text/html","url":body["url"]}))
            }))).await;
        let (base,server)=spawn(router(TOKEN.into(),format!("{backend}/search"),format!("{backend}/scrape"),None,false,None,true).unwrap().0).await;
        let c=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().unwrap();
        assert_eq!(c.get(format!("{base}/health")).send().await.unwrap().status(),StatusCode::UNAUTHORIZED);
        assert_eq!(c.get(format!("{base}/health")).bearer_auth(TOKEN).header("Host","bad.example").send().await.unwrap().status(),StatusCode::FORBIDDEN);
        let search:Value=c.post(format!("{base}/v2/search")).bearer_auth(TOKEN).json(&json!({"query":"stock"}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        let actual:Value=c.post(format!("{base}/v2/search")).bearer_auth(TOKEN).json(&json!({
            "query":"stock","limit":2,"sources":["web","alexandria"],"domainTools":true,
            "toolDetail":"compact","origin":"mcp-candidate-contract@3.40.0"
        })).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert!(actual["warning"].as_str().unwrap().contains("Web results only"));
        assert_eq!(actual["data"]["web"][0]["position"],1);
        let denied=c.post(format!("{base}/v2/scrape")).bearer_auth(TOKEN).json(&json!({
            "url":"https://127.0.0.1/","formats":["markdown"],"origin":"mcp-candidate-contract@3.40.0"
        })).send().await.unwrap();
        assert_eq!(denied.status(),StatusCode::FORBIDDEN);
        assert_eq!(c.post(format!("{base}/v2/scrape")).bearer_auth(TOKEN).json(&json!({"url":"https://example.org/","formats":[{"type":"query","prompt":"Value?","mode":"freeform"}]})).send().await.unwrap().status(),StatusCode::SERVICE_UNAVAILABLE);
        let result:Value=c.post(format!("{base}/research/render")).bearer_auth(TOKEN)
            .json(&json!({"url":search["data"]["web"][0]["url"],"onlyMainContent":false}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert!(result["data"]["markdown"].as_str().unwrap().contains("421.5"));
        let rendered:Value=c.post(format!("{base}/v2/scrape")).bearer_auth(TOKEN).json(&json!({
            "url":"https://93.184.216.34/report","formats":["html","links"],"waitFor":50
        })).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(rendered["success"],true);
        assert!(rendered["data"]["html"].as_str().unwrap().contains("421.5"));
        assert!(rendered["data"].get("markdown").is_none());
        assert_eq!(rendered["data"]["metadata"]["contentType"],"text/html");
        assert_eq!(c.post(format!("{base}/v2/scrape")).bearer_auth(TOKEN).json(&json!({
            "url":"https://93.184.216.34/report.pdf","formats":["markdown"],"parsers":["pdf"]
        })).send().await.unwrap().status(),StatusCode::SERVICE_UNAVAILABLE);
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
            .send().await.unwrap().status(),StatusCode::BAD_REQUEST);
        assert_eq!(c.post(format!("{base}/v2/map")).bearer_auth(TOKEN).json(&json!({"url":"https://127.0.0.1/","limit":2}))
            .send().await.unwrap().status(),StatusCode::FORBIDDEN);
        server.abort();worker.abort();
    }
}
