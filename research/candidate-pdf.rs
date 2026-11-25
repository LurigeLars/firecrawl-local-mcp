//! Candidate-side transport: download only with the upstream guarded fetcher;
//! all PDF parsing/rendering stays in the opt-in isolated OCR service.
#[path="pdf-result.rs"]
mod contract;
use contract::{MAX_PDF,MAX_REPLY,parse_reply};
use super::{CandidateState,decode,refusal,bounded,render};
use crate::fetch::{FetchOptions,FetchResult,FetchError,Fetcher};
use axum::{body::Bytes,extract::State,http::{HeaderMap,StatusCode},response::{IntoResponse,Response},Json};
use serde::Deserialize;
use serde_json::{json,Value};
use std::time::Duration;

const ENDPOINT:&str="http://pdf-ocr:3000/extract";
const MAX_REDIRECTS:usize=3;

// Only public HTTPS, ordinary query parameters and the default TLS port.
// Fragments are client-side page selectors, not transmitted to the server.
pub(super) fn target(raw:&str)->Result<url::Url,StatusCode>{
    let mut target=render::public_url(raw).map_err(|_|StatusCode::FORBIDDEN)?;
    if target.scheme()!="https"||target.port().is_some(){return Err(StatusCode::BAD_REQUEST);}
    target.set_fragment(None);
    Ok(target)
}

fn next_target(current:&url::Url,res:&FetchResult)->Result<Option<url::Url>,StatusCode>{
    if res.final_url!=*current{return Err(StatusCode::BAD_GATEWAY);}
    if matches!(res.status,301|302|303|307|308){
        let location=res.headers.get("location").and_then(|v|v.to_str().ok()).ok_or(StatusCode::BAD_GATEWAY)?;
        if location.len()>8192{return Err(StatusCode::BAD_GATEWAY);}
        let next=current.join(location).map_err(|_|StatusCode::BAD_GATEWAY)?;
        return target(next.as_str()).map(Some);
    }
    if res.status!=200{
        // Preserve bounded upstream status diagnostics without returning its body.
        return Err(match res.status{
            401|403=>StatusCode::FORBIDDEN,404|410=>StatusCode::NOT_FOUND,
            429=>StatusCode::TOO_MANY_REQUESTS,_=>StatusCode::BAD_GATEWAY
        });
    }
    if !matches!(res.mime().as_deref(),Some("application/pdf"|"application/octet-stream"))||
        !res.body.starts_with(b"%PDF-"){
        return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }
    Ok(None)
}

async fn download(fetcher:&Fetcher,initial:&url::Url,budget:Duration)->Result<(url::Url,Bytes),StatusCode>{
    let mut current=target(initial.as_str())?;
    let mut seen=std::collections::HashSet::new();
    tokio::time::timeout(budget.min(Duration::from_secs(25)),async{
        for hop in 0..=MAX_REDIRECTS{
            if !seen.insert(current.clone()){return Err(StatusCode::BAD_GATEWAY);}
            render::check_dns(&current).await.map_err(|_|StatusCode::FORBIDDEN)?;
            let res=fetcher.fetch_once_limited(&current,&FetchOptions{
                timeout:Some(Duration::from_secs(20)),..Default::default()
            },MAX_PDF).await.map_err(|e|match e{
                FetchError::Blocked(_)=>StatusCode::FORBIDDEN,
                FetchError::TooLarge=>StatusCode::PAYLOAD_TOO_LARGE,
                FetchError::Timeout=>StatusCode::GATEWAY_TIMEOUT,
                _=>StatusCode::BAD_GATEWAY
            })?;
            match next_target(&current,&res)?{
                Some(next) if hop<MAX_REDIRECTS=>current=next,
                Some(_)=>return Err(StatusCode::BAD_GATEWAY),
                None=>return Ok((current,res.body)),
            }
        }
        Err(StatusCode::BAD_GATEWAY)
    }).await.map_err(|_|StatusCode::GATEWAY_TIMEOUT)?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PdfRequest{url:String}

async fn ocr_at(endpoint:&str,pdf:Bytes)->Result<Value,StatusCode>{
    if pdf.len()>MAX_PDF||!pdf.starts_with(b"%PDF-"){
        return Err(StatusCode::BAD_REQUEST);
    }
    let client=reqwest::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(85)).build().map_err(|_|StatusCode::BAD_GATEWAY)?;
    let mut res=client.post(endpoint).header("content-type","application/pdf").body(pdf)
        .send().await.map_err(|_|StatusCode::BAD_GATEWAY)?;
    match res.status(){
        StatusCode::OK=>{},
        StatusCode::TOO_MANY_REQUESTS=>return Err(StatusCode::TOO_MANY_REQUESTS),
        StatusCode::UNPROCESSABLE_ENTITY=>return Err(StatusCode::UNPROCESSABLE_ENTITY),
        _=>return Err(StatusCode::BAD_GATEWAY)
    }
    let mut bytes=Vec::new();
    while let Some(chunk)=res.chunk().await.map_err(|_|StatusCode::BAD_GATEWAY)?{
        if chunk.len()>MAX_REPLY+1-bytes.len(){return Err(StatusCode::PAYLOAD_TOO_LARGE);}
        bytes.extend_from_slice(&chunk);
    }
    parse_reply(&bytes).map_err(|_|StatusCode::BAD_GATEWAY)
}

