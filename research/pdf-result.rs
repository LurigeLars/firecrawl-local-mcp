//! Shared bounded PDF worker response contract.
use serde_json::Value;
pub(crate) const MAX_PDF:usize=32*1024*1024;
pub(crate) const MAX_REPLY:usize=2*1024*1024;
pub(crate) const MAX_OFFICE_MARKDOWN:usize=512*1024;
pub(crate) const MAX_PAGES:usize=200;

pub(crate) fn office_magic(bytes:&[u8])->bool{
    bytes.starts_with(b"PK\x03\x04")||
        bytes.starts_with(&[0xD0,0xCF,0x11,0xE0,0xA1,0xB1,0x1A,0xE1])||
        bytes.starts_with(b"{\\rtf")
}

pub(crate) fn parse_office_reply(bytes:&[u8])->Result<Value,&'static str>{
    if bytes.len()>MAX_REPLY+1{return Err("office reply exceeds bound");}
    let v:Value=serde_json::from_slice(bytes).map_err(|_|"invalid office reply")?;
    if v.as_object().is_none_or(|o|o.len()!=3)||v["source_verified"]!=false||
        v["parser"]!="upstream-anydoc"||
        v["markdown"].as_str().is_none_or(|s|s.trim().is_empty()||s.len()>MAX_OFFICE_MARKDOWN){
        return Err("invalid office receipt");
    }
    Ok(v)
}

pub(crate) fn parse_reply(bytes:&[u8])->Result<Value,&'static str>{
    if bytes.len()>MAX_REPLY+1{return Err("OCR reply exceeds bound");}
    let value:Value=serde_json::from_slice(bytes).map_err(|_|"invalid OCR reply")?;
    if value["source_verified"]!=false || value["layout"]!="auto"{
        return Err("unexpected OCR verification or layout claim");
    }
    let pages=value["pages"].as_array().ok_or("missing PDF pages")?;
    let total=value["total_pages"].as_u64().filter(|n|(1..=MAX_PAGES as u64).contains(n)).ok_or("invalid total pages")?;
    let bound=match value.get("requested_max_pages"){
        Some(Value::Null)=>total,
        Some(v)=>v.as_u64().filter(|n|(1..=MAX_PAGES as u64).contains(n)).ok_or("invalid page bound")?,
        None=>return Err("missing page bound")
    };
    if pages.is_empty()||pages.len() as u64!=total.min(bound){return Err("invalid page count");}
    let mut classified=Vec::new();
    let mut unresolved=0u64;
    let mut recognized=0u64;
    for (i,page) in pages.iter().enumerate(){
        if page["page"].as_u64()!=Some((i+1) as u64)||page["text"].as_str().is_none(){
            return Err("invalid PDF page");
        }
        match page["source"].as_str(){
            Some("native") if page["warning"].is_null()=>{},
            Some("ocr")|Some("unresolved")=>{
                if page["warning"].as_str().is_none_or(|s|s.trim().is_empty()){
                    return Err("missing OCR uncertainty");
                }
                classified.push(Value::from(i+1));
                if page["source"]=="unresolved"{unresolved+=1;}
                else{
                    recognized+=1;
                    if page["native_text"].as_str().is_none()||page["text"].as_str().unwrap_or("").trim().is_empty(){
                        return Err("missing OCR evidence");
                    }
                }
            },
            _=>return Err("unknown PDF page source")
        }
    }
    if value["pages_needing_ocr"].as_array()!=Some(&classified)||
        value["unresolved_pages"].as_u64()!=Some(unresolved)||
        value["ocr_attempted"].as_u64().is_none_or(|n|n<recognized||n>4||n>classified.len() as u64){
        return Err("inconsistent OCR accounting");
    }
    Ok(value)
}

#[cfg(test)]
mod tests{
    use super::*;
    use serde_json::json;
    #[test]
    fn validates_page_accounting_and_uncertainty(){
        let mut value=json!({"source_verified":false,"layout":"auto","pages":[
            {"page":1,"source":"native","text":"native","warning":null},
            {"page":2,"source":"ocr","text":"42","native_text":"","warning":"unverified"},
            {"page":3,"source":"unresolved","text":"","warning":"unresolved"}
        ],"pages_needing_ocr":[2,3],"ocr_attempted":2,"unresolved_pages":1,"total_pages":3,"requested_max_pages":null});
        let encode=|v:&Value|serde_json::to_vec(v).unwrap();
        assert!(parse_reply(&encode(&value)).is_ok());
        let mut limited=value.clone();limited["requested_max_pages"]=json!(1);
        assert!(parse_reply(&encode(&limited)).is_err());
        value["pages"][1]["warning"]=Value::Null;
        assert!(parse_reply(&encode(&value)).is_err());
        value["pages"][1]["warning"]=json!("unverified");
        value["source_verified"]=json!(true);
        assert!(parse_reply(&encode(&value)).is_err());
        value["source_verified"]=json!(false);
        value["ocr_attempted"]=json!(0);
        assert!(parse_reply(&encode(&value)).is_err());
    }
}
