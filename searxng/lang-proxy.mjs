// Small proxy between Firecrawl and SearXNG. Node standard library only.
//
// Engine and language routing (tests 2026-09-16, 7 mixed Swedish/English queries per combination):
// - Google and Startpage are on topic with language=all, but return UK junk (trustpilot, pinterest, ebay.co.uk)
//   with en-GB and fail some English queries with sv-SE.
// - Bing returns unrelated pages for en/en-US, is mostly on topic with en-GB (but still junk for e.g.
//   "nvidia earnings date"), and with all/sv-SE matches only part of the query (python.org for "python asyncio tutorial").
// Firecrawl sends one language (default "en") for all engines, so this proxy asks PRIMARY_ENGINES with PRIMARY_LANG
// first and only adds FALLBACK_ENGINES with FALLBACK_LANG when that gives fewer than MIN_RESULTS hits
// (Google is regularly suspended for CAPTCHAs from this home IP).
//
// site: operators: Firecrawl turns includeDomains/excludeDomains into `site:x` / `-site:x` in the query, and AI
// clients write `site:x` themselves. The engines ignore the `site:` operator via SearXNG and return unrelated pages, while the
// same words with the bare domain ("acer laptop elgiganten.se") give on-site results. So `site:x` becomes the plain
// word `x`, `-site:x` is dropped, and the results are filtered by host here.
//
// Requests other than JSON searches, and searches that name their own engines, are only forwarded.
import http from 'node:http';
import { pathToFileURL } from 'node:url';

const logSafe = value => String(value ?? '').replace(/[\r\n\u2028\u2029]/g, ' ');

const UPSTREAM = new URL(process.env.UPSTREAM ?? 'http://searxng:8080');
const PORT = Number(process.env.PORT ?? 8081);
const PRIMARY_ENGINES = process.env.PRIMARY_ENGINES ?? 'google,startpage';
const PRIMARY_LANG = process.env.PRIMARY_LANG ?? 'all';
const FALLBACK_ENGINES = process.env.FALLBACK_ENGINES ?? 'bing';
const FALLBACK_LANG = process.env.FALLBACK_LANG ?? 'en-GB';
const MIN_RESULTS = Number(process.env.MIN_RESULTS ?? 5);
// Languages that mean "Firecrawl default, no real preference"; anything else is passed through as asked.
const DEFAULT_LANGS = new Set(['', 'en', 'en-us', 'auto']);

