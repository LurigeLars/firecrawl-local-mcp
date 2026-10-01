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
  String(process.env.PUBLIC_PROXY_HOSTS || 'curemydisease.com')
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

function requestJson(upstream, body) {
  return new Promise((resolve, reject) => {
    const target = new URL(upstream);
    const payload = Buffer.from(JSON.stringify(body));
    const req = http.request({
      protocol: target.protocol,
      hostname: target.hostname,
      port: target.port,
      path: target.pathname + target.search,
      method: 'POST',
      agent: false,
      headers: {
        'content-type': 'application/json',
        'content-length': String(payload.length),
      },
    }, res => {
      const chunks = [];
      res.on('data', chunk => chunks.push(chunk));
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
} = {}) {
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

      try {
        const direct = await requestJson(directUpstream, body);
        const status = targetStatus(direct);
        const directBlocked = status === 403 || status === 429;

        if (directBlocked) increment(metrics, 'direct_blocked');
        else if (status !== null && status >= 200 && status < 400) increment(metrics, 'direct_success');

        if (classification.eligible && directBlocked) {
          let proxied = null;
          let proxiedStatus = null;
          let attempts = 0;

          for (let i = 0; i < proxyAttempts; i += 1) {
            proxied = await requestJson(proxyUpstream, body);
            attempts += 1;
            increment(metrics, 'proxy_attempts');
            proxiedStatus = targetStatus(proxied);
            if (proxiedStatus !== 403 && proxiedStatus !== 429) break;
          }

          if (proxiedStatus === 403 || proxiedStatus === 429) increment(metrics, 'proxy_exhausted');
          else increment(metrics, 'proxy_success');

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
        res.writeHead(502, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ error: 'playwright upstream unavailable' }));
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
