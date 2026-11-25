//! Candidate-side transport: download only with the upstream guarded fetcher;
//! all PDF parsing/rendering stays in the opt-in isolated OCR service.
#[path="pdf-result.rs"]
mod contract;
use contract::{MAX_PDF,MAX_REPLY,parse_reply};
use super::{CandidateState,decode,refusal,bounded,static_target,render};
use crate::fetch::FetchOptions;
use axum::{body::Bytes,extract::State,http::{HeaderMap,StatusCode},response::{IntoResponse,Response},Json};
use serde::Deserialize;
use serde_json::{json,Value};
use std::time::Duration;

const ENDPOINT:&str="http://pdf-ocr:3000/extract";

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

pub(super) async fn extract(State(state):State<CandidateState>,headers:HeaderMap,body:Bytes)->Response{
    let req:PdfRequest=match decode(&headers,&body){Ok(r)=>r,Err(r)=>return r};
    let target=match static_target(&req.url){Ok(u)=>u,Err(s)=>return refusal(s,"DESTINATION_DENIED")};
    if !state.pdf_enabled{return refusal(StatusCode::SERVICE_UNAVAILABLE,"PDF_WORKER_DISABLED");}
    let Ok(_slot)=state.slots.try_acquire()else{return refusal(StatusCode::TOO_MANY_REQUESTS,"CANDIDATE_BUSY")};
    let fetched=tokio::time::timeout(Duration::from_secs(25),async{
        render::check_dns(&target).await.map_err(|_|StatusCode::FORBIDDEN)?;
        let res=state.crawler.fetcher.fetch_once(&target,&FetchOptions{
            timeout:Some(Duration::from_secs(20)),..Default::default()
        }).await.map_err(|_|StatusCode::BAD_GATEWAY)?;
        if res.status!=200||res.final_url!=target||res.mime().as_deref()!=Some("application/pdf")||
            res.body.len()>MAX_PDF||!res.body.starts_with(b"%PDF-"){
            return Err(StatusCode::UNPROCESSABLE_ENTITY);
        }
        Ok::<_,StatusCode>(res.body)
    }).await;
    let bytes=match fetched{
        Ok(Ok(bytes))=>bytes,
        Ok(Err(s))=>return refusal(s,"PDF_FETCH_FAILED"),
        Err(_)=>return refusal(StatusCode::GATEWAY_TIMEOUT,"PDF_FETCH_TIMEOUT")
    };
    match ocr_at(ENDPOINT,bytes).await{
        Ok(data)=>{
            // A completed OCR operation is not verified source text.
            // Preserve per-page provenance; incomplete pages are explicit.
            let complete=data["unresolved_pages"]==0;
            let value=json!({"success":complete,"source_verified":false,"url":target.as_str(),"data":data});
            let mut response=bounded(value);
            if !complete && response.status().is_success(){*response.status_mut()=StatusCode::UNPROCESSABLE_ENTITY;}
            response
        },
        Err(status)=>refusal(status,"PDF_WORKER_FAILED")
    }
}

#[cfg(test)]
mod tests{
    use super::*;
    use axum::{Router,routing::post};
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
