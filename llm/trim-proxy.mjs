// LLM proxy between Firecrawl and the models. Node standard library only.
//
// Gemini first (optional): when GEMINI_API_KEY is set, OpenAI Responses calls (/v1/responses; Firecrawl's query,
// json and summary formats all use them) go to Gemini's OpenAI-compatible chat endpoint and the answer is converted
// back. Any Gemini failure (rate limit, quota, bad schema, timeout) falls back to the local model; after a 429 Gemini
// is skipped for GEMINI_PAUSE_S seconds. Page text and prompts are sent to Google when this is on.
//
// Local model: Firecrawl sends whole pages (it assumes a 128k-token model). Ollama then silently keeps only the tail
// of an over-long prompt, dropping the instructions and the top of the page. So for Ollama the longest text field is
// cut from the END to fit MAX_INPUT_TOKENS before forwarding.
import http from 'node:http';
import { pathToFileURL } from 'node:url';

const logSafe = value => String(value ?? '').replace(/[\r\n\u2028\u2029]/g, ' ');
function checkedEndpoint(raw, { label, protocols, hosts }) {
  const url = new URL(raw);
  if (!protocols.has(url.protocol) || !hosts.has(url.hostname) || url.username || url.password) {
    throw new Error(`${label} must use an approved endpoint`);
  }
  return url;
}
const UPSTREAM = checkedEndpoint(process.env.UPSTREAM ?? 'http://host.docker.internal:11434', {
  label: 'UPSTREAM', protocols: new Set(['http:']),
  hosts: new Set(['host.docker.internal', '127.0.0.1', 'localhost', 'ollama']),
});
const PORT = Number(process.env.PORT ?? 11435);
const MAX_INPUT_TOKENS = Number(process.env.MAX_INPUT_TOKENS ?? 7000);
// Conservative: real text averages ~3.5-4 chars/token; markdown with URLs is denser.
const CHARS_PER_TOKEN = Number(process.env.CHARS_PER_TOKEN ?? 3);
const MAX_CHARS = MAX_INPUT_TOKENS * CHARS_PER_TOKEN;
const MARKER = '\n\n[... content truncated to fit the local model context ...]';

const GEMINI_API_KEY = process.env.GEMINI_API_KEY ?? '';
const GEMINI_BASE_URL = checkedEndpoint(process.env.GEMINI_BASE_URL ?? 'https://generativelanguage.googleapis.com/v1beta/openai', {
  label: 'GEMINI_BASE_URL', protocols: new Set(['https:']), hosts: new Set(['generativelanguage.googleapis.com']),
}).href.replace(/\/$/, '');
const GEMINI_MODEL = process.env.GEMINI_MODEL ?? 'gemini-3.5-flash-lite';
// Free tier (AI Studio, 2026-09-16): 15 requests and 250k input tokens per minute, 500 requests per day; ~60k chars (~15k tokens) lets 15 pages fit.
const GEMINI_MAX_CHARS = Number(process.env.GEMINI_MAX_CHARS ?? 60_000);
const GEMINI_TIMEOUT_MS = Number(process.env.GEMINI_TIMEOUT_S ?? 60) * 1000;
const GEMINI_PAUSE_MS = Number(process.env.GEMINI_PAUSE_S ?? 60) * 1000;
let geminiPausedUntil = 0;

// Collects references to every string in message/input content (not schemas or options).
function textSlots(body) {
  const slots = [];
  const visit = (holder, key) => {
    const v = holder[key];
    if (typeof v === 'string') slots.push({ holder, key });
    else if (Array.isArray(v)) v.forEach((_, i) => visit(v, i));
    else if (v && typeof v === 'object') {
      for (const k of ['content', 'text', 'input', 'prompt', 'system', 'instructions']) if (k in v) visit(v, k);
    }
  };
  for (const k of ['messages', 'input', 'prompt', 'system', 'instructions']) if (k in body) visit(body, k);
  return slots;
}

export function trim(body, maxChars = MAX_CHARS) {
  const slots = textSlots(body);
  const total = slots.reduce((n, s) => n + s.holder[s.key].length, 0);
  if (total <= maxChars || slots.length === 0) return null;
  const longest = slots.reduce((a, b) => (b.holder[b.key].length > a.holder[a.key].length ? b : a));
  const text = longest.holder[longest.key];
  const keep = Math.max(0, text.length - (total - maxChars) - MARKER.length);
  longest.holder[longest.key] = text.slice(0, keep) + MARKER;
  return { before: total, after: total - text.length + longest.holder[longest.key].length };
}

const partText = c => (typeof c === 'string' ? c
  : Array.isArray(c) ? c.map(p => (typeof p === 'string' ? p : p?.text ?? '')).join('') : '');
const withoutSchemaKey = s => JSON.parse(JSON.stringify(s, (k, v) => (k === '$schema' ? undefined : v)));

