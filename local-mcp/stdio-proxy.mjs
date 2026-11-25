// Local stdio MCP proxy for the self-hosted Firecrawl (Claude desktop, Claude Code, Codex).
// Spawns firecrawl-mcp over stdio and applies the same rules as the public gateway (../public/gateway/policy.mjs):
// only the tools that work on this instance are listed and callable, unsupported formats are refused up front,
// server instructions are replaced, and query/json/summary scrape results drop their bulky page metadata.
// Node standard library only. Usage: node stdio-proxy.mjs   (env FIRECRAWL_API_URL, optional ALLOWED_TOOLS)
import { spawn } from 'node:child_process';
import readline from 'node:readline';
import {createToolUsageStore,createUsageTracker,appendUsageTool,isUsageCall,usageResult} from '../public/gateway/tool-usage.mjs';
import { fileURLToPath } from 'node:url';
import { ensureLockedRuntime } from './locked-runtime.mjs';
import {
  parseAllowedTools, checkRequest, urlSafetyReason, wantsCompactResult, rememberCrawlRequest, rewriteResponse, rpcError, rpcToolError,
} from '../public/gateway/policy.mjs';
import { BROWSER_TOOL_NAMES, appendBrowserTools, callBrowserTool, isBrowserTool } from '../public/gateway/browser-tools.mjs';

// The exact firecrawl-mcp tree locked in public/package.json + public/package-lock.json.
const { entry: mcpEntry } = ensureLockedRuntime(
  fileURLToPath(new URL('../public/', import.meta.url)), fileURLToPath(new URL('./.runtime/', import.meta.url)));

const DEFAULT_LOCAL_TOOLS = `tool_usage,firecrawl_scrape,firecrawl_map,firecrawl_search,firecrawl_crawl,firecrawl_check_crawl_status,${BROWSER_TOOL_NAMES.join(',')}`;
const allowedTools = parseAllowedTools(process.env.ALLOWED_TOOLS ?? DEFAULT_LOCAL_TOOLS);
const usageStore=createToolUsageStore({root:process.env.FIRECRAWL_TOOL_USAGE_DIR,enabled:process.env.FIRECRAWL_MCP_TOOL_USAGE!=='off'&&!process.env.NODE_TEST_CONTEXT});
for(const name of allowedTools)usageStore.register(name);
const usage=createUsageTracker(usageStore,allowedTools);
process.once('exit',()=>{usage.finish('ABORTED');usageStore.flush();});
const browserServiceUrl = process.env.BROWSER_SERVICE_URL ?? 'http://127.0.0.1:3010';
const compactIds = new Set();
const crawlRequests = new Map();
const ctx = { allowedTools, compactIds, crawlRequests };

const child = spawn(process.execPath, [mcpEntry],
  { env: { ...process.env, FIRECRAWL_API_URL: process.env.FIRECRAWL_API_URL ?? 'http://127.0.0.1:3002' }, stdio: ['pipe', 'pipe', 'inherit'] });

const toClient = obj => process.stdout.write(JSON.stringify(obj) + '\n');

// client -> server. Serialize asynchronous DNS checks so pipelined MCP
// requests preserve their original order when forwarded to the child process.
async function handleClientLine(line) {
  if (!line.trim()) return;
  let msg;
  try { msg = JSON.parse(line); } catch { child.stdin.write(line + '\n'); return; }
  const messages = [msg].flat();
  const usageCalls=messages.filter(isUsageCall);
  if(usageCalls.length){
    const m=usageCalls[0];
    if(messages.length!==1||!allowedTools.has('tool_usage'))return toClient(rpcError(m.id,'Tool usage requires one allowed tool call.'));
    usage.expire();
    return toClient(usageResult(m,usageStore,'local_stdio_proxy'));
  }
  const browserCalls = messages.filter(m => m?.method === 'tools/call' && isBrowserTool(m.params?.name));
  if (browserCalls.length) {
    if (messages.length !== 1) return toClient(rpcError(browserCalls[0]?.id, 'Browser tools cannot be batched with other JSON-RPC messages.'));
    const m = browserCalls[0];
    const verdict = checkRequest(m, allowedTools);
    if (verdict.error) return toClient(rpcError(m.id, verdict.error));
    if (verdict.toolError) return toClient(rpcToolError(m.id, verdict.toolError));
    usage.start(m);
    try{
      const result=await callBrowserTool(browserServiceUrl,m.params?.name,m.params?.arguments??{});
      const response={jsonrpc:'2.0',id:m.id??null,result};
      usage.observe(response);return toClient(response);
    }catch(e){usage.observe({id:m.id,error:true});throw e;}
  }
  for (const m of messages) {
    const verdict = checkRequest(m, allowedTools);
    if (verdict.error) return toClient(rpcError(m.id, verdict.error));
    if (verdict.toolError) return toClient(rpcToolError(m.id, verdict.toolError));
    const safetyReason = await urlSafetyReason(m.params);
    if (safetyReason) return toClient(rpcToolError(m.id, safetyReason));
    if (m?.method === 'tools/call' && m.id !== undefined && wantsCompactResult(m.params)) compactIds.add(m.id);
    rememberCrawlRequest(m, crawlRequests);
  }
  for(const m of messages)usage.start(m);
  child.stdin.write(line + '\n');
}

let clientQueue = Promise.resolve();
readline.createInterface({ input: process.stdin }).on('line', line => {
  clientQueue = clientQueue
    .then(() => handleClientLine(line))
    .catch(() => toClient(rpcError(null, 'Firecrawl request policy check failed')));
});
process.stdin.on('end', () => {
  clientQueue.finally(() => child.stdin.end());
});

// server -> client
readline.createInterface({ input: child.stdout }).on('line', line => {
  let msg;
  try { msg = JSON.parse(line); } catch { process.stdout.write(line + '\n'); return; }
  const out = [msg].flat().map(m => {
    usage.observe(m);
    const rewritten = appendUsageTool(appendBrowserTools(rewriteResponse(m, ctx), allowedTools),allowedTools);
    if (m?.id !== undefined && (m.result !== undefined || m.error !== undefined)) compactIds.delete(m.id);
    return rewritten;
  });
  toClient(Array.isArray(msg) ? out : out[0]);
});

child.on('exit', code => process.exit(code ?? 0));
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => { child.kill(); process.exit(0); });
