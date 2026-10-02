const SESSION_ID = { type: 'string', minLength: 1, maxLength: 128, pattern: '^browser_[a-f0-9]+$' };
const TAB_ID = { type: 'string', minLength: 1, maxLength: 128, pattern: '^tab_[a-f0-9]+$' };

export const BROWSER_TOOL_DEFINITIONS = Object.freeze([
  {
    name: 'firecrawl_browser_open',
    description: 'Open a new ephemeral local Chromium session for a public HTTP(S) page. No saved profile or credentials are used. Use this only when stateful browsing is needed beyond ordinary firecrawl_scrape.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: { url: { type: 'string', minLength: 1, maxLength: 8192 } },
    },
  },
  {
    name: 'firecrawl_browser_list',
    description: 'List active ephemeral browser sessions and their tabs. Returns URLs and lifecycle metadata only.',
    inputSchema: { type: 'object', additionalProperties: false, properties: {} },
  },
  {
    name: 'firecrawl_browser_navigate',
    description: 'Navigate an existing ephemeral browser tab to a public HTTP(S) URL. Private/local destinations and credential-bearing URLs are blocked.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: {
        sessionId: SESSION_ID,
        tabId: TAB_ID,
        url: { type: 'string', minLength: 1, maxLength: 8192 },
        waitUntil: { type: 'string', enum: ['domcontentloaded', 'load', 'networkidle'], default: 'domcontentloaded' },
      },
      required: ['sessionId', 'url'],
    },
  },
  {
    name: 'firecrawl_browser_tabs',
    description: 'List tabs in an active ephemeral browser session.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: { sessionId: SESSION_ID }, required: ['sessionId'],
    },
  },
  {
    name: 'firecrawl_browser_snapshot',
    description: 'Read a token-efficient accessibility snapshot of an active browser tab. Falls back to visible text when an accessibility snapshot is unavailable.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: {
        sessionId: SESSION_ID,
        tabId: TAB_ID,
        maxChars: { type: 'integer', minimum: 1000, maximum: 30000, default: 20000 },
      },
      required: ['sessionId'],
    },
  },
  {
    name: 'firecrawl_browser_network',
    description: 'Read recent sanitized network events from an ephemeral browser session. Headers, cookies and request/response bodies are never returned; credential-like query values are redacted.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: {
        sessionId: SESSION_ID,
        tabId: TAB_ID,
        limit: { type: 'integer', minimum: 1, maximum: 200, default: 100 },
      },
      required: ['sessionId'],
    },
  },
  {
    name: 'firecrawl_browser_console',
    description: 'Read recent sanitized console and page-error messages from an ephemeral browser session. Output is length-bounded and common credential patterns are redacted.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: {
        sessionId: SESSION_ID,
        tabId: TAB_ID,
        limit: { type: 'integer', minimum: 1, maximum: 200, default: 100 },
      },
      required: ['sessionId'],
    },
  },
  {
    name: 'firecrawl_browser_screenshot',
    description: 'Capture a PNG screenshot of an active ephemeral browser tab.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: {
        sessionId: SESSION_ID,
        tabId: TAB_ID,
      },
      required: ['sessionId'],
    },
  },
  {
    name: 'firecrawl_browser_close',
    description: 'Close and destroy an ephemeral browser session and all of its tabs and storage.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: { sessionId: SESSION_ID }, required: ['sessionId'],
    },
  },
]);

export const BROWSER_TOOL_NAMES = Object.freeze(BROWSER_TOOL_DEFINITIONS.map(tool => tool.name));
const BROWSER_TOOL_SET = new Set(BROWSER_TOOL_NAMES);

export function isBrowserTool(name) { return BROWSER_TOOL_SET.has(String(name || '')); }

export function appendBrowserTools(msg, allowedTools) {
  if (!msg?.result?.tools || !Array.isArray(msg.result.tools)) return msg;
  const existing = new Set(msg.result.tools.map(tool => tool?.name));
  for (const tool of BROWSER_TOOL_DEFINITIONS) {
    if ((!allowedTools || allowedTools.has(tool.name)) && !existing.has(tool.name)) msg.result.tools.push(structuredClone(tool));
  }
  return msg;
}

function boundedInt(value, fallback, min, max) {
  const parsed = Number(value);
  if (!Number.isInteger(parsed)) return fallback;
  return Math.max(min, Math.min(max, parsed));
}