// OpenAI Responses request -> OpenAI chat completions request (Gemini). Returns null for anything not plain text.
export function responsesToChat(body) {
  if (body.stream || (Array.isArray(body.tools) && body.tools.length)) return null;
  const messages = [];
  if (body.instructions) messages.push({ role: 'system', content: String(body.instructions) });
  const input = typeof body.input === 'string' ? [{ role: 'user', content: body.input }] : body.input ?? [];
  for (const item of input) {
    if (!item?.role || (item.type && item.type !== 'message')) return null;
    if (Array.isArray(item.content) && item.content.some(p => p?.type && !/text$/.test(p.type))) return null; // images, files
    messages.push({ role: item.role === 'developer' ? 'system' : item.role, content: partText(item.content) });
  }
  if (!messages.length) return null;
  const chat = { model: GEMINI_MODEL, messages };
  const format = body.text?.format;
  if (format?.type === 'json_schema') {
    chat.response_format = { type: 'json_schema', json_schema: { name: format.name ?? 'output', schema: withoutSchemaKey(format.schema ?? {}) } };
  } else if (format?.type === 'json_object') chat.response_format = { type: 'json_object' };
  if (typeof body.temperature === 'number') chat.temperature = body.temperature;
  if (typeof body.max_output_tokens === 'number') chat.max_tokens = body.max_output_tokens;
  return chat;
}

// OpenAI chat completion response -> OpenAI Responses response.
export function chatToResponses(chat, request) {
  const text = chat.choices?.[0]?.message?.content ?? '';
  const now = Math.floor(Date.now() / 1000);
  const id = `resp_gemini_${now}_${Math.random().toString(36).slice(2, 10)}`;
  return {
    id, object: 'response', created_at: now, status: 'completed', model: chat.model ?? GEMINI_MODEL,
    output: [{ type: 'message', id: `msg_${id}`, status: 'completed', role: 'assistant',
      content: [{ type: 'output_text', text, annotations: [] }] }],
    usage: {
      input_tokens: chat.usage?.prompt_tokens ?? 0, output_tokens: chat.usage?.completion_tokens ?? 0,
      total_tokens: chat.usage?.total_tokens ?? 0,
      input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 },
    },
    text: request.text ?? { format: { type: 'text' } },
  };
}

async function askGemini(chat) {
  const res = await fetch(`${GEMINI_BASE_URL}/chat/completions`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${GEMINI_API_KEY}` },
    body: JSON.stringify(chat),
    signal: AbortSignal.timeout(GEMINI_TIMEOUT_MS),
  });
  if (!res.ok) {
    const detail = (await res.text().catch(() => '')).replace(/\s+/g, ' ').slice(0, 200);
    throw Object.assign(new Error(`HTTP ${res.status} ${detail}`), { status: res.status });
  }
  const out = await res.json();
  if (!out.choices?.[0]?.message?.content) throw new Error('empty answer');
  return out;
}

// Returns a Responses JSON body from Gemini, or null to use the local model.
async function tryGemini(path, raw) {
  if (!GEMINI_API_KEY || path !== '/v1/responses' || Date.now() < geminiPausedUntil) return null;
  let request, chat;
  try { request = JSON.parse(raw.toString('utf8')); chat = responsesToChat(request); } catch { return null; }
  if (!chat) return null;
  trim(chat, GEMINI_MAX_CHARS);
  const started = Date.now();
  try {
    const answer = await askGemini(chat);
    console.log(`${new Date().toISOString()} ${logSafe(path)} gemini ok in ${Date.now() - started} ms, ${answer.usage?.total_tokens ?? "?"} tokens`);
    return chatToResponses(answer, request);
  } catch (err) {
    if (err.status === 429) geminiPausedUntil = Date.now() + GEMINI_PAUSE_MS;
    console.warn(`${new Date().toISOString()} ${logSafe(path)} gemini failed (${err.name === 'TimeoutError' ? 'timeout' : logSafe(err.message)}), using local model`);
    return null;
  }
}

function forwardLocal(req, res, body) {
  const ct = req.headers['content-type'] ?? '';
  if (req.method === 'POST' && ct.includes('application/json') && body.length > MAX_CHARS) {
    try {
      const json = JSON.parse(body.toString('utf8'));
      const r = trim(json);
      if (r) {
        body = Buffer.from(JSON.stringify(json));
        console.log(`${new Date().toISOString()} ${logSafe(req.url)} trimmed text ${r.before} -> ${r.after} chars`);
      }
    } catch { /* not JSON we understand: forward as-is */ }
  }
  const headers = { ...req.headers, host: UPSTREAM.host, 'content-length': String(body.length) };
  const up = http.request(
    { hostname: UPSTREAM.hostname, port: UPSTREAM.port, method: req.method, path: req.url, headers },
    upRes => { res.writeHead(upRes.statusCode ?? 502, upRes.headers); upRes.pipe(res); },
  );
  up.on('error', err => {
    console.warn(`upstream error: ${logSafe(err.code ?? err.message)}`);
    if (!res.headersSent) { res.writeHead(502); res.end('ollama unavailable'); } else res.destroy();
  });
  res.on('close', () => { if (!res.writableFinished) up.destroy(); });
  up.end(body);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  http.createServer((req, res) => {
    const chunks = [];
    req.on('data', c => chunks.push(c));
    req.on('end', async () => {
      const body = Buffer.concat(chunks);
      const answer = req.method === 'POST' ? await tryGemini(new URL(req.url, 'http://x').pathname, body) : null;
      if (!answer) return forwardLocal(req, res, body);
      const out = JSON.stringify(answer);
      res.writeHead(200, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(out) });
      res.end(out);
    });
  }).listen(PORT, '0.0.0.0', () => console.log(
    `llm proxy on ${PORT}: ${GEMINI_API_KEY ? `gemini ${GEMINI_MODEL} first, then ` : ''}${UPSTREAM.origin}, local max ${MAX_INPUT_TOKENS} tokens (~${MAX_CHARS} chars)`));
}
