// Fixed local diagnostic, distinct from SDK acceptance. Run ONLY inside
// isolated canary MCP. Existing helper scopes the token to internal API.
import { candidateApiEnvironment } from '/app/mcp-api-env.mjs';
const api='http://api:3002';
const result={verdict:'UNVERIFIED',scope:'fixed canary API search diagnostic; not MCP transport'};
const reasons=new Set(['not_attempted','ok','no_hits','provider_challenge','parse_failed','provider_unavailable','redirect_refused','unexpected_destination','unexpected_content_type','unexpected_http_status','destination_denied','response_too_large','timeout','transport_failed','transport_unverified','deadline_exhausted','unsupported_locale','unavailable']);
try {
  if(process.env.FIRECRAWL_API_URL!==api||!process.env.FIRECRAWL_API_KEY_FILE)throw Error();
  const env=candidateApiEnvironment(process.env,api,api);
  const response=await fetch(api+'/v2/search',{
    method:'POST',headers:{authorization:'Bearer '+env.FIRECRAWL_API_KEY,'content-type':'application/json'},
    body:JSON.stringify({query:'example domains RFC 2606',limit:2,sources:['web'],includeDomains:['iana.org'],timeout:60000}),
    redirect:'error',signal:AbortSignal.timeout(65000)
  });
  result.http_status=response.status;
  if(response.status!==200||!response.body)throw Error();
  const reader=response.body.getReader(),parts=[];
  let size=0;
  try {
    for(;;){const {value,done}=await reader.read();if(done)break;size+=value.byteLength;if(size>2*1024*1024)throw Error();parts.push(value);}
  }finally{await reader.cancel().catch(()=>{});}
  const body=JSON.parse(Buffer.concat(parts).toString('utf8'));
  if(body.success!==true||!body.research||!Array.isArray(body.data?.web))throw Error();
  const meta=body.research;
  if(!['local-searxng','local-duckduckgo'].includes(meta.source))throw Error();
  const diagnostics={source:meta.source};
  for(const k of ['returned','omitted','pages_read','proxy_hits_observed','engine_failure_reports','duplicate_hits','fallback_omitted']){
    if(meta[k]===undefined&&k==='fallback_omitted')continue;
    if(!Number.isSafeInteger(meta[k])||meta[k]<0)throw Error();
    diagnostics[k]=meta[k];
  }
  const reason=meta.fallback_status??'not_attempted';
  if(!reasons.has(reason))throw Error();
  diagnostics.fallback_status=reason;
  if(meta.fallback_http_status!==undefined){
    if(!Number.isInteger(meta.fallback_http_status)||meta.fallback_http_status<100||meta.fallback_http_status>599)throw Error();
    diagnostics.fallback_http_status=meta.fallback_http_status;
  }
  result.diagnostics=diagnostics;
  const hits=body.data.web;
  if(hits.length!==diagnostics.returned||hits.length>2)throw Error();
  for(const h of hits){const host=new URL(h.url).hostname;if(h.source!=='web'||!(host==='iana.org'||host.endsWith('.iana.org')))throw Error();}
  if(new Set(hits.map(h=>h.url)).size!==hits.length)throw Error();
  if(hits.length>0)result.verdict='PASS';
}catch{result.reason='diagnostic_unavailable_or_invalid';}
console.log(JSON.stringify(result));
if(result.verdict!=='PASS')process.exitCode=1;
