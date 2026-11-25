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
const server=http.createServer(async(req,res)=>{
  try {
    const parts=[]; let bytes=0;
    for await (const part of req){
      bytes+=part.length;
      if(bytes>65536){res.writeHead(413).end();return;}
      parts.push(part);
    }
    const body=bytes?JSON.parse(Buffer.concat(parts)):null;
    const u=new URL(req.url,'http://127.0.0.1');
    traffic.push({method:req.method,path:u.pathname,query:u.search,body});
    const origin='http://127.0.0.1:'+server.address().port;
    let data;
    if(req.method==='POST'&&u.pathname==='/v2/scrape'){
      const names=(body?.formats??[]).map(x=>typeof x==='string'?x:x?.type);
      data={success:true,data:{...doc,
        ...(names.includes('summary')?{summary:'Synthetic summary'}:{}),
        ...(names.includes('query')?{answer:'42'}:{})
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
    FIRECRAWL_API_KEY:'fc-synthetic-ci-not-a-real-key'},
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
    const timer=setTimeout(()=>{pending.delete(id);reject(new Error('MCP response deadline: '+method));},20000);
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
  const calls=[
    ['firecrawl_scrape',{url:fixtureUrl,formats:['markdown']},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['summary']},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['query'],queryOptions:{prompt:'What is the value?'}},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['query'],queryOptions:{prompt:'Quote the value',mode:'directQuote'}},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:fixtureUrl,formats:['html','links'],waitFor:100},'POST','/v2/scrape'],
    ['firecrawl_scrape',{url:'https://example.org/test.pdf',formats:['markdown'],parsers:['pdf']},'POST','/v2/scrape'],
    ['firecrawl_search',{query:'synthetic fixture',limit:2},'POST','/v2/search'],
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
    console.log(JSON.stringify({tool:name,required:definition.inputSchema?.required??[],
      transport:captured,toolError:reply.result?.isError===true||!!reply.error}));
    outcomes.push({name,ok:!reply.error&&!!reply.result&&!reply.result.isError&&
      captured.some(x=>x.method===method&&x.path===path)});
  }
  console.log(JSON.stringify({locked_mcp:pkg.version,outcomes}));
  assert.ok(outcomes.every(x=>x.ok),'locked client transport differs from expected API fixture');
}finally{
  lines.close();child.stdin.end();child.kill();
  server.closeAllConnections();await new Promise(resolve=>server.close(resolve));
}
