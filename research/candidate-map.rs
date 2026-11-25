//! Bounded local map: reuse the crawl fetcher, robots rules, upstream sitemap
//! parser and path scope. No remote index or arbitrary sitemap backend.
use super::{CandidateState,decode,refusal,bounded,static_target,render,crawl};
use crate::{crawl::{CrawlOptions,Scope,ROBOTS_AGENT},crawler,fetch::FetchOptions};
use axum::{body::Bytes,extract::State,http::{HeaderMap,StatusCode},response::Response};
use serde::Deserialize;
use serde_json::{json,Value};
use std::{collections::{HashSet,VecDeque},time::Duration};
use texting_robots::Robot;
use tokio::time::Instant;
use url::Url;

const MAX_SITEMAPS:usize=6;
const MAX_XML:usize=512*1024;
const MAX_LINKS:usize=100;
fn yes()->bool{true}
fn default_mode()->String{"include".into()}
fn default_timeout()->u64{45000}

#[derive(Deserialize)]
#[serde(rename_all="camelCase",deny_unknown_fields)]
struct MapRequest {
    url:String,
    limit:usize,
    #[serde(default)] search:Option<String>,
    #[serde(default="default_mode")] sitemap:String,
    #[serde(default="yes")] include_subdomains:bool,
    #[serde(default="yes")] filter_by_path:bool,
    #[serde(default="yes")] ignore_query_parameters:bool,
    #[serde(default)] use_index:Option<bool>,
    #[serde(default="default_timeout")] timeout:u64,
    #[serde(default)] origin:Option<String>,
}

impl MapRequest {
    fn valid(&self)->bool{
        (1..=MAX_LINKS).contains(&self.limit) &&
        matches!(self.sitemap.as_str(),"include"|"only"|"skip") &&
        (1000..=60000).contains(&self.timeout) &&
        self.search.as_ref().is_none_or(|s|!s.is_empty()&&s.len()<=256&&!s.chars().any(char::is_control)) &&
        self.origin.as_ref().is_none_or(|s|!s.is_empty()&&s.len()<=256&&!s.chars().any(char::is_control))
    }
}

struct Results<'a>{
    request:&'a MapRequest,
    seed:&'a Url,
    scope:Scope,
    seen:HashSet<String>,
    links:Vec<Value>,
    crawl_policy:bool,
}

impl<'a> Results<'a>{
    fn new(request:&'a MapRequest,seed:&'a Url)->Result<Self,StatusCode>{
        Self::with_policy(request,seed,None)
    }

    fn with_policy(request:&'a MapRequest,seed:&'a Url,policy:Option<&crawl::Policy>)->Result<Self,StatusCode>{
        let scope=if let Some(policy)=policy{policy.scope(seed)?}else{Scope::new(seed,&CrawlOptions{
            allow_subdomains:request.include_subdomains,
            crawl_entire_domain:!request.filter_by_path,
            ..Default::default()
        }).map_err(|_|StatusCode::BAD_REQUEST)?};
        Ok(Self{request,seed,scope,seen:HashSet::new(),links:Vec::new(),crawl_policy:policy.is_some()})
    }

    fn full(&self)->bool{self.links.len()>=self.request.limit}

    // Results are inert URLs. Cross-origin *sitemap fetches* are never allowed.
    fn push(&mut self,raw:&str){
        if self.full(){return;}
        let Ok(mut url)=render::public_url(raw)else{return;};
        if url.scheme()!="https"||url.port().is_some(){return;}
        url.set_fragment(None);
        if self.request.ignore_query_parameters{url.set_query(None);}
        if self.crawl_policy&&url.query().is_some(){return;}
        if (!self.request.include_subdomains && url.origin()!=self.seed.origin()) ||
            !self.scope.allows(&url){return;}
        let key=url.to_string();
        if self.request.search.as_ref().is_some_and(|s|!key.to_lowercase().contains(&s.to_lowercase())){return;}
        if self.seen.insert(key.clone()){self.links.push(json!({"url":key}));}
    }
}

