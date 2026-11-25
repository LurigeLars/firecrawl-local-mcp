// LLM proxy between Firecrawl and the models. Node standard library only.
//
// Gemini first (optional): when the runtime tmpfs secret is present, OpenAI Responses calls (/v1/responses;
// Firecrawl's query, json and summary formats all use them) go to Gemini's OpenAI-compatible chat endpoint and the answer is converted
// back. Any Gemini failure (rate limit, quota, bad schema, timeout) falls back to the local model; after a 429 Gemini
// is skipped for GEMINI_PAUSE_S seconds. Page text and prompts are sent to Google when this is on.
//
// Local model: Firecrawl sends whole pages (it assumes a 128k-token model). Ollama then silently keeps only the tail
// of an over-long prompt, dropping the instructions and the top of the page. So for Ollama the longest text field is
// cut from the END to fit MAX_INPUT_TOKENS before forwarding.
import http from 'node:http';
import { readFileSync } from 'node:fs';
import { pathToFileURL } from 'node:url';

const UPSTREAM = new URL('http://host.docker.internal:11434');
const PORT = Number(process.env.PORT ?? 11435);
const MAX_INPUT_TOKENS = Number(process.env.MAX_INPUT_TOKENS ?? 7000);
// Conservative: real text averages ~3.5-4 chars/token; markdown with URLs is denser.
const CHARS_PER_TOKEN = Number(process.env.CHARS_PER_TOKEN ?? 3);
const MAX_CHARS = MAX_INPUT_TOKENS * CHARS_PER_TOKEN;
const MARKER = '\n\n[... content truncated to fit the local model context ...]';
export const STRICT_CONTRACT = 'strict-v1';
const CONTRACT_HEADER = 'x-firecrawl-model-contract';
const STRICT_BODY_LIMIT = 64 * 1024;

export const GEMINI_SECRET_PATH = '/run/firecrawl-secrets/gemini_api_key';
const GEMINI_BASE_URL = 'https://generativelanguage.googleapis.com/v1beta/openai';

export function readGeminiApiKey(path = GEMINI_SECRET_PATH) {
  try {
    return readFileSync(path, 'utf8').trim();
  } catch {
    return '';
  }
}
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

export function fitsContext(body, maxChars = MAX_CHARS) {
  return Number.isFinite(maxChars) && maxChars > 0 &&
    textSlots(body).reduce((n,s)=>n+s.holder[s.key].length,0) <= maxChars;
}

// Strict extraction accepts only the bounded, plain-text request shape the
// Rust candidate constructs. No tools, caller backend, file or streaming.
export function validStrictRequest(body) {
  if (!body || typeof body !== 'object' || Array.isArray(body)) return false;
  if (Object.keys(body).some(k=>!['model','instructions','input','stream','max_output_tokens','text'].includes(k))) return false;
  if (typeof body.model !== 'string' || !body.model.trim() || body.model.length>128 ||
      typeof body.instructions !== 'string' || !body.instructions.trim() ||
      body.stream !== false || !Number.isInteger(body.max_output_tokens) ||
      body.max_output_tokens<1 || body.max_output_tokens>2048 ||
      !Array.isArray(body.input) || body.input.length!==1) return false;
  const item=body.input[0];
  if (item?.role!=='user' || !Array.isArray(item.content) || item.content.length!==1 ||
      Object.keys(item).some(k=>!['role','content'].includes(k))) return false;
  const part=item.content[0];
  if (part?.type!=='input_text' || typeof part.text!=='string' || !part.text.trim() ||
      Object.keys(part).some(k=>!['type','text'].includes(k))) return false;
  if (body.text!==undefined) {
    const format=body.text?.format;
    if (!format || format.type!=='json_schema' || format.name!=='extraction' ||
        !format.schema || typeof format.schema!=='object' || Array.isArray(format.schema) ||
        JSON.stringify(format.schema).length>8192 ||
        Object.keys(body.text).some(k=>k!=='format') ||
        Object.keys(format).some(k=>!['type','name','schema'].includes(k))) return false;
  }
  return fitsContext(body,16000);
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
  const choice = chat.choices?.[0];
  const message = choice?.message;
  const text = typeof message?.content === 'string' ? message.content : '';
  const finish = choice?.finish_reason;
  const complete = finish === 'stop' && !!text.trim() && !message?.refusal &&
    !message?.tool_calls?.length && !message?.function_call;
  const incomplete = finish === 'length' || finish === 'content_filter';
  const status = complete ? 'completed' : incomplete ? 'incomplete' : 'failed';
  const now = Math.floor(Date.now() / 1000);
  const id = `resp_gemini_${now}_${Math.random().toString(36).slice(2, 10)}`;
  return {
    id, object: 'response', created_at: now, status, model: chat.model ?? GEMINI_MODEL,
    ...(incomplete ? { incomplete_details: { reason: finish === 'length' ? 'max_output_tokens' : 'content_filter' } } : {}),
    ...(status === 'failed' ? { error: { code: 'server_error', message: 'Provider did not return a complete text answer.' } } : {}),
    output: status === 'failed' ? [] : [{ type: 'message', id: `msg_${id}`, status, role: 'assistant',
      content: [{ type: 'output_text', text, annotations: [] }] }],
    usage: {
      input_tokens: chat.usage?.prompt_tokens ?? 0, output_tokens: chat.usage?.completion_tokens ?? 0,
      total_tokens: chat.usage?.total_tokens ?? 0,
      input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 },
    },
    text: request.text ?? { format: { type: 'text' } },
  };
}

