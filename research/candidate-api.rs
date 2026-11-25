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
use std::{collections::{HashMap,VecDeque}, sync::{Arc,Mutex}, time::{Duration,Instant}};
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
    html_cache:Arc<Mutex<HtmlCache>>,
    network:NetworkMode,
}

// Default loopback remains unchanged. Internal mode is operator-selected,
// has one fixed service name, and never disables bearer authentication.
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
enum NetworkMode{Loopback,InternalApi}
impl NetworkMode{
    fn parse(raw:&str)->Result<Self,String>{
        match raw{"loopback"=>Ok(Self::Loopback),"internal-api"=>Ok(Self::InternalApi),
            _=>Err("invalid candidate network mode".into())}
    }
    fn bind(self)->&'static str{
        match self{Self::Loopback=>"127.0.0.1:3002",Self::InternalApi=>"0.0.0.0:3002"}
    }
    fn allowed_hosts(self)->Vec<String>{
        match self{Self::Loopback=>vec![],Self::InternalApi=>vec!["api".into()]}
    }
    fn base(self,headers:&HeaderMap)->&'static str{
        let host=headers.get("host").and_then(|v|v.to_str().ok()).unwrap_or("");
        if self==Self::InternalApi&&super::host_without_port(host).eq_ignore_ascii_case("api"){
            "http://api:3002"
        }else{"http://127.0.0.1:3002"}
    }
}

#[derive(Clone,Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct StaticRequest {
    url: String,
    #[serde(default="default_static_formats")]
    formats: Vec<Value>,
    #[serde(default="default_main")]
    only_main_content: bool,
    #[serde(default)]
    include_tags:Vec<String>,
    #[serde(default)]
    exclude_tags:Vec<String>,
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

const CACHE_MAX_AGE_MS:u64=7*24*60*60*1000;
const CACHE_MAX_BYTES:usize=8*1024*1024;
const CACHE_MAX_ENTRY:usize=256*1024;
const CACHE_MAX_ENTRIES:usize=64;

// Stop encoding at the entry cap instead of allocating a full oversized
// result first. The cache must not add a second unbounded copy of raw HTML.
struct CacheBuffer{bytes:Vec<u8>,limit:usize}
impl std::io::Write for CacheBuffer{
    fn write(&mut self,part:&[u8])->std::io::Result<usize>{
        if part.len()>self.limit-self.bytes.len(){return Err(std::io::Error::other("cache entry exceeds bound"));}
        self.bytes.extend_from_slice(part);
        Ok(part.len())
    }
    fn flush(&mut self)->std::io::Result<()>{Ok(())}
}
struct CacheEntry{key:String,created:Instant,encoded:Vec<u8>}
#[derive(Default)]
struct HtmlCache{entries:VecDeque<CacheEntry>,bytes:usize}

impl HtmlCache{
    fn purge(&mut self,now:Instant){
        self.entries.retain(|e|now.saturating_duration_since(e.created).as_millis()<=u128::from(CACHE_MAX_AGE_MS));
        self.bytes=self.entries.iter().map(|e|e.key.len()+e.encoded.len()).sum();
    }

    fn get(&mut self,key:&str,max_age:u64,now:Instant)->Option<(Value,u64)>{
        if max_age==0{return None;}
        self.purge(now);
        let index=self.entries.iter().position(|e|e.key==key)?;
        let age=now.saturating_duration_since(self.entries[index].created).as_millis();
        if age>u128::from(max_age){return None;}
        let entry=self.entries.remove(index)?;
        let doc=serde_json::from_slice(&entry.encoded).ok();
        if doc.is_some(){self.entries.push_back(entry);}
        else{self.bytes-=entry.key.len()+entry.encoded.len();}
        doc.map(|value|(value,age as u64))
    }

    fn put(&mut self,key:String,doc:&Value,now:Instant)->bool{
        if !doc.is_object()||key.len()>=CACHE_MAX_ENTRY{return false;}
        let mut buffer=CacheBuffer{bytes:Vec::new(),limit:CACHE_MAX_ENTRY-key.len()};
        if serde_json::to_writer(&mut buffer,doc).is_err(){return false;}
        let encoded=buffer.bytes;
        self.purge(now);
        if let Some(index)=self.entries.iter().position(|e|e.key==key){
            let old=self.entries.remove(index).unwrap();
            self.bytes-=old.key.len()+old.encoded.len();
        }
        let size=key.len()+encoded.len();
        while self.entries.len()>=CACHE_MAX_ENTRIES||self.bytes+size>CACHE_MAX_BYTES{
            let Some(old)=self.entries.pop_front()else{return false;};
            self.bytes-=old.key.len()+old.encoded.len();
        }
        self.bytes+=size;
        self.entries.push_back(CacheEntry{key,created:now,encoded});
        true
    }
}

// Exact public URL and content-affecting options, no user credentials,
// request origin, arbitrary headers, or persistent storage.
fn cache_key(target:&url::Url,req:&StaticRequest)->String{
    json!([target.as_str(),req.formats,req.only_main_content,req.include_tags,req.exclude_tags]).to_string()
}
fn cache_receipt(used:bool,stored:bool,age:Option<u64>,requested:u64)->Value{
    json!({"cache_supported":true,"cache_used":used,"cache_stored":stored,
        "cache_age_ms":age,"cache_max_age_ms":requested,"cache_persistent":false})
}
fn valid_metadata(req:&StaticRequest)->bool{
        render::valid_tag_filters(&req.include_tags,&req.exclude_tags)&&
        req.origin.as_ref().is_none_or(|s|!s.is_empty()&&s.len()<=256&&!s.chars().any(char::is_control))
}

