import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const compose = readFileSync(new URL("../compose.local.yaml", import.meta.url), "utf8");

function serviceBlock(name) {
  const lines = compose.split("\n");
  const start = lines.findIndex((line) => line === `  ${name}:`);
  assert.notEqual(start, -1, `missing service ${name}`);

  let end = lines.length;
  for (let i = start + 1; i < lines.length; i += 1) {
    if (/^  [A-Za-z0-9_-]+:\s*$/.test(lines[i])) {
      end = i;
      break;
    }
  }
  return lines.slice(start, end).join("\n");
}

const expected = {
  api: ["cpus: 4.0", "mem_limit: 8G", "memswap_limit: 8G", "pids_limit: 512"],
  "playwright-service": ["cpus: 2.0", "mem_limit: 4G", "memswap_limit: 4G", "pids_limit: 512"],
  "browser-session-service": ["cpus: 1.0", "mem_limit: 2G", "memswap_limit: 2G", "pids_limit: 384"],
  "playwright-public-proxy-service": ["cpus: 1.0", "mem_limit: 2G", "memswap_limit: 2G", "pids_limit: 256"],
  "playwright-router": ["cpus: 0.25", "mem_limit: 128M", "memswap_limit: 128M", "pids_limit: 64"],
};

for (const [service, limits] of Object.entries(expected)) {
  test(`${service} keeps explicit DoS resource guardrails`, () => {
    const block = serviceBlock(service);
    for (const limit of limits) {
      assert.ok(block.includes(`    ${limit}`), `${service} missing ${limit}`);
    }
  });
}
