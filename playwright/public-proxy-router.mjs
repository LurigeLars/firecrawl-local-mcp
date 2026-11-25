import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const DEFAULT_DIRECT_UPSTREAM = process.env.DIRECT_UPSTREAM || 'http://playwright-service:3000/scrape';
const DEFAULT_PROXY_UPSTREAM = process.env.PROXY_UPSTREAM || 'http://playwright-public-proxy-service:3000/scrape';
const PORT = Number(process.env.PORT || 3000);
const DEFAULT_PROXY_ATTEMPTS = Math.min(5, Math.max(1, Number(process.env.PUBLIC_PROXY_ATTEMPTS || 3)));
const DEFAULT_METRICS_PATH = process.env.PUBLIC_PROXY_METRICS_PATH || '';
const DEFAULT_PROXY_HOSTS = new Set(
  String(process.env.PUBLIC_PROXY_HOSTS || 'curemydisease.com,5e.tools')
    .split(',')
    .map(x => x.trim().toLowerCase())
    .filter(Boolean),
);

const SENSITIVE_HEADERS = new Set([
  'authorization',
  'cookie',
  'proxy-authorization',
  'x-api-key',
  'x-auth-token',
  'x-access-token',
]);

const EMPTY_METRICS = Object.freeze({
  requests_total: 0,
  direct_success: 0,
  direct_blocked: 0,
  proxy_attempts: 0,
  proxy_success: 0,
  proxy_exhausted: 0,
  sensitive_rejected: 0,
  policy_denied: 0,
  upstream_errors: 0,
  total_latency_ms: 0,
});

function newMetrics() {
  return { ...EMPTY_METRICS };
}

function sanitizeMetrics(value) {
  const out = newMetrics();
  if (!value || typeof value !== 'object') return out;
  for (const key of Object.keys(out)) {
    const n = Number(value[key]);
    if (Number.isFinite(n) && n >= 0) out[key] = Math.floor(n);
  }
  return out;
}

function loadMetrics(metricsPath) {
  if (!metricsPath) return newMetrics();
  try {
    return sanitizeMetrics(JSON.parse(fs.readFileSync(metricsPath, 'utf8')));
  } catch {
    return newMetrics();
  }
}

function persistMetrics(metricsPath, metrics) {
  if (!metricsPath) return;
  const dir = path.dirname(metricsPath);
  fs.mkdirSync(dir, { recursive: true });
  const tmp = `${metricsPath}.tmp`;
  fs.writeFileSync(tmp, JSON.stringify(sanitizeMetrics(metrics)) + '\n', {
    encoding: 'utf8',
    mode: 0o600,
  });
  fs.renameSync(tmp, metricsPath);
}

function increment(metrics, key, by = 1) {
  metrics[key] = Math.max(0, Math.floor(Number(metrics[key] || 0) + by));
}

function recordLatency(metrics, startedAtMs) {
  increment(metrics, 'total_latency_ms', Math.max(0, Date.now() - startedAtMs));
}

function hostnameMatchesProxyList(hostname, proxyHosts) {
  const host = hostname.toLowerCase().replace(/\.$/, '');
  for (const entry of proxyHosts) {
    if (host === entry || host.endsWith('.' + entry)) return true;
  }
  return false;
}

function classifyRequest(body, proxyHosts) {
  if (!body || typeof body !== 'object' || typeof body.url !== 'string') {
    return { error: 'body.url is required', reason: 'invalid' };
  }

  let url;
  try {
    url = new URL(body.url);
  } catch {
    return { error: 'body.url must be a valid URL', reason: 'invalid' };
  }

  if (!['http:', 'https:'].includes(url.protocol)) {
    return { error: 'only http/https URLs are allowed', reason: 'invalid' };
  }
  if (url.username || url.password) {
    return { error: 'URL credentials are not allowed', reason: 'sensitive' };
  }

  const eligible = hostnameMatchesProxyList(url.hostname, proxyHosts);
  const headerKeys = Object.keys(body.headers || {}).map(x => x.toLowerCase());
  const hasSensitiveHeaders = headerKeys.some(x => SENSITIVE_HEADERS.has(x));

  if (eligible && hasSensitiveHeaders) {
    return {
      error: 'proxy-eligible public requests must not carry authentication/session headers',
      reason: 'sensitive',
    };
  }

  return { eligible };
}