fn valid_common(req:&StaticRequest)->bool{
    req.max_age==0&&!req.store_in_cache&&valid_metadata(req)
}

fn valid_static(req:&StaticRequest) -> bool {
    req.max_age<=CACHE_MAX_AGE_MS&&valid_metadata(req)&&req.wait_for==0&&req.parsers.is_none()&&!req.formats.is_empty()&&req.formats.len()<=4&&
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JsonFormat{
    #[serde(rename="type")] kind:String,
    prompt:String,
    schema:Value,
}

fn model_format(req:&StaticRequest)->Option<(model::Mode,Option<String>,Option<Value>)>{
    if !valid_common(req)||req.wait_for!=0||req.parsers.is_some()||req.formats.len()!=1||!(1..=180000).contains(&req.timeout.unwrap_or(180000)){return None;}
    let f=&req.formats[0];
    if f=="summary"{return Some((model::Mode::Summary,None,None));}
    if f.get("type").and_then(Value::as_str)==Some("json"){
        let j:JsonFormat=serde_json::from_value(f.clone()).ok()?;
        if j.kind!="json"||j.prompt.trim().is_empty()||j.prompt.len()>2048||!model::valid_schema(&j.schema){return None;}
        return Some((model::Mode::Json,Some(j.prompt),Some(j.schema)));
    }
    let q:QueryFormat=serde_json::from_value(f.clone()).ok()?;
    if q.kind!="query"||q.prompt.trim().is_empty()||q.prompt.len()>2048{return None;}
    let mode=match q.mode.as_str(){"freeform"=>model::Mode::Query,"directQuote"=>model::Mode::DirectQuote,_=>return None};
    Some((mode,Some(q.prompt),None))
}

async fn add_model_result(data:&mut Value,mode:model::Mode,prompt:Option<String>,schema:Option<Value>,endpoint:&str,
    model_name:&str,budget:Duration)->Result<(),StatusCode>{
    let markdown=data["markdown"].as_str().ok_or(StatusCode::BAD_GATEWAY)?.to_owned();
    let key=match &mode{model::Mode::Summary=>"summary",model::Mode::Json=>"json",_=>"answer"};
    let direct=matches!(mode,model::Mode::DirectQuote);
    let structured=matches!(mode,model::Mode::Json);
    let result=model::extract_at(endpoint,model::Request{mode,markdown,prompt,schema},
        model_name,budget).await.map_err(|_|StatusCode::BAD_GATEWAY)?;
    data[key]=if structured{
        if result["research"]["schema_validated"]!=true{return Err(StatusCode::BAD_GATEWAY);}
        result.get("candidate").ok_or(StatusCode::BAD_GATEWAY)?.clone()
    }else{json!(result["candidate"].as_str().ok_or(StatusCode::BAD_GATEWAY)?)};
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
        req.include_tags.is_empty()&&req.exclude_tags.is_empty()&&
        req.parsers.as_ref().is_none_or(|p|p.as_slice()==["pdf"])&&
        (1..=110000).contains(&req.timeout.unwrap_or(110000))
}
fn pdf_request(req:&StaticRequest,target:&url::Url)->bool{
    valid_pdf_options(req)&&(target.path().to_ascii_lowercase().ends_with(".pdf")||req.parsers.as_ref().is_some_and(|p|p.as_slice()==["pdf"]))
}

// Single-page HTML accepts public HTTP(S), including query strings and
// fragments. Crawl/map keep their narrower URL contract until separately tested.
fn scrape_target(raw:&str)->Result<url::Url,StatusCode>{
    let target=render::public_url(raw).map_err(|_|StatusCode::FORBIDDEN)?;
    if target.port().is_some(){return Err(StatusCode::BAD_REQUEST);}
    Ok(target)
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
    let target=scrape_target(&req.url);
    let target=match target{Ok(u)=>u,Err(status)=>return refusal(status,"DESTINATION_DENIED")};
    let is_pdf=pdf_request(&req,&target);
    if target.path().to_ascii_lowercase().ends_with(".pdf")&&(req.max_age>0||req.store_in_cache){
        return refusal(StatusCode::BAD_REQUEST,"PDF_CACHE_UNSUPPORTED");
    }
    if !valid_static(&req)&&model_req.is_none()&&render_formats.is_none()&&!is_pdf{
        return refusal(StatusCode::BAD_REQUEST,"UNSUPPORTED_OPTIONS");
    }
    if is_pdf{
        if let Err(status)=pdf::target(target.as_str()){return refusal(status,"UNSUPPORTED_PDF_URL");}
        if !state.pdf_enabled{return refusal(StatusCode::SERVICE_UNAVAILABLE,"PDF_WORKER_DISABLED");}
        let Ok(slot)=state.slots.clone().try_acquire_owned()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY")};
        let budget=Duration::from_millis(req.timeout.unwrap_or(110000));
        let result=tokio::spawn(async move{
            let _slot=slot;
            let (landed,receipt)=pdf::document(&state,&target,budget).await?;
            pdf::v2_document(&target,&landed,receipt).map_err(|s|(s,"PDF_INCOMPLETE"))
        }).await;
        return match result{
            Ok(Ok(data))=>bounded(json!({"success":true,"data":data,"research":{"cache_used":false,"cache_supported":false}})),
            Ok(Err((status,code)))=>refusal(status,code),
            Err(_)=>refusal(StatusCode::BAD_GATEWAY,"PDF_TASK_FAILED")
        };
    }
    if let Some(formats)=render_formats{
        if !state.browser_enabled{return refusal(StatusCode::SERVICE_UNAVAILABLE,"BROWSER_DISABLED");}
        let Ok(slot)=state.slots.clone().try_acquire_owned()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY")};
        let request=render::Request{url:target.to_string(),wait_for:req.wait_for,only_main_content:req.only_main_content,
            include_tags:req.include_tags,exclude_tags:req.exclude_tags};
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
    let Ok(slot)=state.slots.clone().try_acquire_owned() else {
        return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY");
    };
    // Own admission until blocking conversion finishes, even if the HTTP
    // caller disconnects. Model and automatic browser dispatch share it.
    let result=tokio::spawn(async move{
    let _slot=slot;
    if let Some((mode,prompt,schema))=model_req{
        let Some(name)=state.model_name.as_deref()else{return refusal(StatusCode::SERVICE_UNAVAILABLE,"MODEL_DISABLED")};
        let Ok(_model_slot)=state.model_slot.try_acquire()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"MODEL_BUSY")};
        return match model_html_request(&state,target,req,(mode,prompt,schema),name).await{
            Ok(data)=>bounded(json!({"success":true,"data":data,"research":{"cache_used":false,"cache_supported":false}})),
            Err(status)=>refusal(status,"MODEL_EXTRACTION_FAILED")
        };
    }
    match static_html_request(&state,target,req).await {
        Ok(mut data)=>{
            let receipt=data.as_object_mut().and_then(|m|m.remove("researchCache"))
                .unwrap_or_else(||cache_receipt(false,false,None,0));
            bounded(json!({"success":true,"data":data,"research":receipt}))
        },
        Err(status)=>refusal(status,"STATIC_SCRAPE_FAILED")
    }
    }).await;
    match result{
        Ok(response)=>response,
        Err(_)=>refusal(StatusCode::BAD_GATEWAY,"SCRAPE_TASK_FAILED")
    }
}

