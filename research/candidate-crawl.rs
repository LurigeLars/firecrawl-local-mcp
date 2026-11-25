//! Conservative durable crawl research path. No caller headers, cookies,
//! cross-origin crawl requests or redirect following.
use super::{static_target,render,map};
use crate::{crawl::{CrawlOptions,Scope,ROBOTS_AGENT},fetch::{Fetcher,FetchOptions},
    guard::NetworkPolicy,scrape::{process_html,check_html_only,Format,ScrapeOptions},
    browser::{needs_browser,looks_like_nav_only_shell},cloud};
use axum::http::StatusCode;
use serde::{Deserialize,Serialize};
use serde_json::{json,Value};
use std::{collections::{HashSet,hash_map::DefaultHasher},hash::{Hash,Hasher},sync::Arc,time::Duration};
use tokio::sync::Mutex;
use texting_robots::Robot;
use url::Url;

const ROBOTS_LIMIT:usize=512*1024;
const HTML_LIMIT:usize=2*1024*1024;
const DISCOVERY_LIMIT:usize=100;

#[derive(Clone)]
pub(super) struct Crawler {pub(super) fetcher:Fetcher,domain_locks:Arc<Vec<Arc<Mutex<()>>>>}
const DOMAIN_LOCKS:usize=32;

#[derive(Debug,Clone,Serialize,Deserialize)]
#[serde(default,rename_all="camelCase",deny_unknown_fields)]
pub(super) struct Policy{
    pub(super) include_paths:Vec<String>,
    pub(super) exclude_paths:Vec<String>,
    pub(super) crawl_entire_domain:bool,
    pub(super) delay_ms:u32,
    pub(super) only_main_content:bool,
    pub(super) formats:Vec<String>,
    pub(super) include_tags:Vec<String>,
    pub(super) exclude_tags:Vec<String>,
}
impl Default for Policy{
    fn default()->Self{Self{include_paths:vec![],exclude_paths:vec![],crawl_entire_domain:false,delay_ms:0,
        only_main_content:true,formats:super::default_formats(),include_tags:vec![],exclude_tags:vec![]}}
}
impl Policy{
    fn formats(&self)->Result<Vec<Format>,StatusCode>{
        if self.formats.is_empty()||self.formats.len()>4||
            self.formats.iter().collect::<HashSet<_>>().len()!=self.formats.len(){
            return Err(StatusCode::BAD_REQUEST);
        }
        self.formats.iter().map(|f|match f.as_str(){
            "markdown"=>Ok(Format::Markdown),"html"=>Ok(Format::Html),
            "rawHtml"=>Ok(Format::RawHtml),"links"=>Ok(Format::Links),_=>Err(StatusCode::BAD_REQUEST)
        }).collect()
    }
    pub(super) fn scope(&self,seed:&Url)->Result<Scope,StatusCode>{
        if self.formats().is_err()||!render::valid_tag_filters(&self.include_tags,&self.exclude_tags)||
            self.delay_ms>10000 || self.include_paths.len()+self.exclude_paths.len()>16 ||
            self.include_paths.iter().chain(&self.exclude_paths).any(|p|p.is_empty()||p.len()>256||p.chars().any(char::is_control)){
            return Err(StatusCode::BAD_REQUEST);
        }
        Scope::new(seed,&CrawlOptions{include_paths:self.include_paths.clone(),exclude_paths:self.exclude_paths.clone(),
            crawl_entire_domain:self.crawl_entire_domain,..Default::default()}).map_err(|_|StatusCode::BAD_REQUEST)
    }
    pub(super) fn delay(&self)->Duration{Duration::from_millis(u64::from(self.delay_ms))}
}

pub(super) fn scoped(seed:&Url,target:&Url)->bool {
    seed.origin()==target.origin() &&
        Scope::new(seed,&CrawlOptions::default()).is_ok_and(|scope|scope.allows(target))
}