fn enqueue(seed:&Url,raw:&str,seen:&mut HashSet<String>,queue:&mut VecDeque<Url>)->bool{
    if seen.len()>=MAX_SITEMAPS{return false;}
    let Ok(url)=static_target(raw)else{return false;};
    if url.origin()!=seed.origin(){return false;}
    if seen.insert(url.to_string()){queue.push_back(url);true}else{false}
}

async fn parse_sitemap_into(xml:&str,seed:&Url,results:&mut Results<'_>,seen:&mut HashSet<String>,
    queue:&mut VecDeque<Url>)->Result<usize,()>{
    if xml.len()>MAX_XML{return Err(());}
    let parsed=crawler::process_sitemap(xml.to_owned()).await.map_err(|_|())?;
    let mut skipped=0;
    for instruction in parsed.instructions{
        if instruction.action=="recurse"{
            for raw in instruction.urls{
                if !enqueue(seed,&raw,seen,queue){skipped+=1;}
            }
        }else if instruction.action=="process"{
            for raw in instruction.urls{
                results.push(&raw);
                if results.full(){break;}
            }
        }
        if results.full(){break;}
    }
    Ok(skipped)
}

async fn discover(crawler:&crawl::Crawler,req:&MapRequest,seed:&Url,policy:Option<&crawl::Policy>)->Result<Value,StatusCode>{
    let mut results=Results::with_policy(req,seed,policy)?;
    let end=Instant::now()+Duration::from_millis(req.timeout);
    let mut attempted=0usize;
    let mut read=0usize;
    let mut skipped=0usize;
    let mut seed_read=false;
    if req.sitemap!="skip"{
        let (status,body)=tokio::time::timeout_at(end,crawler.robots(seed)).await
            .map_err(|_|StatusCode::GATEWAY_TIMEOUT)??;
        // Validate robots even for sitemap-only; a forbidden seed need not
        // prohibit a separately allowed sitemap on the same origin.
        let (_,robots_delay)=crawl::robots_decision(status,&body,seed)?;
        let delay=robots_delay.max(policy.map_or(Duration::ZERO,crawl::Policy::delay));
        let mut queue=VecDeque::new();
        let mut seen=HashSet::new();
        if status==200{
            let robot=Robot::new(ROBOTS_AGENT,&body).map_err(|_|StatusCode::BAD_GATEWAY)?;
            for raw in robot.sitemaps.iter().take(MAX_SITEMAPS){
                if !enqueue(seed,raw,&mut seen,&mut queue){skipped+=1;}
            }
        }
        if let Ok(url)=seed.join("/sitemap.xml"){
            enqueue(seed,url.as_str(),&mut seen,&mut queue);
        }
        // At most six same-origin files, no redirects or retries. Respect
        // crawl-delay before and after the request, not merely between pages.
        while let Some(url)=queue.pop_front(){
            if results.full()||Instant::now()+delay>=end{break;}
            let (allowed,_)=crawl::robots_decision(status,&body,&url)?;
            if !allowed{skipped+=1;continue;}
            tokio::time::sleep(delay).await;
            let remaining=end.saturating_duration_since(Instant::now()).min(Duration::from_secs(8));
            if remaining.is_zero(){break;}
            attempted+=1;
            let res=tokio::time::timeout(remaining,crawler.fetcher.fetch_once(&url,&FetchOptions{
                timeout:Some(remaining),..Default::default()
            })).await;
            // Preserve spacing before any subsequent seed request, even if
            // the remaining map budget is exhausted.
            tokio::time::sleep(delay).await;
            let Ok(Ok(res))=res else{skipped+=1;continue;};
            if res.status!=200 || res.final_url!=url || res.body.len()>MAX_XML ||
                !matches!(res.mime().as_deref(),Some("application/xml"|"text/xml")){
                skipped+=1;continue;
            }
            let Ok(xml)=std::str::from_utf8(&res.body)else{skipped+=1;continue;};
            // Upstream native parser rejects entity declarations/deep nesting.
            // Bounded input; do not drop its blocking task on an async timeout.
            match parse_sitemap_into(xml,seed,&mut results,&mut seen,&mut queue).await{
                Ok(n)=>{read+=1;skipped+=n;},
                Err(())=>{skipped+=1;continue;}
            }
        }
    }
    if req.sitemap!="only" && !results.full() && Instant::now()<end{
        let remaining=end.saturating_duration_since(Instant::now());
        // The shared HTML reader bounds its network stage and retains the
        // blocking parser until completion.
        if let Ok((_,links))=crawler.read_links(seed,remaining).await{
            seed_read=true;
            results.push(seed.as_str());
            for link in links.iter().filter_map(Value::as_str){
                results.push(link);
                if results.full(){break;}
            }
        }
    }
    if read==0&&!seed_read{return Err(StatusCode::BAD_GATEWAY);}
    let boundary=results.full();
    let search_warning=if req.search.is_some(){" Search is a literal URL substring filter, not remote index relevance."}else{""};
    Ok(json!({"success":true,"links":results.links,
        "warning":format!("Bounded local discovery only; no remote index and complete site coverage is unverified.{search_warning}"),
        "research":{"sitemaps_attempted":attempted,"sitemaps_read":read,"sitemaps_skipped":skipped,
            "sitemap_limit":MAX_SITEMAPS,"seed_read":seed_read,"result_limit_reached":boundary,
            "index_requested":req.use_index.unwrap_or(true),"index_available":false,
            "site_coverage_verified":false}}))
}