// Stage-specific errors carry only fixed codes and HTTP status, never URLs,
// response bodies, credentials or parser stderr.
pub(super) async fn document(state:&CandidateState,target:&url::Url,budget:Duration)
    ->Result<(url::Url,Value),(StatusCode,&'static str)>{
    if !state.pdf_enabled{return Err((StatusCode::SERVICE_UNAVAILABLE,"PDF_WORKER_DISABLED"));}
    let deadline=tokio::time::Instant::now()+budget;
    let (landed,bytes)=download(&state.crawler.fetcher,target,budget).await.map_err(|s|(s,"PDF_DOWNLOAD_FAILED"))?;
    let receipt=tokio::time::timeout_at(deadline,ocr_at(ENDPOINT,bytes)).await
        .map_err(|_|(StatusCode::GATEWAY_TIMEOUT,"PDF_WORKER_TIMEOUT"))?
        .map_err(|s|(s,"PDF_WORKER_FAILED"))?;
    Ok((landed,receipt))
}

pub(super) fn v2_document(target:&url::Url,landed:&url::Url,receipt:Value)->Result<Value,StatusCode>{
    if receipt["unresolved_pages"]!=0{return Err(StatusCode::UNPROCESSABLE_ENTITY);}
    let pages=receipt["pages"].as_array().ok_or(StatusCode::BAD_GATEWAY)?;
    let mut text=String::new();
    for p in pages{
        let page=p["page"].as_u64().ok_or(StatusCode::BAD_GATEWAY)?;
        let body=p["text"].as_str().ok_or(StatusCode::BAD_GATEWAY)?;
        text.push_str(&format!("<!-- PDF page {page} -->\n{body}\n\n"));
    }
    if pages.iter().all(|p|p["text"].as_str().is_none_or(|s|s.trim().is_empty())){
        return Err(StatusCode::UNPROCESSABLE_ENTITY);
    }
    let warning=if receipt["ocr_attempted"]==0{
        "PDF parsed locally; page text and reading order are not independently verified."
    }else{
        "PDF includes OCR text; numbers, layout and completeness are not independently verified. See per-page evidence."
    };
    Ok(json!({"markdown":text,"metadata":{"url":landed.as_str(),"sourceURL":target.as_str(),
        "statusCode":200,"contentType":"application/pdf","numPages":pages.len()},
        "source":"local","researchSourceVerified":false,"warning":warning,"researchPdf":receipt}))
}