// "https://www.Spotify.com/se)" -> "spotify.com" (host only; paths are not filtered on).
const normDomain = d => d.toLowerCase().replace(/^https?:\/\//, '').replace(/^www\./, '').split(/[/)?#]/)[0];

// Returns { q, include, exclude } or null when the query has no site: operator.
export function splitSiteOperators(query) {
  const include = [];
  const exclude = [];
  const words = [];
  for (const token of query.split(/\s+/).filter(Boolean)) {
    const m = /^(-?)\(?site:(\S+?)\)?$/i.exec(token);
    if (!m) { words.push(token); continue; }
    const domain = normDomain(m[2]);
    if (domain) (m[1] ? exclude : include).push(domain);
  }
  if (!include.length && !exclude.length) return null;
  // Drop OR / | left over from "site:a OR site:b" chains, and at the ends.
  const cleaned = words.filter((w, i, all) => !(/^(OR|\|)$/.test(w) && (i === 0 || i === all.length - 1 || /^(OR|\|)$/.test(all[i + 1] ?? ''))));
  while (cleaned.length && /^(OR|\|)$/.test(cleaned.at(-1))) cleaned.pop();
  const uniqueInclude = [...new Set(include)];
  const text = cleaned.join(' ');
  const extra = uniqueInclude.filter(d => d && !text.toLowerCase().includes(d));
  return { q: [text, ...extra].filter(Boolean).join(' '), include: uniqueInclude, exclude: [...new Set(exclude)] };
}

const hostMatches = (host, domain) => host === domain || host.endsWith('.' + domain);

export function filterResults(results, { include, exclude }) {
  return results.filter(r => {
    let host;
    try { host = normDomain(new URL(r.url).hostname); } catch { return false; }
    if (exclude.some(d => hostMatches(host, d))) return false;
    return !include.length || include.some(d => hostMatches(host, d));
  });
}

export function mergeResults(first, second) {
  const seen = new Set(first.map(r => r.url));
  return [...first, ...second.filter(r => !seen.has(r.url) && seen.add(r.url))];
}

async function searchUpstream(params, engines, language) {
  const p = new URLSearchParams(params);
  p.delete('categories'); // categories would add every engine of that category on top of `engines`
  p.set('engines', engines);
  p.set('language', language);
  const res = await fetch(new URL('/search?' + p, UPSTREAM), { signal: AbortSignal.timeout(20_000) });
  if (!res.ok) throw new Error(`searxng HTTP ${res.status}`);
  return res.json();
}

async function routedSearch(url) {
  const params = new URLSearchParams(url.searchParams);
  const split = splitSiteOperators(params.get('q') ?? '');
  if (split) params.set('q', split.q);
  const asked = params.get('language') ?? '';
  const isDefault = DEFAULT_LANGS.has(asked.toLowerCase());
  const keep = results => (split ? filterResults(results, split) : results);

  const primary = await searchUpstream(params, PRIMARY_ENGINES, isDefault ? PRIMARY_LANG : asked);
  let results = keep(primary.results ?? []);
  const unresponsive = [...(primary.unresponsive_engines ?? [])];
  if (results.length < MIN_RESULTS && FALLBACK_ENGINES) {
    try {
      const fallback = await searchUpstream(params, FALLBACK_ENGINES, isDefault ? FALLBACK_LANG : asked);
      results = mergeResults(results, keep(fallback.results ?? []));
      unresponsive.push(...(fallback.unresponsive_engines ?? []));
    } catch (err) {
      console.warn(`fallback search failed: ${logSafe(err.message)}`);
    }
  }
  return { ...primary, query: params.get('q'), results, unresponsive_engines: unresponsive };
}

function passThrough(req, res) {
  const up = http.request(
    { hostname: UPSTREAM.hostname, port: UPSTREAM.port, method: req.method, path: req.url,
      headers: { ...req.headers, host: UPSTREAM.host } },
    upRes => { res.writeHead(upRes.statusCode ?? 502, upRes.headers); upRes.pipe(res); },
  );
  up.on('error', err => {
    console.warn(`searxng unavailable: ${logSafe(err.code ?? err.message)}`);
    if (!res.headersSent) { res.writeHead(502); res.end('searxng unavailable'); } else res.destroy();
  });
  res.on('close', () => { if (!res.writableFinished) up.destroy(); });
  req.pipe(up);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  http.createServer(async (req, res) => {
    if (req.url === '/healthz') { res.writeHead(200); return res.end('ok'); }
    const url = new URL(req.url, 'http://placeholder');
    const routable = req.method === 'GET' && url.pathname === '/search'
      && url.searchParams.get('format') === 'json' && !url.searchParams.get('engines');
    if (!routable) return passThrough(req, res);
    try {
      const body = JSON.stringify(await routedSearch(url));
      res.writeHead(200, { 'content-type': 'application/json', 'content-length': Buffer.byteLength(body) });
      res.end(body);
    } catch (err) {
      console.warn(`search failed: ${logSafe(err.message)}`);
      res.writeHead(502, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ error: 'searxng unavailable' }));
    }
  }).listen(PORT, '0.0.0.0', () => console.log(
    `searxng proxy on ${PORT} -> ${UPSTREAM.origin}: ${PRIMARY_ENGINES}/${PRIMARY_LANG}, fallback below ${MIN_RESULTS} hits ${FALLBACK_ENGINES}/${FALLBACK_LANG}, site: filtering on`));
}
