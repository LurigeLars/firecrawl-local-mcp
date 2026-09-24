import assert from 'node:assert/strict';
import { BROWSER_TOOL_DEFINITIONS, BROWSER_TOOL_NAMES, browserBridgeRequest, browserToolsFor } from './browser-tools.mjs';

assert.equal(BROWSER_TOOL_DEFINITIONS.length, 7);
assert.deepEqual([...BROWSER_TOOL_NAMES].sort(), ['browser_category_probe','browser_network_log','browser_product_open','browser_product_probe','browser_session_open','browser_session_status','browser_snapshot']);
assert.equal(browserToolsFor(new Set(['browser_snapshot'])).length, 1);
assert.deepEqual(browserBridgeRequest('browser_session_open', { session: 'season-spendrups' }), { method: 'POST', path: '/session/open', body: { session: 'season-spendrups' } });
assert.deepEqual(browserBridgeRequest('browser_product_open', { session: 'season-spendrups', productId: '1110911' }), { method: 'POST', path: '/session/product-open', body: { session: 'season-spendrups', productId: '1110911' } });
assert.deepEqual(browserBridgeRequest('browser_product_open', { session: 'season-ms', productId: '724417' }), { method: 'POST', path: '/session/product-open', body: { session: 'season-ms', productId: '724417' } });
assert.throws(() => browserBridgeRequest('browser_product_open', { session: 'season-ms', productId: '../724417?q=x' }), /invalid product ID/);
assert.equal(browserBridgeRequest('browser_network_log', { session: 'season-ms', limit: 999 }).path, '/session/network?session=season-ms&limit=200');
assert.equal(browserBridgeRequest('browser_product_probe', { session: 'season-spendrups' }).path, '/session/product-probe?session=season-spendrups');
assert.throws(() => browserBridgeRequest('browser_product_probe', { session: 'season-ms' }), /only for season-spendrups/);
assert.equal(browserBridgeRequest('browser_category_probe', { session: 'season-spendrups' }).path, '/session/category-probe?session=season-spendrups');
assert.equal(browserBridgeRequest('browser_category_probe', { session: 'season-spendrups', pageNumber: 4 }).path, '/session/category-probe?session=season-spendrups&pageNumber=4');
assert.throws(() => browserBridgeRequest('browser_category_probe', { session: 'season-ms' }), /only for season-spendrups/);
assert.throws(() => browserBridgeRequest('browser_category_probe', { session: 'season-spendrups', pageNumber: 0 }), /invalid category page number/);
assert.throws(() => browserBridgeRequest('browser_session_open', { session: 'arbitrary' }), /invalid browser session/);
console.log('PASS browser tool contract');