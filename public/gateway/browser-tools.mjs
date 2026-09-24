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
    name: 'browser_snapshot',
    description: 'Read a bounded redacted DOM summary from one approved authenticated supplier browser session. Input values, cookies and storage are not returned.',
    inputSchema: {
      type: 'object', additionalProperties: false,
      properties: { session: SESSION_SCHEMA }, required: ['session'],
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

export function browserToolsFor(allowedTools) {
  return BROWSER_TOOL_DEFINITIONS.filter(t => allowedTools.has(t.name));
}

export function browserBridgeRequest(name, args = {}) {
  const session = String(args.session || '');
  if (!['season-spendrups', 'season-ms'].includes(session)) throw new Error('invalid browser session');
  if (name === 'browser_session_open') return { method: 'POST', path: '/session/open', body: { session } };
  if (name === 'browser_session_status') return { method: 'GET', path: `/session/status?session=${encodeURIComponent(session)}` };
  if (name === 'browser_snapshot') return { method: 'GET', path: `/session/snapshot?session=${encodeURIComponent(session)}` };
  if (name === 'browser_network_log') {
    const limit = Math.max(1, Math.min(200, Number(args.limit || 100)));
    return { method: 'GET', path: `/session/network?session=${encodeURIComponent(session)}&limit=${limit}` };
  }
  throw new Error(`unknown browser tool: ${name}`);
}