// Shared model route: caller holds global admission and the one model slot.
// The search caller clamps the requested deadline to its own remaining budget.
async fn model_html_request(state:&CandidateState,target:url::Url,req:StaticRequest,
    (mode,prompt,schema):(model::Mode,Option<String>,Option<Value>),name:&str)->Result<Value,StatusCode>{
    let deadline=tokio::time::Instant::now()+Duration::from_millis(req.timeout.unwrap_or(180000));
    let mut data=static_document_with_options(state,target,ScrapeOptions{
        timeout:Some(req.timeout.unwrap_or(180000).min(45000)),only_main_content:req.only_main_content,
        include_tags:req.include_tags,exclude_tags:req.exclude_tags,..Default::default()
    }).await?;
    let remaining=deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero(){return Err(StatusCode::GATEWAY_TIMEOUT);}
    add_model_result(&mut data,mode,prompt,schema,model::ENDPOINT,name,remaining).await?;
    Ok(data)
}

// Shared single-page HTML path for direct scrape and search enrichment.
// Admission is owned by the caller for the whole operation.
async fn static_html_request(state:&CandidateState,target:url::Url,req:StaticRequest)->Result<Value,StatusCode>{
    let max_age=req.max_age;
    let store=req.store_in_cache;
    let key=(max_age>0||store).then(||cache_key(&target,&req));
    if max_age>0{
        if let (Some(key),Ok(mut cache))=(key.as_ref(),state.html_cache.lock()){
            if let Some((mut data,age))=cache.get(key,max_age,Instant::now()){
                data["researchCache"]=cache_receipt(true,false,Some(age),max_age);
                return Ok(data);
            }
        }
    }
    let mut formats=req.formats.clone();
    let wants_markdown=req.formats.iter().any(|s|s=="markdown");
    if !wants_markdown{formats.push(json!("markdown"));}
    let opts=ScrapeOptions{formats,only_main_content:req.only_main_content,
        include_tags:req.include_tags,exclude_tags:req.exclude_tags,
        timeout:Some(req.timeout.unwrap_or(30000)),..Default::default()};
    let mut data=static_document_with_options(state,target,opts).await?;
    if !wants_markdown{data.as_object_mut().ok_or(StatusCode::BAD_GATEWAY)?.remove("markdown");}
    let stored=if store{
        if let (Some(key),Ok(mut cache))=(key,state.html_cache.lock()){cache.put(key,&data,Instant::now())}else{false}
    }else{false};
    data["researchCache"]=cache_receipt(false,stored,None,max_age);
    Ok(data)
}

async fn static_document(state:&CandidateState,target:url::Url,timeout:u64)->Result<Value,StatusCode>{
    static_document_with_options(state,target,ScrapeOptions{timeout:Some(timeout),..Default::default()}).await
}

