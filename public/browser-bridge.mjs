import http from 'node:http';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import os from 'node:os';
import { URL } from 'node:url';

const PORT = Number(process.env.BROWSER_BRIDGE_PORT || 8765);
const TOKEN = String(process.env.BROWSER_BRIDGE_TOKEN || '');
if (TOKEN.length < 32) {
  console.error('BROWSER_BRIDGE_TOKEN missing or shorter than 32 chars; refusing to start');
  process.exit(1);
}

const SESSION_ROOT = path.join(process.env.LOCALAPPDATA || path.join(os.homedir(), 'AppData', 'Local'), 'FirecrawlLocal', 'browser-profiles');
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
function validAuth(req) {
  const expected = `Bearer ${TOKEN}`;
  const got = String(req.headers.authorization || '');
  if (got.length !== expected.length) return false;
  return timingSafeEqual(Buffer.from(got), Buffer.from(expected));
}
function timingSafeEqual(a, b) {
  if (a.length !== b.length) return false;
  let diff = 0;
  for (let i = 0; i < a.length; i++) diff |= a[i] ^ b[i];
  return diff === 0;
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
function resolveChrome() {
  const configured = String(process.env.BROWSER_CHROME_EXE || '').trim();
  const candidates = configured ? [configured] : chromeCandidates();
  for (const candidate of candidates) if (fs.existsSync(candidate)) return path.resolve(candidate);
  throw new Error('Google Chrome executable not found');
}
function allowedUrl(value, cfg) {
  try {
    const u = new URL(value);
    if (u.protocol !== 'https:') return false;
    const h = u.hostname.toLowerCase();
    return cfg.hosts.some(base => h === base || h.endsWith(`.${base}`));
  } catch { return false; }
}
const SENSITIVE_QUERY_KEY = /(token|access[_-]?token|refresh[_-]?token|auth|authorization|session|sid|code|key|secret|sig|signature|jwt|sso)/i;
function redactUrl(value) {
  try {
    const u = new URL(String(value || ''));
    for (const key of [...u.searchParams.keys()]) if (SENSITIVE_QUERY_KEY.test(key)) u.searchParams.set(key, '[REDACTED]');
    u.hash = '';
    return u.href;
  } catch { return String(value || '').slice(0, 2048); }
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
    // Intentionally omit headers, cookies and request bodies, and keep only the approved supplier host family.
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
  // Prefer the most recently listed matching page and never attach to unrelated tabs.
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
    const profileDir = path.join(SESSION_ROOT, sessionName);
    fs.mkdirSync(profileDir, { recursive: true });
    const args = [
      `--remote-debugging-address=127.0.0.1`,
      `--remote-debugging-port=${cfg.port}`,
      `--user-data-dir=${profileDir}`,
      '--profile-directory=Default', '--new-window', '--no-first-run', '--no-default-browser-check', '--start-maximized', cfg.startUrl,
    ];
    const child = spawn(resolveChrome(), args, { detached: true, stdio: 'ignore', windowsHide: false });
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
  value.url = redactUrl(value.url);
  value.forms = Array.isArray(value.forms) ? value.forms.map(form => ({ ...form, action: form.action ? redactUrl(form.action) : null })) : [];
  return value;
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
    if (req.method === 'POST' && u.pathname === '/session/open') {
      const body = await parseBody(req); return json(res, 200, await ensureSession(body.session));
    }
    if (req.method === 'GET' && u.pathname === '/session/status') return json(res, 200, await status(u.searchParams.get('session')));
    if (req.method === 'GET' && u.pathname === '/session/snapshot') return json(res, 200, await snapshot(u.searchParams.get('session')));
    if (req.method === 'GET' && u.pathname === '/session/network') return json(res, 200, await networkLog(u.searchParams.get('session'), u.searchParams.get('limit')));
    return json(res, 404, { error: 'not found' });
  } catch (error) { return json(res, 400, { error: String(error?.message || error) }); }
}).listen(PORT, '127.0.0.1', () => {
  console.log(`browser bridge listening on ${PORT}; sessions: ${Object.keys(SESSIONS).join(', ')}`);
});