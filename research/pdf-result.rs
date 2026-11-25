//! Shared bounded PDF worker response contract.
use serde_json::Value;
pub(crate) const MAX_PDF:usize=32*1024*1024;
pub(crate) const MAX_REPLY:usize=2*1024*1024;

pub(crate) fn parse_reply(bytes:&[u8])->Result<Value,&'static str>{
    if bytes.len()>MAX_REPLY+1{return Err("OCR reply exceeds bound");}
    let value:Value=serde_json::from_slice(bytes).map_err(|_|"invalid OCR reply")?;
    if value["source_verified"]!=false || value["layout"]!="auto"{
        return Err("unexpected OCR verification or layout claim");
    }
    let pages=value["pages"].as_array().ok_or("missing PDF pages")?;
    if pages.is_empty()||pages.len()>200{return Err("invalid page count");}
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
        ],"pages_needing_ocr":[2,3],"ocr_attempted":2,"unresolved_pages":1});
        let encode=|v:&Value|serde_json::to_vec(v).unwrap();
        assert!(parse_reply(&encode(&value)).is_ok());
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