// Bound retained response bytes and total wall time, not just socket-idle time.
function requestJson(upstream, body, signal, maxBytes) {
  return new Promise((resolve, reject) => {
    if (signal.aborted) return reject(new Error('upstream deadline or caller disconnected'));
    const target = new URL(upstream);
    const payload = Buffer.from(JSON.stringify(body));
    const req = http.request({
      protocol: target.protocol,
      hostname: target.hostname,
      port: target.port,
      path: target.pathname + target.search,
      method: 'POST',
      agent: false,
      signal,
      headers: {
        'content-type': 'application/json',
        'content-length': String(payload.length),
      },
    }, res => {
      const chunks = [];
      let size = 0;
      res.on('error', reject);
      res.on('aborted', () => reject(new Error('upstream response aborted')));
      const declared = Number(res.headers['content-length']);
      if (Number.isFinite(declared) && declared > maxBytes) {
        res.destroy(new Error('upstream response too large'));
        return;
      }
      res.on('data', chunk => {
        size += chunk.length;
        if (size > maxBytes) {
          chunks.length = 0;
          res.destroy(new Error('upstream response too large'));
        } else {
          chunks.push(chunk);
        }
      });
      res.on('end', () => resolve({
        statusCode: res.statusCode || 502,
        headers: res.headers,
        body: Buffer.concat(chunks),
      }));
    });
    req.on('error', reject);
    req.end(payload);
  });
}

function targetStatus(response) {
  try {
    const parsed = JSON.parse(response.body.toString('utf8'));
    const value = Number(parsed?.pageStatusCode);
    return Number.isFinite(value) ? value : null;
  } catch {
    return null;
  }
}

// The pinned Playwright service reports SSRF denial as HTTP 200 with
// pageStatusCode=403 and this exact error prefix. It is not a target block.
function policyDenied(response) {
  try {
    const data = JSON.parse(response.body.toString('utf8'));
    return typeof data?.pageError === 'string' && data.pageError.startsWith('Blocked insecure target URL ');
  } catch { return false; }
}

// The target can return HTTP 200 for a bot gate or unrendered SPA shell.
// Restrict heuristic fallback to explicit public research hosts to avoid false positives.
function challengeResponse(response, hostname) {
  try {
    const data = JSON.parse(response.body.toString('utf8'));
    const status = Number(data?.pageStatusCode);
    if (status === 403 || status === 429) return true;
    if (status !== 200) return false;
    const html = String(data?.html || data?.content || data?.text || '');
    if (/Just a moment|verify you are human|cf-challenge|attention required.*cloudflare/i.test(html)) return true;
    if (hostname === '5e.tools' || hostname.endsWith('.5e.tools')) {
      return /Loading\.\.\./i.test(html) && !/Brimstone Hold|Fire and Darkness|Book of Vile Darkness/i.test(html);
    }
  } catch { /* malformed upstream responses are not silently reclassified */ }
  return false;
}

function sendBuffered(res, upstreamResponse, extraHeaders = {}) {
  const headers = { ...upstreamResponse.headers, ...extraHeaders };
  delete headers['content-length'];
  delete headers['transfer-encoding'];
  headers['content-length'] = String(upstreamResponse.body.length);
  res.writeHead(upstreamResponse.statusCode, headers);
  res.end(upstreamResponse.body);
}

function metricsResponse(metrics) {
  const requests = Math.max(1, metrics.requests_total);
  return {
    ...sanitizeMetrics(metrics),
    avg_latency_ms: Math.round(metrics.total_latency_ms / requests),
  };
}

