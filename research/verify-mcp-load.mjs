// Run inside the respective existing MCP container. The actual locked SDK
// handles its own credential, so this client never reads any secret file.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
const MCP='http://mcp:3000/mcp';
const modeArg=process.argv[2];
assert.ok(['legacy','candidate'].includes(modeArg));
const documentMode=process.argv[3]==='--documents';
const searchMode=process.argv[3]==='--search';
assert.ok(process.argv[3]===undefined||documentMode||searchMode,'unsupported workload');
const deadline=performance.now()+120000;
let session;
let version = '2025-06-18';
let nextId = 1;

async function rpc(method, params, notification = false) {
  const id = nextId++;
  const headers = {
    'content-type': 'application/json',
    accept: 'application/json, text/event-stream',
    ...(session ? {'mcp-session-id': session} : {}),
    'mcp-protocol-version': version,
  };
  const res = await fetch(MCP, {
    method: 'POST', headers, redirect: 'error',
    body: JSON.stringify({jsonrpc: '2.0', method, params, ...(!notification ? {id} : {})}),
    signal: AbortSignal.timeout(Math.min(45000,Math.max(1,Math.floor(deadline-performance.now())))),
  });
  session = res.headers.get('mcp-session-id') ?? session;
  if (notification) {
    assert.ok([200, 202, 204].includes(res.status), 'MCP notification failed');
    await res.body?.cancel();
    return;
  }
  assert.equal(res.status, 200, 'MCP HTTP request failed');
  const reader = res.body.getReader();
  const decoder = new TextDecoder();
  const sse = res.headers.get('content-type')?.includes('text/event-stream');
  let raw = '', bytes = 0;
  try {
    while (true) {
      const {done, value} = await reader.read();
      if (done) break;
      bytes += value.byteLength;
      assert.ok(bytes <= 2 * 1024 * 1024, 'MCP response exceeds fixture cap');
      raw += decoder.decode(value, {stream: true});
      if (sse) {
        const events = raw.split(/\r?\n\r?\n/);
        raw = events.pop();
        for (const event of events) {
          const data = event.split(/\r?\n/).filter(l => l.startsWith('data:')).map(l => l.slice(5).trimStart()).join('\n');
          if (!data) continue;
          const json = JSON.parse(data);
          if (json.id === id) return json;
        }
      }
    }
    if (!sse) {
      const json = JSON.parse(raw + decoder.decode());
      assert.equal(json.id, id);
      return json;
    }
    throw new Error('MCP response ended without a matching JSON-RPC message');
  } finally {
    await reader.cancel().catch(() => {});
  }
}


let init;
for (let i = 0; i < 30; i++) {
  try {
    init = await rpc('initialize', {protocolVersion:version,capabilities:{},clientInfo:{name:'local-paired-benchmark',version:'1'}});
    break;
  } catch (error) {
    if (error.cause?.code !== 'ECONNREFUSED' && error.cause?.code !== 'ENOTFOUND') throw error;
    await new Promise(resolve => setTimeout(resolve, 1000));
  }
}
assert.ok(init?.result?.serverInfo, 'MCP HTTP initialization unavailable');
version = init.result.protocolVersion ?? version;
await rpc('notifications/initialized', {}, true);
const list = await rpc('tools/list', {});
assert.ok(list.result?.tools?.some(tool=>tool.name==='firecrawl_scrape'));


