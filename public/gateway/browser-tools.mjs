const SESSION_SCHEMA = { type: 'string', enum: ['season-spendrups', 'season-ms'] };

export const BROWSER_TOOL_DEFINITIONS = Object.freeze([
  {
    name: 'browser_session_open',
    description: 'Open or attach to one approved visible local Chrome session for manual supplier login. Credentials are entered by the user in Chrome and are never accepted by this tool.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: { session: SESSION_SCHEMA }, required: ['session'],
    },
  },
  {
    name: 'browser_session_status',
    description: 'Read status and matching tabs for one approved local supplier browser session.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: { session: SESSION_SCHEMA }, required: ['session'],
    },
  },
  {
    name: 'browser_product_open',
    description: 'Navigate one approved supplier browser session directly to a product detail page by numeric product ID. This tool does not accept arbitrary URLs, query strings or search terms and does not automate supplier search.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: {
        session: SESSION_SCHEMA,
        productId: { type: 'string', pattern: '^[0-9]+$', minLength: 1, maxLength: 12 },
      },
      required: ['session', 'productId'],
    },
  },
  {
    name: 'browser_category_open',
    description: 'Navigate the authenticated Season Spendrups browser only to the fixed All sprit category page for a bounded numeric pageNumber. No arbitrary URL, category, filter or search term is accepted.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: {
        session: { type: 'string', enum: ['season-spendrups'] },
        pageNumber: { type: 'integer', minimum: 1, maximum: 100 },
      },
      required: ['session', 'pageNumber'],
    },
  },
  {
    name: 'browser_snapshot',
    description: 'Read a bounded redacted DOM summary from one approved authenticated supplier browser session. Input values, cookies and storage are not returned.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: { session: SESSION_SCHEMA }, required: ['session'],
    },
  },
  {
    name: 'browser_product_probe',
    description: 'Read a bounded sanitized JSON response from the most recently observed Spendrups LoadProductDetailsMapped request. Available only for season-spendrups. Headers, cookies and request bodies are never returned; customer/account-like fields are redacted.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: { session: { type: 'string', enum: ['season-spendrups'] } }, required: ['session'],
    },
  },
  {
    name: 'browser_category_probe',
    description: 'Read a bounded sanitized JSON response from an already observed Spendrups GetCategoryProducts request. Optional pageNumber selects an observed category page; this tool does not initiate a new supplier request. Available only for season-spendrups. Headers, cookies and request bodies are never returned; customer/account-like fields are redacted.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: {
        session: { type: 'string', enum: ['season-spendrups'] },
        pageNumber: { type: 'integer', minimum: 1, maximum: 100 },
      },
      required: ['session'],
    },
  },
  {
    name: 'browser_network_log',
    description: 'Read recent bounded network request/response metadata captured after attachment for one approved supplier session. Headers, cookies and request bodies are intentionally omitted.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: { session: SESSION_SCHEMA, limit: { type: 'integer', minimum: 1, maximum: 200, default: 100 } },
      required: ['session'],
    },
  },
]);

export const BROWSER_TOOL_NAMES = new Set(BROWSER_TOOL_DEFINITIONS.map(t => t.name));

export const BROWSER_INSTRUCTIONS = 'Season Hotel browser tools are available only on this ChatGPT gateway: browser_session_open, browser_session_status, browser_product_open, browser_category_open, browser_snapshot, browser_network_log, browser_product_probe and browser_category_probe. They only accept season-spendrups and season-ms. browser_session_open opens a visible dedicated Chrome profile on the user\'s Windows machine; the user enters credentials there manually. Never ask for credentials in chat and never type credentials through browser automation. After manual login, browser_product_open may navigate only to direct numeric product-detail paths without query strings; it must not be used to automate search. Use browser_snapshot and browser_network_log read-only. Network output omits headers, cookies, request bodies and non-supplier hosts.';

export function browserToolsFor(allowedTools) {
  return BROWSER_TOOL_DEFINITIONS.filter(t => allowedTools.has(t.name));
}

export function browserBridgeRequest(name, args = {}) {
  const session = String(args.session || '');
  if (!['season-spendrups', 'season-ms'].includes(session)) throw new Error('invalid browser session');
  if (name === 'browser_session_open') return { method: 'POST', path: '/session/open', body: { session } };
  if (name === 'browser_session_status') return { method: 'GET', path: `/session/status?session=${encodeURIComponent(session)}` };
  if (name === 'browser_product_open') {
    const productId = String(args.productId || '');
    if (!/^[0-9]{1,12}$/.test(productId)) throw new Error('invalid product ID');
    return { method: 'POST', path: '/session/product-open', body: { session, productId } };
  }
  if (name === 'browser_category_open') {
    if (session !== 'season-spendrups') throw new Error('browser_category_open is available only for season-spendrups');
    const pageNumber = Number(args.pageNumber);
    if (!Number.isInteger(pageNumber) || pageNumber < 1 || pageNumber > 100) throw new Error('invalid category page number');
    return { method: 'POST', path: '/session/category-open', body: { session, pageNumber } };
  }
  if (name === 'browser_snapshot') return { method: 'GET', path: `/session/snapshot?session=${encodeURIComponent(session)}` };
  if (name === 'browser_product_probe') {
    if (session !== 'season-spendrups') throw new Error('browser_product_probe is available only for season-spendrups');
    return { method: 'GET', path: `/session/product-probe?session=${encodeURIComponent(session)}` };
  }
  if (name === 'browser_category_probe') {
    if (session !== 'season-spendrups') throw new Error('browser_category_probe is available only for season-spendrups');
    const pageNumber = args.pageNumber === undefined || args.pageNumber === null ? null : Number(args.pageNumber);
    if (pageNumber !== null && (!Number.isInteger(pageNumber) || pageNumber < 1 || pageNumber > 100)) throw new Error('invalid category page number');
    const suffix = pageNumber === null ? '' : `&pageNumber=${pageNumber}`;
    return { method: 'GET', path: `/session/category-probe?session=${encodeURIComponent(session)}${suffix}` };
  }
  if (name === 'browser_network_log') {
    const limit = Math.max(1, Math.min(200, Number(args.limit || 100)));
    return { method: 'GET', path: `/session/network?session=${encodeURIComponent(session)}&limit=${limit}` };
  }
  throw new Error(`unknown browser tool: ${name}`);
}