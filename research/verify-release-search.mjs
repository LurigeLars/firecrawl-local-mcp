// One fixed public-domain query using the actual reused SearXNG proxy.
import assert from 'node:assert/strict';
// Only the isolated CI fixture mode uses this intentionally public test
// credential. Never read or transmit arbitrary local filesystem data.
const token='0123456789abcdef'.repeat(4);
const start=performance.now();
const response=await fetch('http://api:3002/v2/search',{
  method:'POST',headers:{authorization:'Bearer '+token,'content-type':'application/json'},
  body:JSON.stringify({query:'example domains RFC 2606',limit:2,sources:['web'],includeDomains:['iana.org'],timeout:60000}),
  redirect:'error',signal:AbortSignal.timeout(65000)
});
const reader=response.body.getReader();
let size=0;const chunks=[];
try{
  for(;;){
    const {value,done}=await reader.read();if(done)break;
    size+=value.byteLength;assert.ok(size<=2*1024*1024);chunks.push(value);
  }
}finally{await reader.cancel().catch(()=>{});}
assert.equal(response.status,200,'search response failed');
const body=JSON.parse(Buffer.concat(chunks).toString('utf8'));
assert.equal(body.success,true);
assert.equal(body.research.source,'local-searxng');
assert.equal(body.research.catalogue_available,false);
const results=body.data.web;
assert.ok(results.length>=1&&results.length<=2,'no verified search hits');
for(const hit of results){
  const host=new URL(hit.url).hostname;
  assert.ok(host==='iana.org'||host.endsWith('.iana.org'));
  assert.equal(hit.source,'web');
}
assert.equal(new Set(results.map(x=>x.url)).size,results.length);
console.log('RELEASE_SEARXNG_LIVE_PASS',JSON.stringify({results:results.length,elapsed_ms:Math.round(performance.now()-start)}));
