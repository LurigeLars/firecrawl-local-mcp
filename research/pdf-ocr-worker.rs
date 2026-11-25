//! Offline PDF OCR worker. No listener, URL, caller-controlled executable,
//! filesystem path or provider. Run in a disposable network-none sandbox.
#[path="../pdf-result.rs"]
mod contract;
#[path="../pdf-ocr-layout.rs"]
mod layout;
use contract::{office_magic,PageRange,MAX_OFFICE_MARKDOWN,MAX_PAGES};
use serde_json::{json, Value};
use std::{io::{self, Read}, process::Stdio, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, process::Command};

const MAX_PDF: usize = 32 * 1024 * 1024;
const MAX_PGM: usize = 5 * 1024 * 1024;
const MAX_TEXT: usize = 256 * 1024;
const MAX_TSV: usize = 1024 * 1024;
const MAX_OUTPUT: usize = 2 * 1024 * 1024;
const MAX_OCR_PAGES: usize = 4;

async fn run_fixed(program: &str, args: &[&str], input: &[u8], cap: usize,
                   deadline: Duration) -> Result<Vec<u8>, &'static str> {
    let mut child = Command::new(program).args(args)
        .env_clear().env("PATH", "/usr/bin:/bin").env("LC_ALL", "C")
        .env("OMP_THREAD_LIMIT", "1").env("HOME", "/tmp")
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null())
        .kill_on_drop(true).spawn().map_err(|_| "OCR executable unavailable")?;
    let mut stdin = child.stdin.take().ok_or("missing subprocess stdin")?;
    let stdout = child.stdout.take().ok_or("missing subprocess stdout")?;
    let result = tokio::time::timeout(deadline, async {
        // Concurrent pipes avoid deadlock if the renderer writes before it
        // has finished reading. Never buffer more than the output limit.
        let write = async move {
            stdin.write_all(input).await.map_err(|_| "OCR input failed")?;
            stdin.shutdown().await.map_err(|_| "OCR input close failed")?;
            drop(stdin);
            Ok::<_, &'static str>(())
        };
        let read = async {
            let mut data = Vec::new();
            stdout.take((cap + 1) as u64).read_to_end(&mut data).await
                .map_err(|_| "OCR output failed")?;
            if data.len() > cap { return Err("OCR output too large"); }
            Ok::<_, &'static str>(data)
        };
        let (_, data) = tokio::try_join!(write, read)?;
        let exit = child.wait().await.map_err(|_| "OCR process wait failed")?;
        if !exit.success() { return Err("OCR subprocess failed"); }
        Ok(data)
    }).await;
    match result {
        Ok(Ok(data)) => Ok(data),
        Ok(Err(e)) => { let _ = child.kill().await; Err(e) },
        Err(_) => { let _ = child.kill().await; Err("OCR subprocess deadline exceeded") },
    }
}

fn white_raster(pgm: &[u8]) -> bool {
    // Poppler emits an 8-bit PGM with a three-line header.
    let mut parts = pgm.splitn(4, |byte| *byte == b'\n');
    let (Some(b"P5"), Some(size), Some(b"255"), Some(pixels)) =
        (parts.next(), parts.next(), parts.next(), parts.next()) else { return false; };
    let dimensions = std::str::from_utf8(size).ok().and_then(|size| {
        let (width, height) = size.split_once(' ')?;
        width.parse::<usize>().ok()?.checked_mul(height.parse().ok()?)
    });
    !pixels.is_empty() && dimensions == Some(pixels.len()) && pixels.iter().all(|&byte| byte == 255)
}

enum RasterText {
    Blank,
    Recognized(String),
}

async fn read_page(pdf: &[u8], number: usize, single_block: bool, end: tokio::time::Instant)
    -> Result<RasterText, &'static str> {
    let page = number.to_string();
    let pgm = run_fixed("/usr/bin/pdftoppm",
        &["-f", &page, "-l", &page, "-scale-to", "2200", "-singlefile", "-gray", "-"],
        pdf, MAX_PGM, Duration::from_secs(15)).await?;
    if !pgm.starts_with(b"P5") { return Err("invalid rendered image"); }
    if white_raster(&pgm) { return Ok(RasterText::Blank); }
    let psm = if single_block { "6" } else { "3" };
    let text = run_fixed("/usr/bin/tesseract", &["stdin", "stdout", "-l", "eng", "--psm", psm],
        &pgm, MAX_TEXT, Duration::from_secs(15)).await?;
    let text = String::from_utf8(text).map_err(|_| "OCR output not UTF-8")?;
    if !single_block && layout::has_detached_numbers(&text) {
        let time = end.saturating_duration_since(tokio::time::Instant::now())
            .saturating_sub(Duration::from_millis(100)).min(Duration::from_secs(4));
        if !time.is_zero() {
            if let Ok(tsv) = run_fixed("/usr/bin/tesseract", &["stdin", "stdout", "-l", "eng", "--psm", psm, "tsv"],
                &pgm, MAX_TSV, time).await {
                if let Some(repaired) = std::str::from_utf8(&tsv).ok()
                    .and_then(|tsv| layout::restore_numeric_columns(&text, tsv)) {
                    return Ok(RasterText::Recognized(repaired));
                }
            }
        }
    }
    Ok(RasterText::Recognized(text))
}

