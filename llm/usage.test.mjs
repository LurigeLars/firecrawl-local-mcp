import assert from 'node:assert/strict';
import http from 'node:http';
import { EventEmitter } from 'node:events';
import { PassThrough, Writable } from 'node:stream';
import { once } from 'node:events';
import test from 'node:test';
import { beginUsage, forwardLocal, tryGemini } from './trim-proxy.mjs';

function capture(t) {
  const events = [];
  t.mock.method(console, 'log', line => {
    if (typeof line === 'string' && line.startsWith('llm_usage ')) {
      events.push(JSON.parse(line.slice('llm_usage '.length)));
    }
  });
  t.mock.method(console, 'warn', () => {});
  return events;
}
const event = (provider, outcome, fallback = false) => ({ provider, outcome, fallback });

test('usage has fixed categories and exactly one terminal event', t => {
  const events = capture(t);
  const finish = beginUsage('gemini');
  finish('completed');
  finish('failed');
  assert.throws(() => beginUsage('private-model'));
  const second = beginUsage('ollama', true);
  assert.throws(() => second('private prompt'));
  second('http_error');
  assert.deepEqual(events, [
    event('gemini','attempt'), event('gemini','completed'),
    event('ollama','attempt',true), event('ollama','http_error',true)
  ]);
});

test('Gemini records actual attempts, model outcomes and no skipped calls', async t => {
  const events = capture(t);
  const body = Buffer.from(JSON.stringify({model:'private-model', input:'private prompt https://private.test'}));
  t.mock.method(globalThis, 'fetch', async () => ({
    ok:true, json:async () => ({choices:[{finish_reason:'stop',message:{content:'private answer'}}]})
  }));
  const usage = {};
  assert.equal((await tryGemini('/v1/responses',body,false,usage,'fake-secret')).status,'completed');
  assert.equal(await tryGemini('/v1/responses',body,false,{},''),null);
  assert.equal(await tryGemini('/api/generate',body,false,{},'fake-secret'),null);
  assert.deepEqual(events,[event('gemini','attempt'),event('gemini','completed')]);
});

test('Gemini incomplete responses and failed attempts are distinguished', async t => {
  const events = capture(t);
  const body = Buffer.from(JSON.stringify({input:'private prompt'}));
  const mock = t.mock.method(globalThis,'fetch',async () => ({
    ok:true, json:async () => ({choices:[{finish_reason:'length',message:{content:'partial'}}]})
  }));
  assert.equal((await tryGemini('/v1/responses',body,false,{},'fake-secret')).status,'incomplete');
  mock.mock.mockImplementation(async () => { throw new Error('private provider error'); });
  const usage = {};
  assert.equal(await tryGemini('/v1/responses',body,false,usage,'fake-secret'),null);
  assert.equal(usage.geminiFailed,true);
  assert.deepEqual(events,[event('gemini','attempt'),event('gemini','incomplete'),
    event('gemini','attempt'),event('gemini','failed')]);
});

function mockLocal(t, status, mode = 'end') {
  const upstream = new EventEmitter();
  upstream.destroy = () => {};
  upstream.end = () => queueMicrotask(() => {
    if (mode === 'error') return upstream.emit('error', new Error('private error'));
    const reply = new PassThrough();
    reply.statusCode = status;
    reply.headers = {};
    upstream.callback(reply);
    if (mode === 'aborted') {
      reply.emit('aborted');
      reply.destroy(new Error('private error'));
    } else reply.end('private model response');
  });
  t.mock.method(http, 'request', (_opts, cb) => { upstream.callback = cb; return upstream; });
}

async function local(path, usage = {}, method = 'POST') {
  const res = new Writable({write(_chunk,_encoding,cb){cb();}});
  res.writeHead = () => { res.headersSent = true; };
  const done = once(res,'close');
  forwardLocal({method,url:path,headers:{'content-type':'application/json'}},res,
    Buffer.from('{"prompt":"private prompt"}'),false,null,usage);
  await done;
}
test('Ollama records successful HTTP completion and excludes metadata requests', async t => {
  const events = capture(t);
  mockLocal(t,200);
  await local('/api/generate',{geminiFailed:true});
  await local('/api/tags',{},'GET');
  assert.deepEqual(events,[event('ollama','attempt',true),event('ollama','http_ok',true)]);
});
test('Ollama HTTP failures are counted once', async t => {
  const events = capture(t);
  mockLocal(t,503);
  await local('/v1/responses');
  assert.deepEqual(events,[event('ollama','attempt'),event('ollama','http_error')]);
});
test('Ollama transport errors are counted once', async t => {
  const events = capture(t);
  mockLocal(t,502,'error');
  await local('/api/chat');
  assert.deepEqual(events,[event('ollama','attempt'),event('ollama','transport_error')]);
});
test('Ollama aborted responses are not counted as HTTP success', async t => {
  const events = capture(t);
  mockLocal(t,200,'aborted');
  await local('/v1/chat/completions');
  assert.deepEqual(events,[event('ollama','attempt'),event('ollama','aborted')]);
});
