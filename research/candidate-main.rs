#[tokio::main]
async fn main() -> Result<(), String> {
    firecrawl_local::serve::run_candidate().await
}