fn page_value(page: usize, native: &str, ocr: Option<Result<String, &'static str>>) -> Value {
    match ocr {
        None => json!({"page":page,"text":native,"source":"native","warning":null}),
        Some(Ok(text)) if !text.trim().is_empty() => json!({
            "page":page,"text":text,"native_text":native,"source":"ocr",
            "warning":"OCR text not independently verified"
        }),
        _ => json!({"page":page,"text":native,"source":"unresolved",
            "warning":"OCR unavailable, empty, over budget or incomplete"}),
    }
}

async fn extract(pdf: &[u8], single_block: bool, max_pages:Option<usize>, range:Option<PageRange>) -> Result<Value, &'static str> {
    if pdf.len() > MAX_PDF || !pdf.starts_with(b"%PDF-") {return Err("invalid PDF or input exceeds bound");}
    let parsed = pdf_inspector::extract_pages_markdown_mem(pdf, None)
        .map_err(|_| "PDF parsing failed")?;
    if parsed.pages.len() > MAX_PAGES || parsed.pages.is_empty() {return Err("PDF page budget exceeded");}
    let classified: Vec<_> = parsed.pages.iter().filter(|p| p.needs_ocr).map(|p| p.page + 1).collect();
    if parsed.pages_needing_ocr != classified {return Err("inconsistent OCR classification");}
    // The pinned native parser performs document-wide analysis. Keep its
    // existing hard page cap, then apply the requested output/OCR bound.
    let total_pages=parsed.pages.len();
    let (start,end)=if let Some(range)=range{
        (range.start-1,range.end.min(total_pages))
    }else{(0,max_pages.unwrap_or(total_pages).min(total_pages))};
    let mut classified = Vec::new();
    let mut output = Vec::new();
    let mut attempted = 0;
    let mut blank = 0;
    // Bound the entire OCR stage, not just each renderer/recognizer call.
    // Classification itself is bounded by the external sandbox wall clock.
    let end = tokio::time::Instant::now() + Duration::from_secs(45);
    for (i, p) in parsed.pages.iter().take(end).enumerate().skip(start) {
        if p.page as usize != i || p.markdown.len() > MAX_TEXT {return Err("invalid page order or page text too large");}
        let native = p.markdown.as_str();
        let result = if !p.needs_ocr {None}
            else if attempted >= MAX_OCR_PAGES || tokio::time::Instant::now() >= end {
                Some(Err("OCR page budget exceeded"))
            } else {
                attempted += 1;
                match tokio::time::timeout_at(end, read_page(pdf, i + 1, single_block, end)).await {
                    Ok(Ok(RasterText::Blank)) if native.trim().is_empty() => {
                        blank += 1;
                        output.push(json!({"page":i+1,"text":"","source":"native","warning":null,"blank":true}));
                        continue;
                    }
                    Ok(Ok(RasterText::Recognized(text))) => Some(Ok(text)),
                    Ok(Ok(RasterText::Blank)) => Some(Err("blank raster conflicts with native text")),
                    Ok(Err(error)) => Some(Err(error)),
                    Err(_) => Some(Err("OCR total deadline exceeded")),
                }
            };
        if result.is_some() { classified.push(i+1); }
        output.push(page_value(i+1,native,result));
    }
    let unresolved = output.iter().filter(|p|p["source"]=="unresolved").count();
    let mut result = json!({"pages":output,"pages_needing_ocr":classified,
        "ocr_attempted":attempted - blank,"unresolved_pages":unresolved,
        "total_pages":total_pages,"requested_max_pages":max_pages,
        "source_verified":false,"layout":if single_block {"single_block"} else {"auto"}});
    if let Some(range)=range{result["requested_page_range"]=json!(range);}
    if serde_json::to_vec(&result).map_err(|_|"PDF result encoding failed")?.len()>MAX_OUTPUT {
        return Err("PDF output exceeds bound");
    }
    Ok(result)
}