export function createServer({
  directUpstream = DEFAULT_DIRECT_UPSTREAM,
  proxyUpstream = DEFAULT_PROXY_UPSTREAM,
  proxyHosts = DEFAULT_PROXY_HOSTS,
  proxyAttempts = DEFAULT_PROXY_ATTEMPTS,
  metricsPath = DEFAULT_METRICS_PATH,
  // Internal operator defaults. The request cannot select these limits.
  maxResponseBytes = 32 * 1024 * 1024,
  totalTimeoutMs = 180_000,
} = {}) {
  if (!Number.isSafeInteger(maxResponseBytes) || maxResponseBytes < 1 || maxResponseBytes > 32 * 1024 * 1024 ||
      !Number.isSafeInteger(totalTimeoutMs) || totalTimeoutMs < 1 || totalTimeoutMs > 180_000) {
    throw new Error('invalid router bounds');
  }
  const metrics = loadMetrics(metricsPath);

  const commitMetrics = (startedAtMs) => {
    recordLatency(metrics, startedAtMs);
    persistMetrics(metricsPath, metrics);
  };

  return http.createServer(async (req, res) => {
    if (req.method === 'GET' && req.url === '/health') {
      res.writeHead(200, { 'content-type': 'text/plain' });
      return res.end('ok');
    }
    if (req.method === 'GET' && req.url === '/metrics') {
      const payload = Buffer.from(JSON.stringify(metricsResponse(metrics)));
      res.writeHead(200, {
        'content-type': 'application/json',
        'content-length': String(payload.length),
        'cache-control': 'no-store',
      });
      return res.end(payload);
    }
    if (req.method !== 'POST' || req.url !== '/scrape') {
      res.writeHead(404);
      return res.end();
    }

    const startedAtMs = Date.now();
    increment(metrics, 'requests_total');

    const chunks = [];
    let size = 0;
    req.on('data', chunk => {
      size += chunk.length;
      if (size > 2 * 1024 * 1024) req.destroy();
      else chunks.push(chunk);
    });
    req.on('end', async () => {
      let body;
      try {
        body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      } catch {
        commitMetrics(startedAtMs);
        res.writeHead(400, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ error: 'invalid JSON' }));
      }

      const classification = classifyRequest(body, proxyHosts);
      if (classification.error) {
        if (classification.reason === 'sensitive') increment(metrics, 'sensitive_rejected');
        commitMetrics(startedAtMs);
        res.writeHead(400, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ error: classification.error }));
      }

      // One wall-clock deadline across direct and all existing retry attempts.
      // Closing the caller socket also cancels the router's upstream transport.
      const controller = new AbortController();
      const deadline = setTimeout(() => controller.abort(), totalTimeoutMs);
      const disconnect = () => { if (!res.writableFinished) controller.abort(); };
      res.once('close', disconnect);
      try {
        const direct = await requestJson(directUpstream, body, controller.signal, maxResponseBytes);
        const status = targetStatus(direct);
        if (policyDenied(direct)) {
          increment(metrics, 'policy_denied');
          commitMetrics(startedAtMs);
          return sendBuffered(res, direct, { 'x-firecrawl-public-proxy': 'direct' });
        }
        const directBlocked = status === 403 || status === 429 || (classification.eligible && challengeResponse(direct, new URL(body.url).hostname));

        if (directBlocked) increment(metrics, 'direct_blocked');
        else if (status !== null && status >= 200 && status < 400) increment(metrics, 'direct_success');

        if (classification.eligible && directBlocked) {
          let proxied = null;
          let proxiedStatus = null;
          let attempts = 0;

          for (let i = 0; i < proxyAttempts; i += 1) {
            proxied = await requestJson(proxyUpstream, body, controller.signal, maxResponseBytes);
            attempts += 1;
            increment(metrics, 'proxy_attempts');
            proxiedStatus = targetStatus(proxied);
            if (policyDenied(proxied)) {
              increment(metrics, 'policy_denied');
              commitMetrics(startedAtMs);
              return sendBuffered(res, proxied, {
                'x-firecrawl-public-proxy': 'used',
                'x-firecrawl-public-proxy-attempts': String(attempts),
              });
            }
            if (!challengeResponse(proxied, new URL(body.url).hostname) && proxiedStatus !== 403 && proxiedStatus !== 429) break;
          }

          if (proxiedStatus === 403 || proxiedStatus === 429 || challengeResponse(proxied, new URL(body.url).hostname)) increment(metrics, 'proxy_exhausted');
          else if (proxied.statusCode >= 200 && proxied.statusCode < 300 &&
              proxiedStatus !== null && proxiedStatus >= 200 && proxiedStatus < 300) increment(metrics, 'proxy_success');
          else increment(metrics, 'upstream_errors');

          commitMetrics(startedAtMs);
          console.log(
            `public proxy fallback direct_status=${status} attempts=${attempts} final_status=${proxiedStatus ?? 'unknown'}`
          );
          return sendBuffered(res, proxied, {
            'x-firecrawl-public-proxy': 'used',
            'x-firecrawl-public-proxy-attempts': String(attempts),
          });
        }

        commitMetrics(startedAtMs);
        return sendBuffered(res, direct, { 'x-firecrawl-public-proxy': 'direct' });
      } catch {
        increment(metrics, 'upstream_errors');
        commitMetrics(startedAtMs);
        console.warn('playwright router upstream error');
        if (res.destroyed || res.writableEnded) return;
        res.writeHead(controller.signal.aborted ? 504 : 502, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ error: 'playwright upstream unavailable' }));
      } finally {
        clearTimeout(deadline);
        res.off('close',disconnect);
      }
    });
  });
}

const invokedDirectly = process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href;
if (invokedDirectly) {
  createServer().listen(PORT, '0.0.0.0', () => {
    console.log(
      `playwright public-proxy router listening on ${PORT}; proxy_host_count=${DEFAULT_PROXY_HOSTS.size}; attempts=${DEFAULT_PROXY_ATTEMPTS}; metrics=${DEFAULT_METRICS_PATH ? 'persistent' : 'memory'}`
    );
  });
}