function safeId(value, prefix) {
  const text = String(value || '');
  if (!new RegExp(`^${prefix}_[a-f0-9]+$`).test(text) || text.length > 128) throw new Error(`invalid ${prefix} id`);
  return encodeURIComponent(text);
}

function query(params = {}) {
  const out = new URLSearchParams();
  if (params.tabId) out.set('tabId', safeId(params.tabId, 'tab'));
  if (params.limit !== undefined) out.set('limit', String(boundedInt(params.limit, 100, 1, 200)));
  if (params.maxChars !== undefined) out.set('maxChars', String(boundedInt(params.maxChars, 20000, 1000, 30000)));
  const value = out.toString();
  return value ? `?${value}` : '';
}

function sanitizeError(value) {
  return String(value ?? 'browser operation failed')
    .replace(/\bBearer\s+[A-Za-z0-9._~+\/-]{8,}=*/gi, 'Bearer [REDACTED]')
    .replace(/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/g, '[REDACTED_JWT]')
    .slice(0, 1000);
}

function textResult(payload, isError = false) {
  return { content: [{ type: 'text', text: JSON.stringify(payload) }], ...(isError ? { isError: true } : {}) };
}

async function responseJson(response) {
  const text = await response.text();
  if (!text) return {};
  try { return JSON.parse(text); } catch { return { error: `browser service returned non-JSON HTTP ${response.status}` }; }
}

export async function callBrowserTool(baseUrl, name, args = {}) {
  if (!isBrowserTool(name)) throw new Error(`unknown browser tool: ${name}`);
  const base = String(baseUrl || '').replace(/\/+$/, '');
  if (!/^https?:\/\//i.test(base)) return textResult({ error: 'browser service is not configured' }, true);

  let path = '';
  let method = 'GET';
  let body;
  try {
    switch (name) {
      case 'firecrawl_browser_open':
        path = '/sessions'; method = 'POST'; body = { ...(args.url ? { url: String(args.url) } : {}) }; break;
      case 'firecrawl_browser_list':
        path = '/sessions'; break;
      case 'firecrawl_browser_navigate':
        path = `/sessions/${safeId(args.sessionId, 'browser')}/navigate`; method = 'POST';
        body = { url: String(args.url || ''), ...(args.tabId ? { tabId: String(args.tabId) } : {}), ...(args.waitUntil ? { waitUntil: String(args.waitUntil) } : {}) };
        break;
      case 'firecrawl_browser_tabs':
        path = `/sessions/${safeId(args.sessionId, 'browser')}/tabs`; break;
      case 'firecrawl_browser_snapshot':
        path = `/sessions/${safeId(args.sessionId, 'browser')}/snapshot${query(args)}`; break;
      case 'firecrawl_browser_network':
        path = `/sessions/${safeId(args.sessionId, 'browser')}/network${query(args)}`; break;
      case 'firecrawl_browser_console':
        path = `/sessions/${safeId(args.sessionId, 'browser')}/console${query(args)}`; break;
      case 'firecrawl_browser_screenshot':
        path = `/sessions/${safeId(args.sessionId, 'browser')}/screenshot`; method = 'POST';
        body = { ...(args.tabId ? { tabId: String(args.tabId) } : {}) };
        break;
      case 'firecrawl_browser_close':
        path = `/sessions/${safeId(args.sessionId, 'browser')}`; method = 'DELETE'; break;
      default:
        throw new Error('unsupported browser tool');
    }

    const response = await fetch(`${base}${path}`, {
      method,
      headers: body ? { 'content-type': 'application/json' } : undefined,
      body: body ? JSON.stringify(body) : undefined,
      signal: AbortSignal.timeout(name === 'firecrawl_browser_open' || name === 'firecrawl_browser_navigate' ? 45_000 : 30_000),
    });
    const payload = await responseJson(response);
    if (!response.ok) return textResult({ error: sanitizeError(payload?.error || `browser service HTTP ${response.status}`) }, true);

    if (name === 'firecrawl_browser_screenshot') {
      if (typeof payload?.data !== 'string' || payload?.mimeType !== 'image/png') return textResult({ error: 'invalid screenshot response' }, true);
      const metadata = { sessionId: payload.sessionId, tabId: payload.tabId, url: payload.url };
      return { content: [{ type: 'text', text: JSON.stringify(metadata) }, { type: 'image', data: payload.data, mimeType: 'image/png' }] };
    }
    return textResult(payload);
  } catch (error) {
    return textResult({ error: sanitizeError(error?.message || error) }, true);
  }
}
