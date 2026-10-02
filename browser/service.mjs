import http from 'node:http';
import crypto from 'node:crypto';
import { chromium } from 'playwright';
import { publicUrlReason, redactUrl, sanitizeText } from './safety.mjs';

const PORT = Number(process.env.PORT || 3010);
const HOST = process.env.HOST || '0.0.0.0';
const MAX_SESSIONS = boundedInt(process.env.BROWSER_MAX_SESSIONS, 2, 1, 8);
const MAX_TABS_PER_SESSION = boundedInt(process.env.BROWSER_MAX_TABS_PER_SESSION, 3, 1, 8);
const SESSION_TTL_MS = boundedInt(process.env.BROWSER_SESSION_TTL_MS, 10 * 60_000, 30_000, 60 * 60_000);
const SESSION_IDLE_MS = boundedInt(process.env.BROWSER_SESSION_IDLE_MS, 5 * 60_000, 10_000, 60 * 60_000);
const BROWSER_IDLE_MS = boundedInt(process.env.BROWSER_PROCESS_IDLE_MS, 60_000, 10_000, 10 * 60_000);
const NAVIGATION_TIMEOUT_MS = boundedInt(process.env.BROWSER_NAVIGATION_TIMEOUT_MS, 30_000, 1_000, 120_000);
const MAX_BODY_BYTES = 64 * 1024;
const MAX_LOG_ENTRIES = 1000;
const SAFE_WAIT_UNTIL = new Set(['load', 'domcontentloaded', 'networkidle']);

const sessions = new Map();
const hostVerdicts = new Map();
let browser = null;
let browserLaunch = null;
let browserIdleSince = Date.now();

function boundedInt(value, fallback, min, max) {
  const parsed = Number.parseInt(String(value ?? ''), 10);
  if (!Number.isFinite(parsed)) return fallback;
  return Math.max(min, Math.min(max, parsed));
}

function nowIso() { return new Date().toISOString(); }
function newId(prefix) { return `${prefix}_${crypto.randomBytes(12).toString('hex')}`; }

function writeJson(res, status, payload) {
  const body = Buffer.from(JSON.stringify(payload));
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': String(body.length),
    'cache-control': 'no-store',
    'x-content-type-options': 'nosniff',
  });
  res.end(body);
}

async function readJson(req) {
  const chunks = [];
  let size = 0;
  for await (const chunk of req) {
    size += chunk.length;
    if (size > MAX_BODY_BYTES) throw Object.assign(new Error('request body too large'), { statusCode: 413 });
    chunks.push(chunk);
  }
  if (!size) return {};
  try { return JSON.parse(Buffer.concat(chunks).toString('utf8')); }
  catch { throw Object.assign(new Error('invalid JSON body'), { statusCode: 400 }); }
}

function logEntry(session, entry) {
  session.network.push({ seq: ++session.networkSeq, time: nowIso(), ...entry });
  if (session.network.length > MAX_LOG_ENTRIES) session.network.splice(0, session.network.length - MAX_LOG_ENTRIES);
}

function consoleEntry(session, entry) {
  session.console.push({ seq: ++session.consoleSeq, time: nowIso(), ...entry });
  if (session.console.length > MAX_LOG_ENTRIES) session.console.splice(0, session.console.length - MAX_LOG_ENTRIES);
}

async function ensureBrowser() {
  if (browser?.isConnected()) return browser;
  if (browserLaunch) return browserLaunch;
  browserLaunch = chromium.launch({
    headless: true,
    args: ['--no-sandbox', '--disable-setuid-sandbox', '--disable-dev-shm-usage'],
  }).then(value => {
    browser = value;
    browserIdleSince = Date.now();
    browser.on('disconnected', () => {
      browser = null;
      browserLaunch = null;
      sessions.clear();
    });
    return value;
  }).finally(() => { browserLaunch = null; });
  return browserLaunch;
}

function touch(session) { session.lastActivity = Date.now(); }

function publicSessionView(session) {
  return {
    sessionId: session.id,
    createdAt: new Date(session.createdAt).toISOString(),
    lastActivityAt: new Date(session.lastActivity).toISOString(),
    expiresAt: new Date(Math.min(session.createdAt + SESSION_TTL_MS, session.lastActivity + SESSION_IDLE_MS)).toISOString(),
    tabs: [...session.tabs.entries()].map(([tabId, page]) => ({
      tabId,
      url: redactUrl(page.url()),
      closed: page.isClosed(),
    })),
  };
}

