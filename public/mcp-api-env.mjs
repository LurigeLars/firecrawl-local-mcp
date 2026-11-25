// Optional operator-provided candidate credential. Never accept a secret file
// for an arbitrary API destination. The locked MCP SDK consumes an env key.
import { closeSync, fstatSync, openSync, readSync } from 'node:fs';

const MAX_FILE_BYTES = 1024;

export function candidateApiEnvironment(env, defaultUrl, allowedUrl) {
  const next = { ...env, FIRECRAWL_API_URL: env.FIRECRAWL_API_URL ?? defaultUrl };
  const path = env.FIRECRAWL_API_KEY_FILE;
  delete next.FIRECRAWL_API_KEY_FILE;
  if (path === undefined) return next;
  if (!path || next.FIRECRAWL_API_URL !== allowedUrl || env.FIRECRAWL_API_KEY) {
    throw new Error('Invalid candidate credential configuration');
  }

  let fd;
  let key;
  const bytes = Buffer.alloc(MAX_FILE_BYTES + 1);
  try {
    fd = openSync(path, 'r');
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size > MAX_FILE_BYTES) throw new Error();
    let total = 0;
    while (total < bytes.length) {
      const n = readSync(fd, bytes, total, bytes.length - total, null);
      if (!n) break;
      total += n;
    }
    if (total > MAX_FILE_BYTES) throw new Error();
    key = bytes.subarray(0, total).toString('utf8').trim();
    // Restrict to printable ASCII for the Authorization header; no controls,
    // whitespace, invalid UTF-8 replacement, or empty/oversized credentials.
    if (!/^[!-~]{32,256}$/.test(key)) throw new Error();
  } catch {
    // Do not surface filesystem details, credential contents or underlying errors.
    throw new Error('Candidate credential file unavailable or invalid');
  } finally {
    bytes.fill(0);
    if (fd !== undefined) closeSync(fd);
  }
  next.FIRECRAWL_API_KEY = key;
  return next;
}
