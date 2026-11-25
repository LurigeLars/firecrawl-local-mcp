//! Private OCR sidecar research server. Must be PID 1 in its own restricted
//! Linux container, on an internal-only network, with no host mounts or secrets.
//! No URL fetches, file paths, shell, arbitrary arguments or provider access.
#[path="../pdf-result.rs"]
mod contract;
use contract::{MAX_PDF,MAX_REPLY,parse_reply};
use axum::{Router,body::to_bytes,extract::{Request,State},http::StatusCode,
    response::{IntoResponse,Response},routing::{get,post},Json};
use serde_json::{json,Value};
use std::{process::Stdio,sync::Arc,time::Duration};
use tokio::{io::{AsyncReadExt,AsyncWriteExt},process::Command,sync::Semaphore};

fn failure(status:StatusCode,code:&str)->Response{
    (status,Json(json!({"success":false,"error":code}))).into_response()
}

// A parser process may have native children. When cleanup is ambiguous, stop
// container PID 1; the private PID namespace is torn down rather than serving
// another request with potentially orphaned descendants.
fn fail_stop()->!{
    eprintln!("OCR sandbox stopped after ambiguous worker failure");
    std::process::exit(1)
}

async fn worker(pdf:&[u8])->Result<Value,StatusCode>{
    let mut child=Command::new("/usr/local/bin/pdf-ocr-worker")
        .env_clear().env("PATH","/usr/bin:/bin").env("HOME","/tmp")
        .env("LC_ALL","C").env("OMP_THREAD_LIMIT","1")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .kill_on_drop(true).spawn().map_err(|_|StatusCode::SERVICE_UNAVAILABLE)?;
    let Some(mut stdin)=child.stdin.take() else{fail_stop()};
    let Some(stdout)=child.stdout.take() else{fail_stop()};
    let result=tokio::time::timeout(Duration::from_secs(65),async{
        let write=async move{
            stdin.write_all(pdf).await?;
            stdin.shutdown().await?;
            drop(stdin);
            Ok::<_,std::io::Error>(())
        };
        let read=async{
            let mut out=Vec::new();
            stdout.take((MAX_REPLY+2) as u64).read_to_end(&mut out).await?;
            if out.len()>MAX_REPLY+1{
                return Err(std::io::Error::other("reply exceeds bound"));
            }
            Ok::<_,std::io::Error>(out)
        };
        let (_,out)=tokio::try_join!(write,read)?;
        let status=child.wait().await?;
        Ok::<_,std::io::Error>((status,out))
    }).await;
    let (status,out)=match result{
        Ok(Ok(v))=>v,
        _=>fail_stop()
    };
    if !status.success(){
        // The worker's ordinary Result error is exit 1; a panic/signal is
        // ambiguous and must stop the isolated container.
        if status.code()==Some(1){return Err(StatusCode::UNPROCESSABLE_ENTITY);}
        fail_stop();
    }
    parse_reply(&out).map_err(|_|StatusCode::BAD_GATEWAY)
}

async fn extract(State(slots):State<Arc<Semaphore>>,req:Request)->Response{
    if req.headers().get("content-type").and_then(|v|v.to_str().ok())!=Some("application/pdf"){
        return failure(StatusCode::UNSUPPORTED_MEDIA_TYPE,"PDF_REQUIRED");
    }
    let Ok(permit)=slots.try_acquire_owned()else{
        return failure(StatusCode::TOO_MANY_REQUESTS,"OCR_BUSY");
    };
    let pdf=match tokio::time::timeout(Duration::from_secs(15),to_bytes(req.into_body(),MAX_PDF)).await{
        Ok(Ok(bytes)) if bytes.starts_with(b"%PDF-")=>bytes,
        Ok(Ok(_))=>return failure(StatusCode::BAD_REQUEST,"INVALID_PDF"),
        Ok(Err(_))=>return failure(StatusCode::PAYLOAD_TOO_LARGE,"PDF_BODY_REJECTED"),
        Err(_)=>return failure(StatusCode::REQUEST_TIMEOUT,"PDF_BODY_TIMEOUT")
    };
    // The bounded task owns the permit even if the HTTP caller disconnects.
    let result=tokio::spawn(async move{
        let _permit=permit;
        worker(&pdf).await
    }).await;
    match result{
        Ok(Ok(value))=>Json(value).into_response(),
        Ok(Err(status))=>failure(status,"OCR_WORKER_REJECTED"),
        Err(_)=>fail_stop()
    }
}

#[tokio::main]
async fn main()->Result<(),String>{
    if std::process::id()!=1{return Err("OCR server must run as container PID 1".into());}
    let app=Router::new()
        .route("/health",get(||async{Json(json!({"status":"ok","research":true}))}))
        .route("/extract",post(extract))
        .with_state(Arc::new(Semaphore::new(1)));
    let listener=tokio::net::TcpListener::bind("0.0.0.0:3000").await.map_err(|e|e.to_string())?;
    axum::serve(listener,app).await.map_err(|e|e.to_string())
}
