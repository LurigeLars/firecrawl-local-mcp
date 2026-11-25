//! Candidate-side transport: download only with the upstream guarded fetcher;
//! all PDF parsing/rendering stays in the opt-in isolated OCR service.
#[path="pdf-result.rs"]
mod contract;
use contract::{MAX_PDF,MAX_REPLY,parse_reply,parse_office_reply,office_magic};
pub(super) use contract::{MAX_PAGES,PageRange};
use super::{CandidateState,decode,refusal,bounded,render};
use crate::fetch::{FetchOptions,FetchResult,FetchError,Fetcher};
use axum::{body::Bytes,extract::State,http::{HeaderMap,StatusCode},response::Response};
use serde::Deserialize;
use serde_json::{json,Value};
use std::time::Duration;

const ENDPOINT:&str="http://pdf-ocr:3000/extract";
const MAX_REDIRECTS:usize=3;
const OFFICE_ENDPOINT:&str="http://pdf-ocr:3000/extract-office";
#[derive(Clone,Copy,PartialEq,Eq,Debug)]
pub(super) enum Kind{Pdf,Office}

pub(super) fn office_url(url:&url::Url)->bool{
    let extension=url.path().rsplit('/').next().and_then(|p|p.rsplit_once('.')).map(|(_,e)|e.to_ascii_lowercase());
    extension.as_deref().is_some_and(|e|matches!(e,"docx"|"doc"|"xlsx"|"xls"|"pptx"|"ppt"|"odt"|"ods"|"odp"|"rtf"|"epub"))
}

fn media_kind(res:&FetchResult)->Option<Kind>{
    let mime=res.mime();
    if matches!(mime.as_deref(),Some("application/pdf"|"application/octet-stream"))&&res.body.starts_with(b"%PDF-"){
        return Some(Kind::Pdf);
    }
    if office_magic(&res.body)&&matches!(crate::documents::sniff(&res.body,mime.as_deref(),&res.final_url),
        Some(crate::documents::DocKind::Office(_)))&&
        mime.as_deref().is_some_and(|m|m!="text/html"&&m!="application/pdf"){
        return Some(Kind::Office);
    }
    None
}

// Only public HTTPS, ordinary query parameters and the default TLS port.
// Fragments are client-side page selectors, not transmitted to the server.
pub(super) fn target(raw:&str)->Result<url::Url,StatusCode>{
    let mut target=render::public_url(raw).map_err(|_|StatusCode::FORBIDDEN)?;
    if target.scheme()!="https"||target.port().is_some(){return Err(StatusCode::BAD_REQUEST);}
    target.set_fragment(None);
    Ok(target)
}

pub(super) fn requested_range(url:&url::Url,max_pages:Option<usize>)->Result<Option<PageRange>,Response>{
    let Some(raw)=url.fragment().and_then(|fragment|fragment.strip_prefix("page="))else{return Ok(None);};
    if max_pages.is_some(){
        return Err(super::super::error_response(StatusCode::BAD_REQUEST,"PDF_PAGE_SELECTION_CONFLICT",
            "Use the URL page range or maxPages as a prefix limit."));
    }
    PageRange::parse(raw).map(Some).ok_or_else(||super::super::error_response(
        StatusCode::BAD_REQUEST,"INVALID_PDF_PAGE_RANGE","Use #page=12 or #page=12-15 with page numbers from 1 to 200."))
}

#[cfg(test)]
fn next_target(current:&url::Url,res:&FetchResult)->Result<Option<url::Url>,StatusCode>{
    next_target_for(current,res,Some(Kind::Pdf))
}

fn next_target_for(current:&url::Url,res:&FetchResult,expected:Option<Kind>)->Result<Option<url::Url>,StatusCode>{
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
    let kind=media_kind(res).ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
    if expected.is_some_and(|e|e!=kind){return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE);}
    Ok(None)
}

async fn download(fetcher:&Fetcher,initial:&url::Url,budget:Duration,expected:Option<Kind>)->Result<(url::Url,Bytes,Kind),StatusCode>{
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
            match next_target_for(&current,&res,expected)?{
                Some(next) if hop<MAX_REDIRECTS=>current=next,
                Some(_)=>return Err(StatusCode::BAD_GATEWAY),
                None=>{
                    let kind=media_kind(&res).ok_or(StatusCode::UNSUPPORTED_MEDIA_TYPE)?;
                    return Ok((current,res.body,kind));
                },
            }
        }
        Err(StatusCode::BAD_GATEWAY)
    }).await.map_err(|_|StatusCode::GATEWAY_TIMEOUT)?
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PdfRequest{url:String}