// Use the pinned upstream Office parser only inside the same isolated
// worker container. Bound ZIP claims and actual decompression before parsing;
// the container memory/CPU/PID/deadline limits remain the real sandbox.
fn extract_office(input:&[u8])->Result<Value,&'static str>{
    if input.len()>MAX_PDF||!office_magic(input){return Err("invalid office input");}
    if input.starts_with(b"PK\x03\x04"){
        let mut archive=zip::ZipArchive::new(std::io::Cursor::new(input)).map_err(|_|"invalid document archive")?;
        if archive.len()>2048{return Err("document archive entry cap");}
        let mut remaining=128*1024*1024u64;
        let mut names=std::collections::HashSet::new();
        let mut buffer=[0u8;8192];
        for i in 0..archive.len(){
            let mut entry=archive.by_index(i).map_err(|_|"invalid document archive entry")?;
            let name=entry.name().map_err(|_|"invalid document archive name")?.into_owned();
            if entry.enclosed_name().is_none()||!names.insert(name){
                return Err("invalid or duplicate document archive path");
            }
            if entry.size()>remaining||entry.size()>32*1024*1024{
                return Err("document archive expansion cap");
            }
            let mut actual=0u64;
            loop{
                let n=entry.read(&mut buffer).map_err(|_|"document archive read failed")?;
                if n==0{break;}
                if n as u64>remaining{return Err("document archive expansion cap");}
                remaining-=n as u64;
                actual+=n as u64;
                if actual>32*1024*1024{return Err("document archive part cap");}
            }
            if actual!=entry.size(){return Err("document archive size mismatch");}
        }
    }
    let markdown=firecrawl_local::document::convert_document_bytes_to_markdown(input,None)
        .map_err(|_|"office parser failed")?;
    if markdown.trim().is_empty()||markdown.len()>MAX_OFFICE_MARKDOWN{return Err("empty or oversized office document");}
    let value=json!({"markdown":markdown,"parser":"upstream-anydoc","source_verified":false});
    if serde_json::to_vec(&value).map_err(|_|"office reply encoding")?.len()>MAX_OUTPUT{return Err("office reply exceeds bound");}
    Ok(value)
}

#[tokio::main]
async fn main() -> Result<(), &'static str> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let office=args.as_slice()==["--office"];
    let (single_block,max_pages,range) = match args.as_slice() {
        [] => (false,None,None),
        [arg] if arg == "--single-block-fixture" => (true,None,None),
        [arg] if arg == "--office" => (false,None,None),
        [flag,value] if flag=="--max-pages"=>(false,Some(value.parse::<usize>().ok()
            .filter(|n|(1..=MAX_PAGES).contains(n)).ok_or("invalid PDF page bound")?),None),
        [flag,value] if flag=="--page-range"=>(false,None,Some(PageRange::parse(value).ok_or("invalid PDF page range")?)),
        _ => return Err("unsupported OCR argument"),
    };
    let mut input=Vec::new();
    io::stdin().take((MAX_PDF+1) as u64).read_to_end(&mut input).map_err(|_|"PDF input failed")?;
    if office{println!("{}",extract_office(&input)?);}
    else{println!("{}", extract(&input,single_block,max_pages,range).await?);}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_a_complete_white_raster_is_blank() {
        assert!(white_raster(b"P5\n2 1\n255\n\xff\xff"));
        assert!(!white_raster(b"P5\n2 1\n255\n\xff\xfe"));
        assert!(!white_raster(b"P5\n2 1\n255\n\xff"));
        assert!(!white_raster(b"P5\n0 0\n255\n"));
    }

    #[test]
    fn unresolved_keeps_native_text_and_ocr_retains_separate_native_text() {
        assert_eq!(page_value(1,"native",None)["source"],"native");
        let failure=page_value(2,"partial",Some(Err("timeout")));
        assert_eq!(failure["source"],"unresolved");
        assert_eq!(failure["text"],"partial");
        assert!(failure["warning"].is_string());
        assert_eq!(page_value(3,"partial",Some(Ok("recognized".into())))["native_text"],"partial");
        assert_eq!(page_value(4,"partial",Some(Ok(" ".into())))["source"],"unresolved");
    }

    #[tokio::test]
    async fn bounded_child_rejects_timeout_and_excess_output() {
        let deadline=Duration::from_millis(50);
        assert_eq!(run_fixed("/usr/bin/sleep",&["10"],b"",32,deadline).await.unwrap_err(),
            "OCR subprocess deadline exceeded");
        assert_eq!(run_fixed("/usr/bin/printf",&["123456789"],b"",4,Duration::from_secs(2))
            .await.unwrap_err(),"OCR output too large");
    }
}
