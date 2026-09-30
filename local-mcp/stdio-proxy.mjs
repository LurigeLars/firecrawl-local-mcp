// Local stdio MCP proxy for the self-hosted Firecrawl (Claude desktop, Claude Code, Codex).
// Spawns firecrawl-mcp over stdio and applies the same rules as the public gateway (../public/gateway/policy.mjs):
// only the tools that work on this instance are listed and callable, unsupported formats are refused up front,
// server instructions are replaced, and query/json/summary scrape results drop their bulky page metadata.
// Node standard library only. Usage: node stdio-proxy.mjs   (env FIRECRAWL_API_URL, optional ALLOWED_TOOLS)
import { spawn } from 'node:child_process';
import readline from 'node:readline';
import { readFileSync } from 'node:fs';
import {
  parseAllowedTools, checkRequest, urlSafetyReason, wantsCompactResult, rememberCrawlRequest, rewriteResponse, rpcError, rpcToolError,
} from '../public/gateway/policy.mjs';

const manifest = JSON.parse(readFileSync(new URL('../public/package.json', import.meta.url), 'utf8'));
const mcpVersion = String(manifest?.dependencies?.['firecrawl-mcp'] ?? '');
if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(mcpVersion)) {
  throw new Error('public/package.json does not contain an exact firecrawl-mcp version');
}
const mcpSpec = `firecrawl-mcp@${mcpVersion}`;

const allowedTools = parseAllowedTools(process.env.ALLOWED_TOOLS);
const compactIds = new Set();
const crawlRequests = new Map();
const ctx = { allowedTools, compactIds, crawlRequests };

const isWindows = process.platform === 'win32';
const child = spawn(isWindows ? 'cmd.exe' : 'npx',
  isWindows ? ['/d', '/s', '/c', 'npx', '-y', mcpSpec] : ['-y', mcpSpec],
  { env: { ...process.env, FIRECRAWL_API_URL: process.env.FIRECRAWL_API_URL ?? 'http://127.0.0.1:3002' }, stdio: ['pipe', 'pipe', 'inherit'] });

const toClient = obj => process.stdout.write(JSON.stringify(obj) + '\n');

// client -> server. Serialize asynchronous DNS checks so pipelined MCP
// requests preserve their original order when forwarded to the child process.
async function handleClientLine(line) {
  if (!line.trim()) return;
  let msg;
  try { msg = JSON.parse(line); } catch { child.stdin.write(line + '\n'); return; }
  for (const m of [msg].flat()) {
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
    const rewritten = rewriteResponse(m, ctx);
    if (m?.id !== undefined && (m.result !== undefined || m.error !== undefined)) compactIds.delete(m.id);
    return rewritten;
  });
  toClient(Array.isArray(msg) ? out : out[0]);
});

child.on('exit', code => process.exit(code ?? 0));
for (const sig of ['SIGINT', 'SIGTERM']) process.on(sig, () => { child.kill(); process.exit(0); });
