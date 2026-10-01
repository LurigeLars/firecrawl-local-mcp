import http from 'node:http';
import { pathToFileURL } from 'node:url';

const DEFAULT_DIRECT_UPSTREAM = process.env.DIRECT_UPSTREAM || 'http://playwright-service:3000/scrape';
const DEFAULT_PROXY_UPSTREAM = process.env.PROXY_UPSTREAM || 'http://playwright-public-proxy-service:3000/scrape';
const PORT = Number(process.env.PORT || 3000);
const DEFAULT_PROXY_ATTEMPTS = Math.min(5, Math.max(1, Number(process.env.PUBLIC_PROXY_ATTEMPTS || 3)));
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

function hostnameMatchesProxyList(hostname, proxyHosts) {
  const host = hostname.toLowerCase().replace(/\.$/, '');
  for (const entry of proxyHosts) {
    if (host === entry || host.endsWith('.' + entry)) return true;
  }
  return false;
}

function classifyRequest(body, proxyHosts) {
  if (!body || typeof body !== 'object' || typeof body.url !== 'string') {
    return { error: 'body.url is required' };
  }

  let url;
  try {
    url = new URL(body.url);
  } catch {
    return { error: 'body.url must be a valid URL' };
  }

  if (!['http:', 'https:'].includes(url.protocol)) {
    return { error: 'only http/https URLs are allowed' };
  }
  if (url.username || url.password) {
    return { error: 'URL credentials are not allowed' };
  }

  const eligible = hostnameMatchesProxyList(url.hostname, proxyHosts);
  const headerKeys = Object.keys(body.headers || {}).map(x => x.toLowerCase());
  const hasSensitiveHeaders = headerKeys.some(x => SENSITIVE_HEADERS.has(x));

  if (eligible && hasSensitiveHeaders) {
    return { error: 'proxy-eligible public requests must not carry authentication/session headers' };
  }

  return { eligible, hostname: url.hostname };
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

export function createServer({
  directUpstream = DEFAULT_DIRECT_UPSTREAM,
  proxyUpstream = DEFAULT_PROXY_UPSTREAM,
  proxyHosts = DEFAULT_PROXY_HOSTS,
  proxyAttempts = DEFAULT_PROXY_ATTEMPTS,
} = {}) {
  return http.createServer(async (req, res) => {
    if (req.method === 'GET' && req.url === '/health') {
      res.writeHead(200, { 'content-type': 'text/plain' });
      return res.end('ok');
    }
    if (req.method !== 'POST' || req.url !== '/scrape') {
      res.writeHead(404);
      return res.end();
    }

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
        res.writeHead(400, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ error: 'invalid JSON' }));
      }

      const classification = classifyRequest(body, proxyHosts);
      if (classification.error) {
        res.writeHead(400, { 'content-type': 'application/json' });
        return res.end(JSON.stringify({ error: classification.error }));
      }

      try {
        const direct = await requestJson(directUpstream, body);
        const status = targetStatus(direct);

        if (classification.eligible && (status === 403 || status === 429)) {
          let proxied = null;
          let proxiedStatus = null;
          let attempts = 0;

          for (let i = 0; i < proxyAttempts; i += 1) {
            proxied = await requestJson(proxyUpstream, body);
            attempts += 1;
            proxiedStatus = targetStatus(proxied);
            if (proxiedStatus !== 403 && proxiedStatus !== 429) break;
          }

          console.log(
            `public proxy fallback host=${classification.hostname} direct_status=${status} attempts=${attempts} final_status=${proxiedStatus ?? 'unknown'}`
          );
          return sendBuffered(res, proxied, {
            'x-firecrawl-public-proxy': 'used',
            'x-firecrawl-public-proxy-attempts': String(attempts),
          });
        }

        return sendBuffered(res, direct, { 'x-firecrawl-public-proxy': 'direct' });
      } catch (error) {
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
    console.log(`playwright public-proxy router listening on ${PORT}; proxy hosts=${[...DEFAULT_PROXY_HOSTS].join(',')}; attempts=${DEFAULT_PROXY_ATTEMPTS}`);
  });
}