async fn ocr_at(endpoint:&str,pdf:Bytes,max_pages:Option<usize>,range:Option<PageRange>)->Result<Value,StatusCode>{
    if pdf.len()>MAX_PDF||!pdf.starts_with(b"%PDF-")||max_pages.is_some_and(|n|!(1..=MAX_PAGES).contains(&n)){
        return Err(StatusCode::BAD_REQUEST);
    }
    let client=reqwest::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(85)).build().map_err(|_|StatusCode::BAD_GATEWAY)?;
    let mut request=client.post(endpoint).header("content-type","application/pdf").body(pdf);
    if let Some(n)=max_pages{request=request.header("x-firecrawl-max-pages",n.to_string());}
    if let Some(r)=range{request=request.header("x-firecrawl-page-range",format!("{}-{}",r.start,r.end));}
    let mut res=request.send().await.map_err(|_|StatusCode::BAD_GATEWAY)?;
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
    let receipt=parse_reply(&bytes).map_err(|_|StatusCode::BAD_GATEWAY)?;
    if receipt["requested_max_pages"]!=json!(max_pages)||receipt["requested_page_range"]!=json!(range){return Err(StatusCode::BAD_GATEWAY);}
    Ok(receipt)
}

// Stage-specific errors carry only fixed codes and HTTP status, never URLs,
// response bodies, credentials or parser stderr.
pub(super) async fn document(state:&CandidateState,target:&url::Url,budget:Duration,max_pages:Option<usize>,range:Option<PageRange>)
    ->Result<(url::Url,Value),(StatusCode,&'static str)>{
    if !state.pdf_enabled{return Err((StatusCode::SERVICE_UNAVAILABLE,"PDF_WORKER_DISABLED"));}
    let deadline=tokio::time::Instant::now()+budget;
    let (landed,bytes,_)=download(&state.crawler.fetcher,target,budget,Some(Kind::Pdf)).await.map_err(|s|(s,"PDF_DOWNLOAD_FAILED"))?;
    let receipt=tokio::time::timeout_at(deadline,ocr_at(ENDPOINT,bytes,max_pages,range)).await
        .map_err(|_|(StatusCode::GATEWAY_TIMEOUT,"PDF_WORKER_TIMEOUT"))?
        .map_err(|s|(s,"PDF_WORKER_FAILED"))?;
    Ok((landed,receipt))
}

async fn office_at(endpoint:&str,bytes:Bytes,budget:Duration)->Result<Value,StatusCode>{
    if bytes.len()>MAX_PDF||!office_magic(&bytes){return Err(StatusCode::BAD_REQUEST);}
    let client=reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
        .timeout(budget).build().map_err(|_|StatusCode::BAD_GATEWAY)?;
    let mut res=client.post(endpoint).header("content-type","application/octet-stream").body(bytes)
        .send().await.map_err(|_|StatusCode::BAD_GATEWAY)?;
    if res.status()!=StatusCode::OK{return Err(match res.status(){
        StatusCode::TOO_MANY_REQUESTS=>StatusCode::TOO_MANY_REQUESTS,
        StatusCode::UNPROCESSABLE_ENTITY=>StatusCode::UNPROCESSABLE_ENTITY,
        _=>StatusCode::BAD_GATEWAY
    });}
    let mut body=Vec::new();
    while let Some(chunk)=res.chunk().await.map_err(|_|StatusCode::BAD_GATEWAY)?{
        if chunk.len()>MAX_REPLY+1-body.len(){return Err(StatusCode::PAYLOAD_TOO_LARGE);}
        body.extend_from_slice(&chunk);
    }
    parse_office_reply(&body).map_err(|_|StatusCode::BAD_GATEWAY)
}

pub(super) async fn isolated_document(state:&CandidateState,target:&url::Url,budget:Duration,expected:Option<Kind>)
    ->Result<Value,Response>{
    if !state.pdf_enabled{return Err(refusal(StatusCode::SERVICE_UNAVAILABLE,"DOCUMENT_WORKER_DISABLED"));}
    let deadline=tokio::time::Instant::now()+budget;
    let (landed,bytes,kind)=download(&state.crawler.fetcher,target,budget,expected).await
        .map_err(|s|refusal(s,"DOCUMENT_DOWNLOAD_FAILED"))?;
    let remaining=deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero(){return Err(refusal(StatusCode::GATEWAY_TIMEOUT,"DOCUMENT_DEADLINE"));}
    match kind{
        Kind::Pdf=>{
            let range=requested_range(target,None)?;
            let receipt=tokio::time::timeout_at(deadline,ocr_at(ENDPOINT,bytes,None,range)).await
                .map_err(|_|refusal(StatusCode::GATEWAY_TIMEOUT,"PDF_WORKER_TIMEOUT"))?
                .map_err(|s|refusal(s,"PDF_WORKER_FAILED"))?;
            v2_document(target,&landed,receipt,range)
        },
        Kind::Office=>{
            let receipt=tokio::time::timeout_at(deadline,office_at(OFFICE_ENDPOINT,bytes,remaining)).await
                .map_err(|_|refusal(StatusCode::GATEWAY_TIMEOUT,"OFFICE_WORKER_TIMEOUT"))?
                .map_err(|s|refusal(s,"OFFICE_WORKER_FAILED"))?;
            let markdown=receipt["markdown"].as_str().ok_or_else(||refusal(StatusCode::BAD_GATEWAY,"OFFICE_WORKER_FAILED"))?;
            Ok(json!({"markdown":markdown,"metadata":{"url":landed.as_str(),"sourceURL":target.as_str(),"statusCode":200,"proxyUsed":"basic"},
                "source":"local","researchSourceVerified":false,"warning":"Office document parsed locally; layout, formulas and completeness are not independently verified.",
                "researchDocument":{"parser":"upstream-anydoc","source_verified":false}}))
        }
    }
}

