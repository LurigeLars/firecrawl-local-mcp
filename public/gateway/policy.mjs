// Shared MCP policy for the self-hosted Firecrawl: used by the public gateway (ChatGPT) and by the local stdio
// proxy (Claude desktop, Claude Code, Codex). Node standard library only.
import fs from 'node:fs';
import { lookup } from 'node:dns/promises';
import { BlockList, isIP } from 'node:net';

export const DEFAULT_ALLOWED_TOOLS =
  'firecrawl_scrape,firecrawl_map,firecrawl_search,firecrawl_crawl,firecrawl_check_crawl_status';

export function parseAllowedTools(value) {
  return new Set((value ?? DEFAULT_ALLOWED_TOOLS).split(',').map(s => s.trim()).filter(Boolean));
}

// Server instructions shown to the model; read on each initialize so edits apply without a restart.
export function loadInstructions() {
  try { return fs.readFileSync(new URL('./instructions.md', import.meta.url), 'utf8').trim() || null; } catch { return null; }
}

// Formats/options this instance cannot serve. Refusing them up front saves the client a slow round trip.
const UNSUPPORTED_FORMATS = new Set(['screenshot', 'branding', 'audio']);
const LOCAL_MODEL_FORMATS = new Set(['query', 'json', 'summary']);
const MAX_SEARCH_MODEL_RESULTS = 5;
const URL_FETCH_TOOLS = new Set(['firecrawl_scrape', 'firecrawl_map', 'firecrawl_crawl']);
const BLOCKED_DESTINATIONS = new BlockList();

for (const [network, prefix] of [
  ['0.0.0.0', 8],
  ['10.0.0.0', 8],
  ['100.64.0.0', 10],
  ['127.0.0.0', 8],
  ['169.254.0.0', 16],
  ['172.16.0.0', 12],
  ['192.0.0.0', 24],
  ['192.0.2.0', 24],
  ['192.168.0.0', 16],
  ['198.18.0.0', 15],
  ['198.51.100.0', 24],
  ['203.0.113.0', 24],
  ['224.0.0.0', 4],
  ['240.0.0.0', 4],
]) BLOCKED_DESTINATIONS.addSubnet(network, prefix, 'ipv4');

for (const [network, prefix] of [
  ['::', 128],
  ['::1', 128],
  ['::', 96],
  ['64:ff9b::', 96],
  ['100::', 64],
  ['2001:db8::', 32],
  ['fc00::', 7],
  ['fe80::', 10],
  ['ff00::', 8],
]) BLOCKED_DESTINATIONS.addSubnet(network, prefix, 'ipv6');

const formatNames = opts => (Array.isArray(opts?.formats) ? opts.formats : []).map(f => (typeof f === 'string' ? f : f?.type));
const scrapeOptions = params => (params?.name === 'firecrawl_crawl' ? params?.arguments?.scrapeOptions : params?.arguments) ?? {};

function normalizeHostname(hostname) {
  const value = String(hostname ?? '').trim().toLowerCase();
  return value.startsWith('[') && value.endsWith(']') ? value.slice(1, -1) : value;
}

function ipv4FromMappedIpv6(address) {
  const normalized = normalizeHostname(address);
  const dotted = normalized.match(/^(?:::ffff:|0:0:0:0:0:ffff:)(\d{1,3}(?:\.\d{1,3}){3})$/i);
  if (dotted && isIP(dotted[1]) === 4) return dotted[1];

  const hex = normalized.match(/^(?:::ffff:|0:0:0:0:0:ffff:)([0-9a-f]{1,4}):([0-9a-f]{1,4})$/i);
  if (!hex) return null;
  const high = Number.parseInt(hex[1], 16);
  const low = Number.parseInt(hex[2], 16);
  return [high >> 8, high & 255, low >> 8, low & 255].join('.');
}

function blockedAddress(address) {
  const normalized = normalizeHostname(address);
  const mapped = ipv4FromMappedIpv6(normalized);
  if (mapped) return BLOCKED_DESTINATIONS.check(mapped, 'ipv4');

  const family = isIP(normalized);
  if (family === 4) return BLOCKED_DESTINATIONS.check(normalized, 'ipv4');
  if (family === 6) return BLOCKED_DESTINATIONS.check(normalized, 'ipv6');
  return true;
}

export async function urlSafetyReason(params) {
  if (!URL_FETCH_TOOLS.has(params?.name)) return null;
  const raw = params?.arguments?.url;
  if (typeof raw !== 'string' || !raw.trim()) return 'A valid public HTTP(S) URL is required.';

  let target;
  try { target = new URL(raw); } catch { return 'A valid public HTTP(S) URL is required.'; }
  if (!['http:', 'https:'].includes(target.protocol)) return 'Only public HTTP(S) destinations are allowed.';
  if (target.username || target.password) return 'URLs containing credentials are not allowed.';

  const hostname = normalizeHostname(target.hostname);
  if (!hostname || hostname === 'localhost' || hostname.endsWith('.localhost')) {
    return 'Private or local network destinations are not allowed.';
  }

  const literalFamily = isIP(hostname);
  if (literalFamily) {
    return blockedAddress(hostname) ? 'Private or local network destinations are not allowed.' : null;
  }

  let resolved;
  try {
    resolved = await lookup(hostname, { all: true, verbatim: true });
  } catch {
    return 'Destination could not be safely resolved.';
  }
  if (!resolved.length || resolved.some(item => blockedAddress(item.address))) {
    return 'Private or local network destinations are not allowed.';
  }
  return null;
}