function requireSession(id) {
  const session = sessions.get(String(id || ''));
  if (!session) throw Object.assign(new Error('browser session not found'), { statusCode: 404 });
  if (Date.now() - session.createdAt > SESSION_TTL_MS || Date.now() - session.lastActivity > SESSION_IDLE_MS) {
    void closeSession(session.id);
    throw Object.assign(new Error('browser session expired'), { statusCode: 410 });
  }
  touch(session);
  return session;
}

function requireTab(session, requestedTabId) {
  const tabId = requestedTabId ? String(requestedTabId) : session.defaultTabId;
  const page = session.tabs.get(tabId);
  if (!page || page.isClosed()) throw Object.assign(new Error('browser tab not found'), { statusCode: 404 });
  return { tabId, page };
}

async function safeTargetReason(raw) {
  let parsed;
  try { parsed = new URL(raw); } catch { return 'A valid public HTTP(S) URL is required.'; }
  const host = parsed.hostname.toLowerCase();
  const cached = hostVerdicts.get(host);
  if (cached && cached.expiresAt > Date.now()) return cached.reason;
  const reason = await publicUrlReason(raw);
  hostVerdicts.set(host, { reason, expiresAt: Date.now() + (reason ? 60_000 : 5_000) });
  return reason;
}

function pageTabId(session, page) {
  for (const [tabId, candidate] of session.tabs) if (candidate === page) return tabId;
  return null;
}

async function registerPage(session, page) {
  const existing = pageTabId(session, page);
  if (existing) return existing;
  if (session.tabs.size >= MAX_TABS_PER_SESSION) {
    await page.close().catch(() => {});
    return null;
  }

  const tabId = newId('tab');
  session.tabs.set(tabId, page);
  if (!session.defaultTabId) session.defaultTabId = tabId;

  page.setDefaultNavigationTimeout(NAVIGATION_TIMEOUT_MS);
  page.setDefaultTimeout(Math.min(NAVIGATION_TIMEOUT_MS, 15_000));
  page.on('dialog', dialog => void dialog.dismiss().catch(() => {}));
  page.on('download', download => void download.cancel().catch(() => {}));
  page.on('console', msg => consoleEntry(session, {
    tabId,
    kind: 'console',
    level: sanitizeText(msg.type(), 32),
    text: sanitizeText(msg.text(), 4000),
  }));
  page.on('pageerror', error => consoleEntry(session, {
    tabId,
    kind: 'pageerror',
    level: 'error',
    text: sanitizeText(error?.message || error, 4000),
  }));
  page.on('request', request => logEntry(session, {
    tabId,
    phase: 'request',
    method: request.method(),
    resourceType: request.resourceType(),
    url: redactUrl(request.url()),
  }));
  page.on('response', response => logEntry(session, {
    tabId,
    phase: 'response',
    method: response.request().method(),
    resourceType: response.request().resourceType(),
    status: response.status(),
    url: redactUrl(response.url()),
  }));
  page.on('requestfailed', request => logEntry(session, {
    tabId,
    phase: 'failed',
    method: request.method(),
    resourceType: request.resourceType(),
    url: redactUrl(request.url()),
    failure: sanitizeText(request.failure()?.errorText || 'request failed', 500),
  }));
  page.on('close', () => {
    session.tabs.delete(tabId);
    if (session.defaultTabId === tabId) session.defaultTabId = session.tabs.keys().next().value || null;
  });
  return tabId;
}

