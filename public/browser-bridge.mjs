import http from 'node:http';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { URL } from 'node:url';
import crypto from 'node:crypto';

const PORT = Number(process.env.BROWSER_BRIDGE_PORT || 8765);
if (!Number.isInteger(PORT) || PORT < 1024 || PORT > 65535) {
  console.error('BROWSER_BRIDGE_PORT must be an integer from 1024 to 65535; refusing to start');
  process.exit(1);
}
const TOKEN = String(process.env.BROWSER_BRIDGE_TOKEN || '');
// The token is deployment configuration; strict format/length validation fails closed before the server starts.
// lgtm[js/user-controlled-bypass]
if (!/^[A-Za-z0-9_-]{43,128}$/.test(TOKEN)) {
  console.error('BROWSER_BRIDGE_TOKEN must be a 43-128 character base64url-style secret; refusing to start');
  process.exit(1);
}

const LOCAL_ROOT = path.resolve(process.env.LOCALAPPDATA || path.join(os.homedir(), 'AppData', 'Local'), 'FirecrawlLocal');
const SESSION_ROOT = path.resolve(LOCAL_ROOT, 'browser-profiles');
const SESSIONS = Object.freeze({
  'season-spendrups': { port: 9440, startUrl: 'https://ehandel.spendrups.se/', hosts: ['ehandel.spendrups.se', 'spendrups.se'] },
  'season-ms': { port: 9441, startUrl: 'https://www.martinservera.se/', hosts: ['martinservera.se'] },
});
const states = new Map();

function json(res, status, body) {
  const data = Buffer.from(JSON.stringify(body));
  res.writeHead(status, { 'content-type': 'application/json', 'content-length': String(data.length), 'cache-control': 'no-store' });
  res.end(data);
}
function unauthorized(res) { json(res, 401, { error: 'unauthorized' }); }
// Compare the complete bearer credential in constant time; never authorize on user-derived metadata.
const EXPECTED_AUTH = Buffer.from(`Bearer ${TOKEN}`);
function validAuth(req) {
  const got = Buffer.from(String(req.headers.authorization || ''));
  return got.length === EXPECTED_AUTH.length && crypto.timingSafeEqual(got, EXPECTED_AUTH);
}
function sessionConfig(name) {
  const cfg = SESSIONS[String(name || '')];
  if (!cfg) throw new Error('unknown session');
  return cfg;
}
function chromeCandidates() {
  const out = [];
  for (const base of [process.env.PROGRAMFILES, process.env['PROGRAMFILES(X86)'], process.env.LOCALAPPDATA]) {
    if (base) out.push(path.join(base, 'Google', 'Chrome', 'Application', 'chrome.exe'));
  }
  return out;
}
function isWithin(root, candidate) {
  const relative = path.relative(path.resolve(root), path.resolve(candidate));
  return relative !== '' && !relative.startsWith('..') && !path.isAbsolute(relative);
}
function allowedChromeRoots() {
  return [process.env.PROGRAMFILES, process.env['PROGRAMFILES(X86)'], process.env.LOCALAPPDATA]
    .filter(Boolean)
    .map(root => path.resolve(root));
}
function validChromeExecutable(candidate) {
  const resolved = path.resolve(String(candidate || ''));
  if (path.basename(resolved).toLowerCase() !== 'chrome.exe') return null;
  if (!allowedChromeRoots().some(root => isWithin(root, resolved))) return null;
  try {
    if (!fs.statSync(resolved).isFile()) return null;
  } catch {
    return null;
  }
  return resolved;
}
function resolveChrome() {
  const configured = String(process.env.BROWSER_CHROME_EXE || '').trim();
  const candidates = configured ? [configured] : chromeCandidates();
  for (const candidate of candidates) {
    const resolved = validChromeExecutable(candidate);
    if (resolved) return resolved;
  }
  throw new Error('Google Chrome executable not found');
}
function profilePath(sessionName) {
  const candidate = path.resolve(SESSION_ROOT, String(sessionName || ''));
  const relative = path.relative(SESSION_ROOT, candidate);
  if (!relative || relative.startsWith('..') || path.isAbsolute(relative)) throw new Error('invalid browser profile path');
  return candidate;
}
function allowedUrl(value, cfg) {
  try {
    const u = new URL(value);
    if (u.protocol !== 'https:') return false;
    const h = u.hostname.toLowerCase();
    return cfg.hosts.some(base => h === base || h.endsWith(`.${base}`));
  } catch { return false; }
}
const SENSITIVE_QUERY_KEY = /^(?:.*token.*|auth|authorization|session(?:id)?|sid|code|api[_-]?key|key|secret|sig|signature|jwt|sso|state|nonce)$/i;
const SENSITIVE_RESPONSE_KEY = /(?:token|auth|authorization|session|cookie|secret|password|credential|customer|account|user|profile|email|phone|address)/i;
const SPENDRUPS_PRODUCT_DETAILS_PATH = '/jss/api/productjss/LoadProductDetailsMapped';
const SPENDRUPS_CATEGORY_PRODUCTS_PATH = '/jss/api/categoryjss/getcategoryproducts';
function redactUrl(value) {
  try {
    const u = new URL(String(value || ''));
    for (const key of [...u.searchParams.keys()]) if (SENSITIVE_QUERY_KEY.test(key)) u.searchParams.set(key, '[REDACTED]');
    u.hash = '';
    return u.href;
  } catch { return String(value || '').slice(0, 2048); }
}
function sanitizeProductPayload(value, depth = 0) {
  if (depth > 8) return '[MAX_DEPTH]';
  if (value === null || typeof value === 'boolean' || typeof value === 'number') return value;
  if (typeof value === 'string') return value.slice(0, 4000);
  if (Array.isArray(value)) return value.slice(0, 200).map(v => sanitizeProductPayload(v, depth + 1));
  if (!value || typeof value !== 'object') return String(value).slice(0, 4000);
  const out = {};
  let count = 0;
  for (const [key, item] of Object.entries(value)) {
    if (++count > 300) break;
    if (SENSITIVE_RESPONSE_KEY.test(key)) {
      out[key] = '[REDACTED]';
      continue;
    }
    out[key] = sanitizeProductPayload(item, depth + 1);
  }
  return out;
}