async function askGemini(chat, apiKey) {
  const res = await fetch(`${GEMINI_BASE_URL}/chat/completions`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${apiKey}` },
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
async function tryGemini(path, raw, strict = false) {
  const geminiApiKey = readGeminiApiKey();
  if (!geminiApiKey || path !== '/v1/responses' || Date.now() < geminiPausedUntil) return null;
  let request, chat;
  try { request = JSON.parse(raw.toString('utf8')); chat = responsesToChat(request); } catch { return null; }
  if (!chat) return null;
  if (strict && !fitsContext(chat, GEMINI_MAX_CHARS)) return null;
  if (!strict) trim(chat, GEMINI_MAX_CHARS);
  const started = Date.now();
  try {
    const answer = await askGemini(chat, geminiApiKey);
    console.log(`${new Date().toISOString()} gemini ok in ${Date.now() - started} ms`);
    return chatToResponses(answer, request);
  } catch (err) {
    if (err.status === 429) geminiPausedUntil = Date.now() + GEMINI_PAUSE_MS;
    console.warn(`${new Date().toISOString()} gemini failed, using local model`);
    return null;
  }
}

function strictHeaders(provider) {
  return { [CONTRACT_HEADER]: STRICT_CONTRACT, 'x-firecrawl-input-trimmed': 'false', 'x-firecrawl-model-provider': provider };
}
function rejectStrict(res,status,code) {
  res.writeHead(status,{'content-type':'application/json'});
  res.end(JSON.stringify({error:code}));
}

function forwardLocal(req, res, body, strict = false, parsed = null) {
  const ct = req.headers['content-type'] ?? '';
  if (strict && !fitsContext(parsed,MAX_CHARS)) return rejectStrict(res,422,'STRICT_CONTEXT_LIMIT');
  if (!strict && req.method === 'POST' && ct.includes('application/json') && body.length > MAX_CHARS) {
    try {
      const json = JSON.parse(body.toString('utf8'));
      const r = trim(json);
      if (r) {
        body = Buffer.from(JSON.stringify(json));
        console.log(`${new Date().toISOString()} trimmed request text ${r.before} -> ${r.after} chars`);
      }
    } catch { /* not JSON we understand: forward as-is */ }
  }
  const headers = { ...req.headers, host: UPSTREAM.host, 'content-length': String(body.length) };
  delete headers[CONTRACT_HEADER];
  delete headers['x-firecrawl-input-trimmed'];
  delete headers['x-firecrawl-model-provider'];
  const up = http.request(
    { hostname: UPSTREAM.hostname, port: UPSTREAM.port, method: req.method, path: req.url, headers },
    upRes => {
      const responseHeaders={...upRes.headers};
      for (const key of Object.keys(strictHeaders('ollama'))) delete responseHeaders[key];
      if (strict) Object.assign(responseHeaders,strictHeaders('ollama'));
      res.writeHead(upRes.statusCode ?? 502,responseHeaders);upRes.pipe(res);
    },
  );
  up.on('error', err => {
    console.warn('upstream error');
    if (!res.headersSent) { res.writeHead(502); res.end('ollama unavailable'); } else res.destroy();
  });
  res.on('close', () => { if (!res.writableFinished) up.destroy(); });
  up.end(body);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  http.createServer((req, res) => {
    const contract=req.headers[CONTRACT_HEADER];
    const strict=contract===STRICT_CONTRACT;
    if (contract!==undefined&&!strict) return rejectStrict(res,400,'UNKNOWN_MODEL_CONTRACT');
    if (strict&&(req.method!=='POST'||req.url!=='/v1/responses'||
        !(req.headers['content-type']??'').includes('application/json'))) {
      return rejectStrict(res,400,'INVALID_STRICT_REQUEST');
    }
    const chunks = [];
    let size=0,rejected=false;
    req.on('data', c => {
      if (rejected) return;
      size+=c.length;
      if (strict&&size>STRICT_BODY_LIMIT) {
        rejected=true;chunks.length=0;rejectStrict(res,413,'STRICT_BODY_LIMIT');return;
      }
      chunks.push(c);
    });
    req.on('end', async () => {
      if (rejected) return;
      const body = Buffer.concat(chunks);
      let parsed=null;
      if (strict) {
        try {parsed=JSON.parse(body.toString('utf8'));} catch {return rejectStrict(res,400,'INVALID_STRICT_JSON');}
        if (!validStrictRequest(parsed)) return rejectStrict(res,400,'INVALID_STRICT_REQUEST');
      }
      const answer = req.method === 'POST' ? await tryGemini(new URL(req.url, 'http://x').pathname, body,strict) : null;
      if (!answer) return forwardLocal(req, res, body,strict,parsed);
      const out = JSON.stringify(answer);
      res.writeHead(200, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(out),
        ...(strict?strictHeaders('gemini'):{}) });
      res.end(out);
    });
  }).listen(PORT, '0.0.0.0', () => console.log(
    `llm proxy on ${PORT}: gemini ${GEMINI_MODEL} when runtime secret is present, then ${UPSTREAM.origin}; local max ${MAX_INPUT_TOKENS} tokens (~${MAX_CHARS} chars)`));
}
