//! Offline PDF OCR worker. No listener, URL, caller-controlled executable,
//! filesystem path or provider. Run in a disposable network-none sandbox.
#[path="../pdf-result.rs"]
mod contract;
use contract::{office_magic,MAX_OFFICE_MARKDOWN};
use serde_json::{json, Value};
use std::{io::{self, Read}, process::Stdio, time::Duration};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, process::Command};

const MAX_PDF: usize = 32 * 1024 * 1024;
const MAX_PGM: usize = 5 * 1024 * 1024;
const MAX_TEXT: usize = 256 * 1024;
const MAX_OUTPUT: usize = 2 * 1024 * 1024;
const MAX_OCR_PAGES: usize = 4;
const MAX_PAGES: usize = 200;

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

async fn read_page(pdf: &[u8], number: usize, single_block: bool) -> Result<String, &'static str> {
    let page = number.to_string();
    let pgm = run_fixed("/usr/bin/pdftoppm",
        &["-f", &page, "-l", &page, "-scale-to", "2200", "-singlefile", "-gray", "-"],
        pdf, MAX_PGM, Duration::from_secs(15)).await?;
    if !pgm.starts_with(b"P5") { return Err("invalid rendered image"); }
    let psm = if single_block { "6" } else { "3" };
    let text = run_fixed("/usr/bin/tesseract", &["stdin", "stdout", "-l", "eng", "--psm", psm],
        &pgm, MAX_TEXT, Duration::from_secs(15)).await?;
    String::from_utf8(text).map_err(|_| "OCR output not UTF-8")
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

async fn extract(pdf: &[u8], single_block: bool) -> Result<Value, &'static str> {
    if pdf.len() > MAX_PDF || !pdf.starts_with(b"%PDF-") {return Err("invalid PDF or input exceeds bound");}
    let parsed = pdf_inspector::extract_pages_markdown_mem(pdf, None)
        .map_err(|_| "PDF parsing failed")?;
    if parsed.pages.len() > MAX_PAGES || parsed.pages.is_empty() {return Err("PDF page budget exceeded");}
    let classified: Vec<_> = parsed.pages.iter().filter(|p| p.needs_ocr).map(|p| p.page + 1).collect();
    if parsed.pages_needing_ocr != classified {return Err("inconsistent OCR classification");}
    let mut output = Vec::new();
    let mut attempted = 0;
    // Bound the entire OCR stage, not just each renderer/recognizer call.
    // Classification itself is bounded by the external sandbox wall clock.
    let end = tokio::time::Instant::now() + Duration::from_secs(45);
    for (i, p) in parsed.pages.iter().enumerate() {
        if p.page as usize != i || p.markdown.len() > MAX_TEXT {return Err("invalid page order or page text too large");}
        let native = p.markdown.as_str();
        let result = if !p.needs_ocr {None}
            else if attempted >= MAX_OCR_PAGES || tokio::time::Instant::now() >= end {
                Some(Err("OCR page budget exceeded"))
            } else {
                attempted += 1;
                Some(tokio::time::timeout_at(end, read_page(pdf, i + 1, single_block))
                    .await.unwrap_or(Err("OCR total deadline exceeded")))
            };
        output.push(page_value(i+1,native,result));
    }
    let unresolved = output.iter().filter(|p|p["source"]=="unresolved").count();
    let result = json!({"pages":output,"pages_needing_ocr":classified,
        "ocr_attempted":attempted,"unresolved_pages":unresolved,
        "source_verified":false,"layout":if single_block {"single_block"} else {"auto"}});
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
    let single_block = match args.as_slice() {
        [] => false,
        [arg] if arg == "--single-block-fixture" => true,
        [arg] if arg == "--office" => false,
        _ => return Err("unsupported OCR argument"),
    };
    let mut input=Vec::new();
    io::stdin().take((MAX_PDF+1) as u64).read_to_end(&mut input).map_err(|_|"PDF input failed")?;
    if office{println!("{}",extract_office(&input)?);}
    else{println!("{}", extract(&input,single_block).await?);}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
