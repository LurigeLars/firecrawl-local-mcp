//! Fixed-batch research route; uses the actual static scraper and SQLite store.
//! Deliberately not exposed as the full Firecrawl batch API.
use super::{CandidateState, bounded, decode, refusal, static_document, static_target,crawl};
use crate::serve::from_a_web_page;
use axum::{body::Bytes, extract::{Path, Query, State}, http::{HeaderMap,StatusCode},
    response::{IntoResponse,Response},Json};
use firecrawl_job_store_research::{Error as StoreError,Store};
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
            batch.notify.notify_one();
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
            batch.notify.notify_one();
            (StatusCode::ACCEPTED,Json(json!({"success":true,"id":id,"url":format!("/research/batch/{id}")}))).into_response()
        }
        Err(e)=>store_refusal(e)
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
        Ok(())=>Json(json!({"success":true,"status":"cancelled"})).into_response(),
        Err(e)=>store_refusal(e)
    }
}

async fn work(state:&CandidateState,batch:&BatchState)->Result<(),StoreError>{
    loop {
        // One worker, sharing the same admission budget as synchronous calls.
        let slot=state.slots.acquire().await.map_err(|_|StoreError::Invalid("worker admission closed"))?;
        let Some(claim)=batch.call(|store|store.claim_next()).await? else {
            drop(slot);
            batch.notify.notified().await;
            continue;
        };
        let result=if let Some(info)=claim.crawl(){
            state.crawler.read(&info.seed,claim.item()).await
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
            Ok(())|Err(StoreError::Invalid("terminal job"))=>{},
            Err(e)=>return Err(e)
        }
    }
}

pub(super) async fn supervise(state:CandidateState){
    if let Some(batch)=state.batch.clone(){
        if work(&state,&batch).await.is_err(){
            batch.ready.store(false,Ordering::Release);
            eprintln!("research batch worker stopped; restart required");
        }
    }
}
