import { lookup } from 'node:dns/promises';
import { BlockList, isIP } from 'node:net';

const BLOCKED_DESTINATIONS = new BlockList();
const SENSITIVE_QUERY_KEYS = /^(?:access[_-]?token|api[_-]?key|auth|authorization|code|credential|jwt|key|password|secret|session|sig|signature|token)$/i;

for (const [network, prefix] of [
  ['0.0.0.0', 8],
  ['10.0.0.0', 8],
  ['100.64.0.0', 10],
  ['127.0.0.0', 8],
  ['169.254.0.0', 16],
  ['172.16.0.0', 12],
  ['192.0.0.0', 24],
  ['192.0.2.0', 24],
  ['192.168.0.0', 16],
  ['198.18.0.0', 15],
  ['198.51.100.0', 24],
  ['203.0.113.0', 24],
  ['224.0.0.0', 4],
  ['240.0.0.0', 4],
]) BLOCKED_DESTINATIONS.addSubnet(network, prefix, 'ipv4');

for (const [network, prefix] of [
  ['::', 128],
  ['::1', 128],
  ['::', 96],
  ['64:ff9b::', 96],
  ['100::', 64],
  ['2001:db8::', 32],
  ['fc00::', 7],
  ['fe80::', 10],
  ['ff00::', 8],
]) BLOCKED_DESTINATIONS.addSubnet(network, prefix, 'ipv6');

function normalizeHostname(hostname) {
  const value = String(hostname ?? '').trim().toLowerCase();
  return value.startsWith('[') && value.endsWith(']') ? value.slice(1, -1) : value;
}

function ipv4FromMappedIpv6(address) {
  const normalized = normalizeHostname(address);
  const dotted = normalized.match(/^(?:::ffff:|0:0:0:0:0:ffff:)(\d{1,3}(?:\.\d{1,3}){3})$/i);
  if (dotted && isIP(dotted[1]) === 4) return dotted[1];

  const hex = normalized.match(/^(?:::ffff:|0:0:0:0:0:ffff:)([0-9a-f]{1,4}):([0-9a-f]{1,4})$/i);
  if (!hex) return null;
  const high = Number.parseInt(hex[1], 16);
  const low = Number.parseInt(hex[2], 16);
  return [high >> 8, high & 255, low >> 8, low & 255].join('.');
}

function blockedAddress(address) {
  const normalized = normalizeHostname(address);
  const mapped = ipv4FromMappedIpv6(normalized);
  if (mapped) return BLOCKED_DESTINATIONS.check(mapped, 'ipv4');

  const family = isIP(normalized);
  if (family === 4) return BLOCKED_DESTINATIONS.check(normalized, 'ipv4');
  if (family === 6) return BLOCKED_DESTINATIONS.check(normalized, 'ipv6');
  return true;
}

export async function publicUrlReason(raw, options = {}) {
  const lookupFn = options.lookupFn ?? lookup;
  if (typeof raw !== 'string' || !raw.trim()) return 'A valid public HTTP(S) URL is required.';

  let target;
  try { target = new URL(raw); } catch { return 'A valid public HTTP(S) URL is required.'; }
  if (!['http:', 'https:'].includes(target.protocol)) return 'Only public HTTP(S) destinations are allowed.';
  if (target.username || target.password) return 'URLs containing credentials are not allowed.';

  const hostname = normalizeHostname(target.hostname);
  if (!hostname || hostname === 'localhost' || hostname.endsWith('.localhost')) {
    return 'Private or local network destinations are not allowed.';
  }

  const literalFamily = isIP(hostname);
  if (literalFamily) return blockedAddress(hostname) ? 'Private or local network destinations are not allowed.' : null;

  let resolved;
  try {
    resolved = await lookupFn(hostname, { all: true, verbatim: true });
  } catch {
    return 'Destination could not be safely resolved.';
  }
  if (!Array.isArray(resolved) || !resolved.length || resolved.some(item => blockedAddress(item.address))) {
    return 'Private or local network destinations are not allowed.';
  }
  return null;
}

export function redactUrl(raw) {
  try {
    const url = new URL(String(raw));
    url.username = '';
    url.password = '';
    for (const key of [...url.searchParams.keys()]) {
      if (SENSITIVE_QUERY_KEYS.test(key)) url.searchParams.set(key, '[REDACTED]');
    }
    const value = url.toString();
    return value.length > 4096 ? `${value.slice(0, 4093)}...` : value;
  } catch {
    return '[invalid-url]';
  }
}

export function sanitizeText(value, maxChars = 4000) {
  let text = String(value ?? '');
  text = text
    .replace(/\bBearer\s+[A-Za-z0-9._~+\/-]{8,}=*/gi, 'Bearer [REDACTED]')
    .replace(/\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\b/g, '[REDACTED_JWT]')
    .replace(/\bsk-[A-Za-z0-9_-]{12,}\b/g, '[REDACTED_KEY]')
    .replace(/([?&](?:access[_-]?token|api[_-]?key|auth|authorization|code|credential|jwt|key|password|secret|session|sig|signature|token)=)[^&#\s]+/gi, '$1[REDACTED]');
  if (text.length > maxChars) text = `${text.slice(0, Math.max(0, maxChars - 3))}...`;
  return text;
}
