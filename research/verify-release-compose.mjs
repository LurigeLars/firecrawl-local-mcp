// Run only inside the disposable release Compose MCP container.
// Fixed internal endpoint and fixed public fixtures; never print credentials.
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';

const BASE='http://api:3002';
// Only the isolated CI fixture mode uses this intentionally public test
// credential. Never read or transmit arbitrary local filesystem data.
const TOKEN='0123456789abcdef'.repeat(4);
const mode=process.argv[2];
assert.ok(['create','verify'].includes(mode));
const uuid=/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/;
const source='https://www.iana.org/help/example-domains';
const pdf='https://raw.githubusercontent.com/LurigeLars/firecrawl-local-mcp/b5a1bddc489c1d39fc2e45ef19ea77f7040c449b/research/fixtures/pdf-transport-native.pdf';
async function request(path,payload,auth=true,timeout=35000){
  const response=await fetch(BASE+path,{method:payload===undefined?'GET':'POST',
    headers:{'content-type':'application/json',...(auth?{authorization:'Bearer '+TOKEN}:{})},
    ...(payload===undefined?{}:{body:JSON.stringify(payload)}),
    redirect:'error',signal:AbortSignal.timeout(timeout)});
  const reader=response.body.getReader();
  let n=0;const chunks=[];
  try{
    for(;;){
      const {value,done}=await reader.read();if(done)break;
      n+=value.byteLength;assert.ok(n<=2*1024*1024);chunks.push(value);
    }
  }finally{await reader.cancel().catch(()=>{});}
  return {status:response.status,body:JSON.parse(Buffer.concat(chunks).toString('utf8'))};
}

for(let i=0;i<40;i++){
  try{
    const {status,body}=await request('/health',undefined,true,1000);
    if(status===200&&body.jobStore==='ready')break;
  }catch{}
  if(i===39)throw Error('release API readiness failed');
  await new Promise(r=>setTimeout(r,250));
}
assert.equal((await request('/health',undefined,false)).status,401);
function verifyJob(job){
  assert.equal(job.status,'completed');
  assert.equal(job.completed,1);
  assert.equal(job.research.failed,0);
  assert.equal(job.data.length,1);
  const doc=job.data[0];
  assert.equal(doc.metadata.url,source);
  assert.ok(['Example Domains','RFC 2606','RFC 6761'].every(m=>doc.markdown.includes(m)));
  return createHash('sha256').update(JSON.stringify(job)).digest('hex');
}
if(mode==='create'){
  const {status,body}=await request('/v2/scrape',{url:pdf,formats:['markdown'],parsers:['pdf'],maxAge:0,storeInCache:false,timeout:110000},true,115000);
  assert.equal(status,200);
  assert.ok(['Firecrawl synthetic PDF transport fixture','Stock 421.5 million barrels',
    'Weekly change -1.2 million barrels','September 8, 2026'].every(m=>body.data.markdown.includes(m)));
  const {status:createdStatus,body:created}=await request('/v2/crawl',{url:source,limit:1,maxConcurrency:1,sitemap:'skip',scrapeOptions:{formats:['markdown']}});
  assert.equal(createdStatus,200);
  assert.match(created.id,uuid);
  assert.equal(created.url,BASE+'/v2/crawl/'+created.id);
  for(let i=0;i<60;i++){
    const {status,body}=await request('/v2/crawl/'+created.id,undefined,true,10000);
    assert.equal(status,200);
    if(body.status==='completed'){
      console.log(JSON.stringify({job:created.id,digest:verifyJob(body),pdf_markers:4,crawl_markers:3}));
      process.exit(0);
    }
    assert.notEqual(body.status,'failed');
    await new Promise(r=>setTimeout(r,500));
  }
  throw Error('release crawl did not complete');
}else{
  const id=process.argv[3],digest=process.argv[4];
  assert.match(id,uuid);assert.match(digest,/^[0-9a-f]{64}$/);
  const {status,body}=await request('/v2/crawl/'+id);
  assert.equal(status,200);
  assert.equal(verifyJob(body),digest,'terminal crawl changed on recreation');
  console.log('RELEASE_COMPOSE_DURABLE_RECREATE_PASS');
}