/// Called only by the seed worker while holding the shared serial lock.
/// A missing/failed sitemap is reported explicitly; a successful seed page
/// still contributes its HTML links. Never expand scope or bypass robots.
pub(super) async fn crawl_sitemap(crawler:&crawl::Crawler,seed:&Url,policy:&crawl::Policy)->(Vec<String>,Value){
    let req=MapRequest{url:seed.to_string(),limit:MAX_LINKS,search:None,sitemap:"only".into(),
        include_subdomains:false,filter_by_path:true,ignore_query_parameters:false,use_index:Some(false),
        timeout:20000,origin:None};
    match discover(crawler,&req,seed,Some(policy)).await{
        Ok(data)=>{
            let mut links=Vec::new();
            if let Some(rows)=data["links"].as_array(){
                for raw in rows.iter().filter_map(|r|r["url"].as_str()){
                    if let Ok(url)=static_target(raw){
                        if url.origin()==seed.origin(){links.push(url.to_string());}
                    }
                }
            }
            (links,json!({"status":"read","discovery":data["research"]}))
        }
        Err(status)=>(Vec::new(),json!({"status":"no_readable_sitemap","http_status":status.as_u16(),
            "site_coverage_verified":false}))
    }
}

pub(super) async fn route(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response{
    let req:MapRequest=match decode(&headers,&body){Ok(v)=>v,Err(r)=>return r};
    if !req.valid(){return refusal(StatusCode::BAD_REQUEST,"UNSUPPORTED_MAP_OPTIONS");}
    let seed=match crawl::seed(&req.url){Ok(v)=>v,Err(s)=>return refusal(s,"DESTINATION_DENIED")};
    let Ok(slot)=state.slots.clone().try_acquire_owned()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY")};
    let Ok(serial)=state.crawler.serial.clone().try_lock_owned()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"CRAWLER_BUSY")};
    // Retain both guards in the bounded task if the HTTP caller disconnects.
    let result=tokio::spawn(async move{
        let _slot=slot;
        let _serial=serial;
        discover(&state.crawler,&req,&seed,None).await
    }).await;
    match result{
        Ok(Ok(data))=>bounded(data),
        Ok(Err(s))=>refusal(s,"MAP_FAILED"),
        Err(_)=>refusal(StatusCode::BAD_GATEWAY,"MAP_TASK_FAILED")
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    #[test]
    fn locked_client_map_envelope_and_scope_filter(){
        let req:MapRequest=serde_json::from_value(json!({"url":"https://example.org/docs/",
            "limit":2,"origin":"mcp-candidate-contract@3.40.0"})).unwrap();
        assert!(req.valid());
        let seed=Url::parse(&req.url).unwrap();
        let mut results=Results::new(&req,&seed).unwrap();
        for url in ["https://example.org/docs-old/a","https://evil.test/docs/a",
            "https://127.0.0.1/docs/a","https://example.org/docs/a?q=1#one",
            "https://example.org/docs/a?q=2","https://sub.example.org/docs/b",
            "https://example.org/docs/c"]{results.push(url);}
        assert_eq!(results.links,json!([
            {"url":"https://example.org/docs/a"},{"url":"https://sub.example.org/docs/b"}
        ]).as_array().unwrap().clone());
        assert!(results.full());
    }

    #[tokio::test]
    async fn actual_native_sitemap_parser_filters_index_and_urlset(){
        let req:MapRequest=serde_json::from_value(json!({"url":"https://example.org/docs/",
            "limit":3,"sitemap":"only","includeSubdomains":false})).unwrap();
        let seed=Url::parse(&req.url).unwrap();
        let mut results=Results::new(&req,&seed).unwrap();
        let mut seen=HashSet::new();let mut queue=VecDeque::new();
        let ns="http://www.sitemaps.org/schemas/sitemap/0.9";
        let index=format!(r#"<sitemapindex xmlns="{ns}">
            <sitemap><loc>https://example.org/one.xml</loc></sitemap>
            <sitemap><loc>https://other.org/two.xml</loc></sitemap>
            <sitemap><loc>https://example.org/one.xml</loc></sitemap>
            </sitemapindex>"#);
        assert!(parse_sitemap_into(&index,&seed,&mut results,&mut seen,&mut queue).await.is_ok());
        assert_eq!(queue.len(),1);
        assert_eq!(queue.pop_front().unwrap().as_str(),"https://example.org/one.xml");
        let pages=format!(r#"<urlset xmlns="{ns}">
            <url><loc>https://example.org/docs/a</loc></url>
            <url><loc>https://example.org/docs-old/a</loc></url>
            <url><loc>https://other.org/docs/b</loc></url>
            <url><loc>https://example.org/docs/a</loc></url>
            <url><loc>https://example.org/docs/b</loc></url>
            </urlset>"#);
        parse_sitemap_into(&pages,&seed,&mut results,&mut seen,&mut queue).await.unwrap();
        assert_eq!(results.links,json!([{"url":"https://example.org/docs/a"},
            {"url":"https://example.org/docs/b"}]).as_array().unwrap().clone());
        let before=results.links.clone();
        assert!(parse_sitemap_into(&"x".repeat(MAX_XML+1),&seed,&mut results,&mut seen,&mut queue).await.is_err());
        assert_eq!(results.links,before);
    }

    #[test]
    fn sitemap_queue_refuses_cross_origin_and_is_bounded(){
        let seed=Url::parse("https://example.org/docs/").unwrap();
        let mut seen=HashSet::new();let mut queue=VecDeque::new();
        for raw in ["https://sub.example.org/sitemap.xml","https://other.org/sitemap.xml",
            "https://127.0.0.1/sitemap.xml","https://example.org/sitemap.xml?q=1"]{
            assert!(!enqueue(&seed,raw,&mut seen,&mut queue));
        }
        for i in 0..20{enqueue(&seed,&format!("https://example.org/sitemap{i}.xml"),&mut seen,&mut queue);}
        assert_eq!(queue.len(),MAX_SITEMAPS);
    }
}
