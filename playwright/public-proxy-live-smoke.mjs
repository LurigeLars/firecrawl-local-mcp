const ROUTER = process.env.ROUTER_URL || 'http://playwright-router:3000';

async function sleep(ms) {
  await new Promise(resolve => setTimeout(resolve, ms));
}

async function waitForRouter() {
  let lastError = null;
  for (let attempt = 1; attempt <= 20; attempt += 1) {
    try {
      const response = await fetch(`${ROUTER}/health`, { headers: { connection: 'close' } });
      if (response.ok) return;
      lastError = new Error(`health status ${response.status}`);
    } catch (error) {
      lastError = error;
    }
    await sleep(250);
  }
  throw new Error(`router health unavailable: ${lastError?.message || 'unknown'}`);
}

async function metrics() {
  const response = await fetch(`${ROUTER}/metrics`, {
    headers: { connection: 'close' },
  });
  if (!response.ok) throw new Error(`metrics status ${response.status}`);
  return response.json();
}

await waitForRouter();
const before = await metrics();

const response = await fetch(`${ROUTER}/scrape`, {
  method: 'POST',
  headers: {
    'content-type': 'application/json',
    connection: 'close',
  },
  body: JSON.stringify({
    url: 'https://curemydisease.com/',
    headers: {
      Cookie: 'smoke=dummy',
    },
  }),
});

const body = await response.json();
if (response.status !== 400) {
  throw new Error(`expected fail-closed HTTP 400, got ${response.status}`);
}
if (!String(body?.error || '').includes('authentication/session headers')) {
  throw new Error('router did not return the expected fail-closed reason');
}

const after = await metrics();
if (after.sensitive_rejected !== before.sensitive_rejected + 1) {
  throw new Error('sensitive_rejected counter did not increment exactly once');
}
if (after.requests_total !== before.requests_total + 1) {
  throw new Error('requests_total counter did not increment exactly once');
}
if (after.proxy_attempts !== before.proxy_attempts) {
  throw new Error('fail-closed request unexpectedly reached the proxy path');
}

console.log('PUBLIC_PROXY_FAIL_CLOSED_SMOKE_PASS');