// Separate document acceptance, not part of the ten-call HTML memory sample.
// URLs refer to existing pinned public synthetic fixtures. Never print content.
if(searchMode){
  assert.ok(list.result.tools.some(tool=>tool.name==='firecrawl_search'),'search unavailable');
  const started=performance.now();
  let reason='transport_or_envelope',hits=0;
  try{
    const result=await rpc('tools/call',{name:'firecrawl_search',arguments:{
      query:'example domains RFC 2606',includeDomains:['iana.org'],sources:['web'],limit:2,timeout:40000
    }});
    if(result.result&&!result.result.isError&&!result.error){
      const item=result.result.content?.find(x=>x.type==='text');
      const body=JSON.parse(item?.text??'null');
      const web=body?.data?.web;
      if(body?.success===true&&Array.isArray(web)){
        hits=web.length;
        const valid=web.every(hit=>{
          try{
            const u=new URL(hit.url);
            return ['https:','http:'].includes(u.protocol)&&!u.username&&!u.password&&
              (u.hostname==='iana.org'||u.hostname.endsWith('.iana.org'))&&hit.source==='web';
          }catch{return false;}
        });
        reason=hits===0?'no_hits':hits>2||!valid||new Set(web.map(h=>h.url)).size!==hits?'invalid_hits':'ok';
      }
    }
  }catch{/* Only fixed category, no raw remote payload. */}
  const passed=reason==='ok';
  console.log(JSON.stringify({verdict:passed?'PASS':'UNVERIFIED',backend:modeArg,
    scope:'fixed IANA-domain query through actual MCP HTTP; not comprehensive search coverage',
    reason,hits,elapsed_ms:Math.round((performance.now()-started)*10)/10}));
  if(modeArg==='candidate'&&!passed)process.exitCode=1;
} else if(documentMode){
  const documents=[
    ['PDF','https://raw.githubusercontent.com/LurigeLars/firecrawl-local-mcp/b5a1bddc489c1d39fc2e45ef19ea77f7040c449b/research/fixtures/pdf-transport-native.pdf',
      ['Firecrawl synthetic PDF transport fixture','Stock 421.5 million barrels','Weekly change -1.2 million barrels','September 8, 2026'],{parsers:['pdf']}],
    ['XLSX','https://raw.githubusercontent.com/LurigeLars/firecrawl-local-mcp/ae200f184f3a2cc9aad17a17b7e82a8eb6e1a424/research/fixtures/office-transport.xlsx',
      ['Stock','Change','421.5','-1.2'],{}]
  ];
  const results=[];
  for(const [source,url,markers,options] of documents){
    const started=performance.now();
    let reason='transport_or_envelope',found=0,bytes=0;
    try{
      const result=await rpc('tools/call',{name:'firecrawl_scrape',arguments:{
        url,formats:['markdown'],maxAge:0,storeInCache:false,timeout:40000,...options
      }});
      if(result.result&&!result.result.isError&&!result.error){
        const item=result.result.content?.find(x=>x.type==='text');
        const envelope=JSON.parse(item?.text??'null');
        const doc=envelope?.data??envelope;
        if(typeof doc?.markdown==='string'){
          const md=doc.markdown;
          bytes=Buffer.byteLength(md);
          const normalized=md.replace(/\\-/g,'-').replace(/\s+/g,' ').toLowerCase();
          found=markers.filter(marker=>normalized.includes(marker.toLowerCase())).length;
          // Legacy XLSX can expose raw ZIP bytes as Markdown even with HTTP
          // 200 and all markers. Reject controls/replacement/ZIP signature.
          const binary=/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f\ufffd]/u.test(md)||md.trimStart().startsWith('PK\u0003\u0004');
          reason=doc.metadata?.statusCode!==200||doc.metadata?.sourceURL!==url?'metadata_mismatch':
            binary?'binary_like_markdown':found!==markers.length?'missing_markers':'ok';
        }
      }
    }catch{/* Fixed failure category; no raw remote data or error text. */}
    results.push({source,verdict:reason==='ok'?'PASS':'UNVERIFIED',reason,
      markers:found,expected_markers:markers.length,markdown_bytes:bytes,
      elapsed_ms:Math.round((performance.now()-started)*10)/10});
  }
  const passed=results.every(r=>r.verdict==='PASS');
  console.log(JSON.stringify({verdict:passed?'PASS':'UNVERIFIED',backend:modeArg,
    scope:'pinned synthetic PDF and XLSX via actual MCP HTTP; not arbitrary document parity',results}));
  if(modeArg==='candidate'&&!passed)process.exitCode=1;
} else {
const sources=[
  ['IANA','https://www.iana.org/help/example-domains',['Example Domains','RFC 2606','RFC 6761']],
  ['CFTC','https://www.cftc.gov/sites/default/files/files/dea/cotarchives/2026/futures/petroleum_sf090826.htm',['September 8, 2026','Open Interest','1,939,911']]
];

async function sample([source,url,markers],mode){
  const start=performance.now();
  assert.ok(start<deadline,'benchmark deadline exceeded');
  const result=await rpc('tools/call',{name:'firecrawl_scrape',arguments:{
    url,formats:['markdown'],onlyMainContent:true,maxAge:0,storeInCache:false,timeout:25000
  }});
  assert.ok(result.result&&!result.result.isError&&!result.error,source+' MCP request failed');
  const item=result.result.content?.find(x=>x.type==='text');
  assert.ok(item?.text,'missing MCP text result');
  const envelope=JSON.parse(item.text);
  const doc=envelope.data??envelope;
  const md=doc.markdown;
  assert.equal(typeof md,'string');
  assert.equal(doc.metadata?.statusCode,200);
  const elapsed_ms=Math.round((performance.now()-start)*10)/10;
  const normalized=md.replace(/\s+/g,' ').toLowerCase();
  assert.ok(markers.every(marker=>normalized.includes(marker.toLowerCase())),source+' content lost');
  return {source,mode,elapsed_ms,markers:markers.length,markdown_bytes:Buffer.byteLength(md),
    sha256:createHash('sha256').update(md).digest('hex')};
}
const pause=()=>new Promise(resolve=>setTimeout(resolve,1000));
const samples=[];
const started=performance.now();
for(let i=0;i<3;i++){
  for(const source of sources){samples.push(await sample(source,'serial'));await pause();}
}
for(let i=0;i<2;i++){samples.push(...await Promise.all(sources.map(s=>sample(s,'concurrent_2'))));await pause();}
const groups={};
for(const mode of ['serial','concurrent_2']){
  for(const [source] of sources){
    const times=samples.filter(x=>x.mode===mode&&x.source===source).map(x=>x.elapsed_ms).sort((a,b)=>a-b);
    groups[mode+':'+source]={n:times.length,median_ms:(times[Math.floor((times.length-1)/2)]+times[Math.floor(times.length/2)])/2,max_ms:times.at(-1)};
  }
}
for(const [source] of sources)assert.equal(new Set(samples.filter(x=>x.source===source).map(x=>x.sha256)).size,1,source+' content varied');
console.log(JSON.stringify({verdict:'PASS',backend:modeArg,scope:'same fixed HTML workload through actual MCP HTTP; not full production parity',
  requests:samples.length,markers:samples.reduce((n,x)=>n+x.markers,0),wall_ms:Math.round(performance.now()-started),groups,samples}));
}