async function createSession(initialUrl) {
  if (sessions.size >= MAX_SESSIONS) throw Object.assign(new Error(`maximum browser sessions reached (${MAX_SESSIONS})`), { statusCode: 429 });
  if (initialUrl) {
    const reason = await safeTargetReason(initialUrl);
    if (reason) throw Object.assign(new Error(reason), { statusCode: 400 });
  }

  const engine = await ensureBrowser();
  const context = await engine.newContext({
    acceptDownloads: false,
    viewport: { width: 1280, height: 900 },
    permissions: [],
  });
  await context.clearPermissions();

  const session = {
    id: newId('browser'),
    context,
    tabs: new Map(),
    defaultTabId: null,
    createdAt: Date.now(),
    lastActivity: Date.now(),
    network: [],
    networkSeq: 0,
    console: [],
    consoleSeq: 0,
  };
  sessions.set(session.id, session);
  context.on('page', page => void registerPage(session, page));

  await context.route('**/*', async route => {
    const request = route.request();
    const raw = request.url();
    let protocol;
    try { protocol = new URL(raw).protocol; } catch { return route.abort('blockedbyclient').catch(() => {}); }
    if (['data:', 'blob:'].includes(protocol)) return route.continue().catch(() => {});
    if (!['http:', 'https:'].includes(protocol)) return route.abort('blockedbyclient').catch(() => {});
    const reason = await safeTargetReason(raw);
    if (reason) {
      let owningPage = null;
      try { owningPage = request.frame().page(); } catch {}
      const tabId = pageTabId(session, owningPage);
      logEntry(session, {
        tabId,
        phase: 'blocked',
        method: request.method(),
        resourceType: request.resourceType(),
        url: redactUrl(raw),
        failure: reason,
      });
      return route.abort('blockedbyclient').catch(() => {});
    }
    return route.continue().catch(() => {});
  });

  await context.routeWebSocket('**/*', async ws => {
    let target;
    try {
      target = new URL(ws.url());
      if (target.protocol === 'ws:') target.protocol = 'http:';
      else if (target.protocol === 'wss:') target.protocol = 'https:';
      else {
        ws.close({ code: 1008, reason: 'blocked protocol' });
        return;
      }
    } catch {
      ws.close({ code: 1008, reason: 'invalid target' });
      return;
    }
    const reason = await safeTargetReason(target.toString());
    if (reason) {
      logEntry(session, { tabId: null, phase: 'websocket-blocked', method: 'GET', resourceType: 'websocket', url: redactUrl(target.toString()), failure: reason });
      ws.close({ code: 1008, reason: 'blocked target' });
      return;
    }
    logEntry(session, { tabId: null, phase: 'websocket', method: 'GET', resourceType: 'websocket', url: redactUrl(target.toString()) });
    ws.connectToServer();
  });

  try {
    const page = await context.newPage();
    const tabId = await registerPage(session, page);
    if (initialUrl) await page.goto(initialUrl, { waitUntil: 'domcontentloaded', timeout: NAVIGATION_TIMEOUT_MS });
    browserIdleSince = Date.now();
    return { ...publicSessionView(session), defaultTabId: tabId };
  } catch (error) {
    await closeSession(session.id);
    throw error;
  }
}

async function closeSession(id) {
  const session = sessions.get(String(id || ''));
  if (!session) return false;
  sessions.delete(session.id);
  await session.context.close().catch(() => {});
  browserIdleSince = Date.now();
  return true;
}

async function navigateSession(sessionId, body) {
  const session = requireSession(sessionId);
  const { tabId, page } = requireTab(session, body.tabId);
  const reason = await safeTargetReason(body.url);
  if (reason) throw Object.assign(new Error(reason), { statusCode: 400 });
  const waitUntil = SAFE_WAIT_UNTIL.has(body.waitUntil) ? body.waitUntil : 'domcontentloaded';
  const response = await page.goto(body.url, { waitUntil, timeout: NAVIGATION_TIMEOUT_MS });
  touch(session);
  return {
    sessionId: session.id,
    tabId,
    url: redactUrl(page.url()),
    title: sanitizeText(await page.title().catch(() => ''), 500),
    status: response?.status() ?? null,
  };
}

async function snapshotSession(sessionId, query) {
  const session = requireSession(sessionId);
  const { tabId, page } = requireTab(session, query.get('tabId'));
  const maxChars = boundedInt(query.get('maxChars'), 20_000, 1_000, 30_000);
  let snapshot = '';
  let mode = 'aria';
  try {
    snapshot = await page.locator('body').ariaSnapshot({ timeout: 5_000 });
  } catch {
    mode = 'text';
    snapshot = await page.locator('body').innerText({ timeout: 5_000 }).catch(() => '');
  }
  const truncated = snapshot.length > maxChars;
  if (truncated) snapshot = `${snapshot.slice(0, Math.max(0, maxChars - 3))}...`;
  touch(session);
  return {
    sessionId: session.id,
    tabId,
    url: redactUrl(page.url()),
    title: sanitizeText(await page.title().catch(() => ''), 500),
    mode,
    truncated,
    snapshot,
  };
}

function boundedLog(session, collection, query) {
  const requestedTab = query.get('tabId');
  const limit = boundedInt(query.get('limit'), 100, 1, 200);
  const filtered = requestedTab ? collection.filter(item => item.tabId === requestedTab) : collection;
  touch(session);
  return filtered.slice(-limit);
}

