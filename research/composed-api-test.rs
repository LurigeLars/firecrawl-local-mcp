//! In-process HTTP composition experiment. Only built as a test harness.
//! Keep existing gateway and upstream authentication in the eventual deployment.
//! No production listener, API keys, cloud routes or arbitrary backend inputs.
#[path = "local-search-probe.rs"]
mod search;
#[path = "local-render-probe.rs"]
mod render;

use axum::{extract::{DefaultBodyLimit, State}, http::StatusCode,
    response::{IntoResponse, Response}, routing::{get, post}, Json, Router};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio::sync::Semaphore;

#[derive(Clone)]
struct StateData {
    search_endpoint: String,
    render_endpoint: String,
    permits: Arc<Semaphore>,
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"success":false,"error":message}))).into_response()
}

// Internal research endpoints only: strict typed decoding rejects unknown
// options rather than approximating Firecrawl's full compatibility contract.
async fn search_handler(State(state): State<StateData>, Json(body): Json<Value>) -> Response {
    let Ok(req) = serde_json::from_value::<search::Request>(body) else {
        return error(StatusCode::BAD_REQUEST, "unsupported search request");
    };
    if req.validate().is_err() {
        return error(StatusCode::BAD_REQUEST, "invalid search request");
    }
    let Ok(_permit) = state.permits.try_acquire() else {
        return error(StatusCode::TOO_MANY_REQUESTS, "candidate busy");
    };
    match search::search_at(&state.search_endpoint,req).await {
        Ok(data) => Json(data).into_response(),
        Err(_) => error(StatusCode::BAD_GATEWAY,"search failed"),
    }
}

async fn render_handler(State(state): State<StateData>, Json(body): Json<Value>) -> Response {
    let Ok(req) = serde_json::from_value::<render::Request>(body) else {
        return error(StatusCode::BAD_REQUEST,"unsupported render request");
    };
    let Ok(_permit) = state.permits.try_acquire() else {
        return error(StatusCode::TOO_MANY_REQUESTS, "candidate busy");
    };
    match render::render_at(&state.render_endpoint,req,Duration::from_secs(10)).await {
        Ok(data) => Json(data).into_response(),
        Err("destination denied") => error(StatusCode::FORBIDDEN,"destination denied"),
        Err(_) => error(StatusCode::BAD_GATEWAY,"render failed"),
    }
}

fn candidate(state: StateData) -> Router {
    Router::new()
        .route("/health",get(|| async { Json(json!({"status":"ok","research":true})) }))
        .route("/research/search",post(search_handler))
        .route("/research/render",post(render_handler))
        .layer(DefaultBodyLimit::max(32*1024))
        .with_state(state)
}

// This file intentionally has no executable listener. The actual API and
// auth gate will be integrated into upstream serve.rs only after this test.
fn main() {}

#[cfg(test)]
mod integration {
    use super::*;

    async fn spawn(router: Router) -> (String,tokio::task::JoinHandle<()>) {
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url=format!("http://{}",listener.local_addr().unwrap());
        let task=tokio::spawn(async move { axum::serve(listener,router).await.unwrap(); });
        (url,task)
    }

    #[tokio::test]
    async fn composed_api_calls_same_adapters_and_fails_closed() {
        // Synthetic responses only. No public network requests.
        let backend=Router::new()
            .route("/search",get(|| async { Json(json!({"results":[
                {"url":"https://93.184.216.34/report","title":"Stock report","content":"Weekly data"}
            ]})) }))
            .route("/scrape",post(|Json(body):Json<Value>| async move {
                assert_eq!(body["url"],"https://93.184.216.34/report");
                Json(json!({"content":"<html><body><h1>Stock report</h1><p>421.5 million barrels</p></body></html>",
                    "pageStatusCode":200,"contentType":"text/html","url":body["url"]}))
            }));
        let (base,backend_task)=spawn(backend).await;
        let permits=Arc::new(Semaphore::new(1));
        let (api,api_task)=spawn(candidate(StateData{
            search_endpoint:format!("{base}/search"),
            render_endpoint:format!("{base}/scrape"),
            permits:permits.clone()
        })).await;
        let client=reqwest::Client::builder().no_proxy()
            .redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(5)).build().unwrap();
        let found:Value=client.post(format!("{api}/research/search")).json(&json!({"query":"stock report"}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        let target=found["data"]["web"][0]["url"].as_str().unwrap();
        let doc:Value=client.post(format!("{api}/research/render")).json(&json!({"url":target,"onlyMainContent":false}))
            .send().await.unwrap().error_for_status().unwrap().json().await.unwrap();
        assert!(doc["data"]["markdown"].as_str().unwrap().contains("421.5 million barrels"));
        assert_eq!(doc["research"]["source_verified"],false);
        let bad=client.post(format!("{api}/research/render")).json(&json!({"url":"http://127.0.0.1/","onlyMainContent":false}))
            .send().await.unwrap();
        assert_eq!(bad.status(),StatusCode::FORBIDDEN);
        let bad=client.post(format!("{api}/research/search")).json(&json!({"query":"x","endpoint":base}))
            .send().await.unwrap();
        assert_eq!(bad.status(),StatusCode::BAD_REQUEST);
        let held=permits.acquire().await.unwrap();
        let busy=client.post(format!("{api}/research/search")).json(&json!({"query":"x"}))
            .send().await.unwrap();
        assert_eq!(busy.status(),StatusCode::TOO_MANY_REQUESTS);
        drop(held);
        let too_large=client.post(format!("{api}/research/search")).json(&json!({"query":"x".repeat(40*1024)}))
            .send().await.unwrap();
        assert_eq!(too_large.status(),StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(client.get(format!("{api}/v2/scrape")).send().await.unwrap().status(),StatusCode::NOT_FOUND);
        api_task.abort(); backend_task.abort();
    }
}
