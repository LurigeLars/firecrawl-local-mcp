// Actual configured model route, one fixed public IANA document. No new
// provider, no direct key access beyond the existing scoped token helper.
import {candidateApiEnvironment} from '/app/mcp-api-env.mjs';
const api='http://api:3002';
try {
  const env=candidateApiEnvironment(process.env,api,api);
  if(!process.env.FIRECRAWL_API_KEY_FILE||env.FIRECRAWL_API_URL!==api)throw Error();
  const schema={type:'object',properties:{rfc:{type:'integer'}},required:['rfc'],additionalProperties:false};
  const r=await fetch(api+'/v2/scrape',{method:'POST',redirect:'error',signal:AbortSignal.timeout(65000),
    headers:{authorization:'Bearer '+env.FIRECRAWL_API_KEY,'content-type':'application/json'},
    body:JSON.stringify({url:'https://www.iana.org/help/example-domains',formats:[{type:'json',prompt:'Extract the integer RFC number 2606 from the supplied document.',schema}],onlyMainContent:true,maxAge:0,storeInCache:false,timeout:60000})});
  if(r.status!==200||!r.body)throw Error();
  const reader=r.body.getReader(),chunks=[];let size=0;
  try{for(;;){const {value,done}=await reader.read();if(done)break;size+=value.byteLength;if(size>2*1024*1024)throw Error();chunks.push(value);}}
  finally{await reader.cancel().catch(()=>{});}
  const b=JSON.parse(Buffer.concat(chunks)),d=b.data,m=d?.researchModel;
  if(b.success!==true||d.json?.rfc!==2606||Object.keys(d.json).length!==1||
    !['gemini','ollama'].includes(m?.provider)||m.schema_validated!==true||
    m.provider_reported_complete!==true||m.input_proxy_truncated!==false)throw Error();
  console.log('RUST_LIVE_MODEL_PASS provider='+m.provider);
}catch{console.error('RUST_LIVE_MODEL_UNVERIFIED');process.exitCode=1;}