export function unsupportedReason(params) {
  const opts = scrapeOptions(params);
  const bad = formatNames(opts).filter(f => UNSUPPORTED_FORMATS.has(f));
  if (bad.length) return `Format(s) ${bad.join(', ')} are not available on this self-hosted Firecrawl. Use markdown, html, rawHtml, links, summary, query or json instead.`;
  if (Array.isArray(opts.actions) && opts.actions.length) return 'Browser actions (click, scroll, write, screenshot) are not available on this self-hosted Firecrawl. Scrape the page as-is, or scrape each paginated URL directly.';
  // query/json/summary on search hits: Gemini (15 requests/min) answers a few pages in seconds, but the local fallback
  // handles one page at a time (1-2 min each), so more hits would run into the 5-minute call limit.
  if (params?.name === 'firecrawl_search') {
    const modelFormats = formatNames(params.arguments?.scrapeOptions).filter(f => LOCAL_MODEL_FORMATS.has(f));
    const limit = Number(params.arguments?.limit ?? 10); // Firecrawl's default
    if (modelFormats.length && !(limit <= MAX_SEARCH_MODEL_RESULTS)) return `firecrawl_search can run ${modelFormats.join(', ')} on at most ${MAX_SEARCH_MODEL_RESULTS} results (limit is ${params.arguments?.limit ?? 'unset, default 10'}). Set limit to ${MAX_SEARCH_MODEL_RESULTS} or less, or search without scrapeOptions and then use firecrawl_scrape on the best URLs.`;
  }
  return null;
}

// A scrape that only asks the local model for an answer, JSON or summary does not need the ~500 tokens of page
// metadata (og tags, analytics ids, favicon ...). Markdown/html/links results keep their metadata (SEO work uses it).
export function wantsCompactResult(params) {
  if (params?.name !== 'firecrawl_scrape') return false;
  const formats = formatNames(params.arguments);
  return formats.length > 0 && formats.every(f => LOCAL_MODEL_FORMATS.has(f));
}

const KEEP_METADATA = ['title', 'url', 'sourceURL', 'statusCode', 'language', 'contentType'];
export function compactToolResult(result) {
  const item = result?.content?.[0];
  if (result?.isError || item?.type !== 'text' || typeof item.text !== 'string') return result;
  let data;
  try { data = JSON.parse(item.text); } catch { return result; }
  if (!data || typeof data !== 'object' || Array.isArray(data)) return result;
  const compact = {};
  for (const key of ['answer', 'json', 'summary', 'warning']) if (data[key] !== undefined) compact[key] = data[key];
  if (data.metadata && typeof data.metadata === 'object') {
    const source = {};
    for (const key of KEEP_METADATA) if (data.metadata[key] !== undefined) source[key] = data.metadata[key];
    compact.source = source;
  }
  if (!('answer' in compact || 'json' in compact || 'summary' in compact)) return result;
  return { ...result, content: [{ ...item, text: JSON.stringify(compact) }, ...result.content.slice(1)] };
}

// Applies tool filtering, instructions and compaction to one JSON-RPC response.
// `compactIds` holds the ids of tools/call requests whose results may be compacted.
export function rewriteResponse(msg, { allowedTools, compactIds }) {
  if (!msg || typeof msg !== 'object') return msg;
  if (msg.result?.tools) msg.result.tools = msg.result.tools.filter(t => allowedTools.has(t.name));
  if (msg.result?.serverInfo) {
    const instructions = loadInstructions();
    if (instructions) msg.result.instructions = instructions;
  }
  if (compactIds?.has(msg.id) && msg.result) msg.result = compactToolResult(msg.result);
  return msg;
}

// Checks one JSON-RPC request. Returns { error } (JSON-RPC error), { toolError } (tool-level error text) or {}.
export function checkRequest(msg, allowedTools) {
  if (msg?.method !== 'tools/call') return {};
  if (!allowedTools.has(msg.params?.name)) return { error: `Tool not available on this server: ${msg.params?.name}` };
  const reason = unsupportedReason(msg.params);
  return reason ? { toolError: reason } : {};
}

export const rpcError = (id, message) => ({ jsonrpc: '2.0', id: id ?? null, error: { code: -32601, message } });
export const rpcToolError = (id, message) => ({ jsonrpc: '2.0', id: id ?? null, result: { content: [{ type: 'text', text: message }], isError: true } });
