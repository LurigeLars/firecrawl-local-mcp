// Offline fixture adapter, same upstream path as the Windows render probe.
use firecrawl_local::{guard::{check_url, NetworkPolicy}, scrape::{process_html, Format, ScrapeOptions}};
use serde_json::{json, Value};
use std::io::{self, Read};
use url::Url;

fn main() -> Result<(), String> {
    let mut bytes = Vec::new();
    io::stdin().take(32 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > 32 * 1024 * 1024 { return Err("input too large".into()); }
    let input: Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let url = Url::parse(input["url"].as_str().ok_or("url required")?).map_err(|e| e.to_string())?;
    if url.scheme() != "https" { return Err("HTTPS required".into()); }
    check_url(&url, NetworkPolicy::PublicOnly).map_err(|e| e.to_string())?;
    let html = input["content"].as_str().filter(|v| !v.trim().is_empty()).ok_or("HTML required")?;
    let main = input["onlyMainContent"].as_bool().ok_or("onlyMainContent required")?;
    let opts = ScrapeOptions { only_main_content: main, ..Default::default() };
    let (data, _, _) = process_html(html.to_owned(), &url, &opts, &[Format::Markdown, Format::Links]).map_err(|e| e.to_string())?;
    println!("{}", json!({"markdown":data.get("markdown")}));
    Ok(())
}