async function screenshotSession(sessionId, body) {
  const session = requireSession(sessionId);
  const { tabId, page } = requireTab(session, body.tabId);
  const data = await page.screenshot({ type: 'png', fullPage: body.fullPage === true });
  touch(session);
  return {
    sessionId: session.id,
    tabId,
    url: redactUrl(page.url()),
    mimeType: 'image/png',
    data: data.toString('base64'),
  };
}

async function routeRequest(req, res) {
  const url = new URL(req.url || '/', 'http://browser.local');
  const path = url.pathname;

  if (req.method === 'GET' && path === '/healthz') {
    return writeJson(res, 200, { ok: true, sessions: sessions.size, browserConnected: Boolean(browser?.isConnected()) });
  }
  if (req.method === 'GET' && path === '/sessions') {
    return writeJson(res, 200, { sessions: [...sessions.values()].map(publicSessionView) });
  }
  if (req.method === 'POST' && path === '/sessions') {
    const body = await readJson(req);
    return writeJson(res, 201, await createSession(body.url ? String(body.url) : null));
  }

  const match = path.match(/^\/sessions\/([^/]+)(?:\/(.*))?$/);
  if (!match) return writeJson(res, 404, { error: 'not found' });
  const sessionId = decodeURIComponent(match[1]);
  const action = match[2] || '';

  if (req.method === 'DELETE' && action === '') {
    const closed = await closeSession(sessionId);
    return writeJson(res, closed ? 200 : 404, closed ? { closed: true, sessionId } : { error: 'browser session not found' });
  }
  if (req.method === 'GET' && action === 'tabs') {
    const session = requireSession(sessionId);
    return writeJson(res, 200, publicSessionView(session));
  }
  if (req.method === 'POST' && action === 'navigate') {
    const body = await readJson(req);
    if (typeof body.url !== 'string') throw Object.assign(new Error('url is required'), { statusCode: 400 });
    return writeJson(res, 200, await navigateSession(sessionId, body));
  }
  if (req.method === 'GET' && action === 'snapshot') {
    return writeJson(res, 200, await snapshotSession(sessionId, url.searchParams));
  }
  if (req.method === 'GET' && action === 'network') {
    const session = requireSession(sessionId);
    return writeJson(res, 200, { sessionId, entries: boundedLog(session, session.network, url.searchParams) });
  }
  if (req.method === 'GET' && action === 'console') {
    const session = requireSession(sessionId);
    return writeJson(res, 200, { sessionId, entries: boundedLog(session, session.console, url.searchParams) });
  }
  if (req.method === 'POST' && action === 'screenshot') {
    const body = await readJson(req);
    return writeJson(res, 200, await screenshotSession(sessionId, body));
  }
  return writeJson(res, 404, { error: 'not found' });
}

const server = http.createServer((req, res) => {
  routeRequest(req, res).catch(error => {
    const status = Number(error?.statusCode) || 500;
    if (status >= 500) console.error('browser request failed', sanitizeText(error?.message || error, 500));
    if (!res.headersSent) writeJson(res, status, { error: sanitizeText(error?.message || 'browser operation failed', 500) });
    else res.destroy();
  });
});

const cleanupTimer = setInterval(async () => {
  const now = Date.now();
  for (const session of [...sessions.values()]) {
    if (now - session.createdAt > SESSION_TTL_MS || now - session.lastActivity > SESSION_IDLE_MS) await closeSession(session.id);
  }
  if (!sessions.size && browser?.isConnected() && now - browserIdleSince > BROWSER_IDLE_MS) {
    const current = browser;
    browser = null;
    await current.close().catch(() => {});
  }
  for (const [host, value] of hostVerdicts) if (value.expiresAt <= now) hostVerdicts.delete(host);
}, 15_000);
cleanupTimer.unref();

async function shutdown() {
  clearInterval(cleanupTimer);
  for (const id of [...sessions.keys()]) await closeSession(id);
  if (browser?.isConnected()) await browser.close().catch(() => {});
  server.close(() => process.exit(0));
  setTimeout(() => process.exit(0), 3_000).unref();
}
for (const signal of ['SIGINT', 'SIGTERM']) process.on(signal, () => void shutdown());

server.listen(PORT, HOST, () => {
  console.log(`ephemeral browser session service listening on ${HOST}:${PORT}; maxSessions=${MAX_SESSIONS}; maxTabs=${MAX_TABS_PER_SESSION}`);
});