pub(super) async fn extract(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response{
    let req:PdfRequest=match decode(&headers,&body){Ok(r)=>r,Err(r)=>return r};
    let target=match target(&req.url){Ok(u)=>u,Err(s)=>return refusal(s,"DESTINATION_DENIED")};
    if !state.pdf_enabled{return refusal(StatusCode::SERVICE_UNAVAILABLE,"PDF_WORKER_DISABLED");}
    let Ok(_slot)=state.slots.try_acquire()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY")};
    match document(&state,&target,Duration::from_secs(110)).await{
        Ok((landed,data))=>{
            let complete=data["unresolved_pages"]==0;
            let value=json!({"success":complete,"source_verified":false,"url":landed.as_str(),"sourceURL":target.as_str(),"data":data});
            let mut response=bounded(value);
            if !complete && response.status().is_success(){*response.status_mut()=StatusCode::UNPROCESSABLE_ENTITY;}
            response
        },
        Err((status,code))=>refusal(status,code)
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    use axum::{Router,routing::post};
    #[test]
    fn v2_pdf_preserves_page_order_and_evidence(){
        let receipt=json!({"pages":[{"page":1,"source":"native","text":"Native 42","warning":null},
            {"page":2,"source":"ocr","text":"OCR 1.2","native_text":"","warning":"unverified"}],
            "unresolved_pages":0,"ocr_attempted":1,"source_verified":false});
        let target=url::Url::parse("https://example.org/test.pdf").unwrap();
        let doc=v2_document(&target,&target,receipt.clone()).unwrap();
        assert!(doc["markdown"].as_str().unwrap().contains("Native 42\n\n<!-- PDF page 2 -->\nOCR 1.2"));
        assert_eq!(doc["researchPdf"],receipt);
        assert_eq!(doc["metadata"]["numPages"],2);
        assert!(doc["warning"].as_str().unwrap().contains("OCR text"));
        let mut incomplete=receipt;
        incomplete["unresolved_pages"]=json!(1);
        assert_eq!(v2_document(&target,&target,incomplete).unwrap_err(),StatusCode::UNPROCESSABLE_ENTITY);
    }

    fn response(status:u16,url:&url::Url,mime:&str,body:&'static [u8],location:Option<&str>)->FetchResult{
        let mut headers=HeaderMap::new();
        if let Some(v)=location{headers.insert("location",v.parse().unwrap());}
        FetchResult{status,final_url:url.clone(),content_type:Some(mime.into()),body:Bytes::from_static(body),headers}
    }

    #[test]
    fn pdf_redirects_and_media_are_explicitly_guarded(){
        let url=target("https://example.org/download?id=1#page=2").unwrap();
        assert_eq!(url.as_str(),"https://example.org/download?id=1");
        for bad in ["http://example.org/a.pdf","https://example.org:8443/a.pdf",
            "https://127.0.0.1/a.pdf","https://u:p@example.org/a.pdf"]{assert!(target(bad).is_err());}
        let res=response(302,&url,"text/html",b"",Some("/file.pdf?version=1"));
        assert_eq!(next_target(&url,&res).unwrap().unwrap().as_str(),"https://example.org/file.pdf?version=1");
        for bad in ["//127.0.0.1/file","http://example.org/file","https://example.org:8443/file"]{
            let res=response(302,&url,"text/html",b"",Some(bad));
            assert!(next_target(&url,&res).is_err());
        }
        for mime in ["application/pdf","application/octet-stream"]{
            assert_eq!(next_target(&url,&response(200,&url,mime,b"%PDF-fixture",None)).unwrap(),None);
            assert_eq!(next_target(&url,&response(200,&url,mime,b"not pdf",None)).unwrap_err(),StatusCode::UNSUPPORTED_MEDIA_TYPE);
        }
        assert_eq!(next_target(&url,&response(200,&url,"text/html",b"%PDF-fixture",None)).unwrap_err(),StatusCode::UNSUPPORTED_MEDIA_TYPE);
        assert_eq!(next_target(&url,&response(403,&url,"text/html",b"blocked",None)).unwrap_err(),StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn fixed_worker_contract_rejects_invalid_result(){
        let app=Router::new().route("/extract",post(|headers:HeaderMap,body:Bytes|async move{
            assert_eq!(headers["content-type"],"application/pdf");
            assert_eq!(&body[..],b"%PDF-fixture");
            Json(json!({"source_verified":false,"layout":"auto","pages":[
                {"page":1,"text":"native","source":"native","warning":null}
            ],"pages_needing_ocr":[],"unresolved_pages":0,"ocr_attempted":0}))
        })).route("/bad",post(||async{Json(json!({"source_verified":true}))}));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base=format!("http://{}",listener.local_addr().unwrap());
        let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap();});
        assert!(ocr_at(&format!("{base}/extract"),Bytes::from_static(b"%PDF-fixture")).await.is_ok());
        assert_eq!(ocr_at(&format!("{base}/bad"),Bytes::from_static(b"%PDF-fixture")).await.unwrap_err(),StatusCode::BAD_GATEWAY);
        assert_eq!(ocr_at(&format!("{base}/extract"),Bytes::from_static(b"not PDF")).await.unwrap_err(),StatusCode::BAD_REQUEST);
        server.abort();
    }
}
