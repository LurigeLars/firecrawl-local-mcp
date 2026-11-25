// Capture the *locked* MCP client's actual HTTP contract, against only a
// synthetic loopback API in a network-none container. No external URLs are
// fetched, no production credentials, no policy or gateway changes.
import assert from 'node:assert/strict';
import http from 'node:http';
import { spawn } from 'node:child_process';
import readline from 'node:readline';
import { readFileSync } from 'node:fs';

const fixtureUrl='https://example.org/docs/';
const id='c7c0b460-fcc0-4abe-97ae-95d14dcda840';
const doc={markdown:'Synthetic source fixture 42',metadata:{
  title:'Fixture',url:fixtureUrl,sourceURL:fixtureUrl,statusCode:200,contentType:'text/html'
}};
const traffic=[];
const mode=process.argv[2]??'fixture';
assert.ok(['fixture','--candidate','--candidate-live','--candidate-network-live'].includes(mode));
const candidateMode=mode!=='fixture';
const networkMode=mode==='--candidate-network-live';
const liveMode=mode==='--candidate-live'||networkMode;
const candidateBase=networkMode?'http://api:3002':'http://127.0.0.1:3002';
const candidateToken='synthetic-ci-token-not-a-real-secret-0123456789';
const fixtureToken='fc-synthetic-ci-not-a-real-key';
// The relay is an isolated test observer, not a general reverse proxy.
// Select only literal POST endpoints and the single status URL registered by
// the candidate's successful create response. The incoming URL is never
// used to construct an upstream request (including its path).
let candidatePollPath=null;
let candidatePollUrl=null;
async function relay(req,res,u,raw,record){
  if(u.origin!=='http://127.0.0.1'||u.search||u.hash||
      req.headers.authorization!=='Bearer '+candidateToken){
    res.writeHead(400).end();return;
  }
  let target;
  const post=req.method==='POST';
  if(post){
    switch(u.pathname){
      case '/v2/scrape':target=networkMode?'http://api:3002/v2/scrape':'http://127.0.0.1:3002/v2/scrape';break;
      case '/v2/map':target=networkMode?'http://api:3002/v2/map':'http://127.0.0.1:3002/v2/map';break;
      case '/v2/crawl':target=networkMode?'http://api:3002/v2/crawl':'http://127.0.0.1:3002/v2/crawl';break;
    }
  }else if(req.method==='GET'&&candidatePollUrl!==null&&u.pathname===candidatePollPath){
    target=candidatePollUrl;
  }
  if(!target){res.writeHead(400).end();return;}
  const upstream=await fetch(target,{
    method:post?'POST':'GET',...(post?{body:raw}:{}),
    headers:{authorization:'Bearer '+candidateToken,'content-type':'application/json'},
    redirect:'manual',signal:AbortSignal.timeout(35000)
  });
  const chunks=[];let size=0;
  for await(const part of upstream.body){
    size+=part.length;
    if(size>2*1024*1024)throw Error('candidate response exceeds test bound');
    chunks.push(part);
  }
  const body=Buffer.concat(chunks);
  record.status=upstream.status;
  record.response=JSON.parse(body.toString('utf8'));
  if(post&&(target==='http://127.0.0.1:3002/v2/crawl'||target==='http://api:3002/v2/crawl')&&upstream.status===200){
    const job=record.response?.id;
    assert.ok(typeof job==='string'&&/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/.test(job),
      'candidate returned invalid job ID');
    candidatePollPath='/v2/crawl/'+job;
    candidatePollUrl=candidateBase+'/v2/crawl/'+job;
    assert.equal(record.response?.url,candidatePollUrl,'creation URL must match the fixed service base');
  }
  res.writeHead(upstream.status,{'content-type':'application/json'});
  res.end(body);
}
const server=http.createServer(async(req,res)=>{
  try {
    const parts=[]; let bytes=0;
    for await (const part of req){
      bytes+=part.length;
      if(bytes>65536){res.writeHead(413).end();return;}
      parts.push(part);
    }
    const raw=Buffer.concat(parts);
    const body=bytes?JSON.parse(raw):null;
    const u=new URL(req.url,'http://127.0.0.1');
    const record={method:req.method,path:u.pathname,query:u.search,body};
    traffic.push(record);
    if(candidateMode){await relay(req,res,u,raw,record);return;}
    const origin='http://127.0.0.1:'+server.address().port;
    let data;
    if(req.method==='POST'&&u.pathname==='/v2/scrape'){
      const names=(body?.formats??[]).map(x=>typeof x==='string'?x:x?.type);
      data={success:true,data:{...doc,
        ...(names.includes('summary')?{summary:'Synthetic summary'}:{}),
        ...(names.includes('query')?{answer:'42'}:{}),
        ...(names.includes('json')?{json:{value:42}}:{})
      }};
    }else if(req.method==='POST'&&u.pathname==='/v2/search'){
      data={success:true,data:{web:[{url:fixtureUrl,title:'Fixture',description:'Synthetic hit'}]}};
    }else if(req.method==='POST'&&u.pathname==='/v2/map'){
      data={success:true,links:[{url:fixtureUrl,title:'Fixture'}]};
    }else if(req.method==='POST'&&u.pathname==='/v2/crawl'){
      data={success:true,id,url:origin+'/v2/crawl/'+id};
    }else if(req.method==='GET'&&u.pathname==='/v2/crawl/'+id){
      data={success:true,status:'completed',total:1,completed:1,creditsUsed:1,
        expiresAt:'2030-01-01T00:00:00.000Z',data:[doc]};
    }else{
      res.writeHead(404,{'content-type':'application/json'});
      res.end(JSON.stringify({success:false,error:'Unexpected fixture API path'}));return;
    }
    res.writeHead(200,{'content-type':'application/json'});res.end(JSON.stringify(data));
  }catch{
    res.writeHead(400,{'content-type':'application/json'});res.end('{"success":false}');
  }
});
await new Promise((resolve,reject)=>{
  server.once('error',reject);server.listen(0,'127.0.0.1',resolve);
});
const pkg=JSON.parse(readFileSync('/app/node_modules/firecrawl-mcp/package.json','utf8'));
const manifest=JSON.parse(readFileSync('/app/package.json','utf8'));
assert.equal(pkg.version,manifest.dependencies['firecrawl-mcp']);
const child=spawn(process.execPath,['/app/node_modules/firecrawl-mcp/dist/index.js'],{
  // Do not inherit proxy credentials, remote URLs, or HTTP transport mode.
  env:{PATH:process.env.PATH,HOME:'/tmp',TMPDIR:'/tmp',
    FIRECRAWL_API_URL:'http://127.0.0.1:'+server.address().port,
    FIRECRAWL_API_KEY:candidateMode?candidateToken:fixtureToken},
  stdio:['pipe','pipe','pipe']
});
let stderr='',nextId=1;
const pending=new Map();
child.stderr.on('data',c=>{if(stderr.length<4096)stderr+=c.toString().slice(0,4096-stderr.length);});
const lines=readline.createInterface({input:child.stdout});
lines.on('line',line=>{
  let msg;try{msg=JSON.parse(line);}catch{return;}
  const slot=pending.get(msg.id);
  if(slot){clearTimeout(slot.timer);pending.delete(msg.id);slot.resolve(msg);}
});
child.on('exit',()=>{for(const slot of pending.values()){clearTimeout(slot.timer);slot.reject(new Error('MCP exited'));}pending.clear();});
child.on('error',()=>{for(const slot of pending.values()){clearTimeout(slot.timer);slot.reject(new Error('MCP spawn failed'));}pending.clear();});
const notify=(method,params)=>child.stdin.write(JSON.stringify({jsonrpc:'2.0',method,params})+'\n');
function rpc(method,params){
  const id=nextId++;
  return new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>{pending.delete(id);reject(new Error('MCP response deadline: '+method));},candidateMode?45000:20000);
    pending.set(id,{resolve,reject,timer});
    child.stdin.write(JSON.stringify({jsonrpc:'2.0',id,method,params})+'\n');
  });
}
try{
  const init=await rpc('initialize',{protocolVersion:'2025-06-18',capabilities:{},clientInfo:{name:'candidate-contract',version:'1'}});
  assert.ok(init.result?.serverInfo);
  notify('notifications/initialized');
  const listed=await rpc('tools/list',{});
  const tools=listed.result?.tools??[];
  if(candidateMode){
    const cases=mode==='--candidate'?[
      {name:'firecrawl_scrape',args:{url:'https://127.0.0.1/',formats:['markdown']},path:'/v2/scrape',status:403},
      {name:'firecrawl_map',args:{url:'https://127.0.0.1/',limit:2},path:'/v2/map',status:403},
      {name:'firecrawl_crawl',args:{url:'https://127.0.0.1/',limit:2,maxConcurrency:2,scrapeOptions:{formats:['markdown']}},path:'/v2/crawl',status:403}
    ]:[
      {name:'firecrawl_scrape',args:{url:'https://www.iana.org/help/example-domains',formats:['markdown']},path:'/v2/scrape',status:200,markers:['Example Domains','RFC 2606','RFC 6761']},
      {name:'firecrawl_scrape',args:{url:'https://www.cftc.gov/sites/default/files/files/dea/cotarchives/2026/futures/petroleum_sf090826.htm',formats:['markdown']},path:'/v2/scrape',status:200,markers:['September 8, 2026','Open Interest','1,939,911']}
    ];
    for(const tc of cases){
      assert.ok(tools.some(t=>t.name===tc.name),tc.name+' unavailable');
      const offset=traffic.length;
      const reply=await rpc('tools/call',{name:tc.name,arguments:tc.args});
      const captured=traffic.slice(offset);
      const exchange=captured.find(x=>x.path===tc.path&&x.status===tc.status);
      assert.ok(exchange,tc.name+' did not reach actual candidate with expected HTTP status');
      const output=JSON.stringify(reply.result??reply.error??{});
      if(tc.status===200){
        assert.equal(exchange.response?.success,true);
        assert.ok(!reply.error&&reply.result&&!reply.result.isError);
        const md=String(exchange.response?.data?.markdown??'').replace(/\s+/g,' ').toLowerCase();
        const visible=output.replace(/\\n|\\r|\\t/g,' ').replace(/\s+/g,' ').toLowerCase();
        assert.ok(tc.markers.every(m=>md.includes(m.toLowerCase())&&visible.includes(m.toLowerCase())),
          tc.name+' lost source markers in HTTP or MCP result');
      }else{
        assert.equal(exchange.response?.success,false);
        assert.ok(reply.error||reply.result?.isError||/error|failed|denied|unsupported|forbidden|bad request/i.test(output),
          tc.name+' refusal not visible to MCP caller');
      }
      console.log(JSON.stringify({actual_candidate:true,mode,tool:tc.name,http_status:exchange.status,
        markers:tc.markers?.length??0,mcp_error:!!reply.error||reply.result?.isError===true}));
    }
    if(liveMode){
      const offset=traffic.length;
      const start=await rpc('tools/call',{name:'firecrawl_crawl',arguments:{
        url:'https://www.iana.org/help',limit:3,maxConcurrency:2,includePaths:['^/help/'],delay:1,
        scrapeOptions:{formats:['markdown']}}});
      assert.ok(!start.error&&start.result&&!start.result.isError,'MCP crawl creation failed');
      const created=traffic.slice(offset).find(x=>x.path==='/v2/crawl'&&x.status===200)?.response;
      assert.equal(created?.research?.sitemap_mode,'include');
      assert.equal(created?.research?.requested_max_concurrency,2);
      assert.equal(created?.research?.same_domain_network_concurrency,1);
      const envelope=traffic.slice(offset).find(x=>x.path==='/v2/crawl'&&x.status===200)?.body;
      assert.deepEqual(envelope?.includePaths,['^/help/']);
      assert.equal(envelope?.delay,1);
      const jobId=created.id;
      assert.match(jobId,/^[0-9a-f-]{36}$/);
      assert.ok(JSON.stringify(start.result).includes(jobId),'MCP omitted crawl job ID');
      const deadline=Date.now()+95000;
      let result;
      while(Date.now()<deadline){
        const at=traffic.length;
        const reply=await rpc('tools/call',{name:'firecrawl_check_crawl_status',arguments:{id:jobId}});
        assert.ok(!reply.error&&reply.result&&!reply.result.isError,'MCP crawl status failed');
        result=traffic.slice(at).find(x=>x.path==='/v2/crawl/'+jobId&&x.status===200)?.response;
        assert.equal(result?.research?.sitemap_mode,'include');
        assert.equal(result?.research?.requested_max_concurrency,2);
        if(result.next!==undefined){
          const next=new URL(result.next);
          assert.equal(next.origin,candidateBase);
          assert.equal(next.pathname,'/v2/crawl/'+jobId);
        }
        if(result.status==='completed'||result.status==='failed'){
          assert.equal(result.status,'completed');
          assert.equal(result.completed,3);
          assert.equal(result.research.failed,0);
          assert.ok(result.data.every(d=>d.metadata?.url==='https://www.iana.org/help'||d.metadata?.url?.startsWith('https://www.iana.org/help/')));
          assert.ok(result.data.some(d=>d.researchSitemap?.status),'sitemap outcome missing');
          assert.ok(JSON.stringify(reply.result).includes('completed'),'MCP omitted terminal state');
          console.log(JSON.stringify({actual_candidate:true,tool:'firecrawl_crawl',sitemap_mode:'include',
            completed:result.completed,sitemap:result.data.find(d=>d.researchSitemap)?.researchSitemap}));
          break;
        }
        await new Promise(resolve=>setTimeout(resolve,500));
      }
      assert.equal(result?.status,'completed','MCP crawl deadline exceeded');
    }
    console.log('LOCKED_MCP_ACTUAL_CANDIDATE_PASS '+mode);
  }else{
  const calls=[
    ['firecrawl_scrape',{url:fixtureUrl,formats:['markdown']},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['images']},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['markdown'],proxy:'basic'},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['markdown'],proxy:'auto'},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl+'?year=2026#data',formats:['markdown'],includeTags:['article'],excludeTags:['.ads']},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['summary']},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['json'],jsonOptions:{prompt:'Extract value',schema:{
      type:'object',properties:{value:{type:'number'}},required:['value'],additionalProperties:false}}},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['query'],queryOptions:{prompt:'What is the value?'}},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['query'],queryOptions:{prompt:'Quote the value',mode:'directQuote'}},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['html','links'],waitFor:100},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:'https://example.org/test.pdf',formats:['markdown'],parsers:['pdf']},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:'https://example.org/test.pdf',formats:['markdown'],parsers:['pdf'],pdfOptions:{maxPages:1}},'POST','/v2/scrape'],
    ['firecrawl_search',{query:'synthetic fixture',limit:2},'POST','/v2/search'],
    ['firecrawl_search',{query:'synthetic fixture',limit:2,scrapeOptions:{formats:['markdown'],includeTags:['article']}},'POST','/v2/search'],
    ['firecrawl_search',{query:'synthetic fixture',limit:2,scrapeOptions:{formats:['query'],queryOptions:{prompt:'What is the value?'}}},'POST','/v2/search'],
    ['firecrawl_map',{url:fixtureUrl,limit:2},'POST','/v2/map'],
    ['firecrawl_crawl',{url:fixtureUrl,limit:2,maxConcurrency:1,scrapeOptions:{formats:['markdown']}},'POST','/v2/crawl'],
    ['firecrawl_check_crawl_status',{id},'GET','/v2/crawl/'+id],
  ];
  const outcomes=[];
  for(const [name,args,method,path] of calls){
    const definition=tools.find(t=>t.name===name);
    assert.ok(definition,name+' missing from locked client');
    const offset=traffic.length;
    const reply=await rpc('tools/call',{name,arguments:args});
    const captured=traffic.slice(offset);
    if(name==='firecrawl_search'&&args.scrapeOptions){
      const request=captured.find(x=>x.method===method&&x.path===path)?.body;
      if(args.scrapeOptions.queryOptions){
        assert.deepEqual(request?.scrapeOptions?.formats,[{type:'query',prompt:args.scrapeOptions.queryOptions.prompt,mode:'freeform'}]);
      }else assert.deepEqual(request?.scrapeOptions?.formats,args.scrapeOptions.formats);
      assert.deepEqual(request?.scrapeOptions?.includeTags,args.scrapeOptions.includeTags);
    }
    if(args.proxy){
      const request=captured.find(x=>x.method===method&&x.path===path)?.body;
      assert.equal(request?.proxy,args.proxy);
    }
    if(args.pdfOptions){
      const request=captured.find(x=>x.method===method&&x.path===path)?.body;
      assert.deepEqual(request?.parsers,[{type:'pdf',maxPages:1}]);
    }
    if(args.jsonOptions){
      const request=captured.find(x=>x.method===method&&x.path===path)?.body;
      assert.deepEqual(request?.formats,[{type:'json',prompt:args.jsonOptions.prompt,schema:args.jsonOptions.schema}]);
    }
    if(args.includeTags){
      const request=captured.find(x=>x.method===method&&x.path===path)?.body;
      assert.equal(request?.url,args.url);
      assert.deepEqual(request?.includeTags,args.includeTags);
      assert.deepEqual(request?.excludeTags,args.excludeTags);
    }
    console.log(JSON.stringify({tool:name,required:definition.inputSchema?.required??[],
      transport:captured,toolError:reply.result?.isError===true||!!reply.error}));
    outcomes.push({name,ok:!reply.error&&!!reply.result&&!reply.result.isError&&
      captured.some(x=>x.method===method&&x.path===path)});
  }
  console.log(JSON.stringify({locked_mcp:pkg.version,outcomes}));
  assert.ok(outcomes.every(x=>x.ok),'locked client transport differs from expected API fixture');
  }
}finally{
  lines.close();child.stdin.end();child.kill();
  server.closeAllConnections();await new Promise(resolve=>server.close(resolve));
}
