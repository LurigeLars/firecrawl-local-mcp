// Actual host stdio acceptance, only during reviewed Rust transition.
import {spawn} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {shouldDelegateRustClient} from './rust-client-mode.mjs';
const env={...process.env,FIRECRAWL_API_URL:'http://127.0.0.1:3002',FIRECRAWL_MCP_TOOL_USAGE:'off',ALLOWED_TOOLS:'firecrawl_scrape'};
delete env.FIRECRAWL_API_KEY;delete env.FIRECRAWL_API_KEY_FILE;
if(!shouldDelegateRustClient(env)){console.error('RUST_STDIO_UNVERIFIED transition marker unavailable');process.exit(1);}
const proxy=spawn(process.execPath,[fileURLToPath(new URL('./stdio-proxy.mjs',import.meta.url))],
  {env,stdio:['pipe','pipe','ignore'],windowsHide:true});
let raw='',bytes=0,nextId=1,terminal=false;
const pending=new Map();
const fail=()=>{
  terminal=true;
  for(const {reject,timer} of pending.values()){clearTimeout(timer);reject(Error('stdio unavailable'));}
  pending.clear();
};
proxy.on('error',fail);proxy.on('exit',fail);proxy.stdin.on('error',fail);
proxy.stdout.on('data',chunk=>{
  if(terminal)return;
  bytes+=chunk.length;
  if(bytes>2*1024*1024){fail();return;}
  raw+=chunk.toString('utf8');
  const lines=raw.split('\n');raw=lines.pop();
  for(const line of lines){
    if(!line.trim())continue;
    try{
      const msg=JSON.parse(line);
      const p=pending.get(msg.id);
      if(p){pending.delete(msg.id);clearTimeout(p.timer);p.resolve(msg);}
    }catch{fail();return;}
  }
});
function rpc(method,params){
  return new Promise((resolve,reject)=>{
    if(terminal){reject(Error('stdio unavailable'));return;}
    const id=nextId++;
    const timer=setTimeout(()=>{pending.delete(id);reject(Error('stdio timeout'));},45000);
    pending.set(id,{resolve,reject,timer});
    proxy.stdin.write(JSON.stringify({jsonrpc:'2.0',id,method,params})+'\n');
  });
}
let ok=false;
try{
  const init=await rpc('initialize',{protocolVersion:'2025-06-18',capabilities:{},clientInfo:{name:'rust-local-acceptance',version:'1'}});
  if(!init.result?.serverInfo)throw Error('initialize');
  proxy.stdin.write(JSON.stringify({jsonrpc:'2.0',method:'notifications/initialized'})+'\n');
  const list=await rpc('tools/list',{});
  if(!list.result?.tools?.some(t=>t.name==='firecrawl_scrape'))throw Error('list');
  const r=await rpc('tools/call',{name:'firecrawl_scrape',arguments:{
    url:'https://www.iana.org/help/example-domains',formats:['markdown'],onlyMainContent:true,maxAge:0,storeInCache:false,timeout:30000}});
  if(r.error||r.result?.isError)throw Error('scrape');
  const data=JSON.parse(r.result?.content?.[0]?.text??'{}');
  const markdown=data.markdown??data.data?.markdown;
  if(typeof markdown!=='string'||!['Example Domains','RFC 2606','RFC 6761'].every(m=>markdown.includes(m)))throw Error('markers');
  ok=true;
}catch{}
finally{
  proxy.stdin.end();
  if(proxy.exitCode===null)await new Promise(resolve=>{
    const timer=setTimeout(()=>{proxy.kill();resolve();},4000);
    proxy.once('exit',()=>{clearTimeout(timer);resolve();});
  });
  proxy.stdout.destroy();
}
console.log(ok?'RUST_STDIO_ACTUAL_PASS markers=3':'RUST_STDIO_ACTUAL_UNVERIFIED');
process.exit(ok?0:1);