function spendrupsCategoryRequestMeta(value) {
  try {
    const u = new URL(String(value || ''));
    if (u.hostname !== 'prod-cd-front-ehandel.spendrups.se' || u.pathname !== SPENDRUPS_CATEGORY_PRODUCTS_PATH) return null;
    const keys = [...new Set(u.searchParams.keys())].sort();
    if (keys.join(',') !== 'categoryId,itemsPerPage,pageNumber') return null;
    const itemsPerPage = Number(u.searchParams.get('itemsPerPage'));
    const pageNumber = Number(u.searchParams.get('pageNumber'));
    const categoryId = String(u.searchParams.get('categoryId') || '');
    if (!Number.isInteger(itemsPerPage) || itemsPerPage < 1 || itemsPerPage > 100) return null;
    if (!Number.isInteger(pageNumber) || pageNumber < 1 || pageNumber > 100) return null;
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(categoryId)) return null;
    return { itemsPerPage, pageNumber, categoryId: categoryId.toLowerCase() };
  } catch { return null; }
}

async function getJson(url, timeoutMs = 1200) {
  const r = await fetch(url, { signal: AbortSignal.timeout(timeoutMs), redirect: 'error', cache: 'no-store' });
  if (!r.ok) throw new Error(`HTTP ${r.status}`);
  return r.json();
}
async function cdpHealth(cfg) {
  try {
    const data = await getJson(`http://127.0.0.1:${cfg.port}/json/version`);
    const ws = new URL(String(data.webSocketDebuggerUrl || ''));
    if (ws.protocol !== 'ws:' || ws.hostname !== '127.0.0.1' || Number(ws.port) !== cfg.port || !ws.pathname.startsWith('/devtools/browser/')) {
      throw new Error('unexpected debugger URL');
    }
    return { healthy: true, browser: String(data.Browser || ''), protocolVersion: String(data['Protocol-Version'] || '') };
  } catch (error) { return { healthy: false, error: String(error?.message || error) }; }
}
async function listTargets(cfg) {
  const rows = await getJson(`http://127.0.0.1:${cfg.port}/json/list`, 1500);
  if (!Array.isArray(rows)) throw new Error('CDP target list was not an array');
  return rows.filter(t => t?.type === 'page' && allowedUrl(String(t.url || ''), cfg));
}

