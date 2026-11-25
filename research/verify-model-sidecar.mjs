// Actual candidate -> existing model proxy -> deterministic private Ollama-shaped fixture.
// No real model, external provider, credential file, or configurable destination.
import assert from 'node:assert/strict';
import http from 'node:http';
import {setTimeout as sleep} from 'node:timers/promises';

const token = 'synthetic-ci-token-not-a-real-secret-0123456789';
const api = 'http://api:3002';
const source = 'https://www.iana.org/help/example-domains';
const schema = {type:'object',properties:{rfc:{type:'integer'}},required:['rfc'],additionalProperties:false};
const good = 'Extract the integer RFC number 2606.';
const bad = 'Fixture: return an invalid type for the RFC number.';

if (process.argv[2] === 'mock') {
  http.createServer(async (req,res) => {
    if (req.method === 'GET' && req.url === '/health') {
      res.writeHead(200); res.end('ok'); return;
    }
    try {
      assert.equal(req.method,'POST');
      assert.equal(req.url,'/v1/responses');
      assert.equal(req.headers['x-firecrawl-model-contract'],undefined);
      let bytes=0; const parts=[];
      for await (const chunk of req) {
        bytes+=chunk.length;
        assert.ok(bytes<=65536);
        parts.push(chunk);
      }
      const body=JSON.parse(Buffer.concat(parts));
      assert.equal(body.model,'fixture');
      assert.equal(body.stream,false);
      assert.equal(body.tools,undefined);
      assert.deepEqual(body.text.format.schema,schema);
      assert.ok(body.instructions.includes('untrusted data'));
      const text=body.input[0].content[0].text;
      assert.ok(text.includes('RFC 2606')&&text.includes('RFC 6761'));
      const valid=body.instructions.endsWith(good);
      assert.ok(valid||body.instructions.endsWith(bad));
      const output=JSON.stringify({rfc:valid?2606:'2606'});
      const envelope=JSON.stringify({status:'completed',output:[{type:'message',role:'assistant',
        status:'completed',content:[{type:'output_text',text:output}]}]});
      res.writeHead(200,{'content-type':'application/json'});res.end(envelope);
    } catch {
      res.writeHead(422,{'content-type':'application/json'});res.end('{"error":"fixture contract rejected"}');
    }
  }).listen(11434,'0.0.0.0');
} else {
  assert.equal(process.argv[2],'verify');
  let ready=false;
  for(let i=0;i<30;i++){
    try{
      const responses=await Promise.all([
        fetch(api+'/health',{headers:{authorization:'Bearer '+token},signal:AbortSignal.timeout(1000),redirect:'error'}),
        fetch('http://llm-proxy:11435/health',{signal:AbortSignal.timeout(1000),redirect:'error'})
      ]);
      ready=responses.every(r=>r.ok);
      await Promise.all(responses.map(r=>r.body?.cancel()));
      if(ready)break;
    }catch{}
    await sleep(500);
  }
  assert.ok(ready,'isolated candidate/proxy not ready');
  async function scrape(prompt){
    const response=await fetch(api+'/v2/scrape',{method:'POST',redirect:'error',signal:AbortSignal.timeout(35000),
      headers:{authorization:'Bearer '+token,'content-type':'application/json'},
      body:JSON.stringify({url:source,formats:[{type:'json',prompt,schema}],onlyMainContent:true,
        maxAge:0,storeInCache:false,timeout:30000})});
    const reader=response.body.getReader(),chunks=[];let bytes=0;
    try{for(;;){const {done,value}=await reader.read();if(done)break;bytes+=value.length;
      assert.ok(bytes<=2*1024*1024);chunks.push(value);}}
    finally{await reader.cancel().catch(()=>{});}
    return {status:response.status,body:JSON.parse(Buffer.concat(chunks))};
  }
  const ok=await scrape(good);
  assert.equal(ok.status,200);
  assert.equal(ok.body.success,true);
  const doc=ok.body.data;
  assert.deepEqual(doc.json,{rfc:2606});
  assert.equal(doc.metadata.sourceURL,source);
  assert.equal(doc.researchModel.provider,'ollama');
  assert.equal(doc.researchModel.schema_validated,true);
  assert.equal(doc.researchModel.source_verified,false);
  assert.equal(doc.researchModel.provider_reported_complete,true);
  assert.equal(doc.researchModel.input_proxy_truncated,false);
  assert.equal(doc.markdown,undefined);
  const refused=await scrape(bad);
  assert.equal(refused.status,502);
  assert.equal(refused.body.success,false);
  console.log('ACTUAL_MODEL_PROXY_FIXTURE_PASS (typed JSON, invalid type rejected; no real model)');
}
