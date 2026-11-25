//! Fixed-batch research route; uses the actual static scraper and SQLite store.
//! Deliberately not exposed as the full Firecrawl batch API.
use super::{CandidateState, bounded, decode, refusal, static_document, static_target,crawl};
use crate::serve::from_a_web_page;
use axum::{body::Bytes, extract::{Path, Query, State}, http::{HeaderMap,StatusCode},
    response::{IntoResponse,Response},Json};
use firecrawl_job_store_research::{Error as StoreError,Store,MAX_WORKERS};
use serde::Deserialize;
use serde_json::{json,Value};
use std::{collections::HashSet, sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}}};
use tokio::sync::Notify;

#[derive(Clone)]
pub(super) struct BatchState {
    store:Arc<Mutex<Store>>,
    notify:Arc<Notify>,
    ready:Arc<AtomicBool>,
}

impl BatchState {
    pub(super) fn open(path:&std::path::Path)->Result<Self,String> {
        Ok(Self{store:Arc::new(Mutex::new(Store::open(path).map_err(|_|"job store unavailable")?)),
            notify:Arc::new(Notify::new()),ready:Arc::new(AtomicBool::new(true))})
    }

    pub(super) fn ready(&self)->bool {self.ready.load(Ordering::Acquire)}

    async fn call<T,F>(&self,f:F)->Result<T,StoreError>
    where T:Send+'static,F:FnOnce(&mut Store)->Result<T,StoreError>+Send+'static {
        let store=self.store.clone();
        tokio::task::spawn_blocking(move||{
            let mut guard=store.lock().map_err(|_|StoreError::Invalid("store lock unavailable"))?;
            f(&mut guard)
        }).await.map_err(|_|StoreError::Invalid("store task failed"))?
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BatchRequest { urls:Vec<String> }

fn store_refusal(error:StoreError)->Response {
    match error {
        StoreError::Invalid("job not found")=>refusal(StatusCode::NOT_FOUND,"JOB_NOT_FOUND"),
        StoreError::Invalid("job store full")=>refusal(StatusCode::TOO_MANY_REQUESTS,"JOB_STORE_FULL"),
        StoreError::Invalid("job missing or terminal")=>refusal(StatusCode::CONFLICT,"JOB_NOT_ACTIVE"),
        StoreError::Invalid("invalid cursor"|"cursor exceeds committed results")=>refusal(StatusCode::BAD_REQUEST,"INVALID_CURSOR"),
        _=>refusal(StatusCode::SERVICE_UNAVAILABLE,"JOB_STORE_UNAVAILABLE")
    }
}

pub(super) async fn create(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response {
    let req:BatchRequest=match decode(&headers,&body){Ok(v)=>v,Err(r)=>return r};
    let Some(batch)=state.batch else{return refusal(StatusCode::SERVICE_UNAVAILABLE,"JOB_STORE_DISABLED")};
    if !batch.ready(){return refusal(StatusCode::SERVICE_UNAVAILABLE,"WORKER_NOT_READY");}
    if req.urls.is_empty()||req.urls.len()>100{return refusal(StatusCode::BAD_REQUEST,"INVALID_BATCH");}
    let mut seen=HashSet::new();
    let mut urls=Vec::new();
    for raw in req.urls {
        let target=match static_target(&raw){Ok(v)=>v,Err(s)=>return refusal(s,"DESTINATION_DENIED")};
        let url=target.to_string();
        if !seen.insert(url.clone()){return refusal(StatusCode::BAD_REQUEST,"DUPLICATE_URL");}
        urls.push(url);
    }
    let id=uuid::Uuid::new_v4().to_string();
    let store_id=id.clone();
    match batch.call(move|store|store.create(&store_id,&urls)).await {
        Ok(())=>{
            batch.notify.notify_waiters();
            (StatusCode::ACCEPTED,Json(json!({"success":true,"id":id,"url":format!("/research/batch/{id}")}))).into_response()
        }
        Err(e)=>store_refusal(e)
    }
}

#[derive(Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct CrawlRequest { url:String, limit:usize, max_discovery_depth:u32 }

pub(super) async fn create_crawl(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response {
    let req:CrawlRequest=match decode(&headers,&body){Ok(v)=>v,Err(r)=>return r};
    let Some(batch)=state.batch else{return refusal(StatusCode::SERVICE_UNAVAILABLE,"JOB_STORE_DISABLED")};
    if !batch.ready(){return refusal(StatusCode::SERVICE_UNAVAILABLE,"WORKER_NOT_READY");}
    if !(1..=100).contains(&req.limit)||req.max_discovery_depth>10{
        return refusal(StatusCode::BAD_REQUEST,"INVALID_CRAWL_LIMITS");
    }
    let seed=match crawl::seed(&req.url){Ok(v)=>v.to_string(),Err(s)=>return refusal(s,"DESTINATION_DENIED")};
    let id=uuid::Uuid::new_v4().to_string();
    let store_id=id.clone();
    match batch.call(move|store|store.create_crawl(&store_id,&seed,req.limit,req.max_discovery_depth)).await {
        Ok(())=>{
            batch.notify.notify_waiters();
            (StatusCode::ACCEPTED,Json(json!({"success":true,"id":id,"url":format!("/research/batch/{id}")}))).into_response()
        }
        Err(e)=>store_refusal(e)
    }
}

// Bounded v2-compatible subset. Default include uses the same guarded
// sitemap discovery as map; bounded workers and same-origin scope remain explicit.
#[derive(Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct V2CrawlRequest{
    url:String,
    limit:usize,
    max_concurrency:usize,
    #[serde(default="default_sitemap")] sitemap:String,
    #[serde(default="default_depth")] max_discovery_depth:u32,
    #[serde(default)] scrape_options:Option<V2ScrapeOptions>,
    #[serde(default)] include_paths:Vec<String>,
    #[serde(default)] exclude_paths:Vec<String>,
    #[serde(default)] crawl_entire_domain:bool,
    #[serde(default)] delay:Option<f64>,
    #[serde(default)] origin:Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct V2ScrapeOptions{
    #[serde(default="super::default_formats")] formats:Vec<String>,
    #[serde(default="super::default_main")] only_main_content:bool,
    #[serde(default)] include_tags:Vec<String>,
    #[serde(default)] exclude_tags:Vec<String>,
    #[serde(default)] max_age:u64,
    #[serde(default)] store_in_cache:bool,
    // Validate the local class; crawl remains static and same-origin.
    #[serde(default,rename="proxy")] _proxy:super::LocalProxy,
}
fn default_depth()->u32{10}
fn default_sitemap()->String{"include".into()}

impl V2CrawlRequest{
    fn valid(&self)->bool{
        (1..=100).contains(&self.limit)&&(1..=4).contains(&self.max_concurrency)&&self.max_discovery_depth<=10&&
        matches!(self.sitemap.as_str(),"include"|"skip")&&self.origin.as_ref().is_none_or(|s|!s.is_empty()&&s.len()<=256&&!s.chars().any(char::is_control))&&
        self.delay.is_none_or(|d|d.is_finite()&&(0.0..=10.0).contains(&d))&&
        self.scrape_options.as_ref().is_none_or(|s|s.max_age==0&&!s.store_in_cache)
    }

    fn policy(&self)->crawl::Policy{
        crawl::Policy{include_paths:self.include_paths.clone(),exclude_paths:self.exclude_paths.clone(),
            crawl_entire_domain:self.crawl_entire_domain,
            // Firecrawl v2/MCP delay is seconds, never round a positive request down.
            delay_ms:(self.delay.unwrap_or(0.0)*1000.0).ceil() as u32,
            only_main_content:self.scrape_options.as_ref().is_none_or(|s|s.only_main_content),
            formats:self.scrape_options.as_ref().map_or_else(super::default_formats,|s|s.formats.clone()),
            include_tags:self.scrape_options.as_ref().map_or_else(Vec::new,|s|s.include_tags.clone()),
            exclude_tags:self.scrape_options.as_ref().map_or_else(Vec::new,|s|s.exclude_tags.clone())}
    }
}

pub(super) async fn create_v2_crawl(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response{
    let req:V2CrawlRequest=match decode(&headers,&body){Ok(v)=>v,Err(r)=>return r};
    if !req.valid(){return refusal(StatusCode::BAD_REQUEST,"UNSUPPORTED_CRAWL_OPTIONS");}
    let api_base=state.network.base(&headers);
    let Some(batch)=state.batch else{return refusal(StatusCode::SERVICE_UNAVAILABLE,"JOB_STORE_DISABLED")};
    if !batch.ready(){return refusal(StatusCode::SERVICE_UNAVAILABLE,"WORKER_NOT_READY");}
    let seed=match crawl::seed(&req.url){Ok(v)=>v,Err(s)=>return refusal(s,"DESTINATION_DENIED")};
    let policy=req.policy();
    if policy.scope(&seed).is_err(){return refusal(StatusCode::BAD_REQUEST,"UNSUPPORTED_CRAWL_FILTERS");}
    let policy=match serde_json::to_value(policy){Ok(v)=>v,Err(_)=>return refusal(StatusCode::BAD_REQUEST,"INVALID_CRAWL_POLICY")};
    let seed=seed.to_string();
    let id=uuid::Uuid::new_v4().to_string();
    let store_id=id.clone();
    let include_sitemap=req.sitemap=="include";
    let max_concurrency=req.max_concurrency as u32;
    match batch.call(move|store|store.create_crawl_parallel(&store_id,&seed,req.limit,req.max_discovery_depth,include_sitemap,&policy,max_concurrency)).await{
        Ok(())=>{
            batch.notify.notify_waiters();
            // Only the two fixed operator-supported bases can be selected.
            // Never concatenate caller-supplied host/forwarded values.
            Json(json!({"success":true,"id":id,"url":format!("{api_base}/v2/crawl/{id}"),
                "warning":"Bounded local same-origin crawl; no cache; site coverage unverified. Sitemap scan is bounded and seed-dependent.",
                "research":{"sitemap_mode":if include_sitemap{"include"}else{"skip"},
                    "requested_max_concurrency":max_concurrency,"worker_concurrency_cap":max_concurrency.min(WORKERS as u32),
                    "same_domain_network_concurrency":1}})).into_response()
        },
        Err(e)=>store_refusal(e)
    }
}

fn default_v2_limit()->usize{100}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct V2PageRequest{
    #[serde(default)] skip:usize,
    #[serde(default="default_v2_limit")] limit:usize,
}

fn timestamp(value:&Value)->Result<String,StatusCode>{
    value.as_i64().and_then(|s|chrono::DateTime::<chrono::Utc>::from_timestamp(s,0))
        .map(|t|t.to_rfc3339()).ok_or(StatusCode::BAD_GATEWAY)
}

fn v2_envelope(id:&str,limit:usize,page:Value,api_base:&'static str)->Result<Value,StatusCode>{
    let next=page["next_skip"].as_u64().map(|skip|
        format!("{api_base}/v2/crawl/{id}?skip={skip}&limit={limit}"));
    let mut out=json!({"success":true,"status":page["status"],"total":page["total"],"completed":page["completed"],
        "creditsUsed":0,"expiresAt":timestamp(&page["expires_at"])?,"createdAt":timestamp(&page["created_at"])?,
        "data":page["data"],"research":page["research"],
        "warning":"Bounded local job; no external credits charged; site coverage unverified. See research counts for failed pages."});
    if let Some(next)=next{out["next"]=json!(next);}
    if !page["finished_at"].is_null(){out["completedAt"]=json!(timestamp(&page["finished_at"])?);}
    Ok(out)
}

pub(super) async fn status_v2_crawl(State(state):State<CandidateState>,Path(id):Path<String>,
    Query(q):Query<V2PageRequest>,headers:HeaderMap)->Response{
    if from_a_web_page(&headers){return refusal(StatusCode::FORBIDDEN,"WEB_PAGE_DENIED");}
    if uuid::Uuid::parse_str(&id).is_err(){return refusal(StatusCode::BAD_REQUEST,"INVALID_JOB_ID");}
    let Some(batch)=state.batch else{return refusal(StatusCode::SERVICE_UNAVAILABLE,"JOB_STORE_DISABLED")};
    let store_id=id.clone();
    let limit=q.limit;
    match batch.call(move|store|store.crawl_status_v2(&store_id,q.skip,limit)).await{
        Ok(page)=>match v2_envelope(&id,limit,page,state.network.base(&headers)){Ok(v)=>bounded(v),Err(s)=>refusal(s,"INVALID_JOB_RECEIPT")},
        Err(e)=>store_refusal(e)
    }
}

#[cfg(test)]
mod v2_tests{
    use super::*;
    #[test]
    fn supports_default_include_and_explicit_skip_with_bounded_scope(){
        let base=json!({"url":"https://example.org/docs/","limit":2,"maxConcurrency":1,
            "sitemap":"skip","scrapeOptions":{"formats":["markdown"]},"origin":"mcp-candidate-contract@3.40.0"});
        assert!(serde_json::from_value::<V2CrawlRequest>(base.clone()).unwrap().valid());
        let mut unknown=base.clone();unknown["ignoreRobotsTxt"]=json!(true);
        assert!(serde_json::from_value::<V2CrawlRequest>(unknown).is_err());
        let mut sitemap=base.clone();sitemap["sitemap"]=json!("include");
        assert!(serde_json::from_value::<V2CrawlRequest>(sitemap.clone()).unwrap().valid());
        sitemap.as_object_mut().unwrap().remove("sitemap");
        let req=serde_json::from_value::<V2CrawlRequest>(sitemap.clone()).unwrap();
        assert!(req.valid());assert_eq!(req.sitemap,"include");
        sitemap["sitemap"]=json!("only");
        assert!(!serde_json::from_value::<V2CrawlRequest>(sitemap).unwrap().valid());
        let mut concurrency=base;
        for n in [2,3,4]{
            concurrency["maxConcurrency"]=json!(n);
            assert!(serde_json::from_value::<V2CrawlRequest>(concurrency.clone()).unwrap().valid());
        }
        concurrency["maxConcurrency"]=json!(5);
        assert!(!serde_json::from_value::<V2CrawlRequest>(concurrency).unwrap().valid());
    }

    #[test]
    fn accepts_gateway_path_and_delay_options_without_ignoring_them(){
        let req:V2CrawlRequest=serde_json::from_value(json!({"url":"https://example.org/docs/","limit":2,
            "maxConcurrency":1,"includePaths":["^/docs/"],"excludePaths":["private"],"delay":1.25,
            "crawlEntireDomain":true,"scrapeOptions":{"formats":["markdown"],"onlyMainContent":false}})).unwrap();
        assert!(req.valid());
        let policy=req.policy();
        assert_eq!(policy.delay_ms,1250);
        assert!(policy.crawl_entire_domain);
        assert!(!policy.only_main_content);
        assert_eq!(policy.include_paths,vec!["^/docs/"]);
        let invalid:V2CrawlRequest=serde_json::from_value(json!({"url":"https://example.org/","limit":1,
            "maxConcurrency":1,"delay":11})).unwrap();
        assert!(!invalid.valid());
    }

    #[test]
    fn crawl_formats_and_selectors_are_validated_before_job_creation(){
        let seed=crawl::seed("https://example.org/docs/").unwrap();
        let base=json!({"url":seed.as_str(),"limit":2,"maxConcurrency":1,
            "scrapeOptions":{"formats":["html","links"],"includeTags":["article"],"excludeTags":[".ads"]}});
        let req:V2CrawlRequest=serde_json::from_value(base.clone()).unwrap();
        assert!(req.valid()&&req.policy().scope(&seed).is_ok());
        for options in [json!({"formats":["markdown","markdown"]}),json!({"formats":["summary"]}),
            json!({"formats":[]}),json!({"includeTags":["article:has(div)"]})]{
            let mut invalid=base.clone();invalid["scrapeOptions"]=options;
            let req:V2CrawlRequest=serde_json::from_value(invalid).unwrap();
            assert!(req.policy().scope(&seed).is_err());
        }
    }

    #[test]
    fn v2_status_preserves_counts_and_fixed_loopback_cursor(){
        let page=json!({"status":"scraping","total":3,"completed":1,"data":[{"markdown":"fixture"}],
            "created_at":1700000000,"expires_at":1700086400,"finished_at":null,"next_skip":1,
            "research":{"failed":1,"site_coverage_verified":false}});
        let internal=v2_envelope("fixture",2,page.clone(),"http://api:3002").unwrap();
        assert_eq!(internal["next"],"http://api:3002/v2/crawl/fixture?skip=1&limit=2");
        let v=v2_envelope("fixture",2,page,"http://127.0.0.1:3002").unwrap();
        assert_eq!(v["completed"],1);
        assert_eq!(v["next"],"http://127.0.0.1:3002/v2/crawl/fixture?skip=1&limit=2");
        assert_eq!(v["research"]["failed"],1);
        assert_eq!(v["expiresAt"],"2023-11-15T22:13:20+00:00");
    }
}

fn default_limit()->usize{10}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PageRequest {
    #[serde(default)] after:i64,
    #[serde(default="default_limit")] limit:usize,
}

pub(super) async fn status(State(state):State<CandidateState>,Path(id):Path<String>,Query(q):Query<PageRequest>,headers:HeaderMap)->Response {
    if from_a_web_page(&headers){return refusal(StatusCode::FORBIDDEN,"WEB_PAGE_DENIED");}
    if uuid::Uuid::parse_str(&id).is_err(){return refusal(StatusCode::BAD_REQUEST,"INVALID_JOB_ID");}
    let Some(batch)=state.batch else{return refusal(StatusCode::SERVICE_UNAVAILABLE,"JOB_STORE_DISABLED")};
    match batch.call(move|store|store.page(&id,q.after,q.limit)).await {
        Ok(value)=>bounded(json!({"success":true,"data":value})),
        Err(e)=>store_refusal(e)
    }
}

pub(super) async fn cancel(State(state):State<CandidateState>,Path(id):Path<String>,headers:HeaderMap)->Response {
    if from_a_web_page(&headers){return refusal(StatusCode::FORBIDDEN,"WEB_PAGE_DENIED");}
    if uuid::Uuid::parse_str(&id).is_err(){return refusal(StatusCode::BAD_REQUEST,"INVALID_JOB_ID");}
    let Some(batch)=state.batch else{return refusal(StatusCode::SERVICE_UNAVAILABLE,"JOB_STORE_DISABLED")};
    match batch.call(move|store|store.cancel(&id)).await {
        Ok(())=>{batch.notify.notify_waiters();Json(json!({"success":true,"status":"cancelled"})).into_response()},
        Err(e)=>store_refusal(e)
    }
}

// Reserve at least one of the four candidate permits for foreground work.
// This is an upper bound; per-domain pacing can reduce effective concurrency.
const WORKERS:usize=MAX_WORKERS as usize;

async fn work(state:&CandidateState,batch:&BatchState)->Result<(),StoreError>{
    loop {
        // Register before checking SQLite so creation/settlement cannot lose
        // a wakeup between an empty claim and the wait.
        let notified=batch.notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let slot=state.slots.acquire().await.map_err(|_|StoreError::Invalid("worker admission closed"))?;
        let Some(claim)=batch.call(|store|store.claim_next()).await? else {
            drop(slot);
            notified.await;
            continue;
        };
        let result=if let Some(info)=claim.crawl(){
            state.crawler.read(&info.seed,claim.item(),info.include_sitemap&&info.depth==0,
                info.max_depth,info.page_limit,&info.policy_json).await
        } else {
            match static_target(claim.item()){
                Ok(target)=>static_document(state,target,25000).await.map(|doc|(doc,Vec::new())),
                Err(status)=>Err(status),
            }
        };
        let (ok,payload,links)=match result{
            Ok((doc,links))=>(true,doc,links),
            Err(status)=>(false,json!({"error":"page fetch failed","status":status.as_u16()}),Vec::new())
        };
        // Oversized documents become explicit per-item failures, never partial
        // JSON. Cancellation fences late writes even if an in-flight read ends.
        let result=batch.call(move|store|{
            match store.settle_discovered(&claim,ok,&payload,&links){
                Err(StoreError::Invalid("result exceeds research bound"))=>
                    store.settle(&claim,false,&json!({"error":"result exceeds research bound"})),
                other=>other
            }
        }).await;
        match result {
            Ok(())|Err(StoreError::Invalid("terminal job"))=>{batch.notify.notify_waiters();},
            Err(e)=>return Err(e)
        }
    }
}

pub(super) async fn supervise(state:CandidateState){
    if let Some(batch)=state.batch.clone(){
        let mut workers=tokio::task::JoinSet::new();
        for _ in 0..WORKERS{
            let state=state.clone();let batch=batch.clone();
            workers.spawn(async move{work(&state,&batch).await});
        }
        // No worker should exit under ordinary operation. Abort the group
        // and expose degraded health; restart recovery fences old claims.
        let _=workers.join_next().await;
        batch.ready.store(false,Ordering::Release);
        workers.shutdown().await;
        eprintln!("research batch worker group stopped; restart required");
    }
}
