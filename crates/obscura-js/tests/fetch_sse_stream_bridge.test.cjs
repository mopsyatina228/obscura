'use strict';
// OBMAIN93: source-attested bridge tests using dummy native op and Node streams.
// No authenticated browser, live HTTP session or nonlocal network is used.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const vm = require('node:vm');
const bootstrap = fs.readFileSync(path.resolve(__dirname, '../js/bootstrap.js'), 'utf8');
const begin = '// OBMAIN93_STREAM_BRIDGE: one native pull per ReadableStream demand;';
const end = 'const _requestHeaderGuards = new WeakSet();';
assert.equal(bootstrap.split(begin).length, 2);
assert.equal(bootstrap.split(end).length, 2);
const helper = bootstrap.split(begin)[1].split(end)[0];
assert.match(helper, /function _createNativeFetchStream\(/);
function deferred() {
  let resolve, reject;
  const promise = new Promise((r, j) => { resolve = r; reject = j; });
  return { promise, resolve, reject };
}
function environment(next) {
  const state = { calls: 0, closed: [], cleanups: 0, observers: new Set() };
  const core = {
    ops: {
      async op_fetch_stream_next(rid) { state.calls++; return next(rid, state.calls); },
      op_try_close(rid) { state.closed.push(rid); },
    },
  };
  const context = vm.createContext({
    ReadableStream,
    __obscuraCore: core,
    _abortSignalState: s => s,
    _addAbortAlgorithm: (signal, target, callback) => {
      const observer = () => callback(target, signal.reason);
      signal.listeners.add(observer);
      state.observers.add(observer);
      return () => { signal.listeners.delete(observer); state.observers.delete(observer); };
    },
  });
  const create = vm.runInContext(helper + '\n_createNativeFetchStream', context);
  const fetchStream = signal => create(721, signal || null, () => { state.cleanups++; });
  return { state, fetchStream };
}

test('no speculative transport request before stream read (zero HWM)', async () => {
  const env = environment(async () => new Uint8Array(0));
  const stream = env.fetchStream();
  await Promise.resolve();
  await Promise.resolve();
  assert.equal(env.state.calls, 0);
  const reader = stream.getReader();
  assert.equal((await reader.read()).done, true);
  assert.equal(env.state.calls, 1);
  assert.equal(env.state.cleanups, 1);
  assert.deepEqual(env.state.closed, []);
});
test('first SSE chunk visible before withheld second event and EOF', async () => {
  const first = deferred(), second = deferred();
  const env = environment(async (_rid, n) => n === 1 ? first.promise : n === 2 ? second.promise : new Uint8Array(0));
  const reader = env.fetchStream().getReader();
  const early = reader.read();
  await Promise.resolve();
  assert.equal(env.state.calls, 1);
  first.resolve(new TextEncoder().encode('data: first\n\n'));
  const one = await early;
  assert.equal(new TextDecoder().decode(one.value), 'data: first\n\n');
  assert.equal(one.done, false);
  assert.equal(env.state.calls, 1, 'no second native pull without consumer demand');
  const later = reader.read();
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(env.state.calls, 2);
  second.resolve(new TextEncoder().encode('data: second\n\n'));
  const two = await later;
  assert.equal(new TextDecoder().decode(two.value), 'data: second\n\n');
  assert.equal((await reader.read()).done, true);
  assert.equal(env.state.calls, 3);
  assert.equal(env.state.cleanups, 1);
});
test('cancel before first read closes rid without fetching', async () => {
  const env = environment(async () => { throw new Error('unexpected pull'); });
  const stream = env.fetchStream();
  await stream.cancel('stop');
  assert.equal(env.state.calls, 0);
  assert.deepEqual(env.state.closed, [721]);
  assert.equal(env.state.cleanups, 1);
});
test('reader cancel after chunk closes the native resource exactly once', async () => {
  const env = environment(async () => Uint8Array.of(42));
  const reader = env.fetchStream().getReader();
  assert.equal((await reader.read()).value[0], 42);
  await reader.cancel('no more');
  assert.deepEqual(env.state.closed, [721]);
  assert.equal(env.state.cleanups, 1);
});
test('network op rejection propagates without an unhandled background loop', async () => {
  const env = environment(async () => { throw new TypeError('synthetic transport failed'); });
  const reader = env.fetchStream().getReader();
  await assert.rejects(reader.read(), /synthetic transport failed/);
  assert.deepEqual(env.state.closed, [721]);
  assert.equal(env.state.cleanups, 1);
});
test('signal abort closes network rid and rejects an outstanding reader', async () => {
  const pending = deferred();
  const env = environment(async () => pending.promise);
  const signal = { aborted: false, reason: null, listeners: new Set() };
  const reader = env.fetchStream(signal).getReader();
  const waiting = reader.read();
  await Promise.resolve();
  signal.aborted = true;
  signal.reason = new Error('cancelled by local dummy signal');
  for (const cb of [...signal.listeners]) cb();
  await assert.rejects(waiting, /cancelled by local dummy signal/);
  pending.reject(new Error('native pull cancelled'));
  await Promise.resolve();
  assert.deepEqual(env.state.closed, [721]);
  assert.equal(env.state.cleanups, 1);
  assert.equal(signal.listeners.size, 0);
});
test('binary chunks preserve individual bytes, including invalid UTF8', async () => {
  let n = 0;
  const env = environment(async () => ++n === 1 ? Uint8Array.of(0, 255, 128, 10) : new Uint8Array(0));
  const reader = env.fetchStream().getReader();
  assert.deepEqual([...((await reader.read()).value)], [0,255,128,10]);
  assert.equal((await reader.read()).done, true);
});
test('native SSE type gate does not match arbitrary content types or XHR', () => {
  const native = fs.readFileSync(path.resolve(__dirname, '../src/ops.rs'), 'utf8');
  assert.match(native, /eq_ignore_ascii_case\("text\/event-stream"\)/);
  const start = native.indexOf('// OBMAIN93_STREAM_BRIDGE: enable true demand-driven');
  assert.ok(start > 0);
  const gate = native.slice(start, native.indexOf('// A resource must not form an ownership cycle', start));
  for (const rule of ['!internal_load', '!opaque', '!was_intercepted', 'ResourceType::Fetch', 'response_observed', 'reqwest::Method::GET']) {
    assert.ok(gate.includes(rule), 'missing SSE streaming guard: ' + rule);
  }
});
