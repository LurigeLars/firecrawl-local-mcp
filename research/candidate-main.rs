// Match upstream's allocator for candidate runtime measurements.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<(), String> {
    firecrawl_local::serve::run_candidate().await
}
