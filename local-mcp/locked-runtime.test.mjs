import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { ensureLockedRuntime } from './locked-runtime.mjs';

function fixture(version = '3.40.0') {
  const root = mkdtempSync(join(tmpdir(), 'locked-runtime-'));
  const pub = join(root, 'public');
  mkdirSync(pub);
  writeFileSync(join(pub, 'package.json'), JSON.stringify({ dependencies: { 'firecrawl-mcp': version } }));
  writeFileSync(join(pub, 'package-lock.json'), JSON.stringify({ lockfileVersion: 3, v: version }));
  return { root, pub, runtime: join(root, '.runtime') };
}

// Stands in for `npm ci`: writes the package the lock file names into the temp tree.
const fakeInstall = (installedVersion, calls) => dir => {
  calls?.push(dir);
  const pkg = join(dir, 'node_modules', 'firecrawl-mcp');
  mkdirSync(join(pkg, 'dist'), { recursive: true });
  writeFileSync(join(pkg, 'package.json'), JSON.stringify({ version: installedVersion, bin: { 'firecrawl-mcp': 'dist/index.js' } }));
  writeFileSync(join(pkg, 'dist', 'index.js'), '');
};

test('installs once into a hash directory and reuses it', () => {
  const f = fixture();
  const calls = [];
  const first = ensureLockedRuntime(f.pub, f.runtime, fakeInstall('3.40.0', calls));
  const second = ensureLockedRuntime(f.pub, f.runtime, fakeInstall('3.40.0', calls));
  assert.equal(calls.length, 1);
  assert.equal(first.entry, second.entry);
  assert.match(first.entry, /[\\/]\.runtime[\\/][0-9a-f]{16}[\\/]node_modules[\\/]firecrawl-mcp[\\/]dist[\\/]index\.js$/);
  assert.deepEqual(readdirSync(f.runtime).filter(n => n.includes('.tmp-')), []);
  rmSync(f.root, { recursive: true, force: true });
});

test('a concurrent install that lands first is reused, not overwritten', () => {
  const f = fixture();
  let winner;
  const racer = dir => {
    fakeInstall('3.40.0')(dir);
    // The other client completes the same hash directory while this one is still installing.
    winner = ensureLockedRuntime(f.pub, f.runtime, fakeInstall('3.40.0'));
  };
  const result = ensureLockedRuntime(f.pub, f.runtime, racer);
  assert.equal(result.entry, winner.entry);
  assert.deepEqual(readdirSync(f.runtime).filter(n => n.includes('.tmp-')), []);
  rmSync(f.root, { recursive: true, force: true });
});

test('a failed install leaves no runtime and no temp tree behind', () => {
  const f = fixture();
  assert.throws(() => ensureLockedRuntime(f.pub, f.runtime, () => { throw new Error('npm ci failed'); }), /npm ci failed/);
  assert.deepEqual(existsSync(f.runtime) ? readdirSync(f.runtime) : [], []);
  rmSync(f.root, { recursive: true, force: true });
});

test('a changed lock file installs a new tree; a wrong installed version is refused', () => {
  const a = fixture('3.40.0');
  const b = fixture('3.41.0');
  assert.notEqual(ensureLockedRuntime(a.pub, a.runtime, fakeInstall('3.40.0')).entry.split(/[\\/]/).at(-5),
    ensureLockedRuntime(b.pub, a.runtime, fakeInstall('3.41.0')).entry.split(/[\\/]/).at(-5));
  const c = fixture('3.42.0');
  assert.throws(() => ensureLockedRuntime(c.pub, c.runtime, fakeInstall('3.39.0')), /has firecrawl-mcp 3\.39\.0, expected 3\.42\.0/);
  for (const f of [a, b, c]) rmSync(f.root, { recursive: true, force: true });
});