class CdpClient {
  constructor(wsUrl, sessionName, cfg) {
    const u = new URL(wsUrl);
    if (u.protocol !== 'ws:' || u.hostname !== '127.0.0.1' || Number(u.port) !== cfg.port || !/^\/devtools\/page\/[A-Za-z0-9_-]+$/.test(u.pathname)) {
      throw new Error('invalid page debugger URL');
    }
    this.sessionName = sessionName;
    this.cfg = cfg;
    this.wsUrl = u.href;
    this.ws = null;
    this.nextId = 1;
    this.pending = new Map();
    this.network = [];
    this.maxNetwork = 1000;
  }
  async open() {
    const ws = new WebSocket(this.wsUrl);
    this.ws = ws;
    ws.addEventListener('message', event => this.onMessage(event));
    ws.addEventListener('close', () => this.failAll(new Error('CDP closed')));
    ws.addEventListener('error', () => this.failAll(new Error('CDP error')));
    await new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('CDP websocket timeout')), 5000);
      ws.addEventListener('open', () => { clearTimeout(timer); resolve(); }, { once: true });
      ws.addEventListener('error', () => { clearTimeout(timer); reject(new Error('CDP websocket failed')); }, { once: true });
    });
    await this.send('Runtime.enable');
    await this.send('Page.enable');
    await this.send('Network.enable', { maxTotalBufferSize: 5_000_000, maxResourceBufferSize: 1_000_000 });
  }
  onMessage(event) {
    let msg;
    try { msg = JSON.parse(String(event.data)); } catch { return; }
    if (Number.isInteger(msg.id)) {
      const p = this.pending.get(msg.id);
      if (!p) return;
      this.pending.delete(msg.id);
      if (msg.error) p.reject(new Error(String(msg.error.message || 'CDP command failed'))); else p.resolve(msg.result || {});
      return;
    }
    const p = msg.params || {};
    if (msg.method === 'Network.requestWillBeSent') {
      const r = p.request || {};
      this.pushNetwork({ ts: Date.now(), kind: 'request', requestId: String(p.requestId || ''), method: String(r.method || ''), url: redactUrl(r.url), resourceType: String(p.type || '') });
    } else if (msg.method === 'Network.responseReceived') {
      const r = p.response || {};
      this.pushNetwork({ ts: Date.now(), kind: 'response', requestId: String(p.requestId || ''), url: redactUrl(r.url), status: Number(r.status || 0), mimeType: String(r.mimeType || ''), resourceType: String(p.type || '') });
    }
  }
  pushNetwork(item) {
    if (!allowedUrl(item?.url, this.cfg)) return;
    this.network.push(item);
    if (this.network.length > this.maxNetwork) this.network.splice(0, this.network.length - this.maxNetwork);
  }
  send(method, params = {}) {
    if (!this.ws || this.ws.readyState !== WebSocket.OPEN) return Promise.reject(new Error('CDP not connected'));
    const id = this.nextId++;
    this.ws.send(JSON.stringify({ id, method, params }));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => { this.pending.delete(id); reject(new Error(`CDP ${method} timeout`)); }, 10_000);
      this.pending.set(id, { resolve: v => { clearTimeout(timer); resolve(v); }, reject: e => { clearTimeout(timer); reject(e); } });
    });
  }
  failAll(error) { for (const p of this.pending.values()) p.reject(error); this.pending.clear(); }
  async evaluate(expression) {
    const result = await this.send('Runtime.evaluate', { expression, returnByValue: true, awaitPromise: true });
    if (result.exceptionDetails) throw new Error(String(result.exceptionDetails.text || 'evaluation failed'));
    return result.result?.value;
  }
  close() { try { this.ws?.close(); } catch {} this.failAll(new Error('CDP detached')); }
}

async function attach(sessionName) {
  const cfg = sessionConfig(sessionName);
  const targets = await listTargets(cfg);
  if (targets.length < 1) throw new Error('no matching page target');
  const target = targets[0];
  const prior = states.get(sessionName);
  if (prior?.targetId === target.id && prior.client?.ws?.readyState === WebSocket.OPEN) return prior;
  prior?.client?.close();
  const client = new CdpClient(String(target.webSocketDebuggerUrl || ''), sessionName, cfg);
  await client.open();
  const state = { client, targetId: String(target.id || ''), attachedAt: new Date().toISOString() };
  states.set(sessionName, state);
  return state;
}

