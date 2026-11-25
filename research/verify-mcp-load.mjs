// Run inside the respective existing MCP container. The actual locked SDK
// handles its own credential, so this client never reads any secret file.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
const MCP='http://mcp:3000/mcp';
const modeArg=process.argv[2];
assert.ok(['legacy','candidate'].includes(modeArg));
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