pub(super) fn seed(raw:&str)->Result<Url,StatusCode>{
    let seed=static_target(raw)?;
    if !scoped(&seed,&seed){return Err(StatusCode::BAD_REQUEST);}
    Ok(seed)
}

fn candidates(seed:&Url,scope:&Scope,links:&[Value])->Vec<String>{
    let mut seen=HashSet::new();
    let mut out=Vec::new();
    // Upstream already resolves relative links against the document base.
    // Retain distinct trailing slashes rather than changing request paths.
    for raw in links.iter().filter_map(Value::as_str){
        let Ok(mut url)=Url::parse(raw) else{continue};
        url.set_fragment(None);
        let Ok(url)=static_target(url.as_str()) else{continue};
        if seed.origin()==url.origin() && scope.allows(&url) && seen.insert(url.to_string()){
            out.push(url.to_string());
            if out.len()>=DISCOVERY_LIMIT{break;}
        }
    }
    out
}

pub(super) fn robots_decision(status:u16,body:&[u8],target:&Url)->Result<(bool,Duration),StatusCode>{
    let default=Duration::from_secs(1);
    match status{
        404|410=>Ok((true,default)),
        200 if body.len()<=ROBOTS_LIMIT=>{
            let robot=Robot::new(ROBOTS_AGENT,body).map_err(|_|StatusCode::BAD_GATEWAY)?;
            let seconds=robot.delay.unwrap_or(1.0);
            // A longer crawl-delay is a refusal, not silently shortened.
            if !seconds.is_finite() || !(0.0..=10.0).contains(&seconds){
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
            Ok((robot.allowed(target.as_str()),Duration::from_secs_f32(seconds.max(1.0))))
        },
        _=>Err(StatusCode::SERVICE_UNAVAILABLE)
    }
}

impl Crawler{
    pub(super) fn new()->Result<Self,String>{
        Ok(Self{fetcher:Fetcher::new_no_redirect(NetworkPolicy::PublicOnly)?,
            domain_locks:Arc::new((0..DOMAIN_LOCKS).map(|_|Arc::new(Mutex::new(()))).collect())})
    }

    // A fixed number of stripes bounds memory, including under hostile
    // host churn. Subdomains of one registrable domain share their lock;
    // collisions only reduce concurrency, never relax pacing.
    pub(super) fn domain_lock(&self,target:&Url)->Arc<Mutex<()>>{
        let host=target.host_str().unwrap_or("").trim_end_matches('.');
        let domain=psl::domain_str(host).unwrap_or(host);
        let mut hash=DefaultHasher::new();
        domain.hash(&mut hash);
        self.domain_locks[hash.finish() as usize%DOMAIN_LOCKS].clone()
    }

    pub(super) async fn robots(&self,target:&Url)->Result<(u16,bytes::Bytes),StatusCode>{
        render::check_dns(target).await.map_err(|_|StatusCode::FORBIDDEN)?;
        let url=target.join("/robots.txt").map_err(|_|StatusCode::BAD_REQUEST)?;
        let res=self.fetcher.fetch_once_limited(&url,&FetchOptions{
            timeout:Some(Duration::from_secs(8)),..Default::default()
        },ROBOTS_LIMIT).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
        if res.status==200 && (res.mime().as_deref()!=Some("text/plain") ||
            res.body.len()>ROBOTS_LIMIT || std::str::from_utf8(&res.body).is_err()){
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok((res.status,res.body))
    }

    pub(super) async fn read(&self,raw_seed:&str,raw_target:&str,include_sitemap:bool,
        max_depth:u32,page_limit:u32,policy_json:&str)->Result<(Value,Vec<String>),StatusCode>{
        let seed=seed(raw_seed)?;
        let target=static_target(raw_target)?;
        let policy:Policy=serde_json::from_str(policy_json).map_err(|_|StatusCode::BAD_REQUEST)?;
        let scope=policy.scope(&seed)?;
        // Match upstream: the explicitly supplied seed is read even when
        // includePaths selects only its descendants. All discoveries are filtered.
        if seed.origin()!=target.origin() || (target!=seed&&!scope.allows(&target)){return Err(StatusCode::FORBIDDEN);}
        let domain=self.domain_lock(&target);
        let guard=domain.lock().await;
        let fetched=self.fetch_page(&target,Duration::from_secs(50),policy.delay()).await?;
        drop(guard);
        let (mut doc,links)=self.convert(fetched,&target,&policy).await?;
        let mut frontier=candidates(&seed,&scope,&links);
        if include_sitemap{
            if max_depth==0||page_limit<=1{
                doc["researchSitemap"]=json!({"status":"skipped_by_crawl_limit"});
            }else{
                let _domain=domain.lock().await;
                let (sitemap,receipt)=map::crawl_sitemap(self,&seed,&policy).await;
                doc["researchSitemap"]=receipt;
                // Prefer sitemap candidates, then HTML discovery. Both enter
                // the same atomic, depth- and count-limited SQLite frontier.
                let mut seen=HashSet::new();
                frontier=sitemap.into_iter().chain(frontier).filter(|s|s!=seed.as_str()&&seen.insert(s.clone()))
                    .take(DISCOVERY_LIMIT).collect();
            }
        }
        Ok((doc,frontier))
    }

    /// Caller holds the shared per-domain crawl/map lock. The target must already
    /// be validated as a public HTTPS seed or in-scope crawl URL.
    pub(super) async fn read_links(&self,target:&Url,budget:Duration)->Result<(Value,Vec<Value>),StatusCode>{
        self.read_links_with_options(target,budget,&Policy::default()).await
    }

    async fn read_links_with_options(&self,target:&Url,budget:Duration,policy:&Policy)->Result<(Value,Vec<Value>),StatusCode>{
        let fetched=self.fetch_page(target,budget,policy.delay()).await?;
        self.convert(fetched,target,policy).await
    }

    async fn fetch_page(&self,target:&Url,budget:Duration,minimum_delay:Duration)->Result<String,StatusCode>{
        // Caller owns the domain lock. Always complete the cooldown before
        // releasing it, including when a network timeout cancels a fetch.
        let mut delay=Duration::from_secs(10);
        let result=tokio::time::timeout(budget.min(Duration::from_secs(50)),async{
            let (status,body)=self.robots(target).await?;
            let (allowed,robots_delay)=robots_decision(status,&body,target)?;
            delay=robots_delay.max(minimum_delay);
            tokio::time::sleep(delay).await;
            if !allowed{return Err(StatusCode::FORBIDDEN);}
            let page=self.fetcher.fetch_once_limited(target,&FetchOptions{
                timeout:Some(Duration::from_secs(15)),..Default::default()
            },HTML_LIMIT).await.map_err(|_|StatusCode::BAD_GATEWAY)?;
            if page.status!=200 || page.final_url!=*target || page.body.len()>HTML_LIMIT{
                return Err(StatusCode::BAD_GATEWAY);
            }
            check_html_only(&page).map_err(|_|StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
            Ok::<_,StatusCode>(page.text())
        }).await;
        tokio::time::sleep(delay).await;
        result.map_err(|_|StatusCode::GATEWAY_TIMEOUT)?
    }

    async fn convert(&self,fetched:String,target:&Url,policy:&Policy)->Result<(Value,Vec<Value>),StatusCode>{
        let target=target.clone();
        let formats=policy.formats()?;
        let opts=ScrapeOptions{only_main_content:policy.only_main_content,include_tags:policy.include_tags.clone(),
            exclude_tags:policy.exclude_tags.clone(),..Default::default()};
        // Network pacing is complete; parsing may overlap with the next
        // bounded claim. Caller retains the global admission permit.
        tokio::task::spawn_blocking(move||convert_page(fetched,&target,&opts,formats))
            .await.map_err(|_|StatusCode::BAD_GATEWAY)?
    }
}

// Retain link discovery independently of requested document formats. Return
// only requested fields. Raw HTML is moved out of the parser, not cloned.
fn convert_page(raw:String,target:&Url,opts:&ScrapeOptions,requested:Vec<Format>)
    ->Result<(Value,Vec<Value>),StatusCode>{
    let mut internal=requested.clone();
    for f in [Format::RawHtml,Format::Links]{
        if !internal.contains(&f){internal.push(f);}
    }
    let (mut data,mut metadata,markdown)=process_html(raw,target,opts,&internal).map_err(|_|StatusCode::BAD_GATEWAY)?;
    if markdown.trim().is_empty()||cloud::looks_like_bot_check(&markdown){
        return Err(StatusCode::BAD_GATEWAY);
    }
    let raw=data.get("rawHtml").and_then(Value::as_str).ok_or(StatusCode::BAD_GATEWAY)?;
    // Crawls cannot safely retry in a browser until robots and redirect scope
    // are enforced on that path as well. Report incomplete shells as failures.
    if needs_browser(raw,&markdown)||looks_like_nav_only_shell(raw,&markdown){
        return Err(StatusCode::PRECONDITION_REQUIRED);
    }
    let links=data.get("links").and_then(Value::as_array).cloned().unwrap_or_default();
    if !requested.contains(&Format::RawHtml){data.remove("rawHtml");}
    if !requested.contains(&Format::Links){data.remove("links");}
    metadata.insert("url".into(),json!(target.as_str()));
    metadata.insert("statusCode".into(),json!(200));
    metadata.insert("contentType".into(),json!("text/html"));
    data.insert("metadata".into(),Value::Object(metadata));
    data.insert("source".into(),json!("local"));
    data.insert("researchSourceVerified".into(),json!(false));
    Ok((Value::Object(data),links))
}

#[cfg(test)]
mod tests{
    use super::*;
    #[tokio::test]
    async fn fixed_domain_locks_share_subdomains_and_allow_independent_work(){
        let crawler=Crawler::new().unwrap();
        let a=Url::parse("https://www.example.org/a").unwrap();
        let b=Url::parse("https://news.example.org/b").unwrap();
        let first=crawler.domain_lock(&a);
        assert!(Arc::ptr_eq(&first,&crawler.domain_lock(&b)));
        let _held=first.lock().await;
        assert!(crawler.domain_lock(&b).try_lock().is_err());
        let independent=(0..128).map(|i|Url::parse(&format!("https://site{i}.com/")).unwrap())
            .find(|u|!Arc::ptr_eq(&first,&crawler.domain_lock(u))).unwrap();
        assert!(crawler.domain_lock(&independent).try_lock().is_ok());
        assert_eq!(crawler.domain_locks.len(),DOMAIN_LOCKS);
    }

    #[test]
    fn discovery_is_same_origin_and_under_seed_path(){
        let seed=seed("https://example.com/docs/").unwrap();
        let links=json!([
            "https://example.com/docs/a#one","https://example.com/docs/a#two",
            "https://example.com/docs/a/","https://example.com/docs/b?q=1",
            "https://example.com/docs-old/a","https://www.example.com/docs/a",
            "https://example.com/docs/file.pdf","http://example.com/docs/a",
            "https://127.0.0.1/docs/a","https://example.com:8443/docs/a",
            "https://user:pw@example.com/docs/a"
        ]);
        assert_eq!(candidates(&seed,&Policy::default().scope(&seed).unwrap(),links.as_array().unwrap()),
            vec!["https://example.com/docs/a","https://example.com/docs/a/"]);
    }

    #[test]
    fn path_filters_are_bounded_and_never_expand_origin(){
        let seed=seed("https://example.com/docs/").unwrap();
        let policy=Policy{include_paths:vec!["^/reports/".into()],exclude_paths:vec!["/private".into()],
            crawl_entire_domain:true,delay_ms:1250,only_main_content:false,..Default::default()};
        let scope=policy.scope(&seed).unwrap();
        let links=json!(["https://example.com/reports/a","https://example.com/reports/private",
            "https://example.com/docs/b","https://other.org/reports/a","https://example.com/reports/a?q=1"]);
        assert_eq!(candidates(&seed,&scope,links.as_array().unwrap()),vec!["https://example.com/reports/a"]);
        assert_eq!(policy.delay(),Duration::from_millis(1250));
        for bad in [Policy{include_paths:vec!["[".into()],..Default::default()},
            Policy{exclude_paths:vec!["x".repeat(257)],..Default::default()},
            Policy{delay_ms:10001,..Default::default()}]{assert!(bad.scope(&seed).is_err());}
    }

    #[test]
    fn crawl_conversion_preserves_requested_formats_and_independent_discovery(){
        let target=Url::parse("https://example.org/docs/").unwrap();
        let raw="<html><body><article><p>Stock 421.5</p><p class=\"ads\">Ad</p><a href=\"/docs/a\">next</a></article></body></html>";
        let opts=ScrapeOptions{include_tags:vec!["article".into()],exclude_tags:vec![".ads".into()],..Default::default()};
        let (doc,links)=convert_page(raw.into(),&target,&opts,vec![Format::Html]).unwrap();
        assert!(doc["html"].as_str().unwrap().contains("Stock 421.5"));
        assert!(!doc["html"].as_str().unwrap().contains("class=\"ads\""));
        for key in ["markdown","links","rawHtml"]{assert!(doc.get(key).is_none());}
        assert_eq!(links,vec![json!("https://example.org/docs/a")]);
        let (doc,_)=convert_page(raw.into(),&target,&opts,vec![Format::Markdown,Format::RawHtml,Format::Links]).unwrap();
        assert!(doc["markdown"].as_str().unwrap().contains("421.5"));
        assert_eq!(doc["rawHtml"],raw);
        assert_eq!(doc["links"][0],"https://example.org/docs/a");
        let shell="<html><body><div id=\"root\">Loading...</div><script src=\"app.js\"></script></body></html>";
        assert_eq!(convert_page(shell.into(),&target,&ScrapeOptions::default(),vec![Format::Markdown]).unwrap_err(),
            StatusCode::PRECONDITION_REQUIRED);
    }

    #[test]
    fn robots_fail_closed_and_delay_is_not_shortened(){
        let target=Url::parse("https://example.com/private/page").unwrap();
        let rules=b"User-agent: FirecrawlLocal\nDisallow: /private/\nCrawl-delay: 3\n";
        let (allowed,delay)=robots_decision(200,rules,&target).unwrap();
        assert!(!allowed);assert_eq!(delay,Duration::from_secs(3));
        assert!(robots_decision(404,b"",&target).unwrap().0);
        for status in [301,401,403,429,500,503]{
            assert!(robots_decision(status,b"",&target).is_err());
        }
        assert!(robots_decision(200,b"User-agent: *\nCrawl-delay: 30\n",&target).is_err());
    }

    #[tokio::test]
    async fn scoped_fetcher_does_not_follow_redirects(){
        use axum::{Router,routing::get,response::Redirect};
        use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
        let hits=Arc::new(AtomicUsize::new(0));
        let counter=hits.clone();
        let app=Router::new().route("/start",get(||async{Redirect::temporary("/destination")}))
            .route("/destination",get(move||{let c=counter.clone();async move{c.fetch_add(1,Ordering::SeqCst);"unexpected"}}));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url=Url::parse(&format!("http://{}/start",listener.local_addr().unwrap())).unwrap();
        let task=tokio::spawn(async move{axum::serve(listener,app).await.unwrap();});
        // Only this test uses AllowPrivate, never the candidate constructor.
        let fetcher=Fetcher::new_no_redirect(NetworkPolicy::AllowPrivate).unwrap();
        let res=fetcher.fetch_once(&url,&FetchOptions{timeout:Some(Duration::from_secs(2)),..Default::default()}).await.unwrap();
        assert_eq!(res.status,307);
        assert_eq!(hits.load(Ordering::SeqCst),0);
        task.abort();
    }
}
