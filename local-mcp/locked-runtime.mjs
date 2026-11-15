// Installs firecrawl-mcp exactly as locked in public/package-lock.json (the same tree the MCP
// image builds with `npm ci`) into local-mcp/.runtime/<hash of package.json + lock>.
// The tree is built in a private temp directory and renamed into place only when complete, so
// concurrent client starts never share a half-written install (what corrupted the old npx cache),
// and a Dependabot lock bump gets a fresh directory automatically. No npx and no unlocked resolve.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { copyFileSync, existsSync, mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const MARKER = '.complete';

export function npmCi(dir) {
  const args = ['ci', '--ignore-scripts', '--omit=dev', '--no-audit', '--no-fund'];
  const isWindows = process.platform === 'win32';
  // npm's stdout would corrupt the MCP stdio stream, so both of its streams go to our stderr.
  const r = spawnSync(isWindows ? 'cmd.exe' : 'npm', isWindows ? ['/d', '/s', '/c', 'npm', ...args] : args,
    { cwd: dir, stdio: ['ignore', 2, 2] });
  if (r.status !== 0) throw new Error(`npm ci for the locked firecrawl-mcp failed (exit ${r.status ?? r.error?.message})`);
}

export function ensureLockedRuntime(publicDir, runtimeRoot, install = npmCi) {
  const manifestText = readFileSync(join(publicDir, 'package.json'));
  const lockText = readFileSync(join(publicDir, 'package-lock.json'));
  const version = String(JSON.parse(manifestText)?.dependencies?.['firecrawl-mcp'] ?? '');
  if (!/^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$/.test(version)) {
    throw new Error('public/package.json does not contain an exact firecrawl-mcp version');
  }
  const key = createHash('sha256').update(manifestText).update(lockText).digest('hex').slice(0, 16);
  const dir = join(runtimeRoot, key);
  if (!existsSync(join(dir, MARKER))) {
    const tmp = `${dir}.tmp-${process.pid}`;
    rmSync(tmp, { recursive: true, force: true });
    mkdirSync(tmp, { recursive: true });
    try {
      copyFileSync(join(publicDir, 'package.json'), join(tmp, 'package.json'));
      copyFileSync(join(publicDir, 'package-lock.json'), join(tmp, 'package-lock.json'));
      install(tmp);
      writeFileSync(join(tmp, MARKER), version);
      renameSync(tmp, dir);
    } catch (error) {
      rmSync(tmp, { recursive: true, force: true });
      // Another client finished the same install first: use its complete tree.
      if (!existsSync(join(dir, MARKER))) throw error;
    }
  }
  // shortcut: older hash directories are left in place (a running client may still use one); delete them by hand if disk matters.
  const pkg = JSON.parse(readFileSync(join(dir, 'node_modules', 'firecrawl-mcp', 'package.json'), 'utf8'));
  if (pkg.version !== version) throw new Error(`Locked runtime has firecrawl-mcp ${pkg.version}, expected ${version}`);
  const bin = typeof pkg.bin === 'string' ? pkg.bin : pkg.bin?.['firecrawl-mcp'];
  if (!bin) throw new Error('firecrawl-mcp package has no firecrawl-mcp bin entry');
  return { version, entry: join(dir, 'node_modules', 'firecrawl-mcp', bin) };
}
