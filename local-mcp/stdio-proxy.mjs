// Local stdio MCP proxy for the self-hosted Firecrawl (Claude desktop, Claude Code, Codex).
// Spawns firecrawl-mcp over stdio and applies the same rules as the public gateway (../public/gateway/policy.mjs):
// only the tools that work on this instance are listed and callable, unsupported formats are refused up front,
// server instructions are replaced, and query/json/summary scrape results drop their bulky page metadata.
// Node standard library only. Usage: node stdio-proxy.mjs   (env FIRECRAWL_API_URL, optional ALLOWED_TOOLS)
import { spawn } from 'node:child_process';
import readline from 'node:readline';
import { fileURLToPath } from 'node:url';
import { ensureLockedRuntime } from './locked-runtime.mjs';
import { candidateApiEnvironment } from '../public/mcp-api-env.mjs';
import {
  parseAllowedTools, checkRequest, urlSafetyReason, wantsCompactResult, rememberCrawlRequest, rewriteResponse, rpcError, rpcToolError,
} from '../public/gateway/policy.mjs';
import { BROWSER_TOOL_NAMES, appendBrowserTools, callBrowserTool, isBrowserTool } from '../public/gateway/browser-tools.mjs';

const childEnv = candidateApiEnvironment(process.env, 'http://127.0.0.1:3002', 'http://127.0.0.1:3002');

// The exact firecrawl-mcp tree locked in public/package.json + public/package-lock.json.
const { entry: mcpEntry } = ensureLockedRuntime(
  fileURLToPath(new URL('../public/', import.meta.url)), fileURLToPath(new URL('./.runtime/', import.meta.url)));

const DEFAULT_LOCAL_TOOLS = `firecrawl_scrape,firecrawl_map,firecrawl_search,firecrawl_crawl,firecrawl_check_crawl_status,${BROWSER_TOOL_NAMES.join(',')}`;
const allowedTools = parseAllowedTools(process.env.ALLOWED_TOOLS ?? DEFAULT_LOCAL_TOOLS);
const browserServiceUrl = process.env.BROWSER_SERVICE_URL ?? 'http://127.0.0.1:3010';
const compactIds = new Set();
const crawlRequests = new Map();
const ctx = { allowedTools, compactIds, crawlRequests };

const child = spawn(process.execPath, [mcpEntry],
  { env: childEnv, stdio: ['pipe', 'pipe', 'inherit'] });

const toClient = obj => process.stdout.write(JSON.stringify(obj) + '\n');

// client -> server. Serialize asynchronous DNS checks so pipelined MCP
// requests preserve their original order when forwarded to the child process.
async function handleClientLine(line) {
  if (!line.trim()) return;
  let msg;
  try { msg = JSON.parse(line); } catch { child.stdin.write(line + '\n'); return; }
  const messages = [msg].flat();
  const browserCalls = messages.filter(m => m?.method === 'tools/call' && isBrowserTool(m.params?.name));
  if (browserCalls.length) {
    if (messages.length !== 1) return toClient(rpcError(browserCalls[0]?.id, 'Browser tools cannot be batched with other JSON-RPC messages.'));
    const m = browserCalls[0];
    const verdict = checkRequest(m, allowedTools);
    if (verdict.error) return toClient(rpcError(m.id, verdict.error));
    if (verdict.toolError) return toClient(rpcToolError(m.id, verdict.toolError));
    const result = await callBrowserTool(browserServiceUrl, m.params?.name, m.params?.arguments ?? {});
    return toClient({ jsonrpc: '2.0', id: m.id ?? null, result });
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
    const rewritten = appendBrowserTools(rewriteResponse(m, ctx), allowedTools);
    if (m?.id !== undefined && (m.result !== undefined || m.error !== undefined)) compactIds.delete(m.id);
    return rewritten;
  });
  toClient(Array.isArray(msg) ? out : out[0]);
});

child.on('exit', code => process.exit(code ?? 0));
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => { child.kill(); process.exit(0); });
