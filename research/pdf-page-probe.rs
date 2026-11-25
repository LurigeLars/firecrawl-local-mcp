// Research probe: no renderer, filesystem or network access.
use serde_json::json;
use std::io::{self, Read};

fn main() -> Result<(), String> {
    let mut input = Vec::new();
    io::stdin().take(32 * 1024 * 1024 + 1).read_to_end(&mut input)
        .map_err(|e| e.to_string())?;
    if input.len() > 32 * 1024 * 1024 || !input.starts_with(b"%PDF-") {
        return Err("invalid PDF or input exceeds bound".into());
    }
    let result = pdf_inspector::extract_pages_markdown_mem(&input, None)
        .map_err(|e| e.to_string())?;
    println!("{}", json!({
        "pages_needing_ocr": result.pages_needing_ocr,
        "pages": result.pages.iter().map(|p| json!({
            "page": p.page + 1,
            "markdown": p.markdown,
            "needs_ocr": p.needs_ocr,
            "ocr_reason": p.ocr_reason
        })).collect::<Vec<_>>()
    }));
    Ok(())
}