async fn static_document_with_options(state:&CandidateState,target:url::Url,opts:ScrapeOptions)->Result<Value,StatusCode>{
    let budget=Duration::from_millis(opts.timeout.unwrap_or(30000));
    let deadline=tokio::time::Instant::now()+budget;
    let direct=tokio::time::timeout(budget,async {
        render::check_dns(&target).await.map_err(|_|StatusCode::FORBIDDEN)?;
        let mut data=cloud::scrape_with_fallback(&state.auth.engine,&state.auth.cloud,target.as_str(),&opts).await
            .map_err(|e|match e.code {
                "BLOCKED_TARGET"=>StatusCode::FORBIDDEN,
                "DOCUMENT_ISOLATION_REQUIRED"=>StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "BROWSER_RENDER_REQUIRED"=>StatusCode::PRECONDITION_REQUIRED,
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
    }).await.map_err(|_|StatusCode::GATEWAY_TIMEOUT)?;
    if !matches!(direct,Err(StatusCode::PRECONDITION_REQUIRED)){return direct;}
    if !state.browser_enabled{return Err(StatusCode::SERVICE_UNAVAILABLE);}
    let remaining=deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero(){return Err(StatusCode::GATEWAY_TIMEOUT);}
    let (formats,unsupported)=opts.resolved_formats();
    if !unsupported.is_empty(){return Err(StatusCode::BAD_REQUEST);}
    let mut result=render::render_auto_at(&state.render_endpoint,render::Request{
        url:target.to_string(),wait_for:0,only_main_content:opts.only_main_content,
        include_tags:opts.include_tags,exclude_tags:opts.exclude_tags
    },remaining,formats).await.map_err(|e|if e=="destination denied"{
        StatusCode::FORBIDDEN
    }else{StatusCode::BAD_GATEWAY})?;
    let receipt=result["research"].clone();
    let mut data=result["data"].take();
    if !data.is_object(){return Err(StatusCode::BAD_GATEWAY);}
    data["researchBrowser"]=receipt;
    data["researchBrowser"]["automatic"]=json!(true);
    Ok(data)
}

const MAX_ENRICHED_HITS:usize=5;
const MAX_ENRICHED_DOC:usize=256*1024;

// Do not let nested options select a destination or a different backend.
// Deserialize against the existing strict scrape contract, excluding models,
// PDF parsers and explicit browser waits in this first enrichment subset.
fn search_options(raw:Value)->Result<(search::Request,Option<StaticRequest>),StatusCode>{
    let mut object=raw.as_object().cloned().ok_or(StatusCode::BAD_REQUEST)?;
    let options=object.remove("scrapeOptions");
    let req:search::Request=serde_json::from_value(Value::Object(object)).map_err(|_|StatusCode::BAD_REQUEST)?;
    req.validate().map_err(|_|StatusCode::BAD_REQUEST)?;
    let options=if let Some(value)=options{
        let mut fields=value.as_object().cloned().ok_or(StatusCode::BAD_REQUEST)?;
        if fields.contains_key("url")||fields.contains_key("origin")||req.limit()>MAX_ENRICHED_HITS{
            return Err(StatusCode::BAD_REQUEST);
        }
        // Placeholder only supplies the required serde field. Actual fetch
        // destination is independently derived and guarded for each hit.
        fields.insert("url".into(),json!(""));
        let opts:StaticRequest=serde_json::from_value(Value::Object(fields)).map_err(|_|StatusCode::BAD_REQUEST)?;
        if !valid_common(&opts)||(!valid_static(&opts)&&model_format(&opts).is_none()){return Err(StatusCode::BAD_REQUEST);}
        Some(opts)
    }else{None};
    Ok((req,options))
}

async fn enrich_search<F,Fut>(value:&mut Value,opts:StaticRequest,deadline:tokio::time::Instant,mut read:F)
    ->Result<(),StatusCode>
where F:FnMut(url::Url,StaticRequest)->Fut,
    Fut:std::future::Future<Output=Result<Value,StatusCode>>{
    let hits=value["data"]["web"].as_array_mut().ok_or(StatusCode::BAD_GATEWAY)?;
    if hits.len()>MAX_ENRICHED_HITS{return Err(StatusCode::BAD_GATEWAY);}
    let mut succeeded=0usize;
    let mut failed=0usize;
    for hit in hits.iter_mut(){
        let model_requested=model_format(&opts).is_some();
        let cap=if model_requested{180000}else{45000};
        let remaining=deadline.saturating_duration_since(tokio::time::Instant::now()).as_millis().min(cap) as u64;
        let target=hit["url"].as_str().ok_or(StatusCode::BAD_GATEWAY).and_then(scrape_target);
        let result=match target{
            Ok(_) if remaining==0=>Err(StatusCode::GATEWAY_TIMEOUT),
            Ok(target)=>{
                let mut options=opts.clone();
                options.timeout=Some(options.timeout.unwrap_or(if model_requested{180000}else{30000}).min(remaining));
                // The inner reader bounds network work and retains parser
                // admission until completion. Do not cancel a blocking parser.
                read(target,options).await
            }
            Err(status)=>Err(status)
        };
        let result=result.and_then(|data|{
            if !data.is_object()||serde_json::to_vec(&data).map_err(|_|StatusCode::BAD_GATEWAY)?.len()>MAX_ENRICHED_DOC{
                return Err(StatusCode::PAYLOAD_TOO_LARGE);
            }
            Ok(data)
        });
        let object=hit.as_object_mut().ok_or(StatusCode::BAD_GATEWAY)?;
        match result{
            Ok(doc)=>{
                // Preserve original search URL, rank, title and snippet.
                // Only requested document fields and source evidence may be merged.
                for key in ["markdown","html","rawHtml","links","metadata","researchBrowser","researchSourceVerified",
                    "answer","summary","json","researchModel","warning"]{
                    if let Some(field)=doc.get(key){object.insert(key.into(),field.clone());}
                }
                object.insert("researchScrape".into(),json!({"status":"completed","source_verified":false}));
                succeeded+=1;
            }
            Err(status)=>{
                object.insert("researchScrape".into(),json!({"status":"failed","http_status":status.as_u16()}));
                failed+=1;
            }
        }
    }
    value["research"]["scrape"]=json!({"requested":succeeded+failed,"completed":succeeded,
        "failed":failed,"complete":failed==0,"cache_used":false,"source_verified":false});
    if failed>0{
        let prefix=value["warning"].as_str().unwrap_or("");
        value["warning"]=json!(format!("{prefix}{}One or more search hits could not be scraped; search snippets are not page content.",if prefix.is_empty(){""}else{" "}));
    }
    Ok(())
}

async fn search_route(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response {
    let raw:Value=match decode(&headers,&body){Ok(v)=>v,Err(r)=>return r};
    let (req,options)=match search_options(raw){Ok(v)=>v,Err(status)=>return refusal(status,"UNSUPPORTED_OPTIONS")};
    if options.as_ref().is_some_and(|opts|model_format(opts).is_some())&&state.model_name.is_none(){
        return refusal(StatusCode::SERVICE_UNAVAILABLE,"MODEL_DISABLED");
    }
    let Ok(slot)=state.slots.clone().try_acquire_owned() else {return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY");};
    let result=tokio::spawn(async move{
        let _slot=slot;
        let deadline=tokio::time::Instant::now()+Duration::from_millis(req.timeout_ms());
        let mut value=search::search_at(&state.search_endpoint,req).await.map_err(|_|StatusCode::BAD_GATEWAY)?;
        if let Some(opts)=options{
            let reader=&state;
            enrich_search(&mut value,opts,deadline,|target,opts|async move{
                if let Some(model)=model_format(&opts){
                    let name=reader.model_name.as_deref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
                    let _model_slot=reader.model_slot.try_acquire().map_err(|_|StatusCode::TOO_MANY_REQUESTS)?;
                    model_html_request(reader,target,opts,model,name).await
                }else{static_html_request(reader,target,opts).await}
            }).await?;
        }
        Ok::<Value,StatusCode>(value)
    }).await;
    match result{
        Ok(Ok(value))=>bounded(value),
        Ok(Err(status))=>refusal(status,"SEARCH_FAILED"),
        Err(_)=>refusal(StatusCode::BAD_GATEWAY,"SEARCH_TASK_FAILED")
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

#[cfg(test)]
fn router(token:String, search_endpoint:String, render_endpoint:String,batch:Option<batch::BatchState>,pdf_enabled:bool,model_name:Option<String>,browser_enabled:bool)->Result<(Router,CandidateState),String> {
    router_with_network(token,search_endpoint,render_endpoint,batch,pdf_enabled,model_name,browser_enabled,NetworkMode::Loopback)
}

fn router_with_network(token:String, search_endpoint:String, render_endpoint:String,batch:Option<batch::BatchState>,pdf_enabled:bool,model_name:Option<String>,browser_enabled:bool,network:NetworkMode)->Result<(Router,CandidateState),String> {
    if !(32..=256).contains(&token.len()) || token.chars().any(|c|c.is_control()||c.is_whitespace()) {
        return Err("invalid candidate token".into());
    }
    if model_name.as_ref().is_some_and(|s|s.trim().is_empty()||s.len()>128||s.chars().any(char::is_control)){return Err("invalid operator model name".into());}
    let engine=Arc::new(Engine::new(EngineConfig{policy:NetworkPolicy::PublicOnly,
        user_agent:None,browser:BrowserMode::Never})?.require_html_only());
    // Even an accidental cloud invocation cannot contact a provider.
    let auth=Arc::new(AppState {
        engine,cloud:Cloud::new("http://127.0.0.1:9".into(),None,CloudMode::Off),
        token:Some(token),use_client_key:false,allowed_hosts:network.allowed_hosts(),
        jobs:Mutex::new(HashMap::new())
    });
    let state=CandidateState{auth:auth.clone(),slots:Arc::new(Semaphore::new(4)),
        search_endpoint,render_endpoint,batch,crawler:crawl::Crawler::new()?,pdf_enabled,model_name,model_slot:Arc::new(Semaphore::new(1)),browser_enabled,
        html_cache:Arc::new(Mutex::new(HtmlCache::default())),network};
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

/// Research binary defaults to loopback. The explicit internal-api mode is
/// intended only for a private Docker network with no published public port.
/// Secret file and bind mode are operator configuration.
pub async fn run_candidate()->Result<(),String> {
    let path=std::env::var("CANDIDATE_TOKEN_FILE").unwrap_or_else(|_|"/run/secrets/firecrawl_candidate_token".into());
    let token=std::fs::read_to_string(path).map_err(|_|"candidate token file unavailable".to_string())?;
    let batch=std::env::var_os("CANDIDATE_JOB_DB_FILE").map(|path|batch::BatchState::open(std::path::Path::new(&path))).transpose()?;
    let pdf_enabled=std::env::var("CANDIDATE_PDF_ENABLED").as_deref()==Ok("1");
    let model_name=std::env::var("CANDIDATE_MODEL_NAME").ok();
    let browser_enabled=std::env::var("CANDIDATE_BROWSER_ENABLED").as_deref()==Ok("1");
    let network=NetworkMode::parse(&std::env::var("CANDIDATE_NETWORK_MODE").unwrap_or_else(|_|"loopback".into()))?;
    let (app,state)=router_with_network(token.trim().to_string(),SEARCH.into(),RENDER.into(),batch,pdf_enabled,model_name,browser_enabled,network)?;
    let listener=tokio::net::TcpListener::bind(network.bind()).await.map_err(|e|e.to_string())?;
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
            json!({"maxAge":CACHE_MAX_AGE_MS+1}),json!({"timeout":45001}),
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
    async fn internal_api_mode_uses_fixed_host_and_required_token(){
        assert!(NetworkMode::parse("anything").is_err());
        assert_eq!(NetworkMode::Loopback.bind(),"127.0.0.1:3002");
        assert_eq!(NetworkMode::InternalApi.bind(),"0.0.0.0:3002");
        let mut headers=HeaderMap::new();
        headers.insert("host","api:3002".parse().unwrap());
        headers.insert("x-forwarded-host","attacker.example".parse().unwrap());
        assert_eq!(NetworkMode::InternalApi.base(&headers),"http://api:3002");
        assert_eq!(NetworkMode::Loopback.base(&headers),"http://127.0.0.1:3002");
        let (app,_)=router_with_network(TOKEN.into(),"http://127.0.0.1:9/search".into(),
            "http://127.0.0.1:9/scrape".into(),None,false,None,false,NetworkMode::InternalApi).unwrap();
        let (base,task)=spawn(app).await;
        let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap();
        for (host,token,status) in [("api:3002",TOKEN,200),("api:3002","wrong",401),("evil.example",TOKEN,403),
            ("127.0.0.1:3002",TOKEN,200)]{
            assert_eq!(client.get(format!("{base}/health")).header("Host",host).bearer_auth(token)
                .send().await.unwrap().status().as_u16(),status);
        }
        task.abort();
    }

    #[test]
    fn explicit_cache_options_and_bounded_lru(){
        let req:StaticRequest=serde_json::from_value(json!({"url":"https://example.org/a","maxAge":60000,"storeInCache":true})).unwrap();
        assert!(valid_static(&req)&&!valid_common(&req));
        let target=scrape_target(&req.url).unwrap();
        let key=cache_key(&target,&req);
        let mut different=req.clone();
        different.only_main_content=false;
        assert_ne!(key,cache_key(&target,&different));
        different=req.clone();different.formats=vec![json!("html")];
        assert_ne!(key,cache_key(&target,&different));
        assert_ne!(key,cache_key(&scrape_target("https://example.org/a?q=1").unwrap(),&req));
        different=req.clone();different.max_age=1;different.store_in_cache=false;
        assert_eq!(key,cache_key(&target,&different));
        let now=Instant::now();
        let doc=json!({"markdown":"Stock 421.5","researchSourceVerified":false});
        let mut cache=HtmlCache::default();
        assert!(cache.put(key.clone(),&doc,now));
        assert!(cache.get(&key,0,now).is_none());
        assert!(cache.get(&key,1,now+Duration::from_millis(2)).is_none());
        assert_eq!(cache.get(&key,60000,now+Duration::from_secs(1)),Some((doc.clone(),1000)));
        assert!(!cache.put("huge".into(),&json!({"markdown":"x".repeat(CACHE_MAX_ENTRY)}),now));
        for i in 0..=CACHE_MAX_ENTRIES{assert!(cache.put(format!("k{i}"),&doc,now));}
        assert!(cache.entries.len()<=CACHE_MAX_ENTRIES&&cache.bytes<=CACHE_MAX_BYTES);
        assert!(cache.get(&key,60000,now).is_none());
        cache.purge(now+Duration::from_millis(CACHE_MAX_AGE_MS+1));
        assert_eq!(cache.bytes,0);
    }

    #[tokio::test]
    async fn actual_route_can_read_explicit_cache_without_network(){
        let (app,state)=router(TOKEN.into(),"http://127.0.0.1:9/search".into(),"http://127.0.0.1:9/scrape".into(),
            None,false,None,false).unwrap();
        let payload=json!({"url":"https://example.org/cache","maxAge":60000,"formats":["markdown"]});
        let req:StaticRequest=serde_json::from_value(payload.clone()).unwrap();
        let key=cache_key(&scrape_target(&req.url).unwrap(),&req);
        let doc=json!({"markdown":"Synthetic cached document 42","researchSourceVerified":false});
        assert!(state.html_cache.lock().unwrap().put(key,&doc,Instant::now()));
        let (base,task)=spawn(app).await;
        let client=reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap();
        let result:Value=client.post(format!("{base}/v2/scrape")).bearer_auth(TOKEN).json(&payload)
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(result["data"],doc);
        assert_eq!(result["research"]["cache_used"],true);
        assert_eq!(result["research"]["cache_stored"],false);
        assert_eq!(result["research"]["cache_persistent"],false);
        task.abort();
    }

    #[test]
    fn single_page_public_query_and_bounded_selector_contract(){
        for url in ["http://example.org/article","https://example.org/report?year=2026#data"]{
            assert!(scrape_target(url).is_ok());
            assert!(static_target(url).is_err());
        }
        for url in ["http://127.0.0.1/?q=1","https://user:pass@example.org/","https://[64:ff9b::5db8:d822]/"]{
            assert!(scrape_target(url).is_err());
        }
        assert!(scrape_target("https://example.org:8443/").is_err());
        let req:StaticRequest=serde_json::from_value(json!({"url":"https://example.org/report?year=2026",
            "includeTags":["article","div.report","#main"],"excludeTags":[".ads"]})).unwrap();
        assert!(valid_static(&req));
        assert!(!valid_pdf_options(&req));
        let bad:StaticRequest=serde_json::from_value(json!({"url":"https://example.org/",
            "includeTags":["article:has(div)"]})).unwrap();
        assert!(!valid_static(&bad));
    }

    #[test]
    fn nested_search_scrape_uses_strict_options(){
        let (req,opts)=search_options(json!({"query":"test","limit":2,"scrapeOptions":{
            "formats":["html","links"],"onlyMainContent":false,"includeTags":["article"]}})).unwrap();
        assert_eq!(req.limit(),2);
        assert_eq!(opts.unwrap().formats,vec![json!("html"),json!("links")]);
        for input in [
            json!({"query":"test","limit":6,"scrapeOptions":{}}),
            json!({"query":"test","limit":1,"scrapeOptions":{"url":"https://example.org/"}}),
            json!({"query":"test","limit":1,"scrapeOptions":{"formats":["summary"]}}),
            json!({"query":"test","limit":1,"scrapeOptions":{"maxAge":1}}),
            json!({"query":"test","limit":1,"scrapeOptions":{"waitFor":1}}),
            json!({"query":"test","limit":1,"scrapeOptions":null})
        ]{assert!(search_options(input).is_err());}
    }

    #[tokio::test]
    async fn search_enrichment_keeps_rank_and_explicit_failure_receipts(){
        let (_,opts)=search_options(json!({"query":"test","limit":3,"scrapeOptions":{"formats":["markdown"]}})).unwrap();
        let mut value=json!({"success":true,"data":{"web":[
            {"url":"https://example.org/a","title":"A","position":1},
            {"url":"https://example.org/b","title":"B","position":2},
            {"url":"https://127.0.0.1/","title":"C","position":3}
        ]},"research":{},"warning":"Web results only."});
        let deadline=tokio::time::Instant::now()+Duration::from_secs(5);
        enrich_search(&mut value,opts.unwrap(),deadline,|target,options|async move{
            assert!(options.timeout.unwrap()<=5000);
            if target.path()=="/b"{return Err(StatusCode::BAD_GATEWAY);}
            assert_eq!(target.path(),"/a");
            Ok(json!({"url":"unexpected","position":99,"title":"other",
                "markdown":"Stock 421.5","metadata":{"url":target.as_str()}}))
        }).await.unwrap();
        assert_eq!(value["data"]["web"][0]["markdown"],"Stock 421.5");
        assert_eq!(value["data"]["web"][0]["position"],1);
        assert_eq!(value["data"]["web"][0]["title"],"A");
        assert_eq!(value["data"]["web"][1]["researchScrape"]["http_status"],502);
        assert_eq!(value["data"]["web"][2]["researchScrape"]["http_status"],403);
        assert!(value["data"]["web"][1].get("markdown").is_none());
        assert_eq!(value["research"]["scrape"]["completed"],1);
        assert_eq!(value["research"]["scrape"]["failed"],2);
        assert_eq!(value["research"]["scrape"]["complete"],false);
        assert!(value["warning"].as_str().unwrap().starts_with("Web results only."));
    }

    #[tokio::test]
    async fn search_enrichment_fails_closed_on_deadline_and_size(){
        let (_,opts)=search_options(json!({"query":"test","limit":1,"scrapeOptions":{}})).unwrap();
        let opts=opts.unwrap();
        let fixture=json!({"success":true,"data":{"web":[{"url":"https://example.org/a"}]},"research":{}});
        let mut expired=fixture.clone();
        enrich_search(&mut expired,opts.clone(),tokio::time::Instant::now(),|_,_|async{
            panic!("no network work after deadline");
            #[allow(unreachable_code)]
            Ok(json!({}))
        }).await.unwrap();
        assert_eq!(expired["data"]["web"][0]["researchScrape"]["http_status"],504);
        let mut oversized=fixture;
        enrich_search(&mut oversized,opts,tokio::time::Instant::now()+Duration::from_secs(3),|_,_|async{
            Ok(json!({"markdown":"x".repeat(MAX_ENRICHED_DOC)}))
        }).await.unwrap();
        assert_eq!(oversized["data"]["web"][0]["researchScrape"]["http_status"],413);
        assert!(oversized["data"]["web"][0].get("markdown").is_none());
    }

    #[tokio::test]
    async fn search_model_enrichment_preserves_evidence_and_partial_failures(){
        let raw=json!({"query":"stock","limit":2,"scrapeOptions":{"formats":[{"type":"query","prompt":"Stock?","mode":"freeform"}]}});
        let (_,opts)=search_options(raw.clone()).unwrap();
        let mut too_many=raw;too_many["limit"]=json!(6);
        assert!(search_options(too_many).is_err());
        let mut value=json!({"data":{"web":[{"url":"https://example.org/a","title":"Original","description":"Snippet","position":1},
            {"url":"https://example.org/b","title":"Second","description":"Other","position":2}]},"research":{}});
        let deadline=tokio::time::Instant::now()+Duration::from_secs(3);
        enrich_search(&mut value,opts.unwrap(),deadline,|target,opts|async move{
            assert!(model_format(&opts).is_some());
            assert!(opts.timeout.unwrap()<=3000);
            if target.path()=="/b"{return Err(StatusCode::TOO_MANY_REQUESTS);}
            Ok(json!({"answer":"421.5","researchModel":{"source_verified":false},"warning":"Model output",
                "metadata":{"url":target.as_str()},"title":"Do not overwrite"}))
        }).await.unwrap();
        assert_eq!(value["data"]["web"][0]["title"],"Original");
        assert_eq!(value["data"]["web"][0]["answer"],"421.5");
        assert_eq!(value["data"]["web"][0]["researchModel"]["source_verified"],false);
        assert_eq!(value["data"]["web"][1]["researchScrape"]["http_status"],429);
        assert_eq!(value["research"]["scrape"]["complete"],false);
    }

    #[tokio::test]
    async fn captured_json_format_requires_supported_schema_and_returns_structured_value(){
        let schema=json!({"type":"object","properties":{"change":{"type":"number"}},"required":["change"],"additionalProperties":false});
        let payload=json!({"url":"https://example.org/","formats":[{"type":"json","prompt":"Extract change","schema":schema}]});
        let req:StaticRequest=serde_json::from_value(payload.clone()).unwrap();
        let (mode,prompt,validated_schema)=model_format(&req).unwrap();
        let mut bad=payload;
        bad["formats"][0]["schema"]["$ref"]=json!("https://example.org/schema");
        assert!(model_format(&serde_json::from_value(bad).unwrap()).is_none());
        let (base,task)=spawn(Router::new().route("/v1/responses",post(move|Json(body):Json<Value>|{
            let schema=schema.clone();
            async move{
                assert_eq!(body["text"]["format"]["schema"],schema);
                ([("x-firecrawl-model-contract","strict-v1"),("x-firecrawl-input-trimmed","false"),
                    ("x-firecrawl-model-provider","gemini")],
                    Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","status":"completed",
                        "content":[{"type":"output_text","text":"{\"change\":-1.2}"}]}]})))
            }
        }))).await;
        let mut data=json!({"markdown":"Change -1.2","metadata":{"url":"https://example.org/"}});
        add_model_result(&mut data,mode,prompt,validated_schema,&format!("{base}/v1/responses"),"fixture",Duration::from_secs(3)).await.unwrap();
        assert_eq!(data["json"]["change"],-1.2);
        assert_eq!(data["researchModel"]["schema_validated"],true);
        assert_eq!(data["researchModel"]["source_verified"],false);
        assert!(data.get("markdown").is_none());
        task.abort();
    }

    #[tokio::test]
    async fn captured_model_format_and_strict_quote_result(){
        let req:StaticRequest=serde_json::from_value(json!({"url":"https://example.org/",
            "formats":[{"type":"query","prompt":"Quote the value","mode":"directQuote"}],
            "origin":"mcp-candidate-contract@3.40.0"})).unwrap();
        let (mode,prompt,schema)=model_format(&req).unwrap();
        let (base,task)=spawn(Router::new().route("/v1/responses",post(|Json(body):Json<Value>|async move{
            assert_eq!(body["input"][0]["content"][0]["text"],"0: Header\n1: Stock 421.5");
            ([("x-firecrawl-model-contract","strict-v1"),("x-firecrawl-input-trimmed","false"),
                ("x-firecrawl-model-provider","gemini")],
                Json(json!({"status":"completed","output":[{"type":"message","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":"[1]"}]}]})))
        }))).await;
        let mut data=json!({"markdown":"Header\nStock 421.5","metadata":{"url":"https://example.org/"}});
        add_model_result(&mut data,mode,prompt,schema,&format!("{base}/v1/responses"),"fixture",Duration::from_secs(3)).await.unwrap();
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
            .route("/tags",get(||async{axum::response::Html("<html><body><article><p>Included fixture 42</p><p class=\"ads\">Omit this ad</p></article><aside>Omit sidebar</aside></body></html>")}))
            .route("/pdf",get(||async{([("content-type","application/pdf")],"%PDF-invalid-fixture")}))
            .route("/mislabelled",get(||async{([("content-type","text/html")],"%PDF-invalid-fixture")}))
            .route("/redirect",get(||async{axum::response::Redirect::temporary("/pdf")}))
            .route("/shell",get(||async{axum::response::Html("<html><body><div id=\"root\">Loading...</div><script src=\"app.js\"></script></body></html>")}))
            .route("/challenge",get(||async{(StatusCode::FORBIDDEN,axum::response::Html("<html><body><p>Verify you are human</p></body></html>"))}))
            .route("/rate-limit",get(||async{(StatusCode::TOO_MANY_REQUESTS,axum::response::Html("<html><body><p>Verify you are human</p></body></html>"))}));
        let (base,task)=spawn(app).await;
        let engine=Engine::new(EngineConfig{policy:NetworkPolicy::AllowPrivate,user_agent:None,
            browser:BrowserMode::Never}).unwrap().require_html_only();
        let opts=ScrapeOptions::default();
        let doc=engine.scrape(&format!("{base}/page"),&opts).await.unwrap();
        assert!(doc["markdown"].as_str().unwrap().contains("Native HTML fixture 42."));
        let filtered=engine.scrape(&format!("{base}/tags?fixture=1"),&ScrapeOptions{
            include_tags:vec!["article".into()],exclude_tags:vec![".ads".into()],
            ..Default::default()
        }).await.unwrap();
        let md=filtered["markdown"].as_str().unwrap();
        assert!(md.contains("Included fixture 42"));
        assert!(!md.contains("Omit this ad")&&!md.contains("Omit sidebar"));
        for path in ["/shell","/challenge"]{
            let err=engine.scrape(&format!("{base}{path}"),&opts).await.unwrap_err();
            assert_eq!(err.code,"BROWSER_RENDER_REQUIRED");
        }
        match engine.scrape(&format!("{base}/rate-limit"),&opts).await{
            Ok(doc)=>assert_eq!(doc["metadata"]["statusCode"],429),
            Err(err)=>assert_ne!(err.code,"BROWSER_RENDER_REQUIRED")
        }
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
        // Exclude the ordinary result using a separate fixed mock source;
        // explicit nested scrape cannot fetch an unsupported target port.
        let (limited_backend,limited_worker)=spawn(Router::new().route("/search",get(||async{
            Json(json!({"results":[{"url":"https://93.184.216.34:8443/denied","title":"Unsupported port"}]}))
        }))).await;
        let (limited,limited_server)=spawn(router(TOKEN.into(),format!("{limited_backend}/search"),
            format!("{backend}/scrape"),None,false,None,false).unwrap().0).await;
        let enriched:Value=c.post(format!("{limited}/v2/search")).bearer_auth(TOKEN).json(&json!({
            "query":"stock","limit":1,"scrapeOptions":{"formats":["markdown"]}}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(enriched["data"]["web"][0]["researchScrape"]["http_status"],400);
        assert_eq!(enriched["research"]["scrape"]["complete"],false);
        assert!(enriched["data"]["web"][0].get("markdown").is_none());
        limited_server.abort();limited_worker.abort();
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
            "url":"https://93.184.216.34/report#chapter","formats":["html","links"],"waitFor":50
        })).send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert_eq!(rendered["success"],true);
        assert!(rendered["data"]["html"].as_str().unwrap().contains("421.5"));
        assert!(rendered["data"].get("markdown").is_none());
        assert_eq!(rendered["data"]["metadata"]["contentType"],"text/html");
        assert_eq!(rendered["data"]["metadata"]["sourceURL"],"https://93.184.216.34/report#chapter");
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