async function ensureSession(sessionName) {
  const cfg = sessionConfig(sessionName);
  let health = await cdpHealth(cfg);
  let started = false;
  if (!health.healthy) {
    const profileDir = profilePath(sessionName);
    fs.mkdirSync(profileDir, { recursive: true });
    const args = [
      '--remote-debugging-address=127.0.0.1',
      `--remote-debugging-port=${cfg.port}`,
      `--user-data-dir=${profileDir}`,
      '--profile-directory=Default', '--new-window', '--no-first-run', '--no-default-browser-check', '--start-maximized', cfg.startUrl,
    ];
    const chromeExe = resolveChrome();
    // chromeExe is constrained to chrome.exe beneath approved install roots; args are fixed/server-derived.
    // lgtm[js/command-line-injection]
    const child = spawn(chromeExe, args, { detached: true, stdio: 'ignore', windowsHide: false, shell: false });
    child.unref();
    started = true;
    const deadline = Date.now() + 15_000;
    while (Date.now() < deadline) {
      await new Promise(r => setTimeout(r, 250));
      health = await cdpHealth(cfg);
      if (health.healthy) break;
    }
    if (!health.healthy) throw new Error(`Chrome CDP did not become ready: ${health.error || 'timeout'}`);
  }
  let state = null;
  const deadline = Date.now() + 10_000;
  while (Date.now() < deadline) {
    try { state = await attach(sessionName); break; } catch { await new Promise(r => setTimeout(r, 250)); }
  }
  if (!state) throw new Error('browser opened but matching page could not be attached');
  return { session: sessionName, started, startUrl: cfg.startUrl, cdp: health, targetId: state.targetId, attachedAt: state.attachedAt };
}

async function status(sessionName) {
  const cfg = sessionConfig(sessionName);
  const health = await cdpHealth(cfg);
  let targets = [];
  if (health.healthy) {
    try { targets = (await listTargets(cfg)).map(t => ({ id: String(t.id || ''), title: String(t.title || ''), url: redactUrl(t.url) })); } catch {}
  }
  return { session: sessionName, running: health.healthy, cdp: health, tabs: targets, attached: Boolean(states.get(sessionName)?.client?.ws?.readyState === WebSocket.OPEN) };
}

function directProductUrl(sessionName, productId) {
  const id = String(productId || '');
  if (!/^[0-9]{1,12}$/.test(id)) throw new Error('invalid product ID');
  if (sessionName === 'season-spendrups') return `https://ehandel.spendrups.se/Product/${id}`;
  if (sessionName === 'season-ms') return `https://www.martinservera.se/produkter/${id}`;
  throw new Error('unknown session');
}

function directCategoryUrl(sessionName, pageNumber) {
  if (sessionName !== 'season-spendrups') throw new Error('category navigation is available only for season-spendrups');
  const page = Number(pageNumber);
  if (!Number.isInteger(page) || page < 1 || page > 100) throw new Error('invalid category page number');
  return `https://ehandel.spendrups.se/c/drycker/sprit/all-sprit?page=${page}`;
}

async function openCategory(sessionName, pageNumber) {
  const cfg = sessionConfig(sessionName);
  const { client } = await attach(sessionName);
  const url = directCategoryUrl(sessionName, pageNumber);
  const parsed = new URL(url);
  if (!allowedUrl(url, cfg) || parsed.hash) throw new Error('category URL rejected');
  if (parsed.pathname !== '/c/drycker/sprit/all-sprit') throw new Error('Spendrups category path rejected');
  const keys = [...new Set(parsed.searchParams.keys())];
  if (keys.length !== 1 || keys[0] !== 'page' || parsed.searchParams.get('page') !== String(Number(pageNumber))) throw new Error('Spendrups category query rejected');
  await client.send('Page.navigate', { url });
  const deadline = Date.now() + 15_000;
  let value = null;
  while (Date.now() < deadline) {
    await new Promise(r => setTimeout(r, 250));
    try {
      value = await client.evaluate(`({ url: location.href, title: document.title, readyState: document.readyState })`);
      if (value?.readyState === 'complete') break;
    } catch {}
  }
  if (!value || !allowedUrl(String(value.url || ''), cfg)) throw new Error('category navigation left the allowed supplier host');
  const finalUrl = new URL(String(value.url || ''));
  if (finalUrl.pathname !== '/c/drycker/sprit/all-sprit' || finalUrl.hash) throw new Error('Spendrups navigation left the approved category path');
  const finalKeys = [...new Set(finalUrl.searchParams.keys())];
  if (finalKeys.length !== 1 || finalKeys[0] !== 'page' || finalUrl.searchParams.get('page') !== String(Number(pageNumber))) throw new Error('Spendrups navigation changed the approved category query');
  return { session: sessionName, pageNumber: Number(pageNumber), url: redactUrl(finalUrl.href), title: String(value.title || ''), readyState: String(value.readyState || '') };
}