pub(super) fn v2_document(target:&url::Url,landed:&url::Url,receipt:Value,range:Option<PageRange>)->Result<Value,Response>{
    let invalid=||refusal(StatusCode::BAD_GATEWAY,"PDF_WORKER_FAILED");
    let total=receipt["total_pages"].as_u64().ok_or_else(invalid)?;
    if let Some(r)=range.filter(|r|r.end as u64>total){
        return Err(super::super::error_response(StatusCode::RANGE_NOT_SATISFIABLE,"PDF_PAGE_RANGE_UNAVAILABLE",
            format!("PDF has {total} pages. Requested pages {}–{} exceed that range.",r.start,r.end)));
    }
    let pages=receipt["pages"].as_array().ok_or_else(invalid)?;
    if receipt["unresolved_pages"]!=0{
        let unread=pages.iter().filter(|p|p["source"]=="unresolved")
            .filter_map(|p|p["page"].as_u64().map(|n|n.to_string())).collect::<Vec<_>>().join(", ");
        return Err(super::super::error_response(StatusCode::UNPROCESSABLE_ENTITY,"PDF_INCOMPLETE",
            format!("PDF pages {unread} could not be read. Retry those pages in a smaller range using #page=12-15.")));
    }
    let mut text=String::new();
    for p in pages{
        let page=p["page"].as_u64().ok_or_else(invalid)?;
        let body=p["text"].as_str().ok_or_else(invalid)?;
        text.push_str(&format!("<!-- PDF page {page} -->\n{body}\n\n"));
    }
    if pages.iter().all(|p|p["text"].as_str().is_none_or(|s|s.trim().is_empty()))&&
        !(range.is_some()&&pages.iter().all(|p|p["blank"]==true)){
        return Err(refusal(StatusCode::UNPROCESSABLE_ENTITY,"PDF_INCOMPLETE"));
    }
    let warning=if receipt["ocr_attempted"]==0{
        "PDF parsed locally; page text and reading order are not independently verified."
    }else{
        "PDF includes OCR text; numbers, layout and completeness are not independently verified. See per-page evidence."
    };
    let warning=if let Some(r)=range{format!("{warning} Pages {}–{} of {total} returned.",r.start,r.end)}
        else if total>pages.len() as u64{format!("{warning} Page limit applied: {} of {total} pages returned.",pages.len())}else{warning.to_string()};
    Ok(json!({"markdown":text,"metadata":{"url":landed.as_str(),"sourceURL":target.as_str(),
        "statusCode":200,"contentType":"application/pdf","numPages":pages.len(),"totalPages":total,"proxyUsed":"basic"},
        "source":"local","researchSourceVerified":false,"warning":warning,"researchPdf":receipt}))
}

