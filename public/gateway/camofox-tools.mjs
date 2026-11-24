// Read-only, one-shot Camoufox fallback for public pages that Chromium cannot render.
// Independent, short-lived sessions. Never imports cookies, runs arbitrary JS or accesses IR's browser sessions.
import fs from 'node:fs';
import crypto from 'node:crypto';

export const CAMOFOX_TOOL_DEFINITION = {
  name: 'firecrawl_camofox_read',
  description: 'Read a public webpage through a dedicated isolated Camoufox browser. Tries direct first, with an optional allowlisted rotating-Webshare fallback on verified block/challenge pages. Read-only, no credentials, browser interactions or persistent sessions.',
  inputSchema: {
    type: 'object', additionalProperties: false,
    properties: {
      url: { type: 'string', minLength: 8, maxLength: 8192 },
      maxChars: { type: 'integer', minimum: 1000, maximum: 30000, default: 12000 },
    },
    required: ['url'],
  },
};
export const isCamofoxTool = name => name === CAMOFOX_TOOL_DEFINITION.name;

export function appendCamofoxTool(message, allowedTools) {
  if (!Array.isArray(message?.result?.tools)) return message;
  if (allowedTools && !allowedTools.has(CAMOFOX_TOOL_DEFINITION.name)) return message;
  if (!message.result.tools.some(x => x.name === CAMOFOX_TOOL_DEFINITION.name))
    message.result.tools.push(structuredClone(CAMOFOX_TOOL_DEFINITION));
  return message;
}

const result = (payload, isError = false) => ({
  content: [{ type: 'text', text: JSON.stringify(payload) }],
  ...(isError ? { isError: true } : {}),
});
const cleanError = e => String(e?.message || e || 'Camofox error').slice(0, 300);
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
function safeUrl(raw) {
  const url = new URL(String(raw));
  if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || !url.hostname)
    throw new Error('Only credential-free HTTP(S) URLs are permitted');
  return url;
}
function hostAllowed(host, csv) {
  const h = host.toLowerCase().replace(/\.$/, '');
  return csv.split(',').map(s => s.trim().toLowerCase()).filter(Boolean)
    .some(part => h === part || h.endsWith('.' + part));
}
function isChallenge(snap, status, url) {
  if ([403, 429].includes(Number(status))) return true;
  const text = String(snap || '');
  if (/Just a moment|verify you are human|checking your browser|cf-challenge|attention required.*cloudflare|security check.*cloudflare/i.test(text)) return true;
  // 5e.tools ships a large SPA skeleton even when the requested chapter never loads.
  return (url.hostname === '5e.tools' || url.hostname.endsWith('.5e.tools')) && /Loading\.\.\./i.test(text) && !/Brimstone Hold|Fire and Darkness|Book of Vile Darkness/i.test(text);
}
function trustedSidecarBase(raw, expectedHost) {
  const url = new URL(String(raw));
  if (url.protocol !== 'http:' || url.hostname !== expectedHost || url.port !== '9377' ||
      url.username || url.password || url.pathname !== '/' || url.search || url.hash)
    throw new Error('Invalid isolated Camoufox upstream');
  return url.origin;
}
async function call(base, key, method, path, body, timeout = 20000) {
  const response = await fetch(base + path, {
    method,
    redirect: 'error',
    headers: { authorization: 'Bearer ' + key, ...(body ? { 'content-type': 'application/json' } : {}) },
    body: body ? JSON.stringify(body) : undefined,
    signal: AbortSignal.timeout(timeout),
  });
  if (!response.ok) throw new Error('Camoufox upstream HTTP ' + response.status);
  const text = await response.text();
  if (text.length > 200000) throw new Error('Camoufox response is too large');
  return text ? JSON.parse(text) : {};
}
async function openAndRead(base, key, target, maxChars, wait = sleep) {
  const userId = 'firecrawl-read-' + crypto.randomUUID();
  try {
    const tab = await call(base, key, 'POST', '/tabs', { userId, sessionKey: 'one-shot', url: target.href, trace: false }, 35000);
    if (!tab?.tabId || !/^[a-zA-Z0-9_-]{1,160}$/.test(String(tab.tabId))) throw new Error('Camoufox did not return a valid tabId');
    const tabId = encodeURIComponent(tab.tabId);
    let snapshot = '';
    for (let attempt = 0; attempt < 3; attempt++) {
      if (attempt) await wait(1500);
      const page = await call(base, key, 'GET', '/tabs/' + tabId + '/snapshot?userId=' + encodeURIComponent(userId), undefined, 12000);
      snapshot = String(page?.snapshot || page?.text || '');
      if (snapshot.length > 30 && !isChallenge(snapshot, tab.httpStatus, target)) break;
    }
    return {
      url: target.href, targetStatus: Number(tab.httpStatus) || null,
      snapshot: snapshot.slice(0, maxChars),
      truncated: snapshot.length > maxChars,
      blocked: isChallenge(snapshot, tab.httpStatus, target),
    };
  } finally {
    try { await call(base, key, 'DELETE', '/sessions/' + encodeURIComponent(userId), undefined, 5000); } catch { /* cleanup best effort */ }
  }
}

export async function callCamofoxTool(args = {}, settings = {}) {
  let target;
  try { target = safeUrl(args.url); } catch { return result({ error: 'invalid public URL' }, true); }
  const maxChars = Math.min(30000, Math.max(1000, Number.isInteger(args.maxChars) ? args.maxChars : 12000));
  let direct, proxy;
  try {
    direct = trustedSidecarBase(settings.directUrl || process.env.CAMOFOX_RESEARCH_URL || 'http://camofox-research:9377', 'camofox-research');
    proxy = trustedSidecarBase(settings.proxyUrl || process.env.CAMOFOX_RESEARCH_PROXY_URL || 'http://camofox-research-proxy:9377', 'camofox-research-proxy');
  } catch {
    return result({ error: 'invalid isolated Camoufox upstream configuration' }, true);
  }
  const hosts = settings.proxyHosts || process.env.CAMOFOX_RESEARCH_PROXY_HOSTS || '5e.tools';
  let key;
  try {
    key = settings.accessKey || fs.readFileSync(settings.keyPath || '/run/camofox-research-secrets/access_key', 'utf8').trim();
    if (key.length < 32) throw new Error('no key');
  } catch {
    return result({ error: 'isolated Camoufox research browser is not configured' }, true);
  }
  let directPage;
  try { directPage = await openAndRead(direct, key, target, maxChars, settings.wait); }
  catch (error) { return result({ error: 'direct Camoufox unavailable: ' + cleanError(error) }, true); }
  if (!directPage.blocked || !hostAllowed(target.hostname, hosts))
    return result({ ...directPage, engine: 'camofox-direct', proxyAttempted: false });
  try {
    const proxyPage = await openAndRead(proxy, key, target, maxChars, settings.wait);
    return result({ ...proxyPage, engine: 'camofox-webshare', proxyAttempted: true,
      directBlocked: true });
  } catch {
    return result({ ...directPage, engine: 'camofox-direct', proxyAttempted: true,
      proxyUnavailable: true });
  }
}