async function openProduct(sessionName, productId) {
  const cfg = sessionConfig(sessionName);
  const { client } = await attach(sessionName);
  const url = directProductUrl(sessionName, productId);
  const parsed = new URL(url);
  if (!allowedUrl(url, cfg) || parsed.search || parsed.hash) throw new Error('product URL rejected');
  if (sessionName === 'season-spendrups' && !/^\/Product\/[0-9]{1,12}$/.test(parsed.pathname)) throw new Error('Spendrups product path rejected');
  if (sessionName === 'season-ms' && !/^\/produkter\/[0-9]{1,12}$/.test(parsed.pathname)) throw new Error('M&S product path rejected');
  await client.send('Page.navigate', { url });
  const deadline = Date.now() + 15_000;
  let value = null;
  while (Date.now() < deadline) {
    await new Promise(r => setTimeout(r, 250));
    try {
      value = await client.evaluate(`({ url: location.href, title: document.title, readyState: document.readyState })`);
      if (value?.readyState === 'complete') break;
    } catch {}
  }
  if (!value || !allowedUrl(String(value.url || ''), cfg)) throw new Error('product navigation left the allowed supplier host');
  const finalUrl = new URL(String(value.url || ''));
  if (finalUrl.search || finalUrl.hash) throw new Error('product navigation added a query string or fragment');
  if (sessionName === 'season-spendrups' && !/^\/Product\/[0-9]{1,12}$/i.test(finalUrl.pathname)) throw new Error('Spendrups navigation left the approved product path');
  if (sessionName === 'season-ms' && !/^\/produkter\/[0-9]{1,12}(?:\/[^/?#]+)?$/.test(finalUrl.pathname)) throw new Error('M&S navigation left the approved product path');
  return { session: sessionName, productId: String(productId), url: redactUrl(finalUrl.href), title: String(value.title || ''), readyState: String(value.readyState || '') };
}

async function snapshot(sessionName) {
  const { client } = await attach(sessionName);
  const value = await client.evaluate(`(() => {
    const clean = s => String(s || '').replace(/\\s+/g, ' ').trim().slice(0, 4000);
    const inputs = Array.from(document.querySelectorAll('input,select,textarea')).slice(0,100).map((e,i)=>({
      i, tag:e.tagName.toLowerCase(), type:e.getAttribute('type')||null, name:e.getAttribute('name')||null,
      id:e.id||null, placeholder:e.getAttribute('placeholder')||null, aria:e.getAttribute('aria-label')||null
    }));
    const forms = Array.from(document.forms).slice(0,30).map((f,i)=>({i, action:f.action||null, method:(f.method||'get').toLowerCase()}));
    const tables = Array.from(document.querySelectorAll('table')).slice(0,20).map((t,i)=>({i, text:clean(t.innerText).slice(0,4000)}));
    return { title:document.title, url:location.href, text:clean(document.body?.innerText).slice(0,20000), forms, inputs, tables };
  })()`);
  if (!value || !allowedUrl(String(value.url || ''), sessionConfig(sessionName))) throw new Error('active page left the allowed supplier host');
  const cfg = sessionConfig(sessionName);
  value.url = redactUrl(value.url);
  value.forms = Array.isArray(value.forms) ? value.forms.map(form => ({ ...form, action: form.action && allowedUrl(form.action, cfg) ? redactUrl(form.action) : null })) : [];
  return value;
}

async function productProbe(sessionName) {
  if (sessionName !== 'season-spendrups') throw new Error('product probe is available only for season-spendrups');
  const { client } = await attach(sessionName);
  const match = [...client.network].reverse().find(item => {
    if (item?.kind !== 'response' || Number(item.status) !== 200 || !String(item.mimeType || '').includes('application/json')) return false;
    try {
      const u = new URL(String(item.url || ''));
      return u.hostname === 'prod-cd-front-ehandel.spendrups.se' && u.pathname === SPENDRUPS_PRODUCT_DETAILS_PATH;
    } catch { return false; }
  });
  if (!match) throw new Error('no observed Spendrups product detail response; open a product page first');
  const result = await client.send('Network.getResponseBody', { requestId: String(match.requestId || '') });
  let raw = String(result?.body || '');
  if (result?.base64Encoded) raw = Buffer.from(raw, 'base64').toString('utf8');
  if (Buffer.byteLength(raw, 'utf8') > 256 * 1024) throw new Error('product response exceeded 256 KiB safety limit');
  let parsed;
  try { parsed = JSON.parse(raw); } catch { throw new Error('product response was not valid JSON'); }
  return { session: sessionName, endpoint: SPENDRUPS_PRODUCT_DETAILS_PATH, observedAt: match.ts, payload: sanitizeProductPayload(parsed) };
}

async function categoryProbe(sessionName, pageNumber) {
  if (sessionName !== 'season-spendrups') throw new Error('category probe is available only for season-spendrups');
  const requestedPage = pageNumber === null || pageNumber === undefined || pageNumber === '' ? null : Number(pageNumber);
  if (requestedPage !== null && (!Number.isInteger(requestedPage) || requestedPage < 1 || requestedPage > 100)) throw new Error('invalid category page number');
  const { client } = await attach(sessionName);
  const match = [...client.network].reverse().find(item => {
    if (item?.kind !== 'response' || Number(item.status) !== 200 || !String(item.mimeType || '').includes('application/json')) return false;
    const meta = spendrupsCategoryRequestMeta(item.url);
    return Boolean(meta && (requestedPage === null || meta.pageNumber === requestedPage));
  });
  if (!match) throw new Error(requestedPage === null ? 'no observed Spendrups category response; open a category page first' : `no observed Spendrups category response for page ${requestedPage}; open that category page first`);
  const request = spendrupsCategoryRequestMeta(match.url);
  if (!request) throw new Error('observed category response failed request validation');
  const result = await client.send('Network.getResponseBody', { requestId: String(match.requestId || '') });
  let raw = String(result?.body || '');
  if (result?.base64Encoded) raw = Buffer.from(raw, 'base64').toString('utf8');
  if (Buffer.byteLength(raw, 'utf8') > 768 * 1024) throw new Error('category response exceeded 768 KiB safety limit');
  let parsed;
  try { parsed = JSON.parse(raw); } catch { throw new Error('category response was not valid JSON'); }
  return { session: sessionName, endpoint: SPENDRUPS_CATEGORY_PRODUCTS_PATH, observedAt: match.ts, request, payload: sanitizeProductPayload(parsed) };
}

async function networkLog(sessionName, limit) {
  const { client } = await attach(sessionName);
  const n = Math.max(1, Math.min(200, Number(limit || 100)));
  return { session: sessionName, entries: client.network.slice(-n) };
}

async function parseBody(req) {
  const chunks = []; let size = 0;
  for await (const c of req) { size += c.length; if (size > 32 * 1024) throw new Error('body too large'); chunks.push(c); }
  if (!chunks.length) return {};
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}

http.createServer(async (req, res) => {
  if (!validAuth(req)) return unauthorized(res);
  try {
    const u = new URL(req.url || '/', `http://${req.headers.host || 'localhost'}`);
    if (req.method === 'GET' && u.pathname === '/health') return json(res, 200, { ok: true, sessions: Object.keys(SESSIONS) });
    if (req.method === 'POST' && u.pathname === '/session/open') { const body = await parseBody(req); return json(res, 200, await ensureSession(body.session)); }
    if (req.method === 'GET' && u.pathname === '/session/status') return json(res, 200, await status(u.searchParams.get('session')));
    if (req.method === 'POST' && u.pathname === '/session/product-open') { const body = await parseBody(req); return json(res, 200, await openProduct(body.session, body.productId)); }
    if (req.method === 'POST' && u.pathname === '/session/category-open') { const body = await parseBody(req); return json(res, 200, await openCategory(body.session, body.pageNumber)); }
    if (req.method === 'GET' && u.pathname === '/session/snapshot') return json(res, 200, await snapshot(u.searchParams.get('session')));
    if (req.method === 'GET' && u.pathname === '/session/network') return json(res, 200, await networkLog(u.searchParams.get('session'), u.searchParams.get('limit')));
    if (req.method === 'GET' && u.pathname === '/session/product-probe') return json(res, 200, await productProbe(u.searchParams.get('session')));
    if (req.method === 'GET' && u.pathname === '/session/category-probe') return json(res, 200, await categoryProbe(u.searchParams.get('session'), u.searchParams.get('pageNumber')));
    return json(res, 404, { error: 'not found' });
  } catch (error) {
    console.warn('browser bridge request failed', error instanceof Error ? error.message : 'unknown error');
    return json(res, 400, { error: 'request failed' });
  }
}).listen(PORT, '127.0.0.1', () => {
  console.log(`browser bridge listening on ${PORT}; sessions: ${Object.keys(SESSIONS).join(', ')}`);
});