pub(super) async fn extract(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response{
    let req:PdfRequest=match decode(&headers,&body){Ok(r)=>r,Err(r)=>return r};
    let target=match target(&req.url){Ok(u)=>u,Err(s)=>return refusal(s,"DESTINATION_DENIED")};
    if !state.pdf_enabled{return refusal(StatusCode::SERVICE_UNAVAILABLE,"PDF_WORKER_DISABLED");}
    let Ok(_slot)=state.slots.try_acquire()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY")};
    match document(&state,&target,Duration::from_secs(110),None,None).await{
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

    #[tokio::test]
    async fn projects_selected_pages_and_names_unread_pages(){
        let source=url::Url::parse("https://example.org/test.pdf?version=2#page=12-13").unwrap();
        let range=requested_range(&source,None).unwrap();
        assert_eq!(range,Some(PageRange{start:12,end:13}));
        assert_eq!(target(source.as_str()).unwrap().as_str(),"https://example.org/test.pdf?version=2");
        assert_eq!(requested_range(&source,Some(2)).unwrap_err().status(),StatusCode::BAD_REQUEST);
        let receipt=json!({"pages":[
            {"page":12,"source":"native","text":"Table"},
            {"page":13,"source":"native","text":"Next"}
        ],"total_pages":20,"unresolved_pages":0,"ocr_attempted":0});
        let doc=v2_document(&source,&source,receipt.clone(),range).unwrap();
        assert!(doc["markdown"].as_str().unwrap().contains("<!-- PDF page 12 -->"));
        assert!(doc["markdown"].as_str().unwrap().contains("<!-- PDF page 13 -->"));
        assert_eq!(doc["metadata"]["numPages"],2);
        let mut incomplete=receipt.clone();
        incomplete["unresolved_pages"]=json!(1);
        incomplete["pages"][1]["source"]=json!("unresolved");
        let error=v2_document(&source,&source,incomplete,range).unwrap_err();
        assert_eq!(error.status(),StatusCode::UNPROCESSABLE_ENTITY);
        let body=axum::body::to_bytes(error.into_body(),2048).await.unwrap();
        let response:Value=serde_json::from_slice(&body).unwrap();
        assert_eq!(response["code"],"PDF_INCOMPLETE");
        assert!(response["error"].as_str().unwrap().contains("pages 13 could not be read"));
        let mut too_short=receipt;
        too_short["total_pages"]=json!(12);
        assert_eq!(v2_document(&source,&source,too_short,range).unwrap_err().status(),StatusCode::RANGE_NOT_SATISFIABLE);
    }
    use axum::{Router,routing::post};
    #[test]
    fn v2_pdf_preserves_page_order_and_evidence(){
        let receipt=json!({"pages":[{"page":1,"source":"native","text":"Native 42","warning":null},
            {"page":2,"source":"ocr","text":"OCR 1.2","native_text":"","warning":"unverified"}],
            "unresolved_pages":0,"ocr_attempted":1,"source_verified":false,"total_pages":2,"requested_max_pages":null});
        let target=url::Url::parse("https://example.org/test.pdf").unwrap();
        let doc=v2_document(&target,&target,receipt.clone(),None).unwrap();
        assert!(doc["markdown"].as_str().unwrap().contains("Native 42\n\n<!-- PDF page 2 -->\nOCR 1.2"));
        assert_eq!(doc["researchPdf"],receipt);
        assert_eq!(doc["metadata"]["numPages"],2);
        assert!(doc["warning"].as_str().unwrap().contains("OCR text"));
        let mut incomplete=receipt;
        incomplete["unresolved_pages"]=json!(1);
        assert_eq!(v2_document(&target,&target,incomplete,None).unwrap_err().status(),StatusCode::UNPROCESSABLE_ENTITY);
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

    #[test]
    fn office_media_magic_and_extensionless_detection(){
        let url=target("https://example.org/download?id=42").unwrap();
        let office=response(200,&url,"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",b"PK\x03\x04fixture",None);
        assert_eq!(media_kind(&office),Some(Kind::Office));
        assert!(next_target_for(&url,&office,Some(Kind::Office)).is_ok());
        assert!(next_target_for(&url,&office,Some(Kind::Pdf)).is_err());
        assert_eq!(media_kind(&response(200,&url,"text/html",b"PK\x03\x04fixture",None)),None);
        assert_eq!(media_kind(&response(200,&url,"application/zip",b"PK\x03\x04fixture",None)),None);
        assert_eq!(media_kind(&response(200,&url,"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",b"not zip",None)),None);
    }

    #[tokio::test]
    async fn fixed_worker_contract_rejects_invalid_result(){
        let app=Router::new().route("/extract",post(|headers:HeaderMap,body:Bytes|async move{
            assert_eq!(headers["content-type"],"application/pdf");
            assert_eq!(&body[..],b"%PDF-fixture");
            Json(json!({"source_verified":false,"layout":"auto","pages":[
                {"page":1,"text":"native","source":"native","warning":null}
            ],"pages_needing_ocr":[],"unresolved_pages":0,"ocr_attempted":0,"total_pages":1,"requested_max_pages":null}))
        })).route("/bad",post(||async{Json(json!({"source_verified":true}))}));
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base=format!("http://{}",listener.local_addr().unwrap());
        let server=tokio::spawn(async move{axum::serve(listener,app).await.unwrap();});
        assert!(ocr_at(&format!("{base}/extract"),Bytes::from_static(b"%PDF-fixture"),None,None).await.is_ok());
        assert_eq!(ocr_at(&format!("{base}/bad"),Bytes::from_static(b"%PDF-fixture"),None,None).await.unwrap_err(),StatusCode::BAD_GATEWAY);
        assert_eq!(ocr_at(&format!("{base}/extract"),Bytes::from_static(b"not PDF"),None,None).await.unwrap_err(),StatusCode::BAD_REQUEST);
        server.abort();
    }
}
