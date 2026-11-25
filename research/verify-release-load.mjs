// Fixed workload shared by release acceptance and eventual local comparison.
// Run in disposable MCP container; no production tokens or external endpoints
// other than the two fixed public fixtures. Measure API latency, not docker exec.
import assert from 'node:assert/strict';
import {readFileSync} from 'node:fs';
import {createHash} from 'node:crypto';
const modeArg=process.argv[2]??'candidate';
assert.ok(['legacy','candidate'].includes(modeArg));
const token=modeArg==='candidate'?readFileSync('/run/research-token/token','utf8').trim():null;
if(token!==null)assert.match(token,/^[0-9a-f]{64}$/);
const deadline=performance.now()+120000;
const sources=[
  ['IANA','https://www.iana.org/help/example-domains',['Example Domains','RFC 2606','RFC 6761']],
  ['CFTC','https://www.cftc.gov/sites/default/files/files/dea/cotarchives/2026/futures/petroleum_sf090826.htm',['September 8, 2026','Open Interest','1,939,911']]
];
async function sample([source,url,markers],mode){
  const start=performance.now();
  const remaining=Math.floor(deadline-start);
  assert.ok(remaining>0,'benchmark deadline exceeded');
  const response=await fetch('http://api:3002/v2/scrape',{
    method:'POST',headers:{...(token===null?{}:{authorization:'Bearer '+token}),'content-type':'application/json'},
    body:JSON.stringify({url,formats:['markdown'],onlyMainContent:true,maxAge:0,storeInCache:false,timeout:25000}),
    redirect:'error',signal:AbortSignal.timeout(Math.min(35000,remaining))
  });
  assert.equal(response.status,200);
  const reader=response.body.getReader();
  let size=0;const chunks=[];
  try{
    for(;;){
      const {value,done}=await reader.read();if(done)break;
      size+=value.byteLength;assert.ok(size<=2*1024*1024);chunks.push(value);
    }
  }finally{await reader.cancel().catch(()=>{});}
  const data=JSON.parse(Buffer.concat(chunks).toString('utf8'));
  const elapsed_ms=Math.round((performance.now()-start)*10)/10;
  assert.equal(data.success,true);assert.equal(data.data.metadata.statusCode,200);
  if(modeArg==='candidate')assert.equal(data.research.cache_used,false);
  const md=data.data.markdown;
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
console.log(JSON.stringify({verdict:'PASS',backend:modeArg,scope:'fixed HTML workload only; not full production parity',
  requests:samples.length,markers:samples.reduce((n,x)=>n+x.markers,0),wall_ms:Math.round(performance.now()-started),groups,samples}));
