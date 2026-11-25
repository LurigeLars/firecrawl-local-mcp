//! Conservative durable crawl research path. No caller headers, cookies,
//! cross-origin crawl requests or redirect following.
use super::{static_target,render,map};
use crate::{crawl::{CrawlOptions,Scope,ROBOTS_AGENT},fetch::{Fetcher,FetchOptions},
    guard::NetworkPolicy,scrape::{process_html,Format,ScrapeOptions},cloud};
use axum::http::StatusCode;
use serde_json::{json,Value};
use std::{collections::HashSet,sync::Arc,time::Duration};
use tokio::sync::Mutex;
use texting_robots::Robot;
use url::Url;

const ROBOTS_LIMIT:usize=512*1024;
const HTML_LIMIT:usize=2*1024*1024;
const DISCOVERY_LIMIT:usize=100;

#[derive(Clone)]
pub(super) struct Crawler {pub(super) fetcher:Fetcher,pub(super) serial:Arc<Mutex<()>>}

pub(super) fn scoped(seed:&Url,target:&Url)->bool {
    seed.origin()==target.origin() &&
        Scope::new(seed,&CrawlOptions::default()).is_ok_and(|scope|scope.allows(target))
}

pub(super) fn seed(raw:&str)->Result<Url,StatusCode>{
    let seed=static_target(raw)?;
    if !scoped(&seed,&seed){return Err(StatusCode::BAD_REQUEST);}
    Ok(seed)
}

fn candidates(seed:&Url,links:&[Value])->Vec<String>{
    let mut seen=HashSet::new();
    let mut out=Vec::new();
    // Upstream already resolves relative links against the document base.
    // Retain distinct trailing slashes rather than changing request paths.
    for raw in links.iter().filter_map(Value::as_str){
        let Ok(mut url)=Url::parse(raw) else{continue};
        url.set_fragment(None);
        let Ok(url)=static_target(url.as_str()) else{continue};
        if scoped(seed,&url) && seen.insert(url.to_string()){
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
        Ok(Self{fetcher:Fetcher::new_no_redirect(NetworkPolicy::PublicOnly)?,serial:Arc::new(Mutex::new(()))})
    }

    pub(super) async fn robots(&self,target:&Url)->Result<(u16,bytes::Bytes),StatusCode>{
        render::check_dns(target).await.map_err(|_|StatusCode::FORBIDDEN)?;
        let url=target.join("/robots.txt").map_err(|_|StatusCode::BAD_REQUEST)?;
        let res=self.fetcher.fetch_once(&url,&FetchOptions{
            timeout:Some(Duration::from_secs(8)),..Default::default()
        }).await.map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
        if res.status==200 && (res.mime().as_deref()!=Some("text/plain") ||
            res.body.len()>ROBOTS_LIMIT || std::str::from_utf8(&res.body).is_err()){
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
        Ok((res.status,res.body))
    }

    pub(super) async fn read(&self,raw_seed:&str,raw_target:&str,include_sitemap:bool,
        max_depth:u32,page_limit:u32)->Result<(Value,Vec<String>),StatusCode>{
        let seed=seed(raw_seed)?;
        let target=static_target(raw_target)?;
        if !scoped(&seed,&target){return Err(StatusCode::FORBIDDEN);}
        let _serial=self.serial.lock().await;
        let (mut doc,links)=self.read_links(&target,Duration::from_secs(50)).await?;
        let mut frontier=candidates(&seed,&links);
        if include_sitemap{
            if max_depth==0||page_limit<=1{
                doc["researchSitemap"]=json!({"status":"skipped_by_crawl_limit"});
            }else{
                let (sitemap,receipt)=map::crawl_sitemap(self,&seed).await;
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

    /// Caller holds the shared crawl/map serial lock. The target must already
    /// be validated as a public HTTPS seed or in-scope crawl URL.
    pub(super) async fn read_links(&self,target:&Url,budget:Duration)->Result<(Value,Vec<Value>),StatusCode>{
        let target=target.clone();
        // Fresh robots check on every claimed page. Fail closed if unavailable.
        // No retries/redirects in either request. Connection-time DNS guard
        // remains in the same upstream Fetcher.
        let fetched=tokio::time::timeout(budget.min(Duration::from_secs(50)),async{
            let (status,body)=self.robots(&target).await?;
            let (allowed,delay)=robots_decision(status,&body,&target)?;
            tokio::time::sleep(delay).await;
            if !allowed{return Err(StatusCode::FORBIDDEN);}
            let page=self.fetcher.fetch_once(&target,&FetchOptions{
                timeout:Some(Duration::from_secs(15)),..Default::default()
            }).await;
            // The one crawl worker spaces the next robots/page request too.
            tokio::time::sleep(delay).await;
            let page=page.map_err(|_|StatusCode::BAD_GATEWAY)?;
            if page.status!=200 || page.final_url!=target || page.body.len()>HTML_LIMIT ||
                page.mime().as_deref()!=Some("text/html"){
                return Err(StatusCode::BAD_GATEWAY);
            }
            Ok::<_,StatusCode>(page.text())
        }).await.map_err(|_|StatusCode::GATEWAY_TIMEOUT)??;

        // Reuse actual upstream HTML conversion. Keep the worker admission
        // slot until the blocking parser finishes; don't abandon a running
        // parse on async timeout. Overall process memory/wall limits remain
        // required for untrusted document conversion.
        tokio::task::spawn_blocking(move||{
            let (mut data,mut metadata,markdown)=process_html(fetched,&target,&ScrapeOptions::default(),
                &[Format::Markdown,Format::Links]).map_err(|_|StatusCode::BAD_GATEWAY)?;
            if markdown.trim().is_empty() || cloud::looks_like_bot_check(&markdown){
                return Err(StatusCode::BAD_GATEWAY);
            }
            let links=data.remove("links").and_then(|v|v.as_array().cloned()).unwrap_or_default();
            metadata.insert("url".into(),json!(target.as_str()));
            metadata.insert("statusCode".into(),json!(200));
            metadata.insert("contentType".into(),json!("text/html"));
            data.insert("metadata".into(),Value::Object(metadata));
            data.insert("source".into(),json!("local"));
            data.insert("researchSourceVerified".into(),json!(false));
            Ok((Value::Object(data),links))
        }).await.map_err(|_|StatusCode::BAD_GATEWAY)?
    }
}

#[cfg(test)]
mod tests{
    use super::*;
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
        assert_eq!(candidates(&seed,links.as_array().unwrap()),
            vec!["https://example.com/docs/a","https://example.com/docs/a/"]);
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
