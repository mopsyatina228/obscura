'use strict';
// OBMAIN95: synthetic local-only transport. Tests exercise exact bootstrap
// EventSource and SSE parser code; no real user session or remote HTTP calls.
const test = require('node:test');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const vm = require('node:vm');
const path = require('node:path');
const source = fs.readFileSync(path.resolve(__dirname, '../js/bootstrap.js'), 'utf8');
function segment(start, stop) {
  assert.equal(source.split(start).length, 2, 'missing unique start '+start);
  assert.equal(source.split(stop).length, 2, 'missing unique end '+stop);
  return source.split(start)[1].split(stop)[0];
}
const listener = 'function _makeListenerBox(self) {' + segment('function _makeListenerBox(self) {','// OBMAIN89_SSE_PARSER_BEGIN');
const parser = segment('// OBMAIN89_SSE_PARSER_BEGIN','// OBMAIN89_SSE_PARSER_END');
const implementation = segment('// OBMAIN95_EVENTSOURCE_BEGIN','// OBMAIN95_EVENTSOURCE_END');
const encoder = new TextEncoder();
class EventStub { constructor(type){this.type=type;} }
class MessageStub extends EventStub {
  constructor(type, options={}){super(type);Object.assign(this,options)}
}
function controlledStream() {
  let controller;
  let canceled = false;
  const stream = new ReadableStream({
    start(c){controller=c},
    cancel(){canceled=true},
  }, {highWaterMark: 0});
  return {
    stream,
    push(s){controller.enqueue(typeof s === 'string' ? encoder.encode(s) : s)},
    end(){controller.close()},
    get canceled(){return canceled},
  };
}
function environment(fetchFn){
  let id=0;
  const timers=new Map();
  let calls=[];
  const ctx=vm.createContext({
    document:{baseURI:'http://127.0.0.1:19695/subdir/fixture'},
    URL, DOMException, AbortController, Event:EventStub, MessageEvent:MessageStub,
    TextDecoder, Promise,
    setTimeout(fn,ms){const next=++id;timers.set(next,{fn,ms});return next},
    clearTimeout(key){timers.delete(key)},
    fetch:async (url,options)=>{
      calls.push({url,options});
      return fetchFn(url,options,calls.length);
    },
  });
  vm.runInContext(listener+parser+implementation,ctx);
  return { EventSource:ctx.EventSource, timers, calls,
    fireTimers(){
      const queued=Array.from(timers.values());
      timers.clear();queued.forEach(t=>t.fn());
    },
  };
}
function response(body,status=200,contentType='text/event-stream; charset=utf-8'){
  return {status,body,headers:{get(name){return String(name).toLowerCase()==='content-type'?contentType:null}}};
}
async function until(predicate, reason='condition'){
  for(let i=0;i<120;i++){
    if(predicate())return;
    await new Promise(resolve=>setTimeout(resolve,3));
  }
  assert.fail('timeout: '+reason);
}
test('validation and close-before-microtask never fetches',async()=>{
  const env=environment(async()=>{throw Error('must not fetch')});
  assert.throws(()=>new env.EventSource(),/URL/);
  assert.throws(()=>new env.EventSource('file:///tmp/a'),/HTTP/);
  assert.throws(()=>new env.EventSource('data:abc'),/HTTP/);
  const e=new env.EventSource('/events');
  assert.equal(e.url,'http://127.0.0.1:19695/events');
  assert.equal(e.readyState,0);
  e.close();
  await Promise.resolve();
  assert.equal(e.readyState,2);
  assert.equal(env.calls.length,0);
  assert.equal(env.EventSource.CONNECTING,0);assert.equal(env.EventSource.OPEN,1);
});
test('real GET with Accept, first message before second and EOF',async()=>{
  const wire=controlledStream(), env=environment(async()=>response(wire.stream));
  const e=new env.EventSource('/events');
  const fired=[];
  e.onopen=()=>fired.push('onopen');
  e.addEventListener('open',()=>fired.push('open'));
  e.onmessage=x=>fired.push('onmessage:'+x.data);
  e.addEventListener('message',x=>fired.push('message:'+x.data));
  assert.deepEqual(fired,[]);
  await until(()=>e.readyState===1,'open');
  assert.equal(env.calls.length,1);
  assert.equal(env.calls[0].url,'http://127.0.0.1:19695/events');
  assert.equal(env.calls[0].options.method,'GET');
  assert.equal(env.calls[0].options.headers.Accept,'text/event-stream');
  assert.equal(env.calls[0].options.credentials,'same-origin');
  assert.deepEqual(fired,['onopen','open']);
  wire.push('data: first\n\n');
  await until(()=>fired.includes('message:first'),'first event');
  assert.equal(e.readyState,1);
  assert.equal(env.timers.size,0);
  wire.push('data: second\n\n');
  await until(()=>fired.includes('message:second'),'second event');
  assert.deepEqual(fired,['onopen','open','onmessage:first','message:first','onmessage:second','message:second']);
  e.close();await until(()=>wire.canceled,'reader cancel');
  assert.equal(env.timers.size,0);
});
test('named events, id, retry, and reconnect sends Last-Event-ID',async()=>{
  const first=controlledStream(), second=controlledStream();
  const env=environment(async(_url,_init,n)=>response((n===1?first:second).stream));
  const e=new env.EventSource('updates', {withCredentials:true});
  let named,secondOpen=0;
  e.addEventListener('open',()=>{secondOpen++});
  e.addEventListener('delta',x=>{named={data:x.data,id:x.lastEventId,origin:x.origin}});
  await until(()=>e.readyState===1,'first open');
  first.push('id: 207\nretry: 750\nevent: delta\ndata: ready\n\n');
  await until(()=>Boolean(named),'delta');
  assert.deepEqual(named,{data:'ready',id:'207',origin:'http://127.0.0.1:19695'});
  first.end();
  await until(()=>env.timers.size===1,'reconnect timer');
  assert.equal(e.readyState,0);
  assert.equal([...env.timers.values()][0].ms,750);
  env.fireTimers();
  await until(()=>secondOpen===2,'second open');
  assert.equal(env.calls.length,2);
  assert.equal(env.calls[1].options.headers['Last-Event-ID'],'207');
  assert.equal(env.calls[1].options.credentials,'include');
  second.push('data: next\n\n');
  await until(()=>e._esLastEventId==='207','id persists');
  e.close();
});
test('retry:0 is throttled and transports can recover after a failure',async()=>{
  const good=controlledStream();
  const env=environment(async (_url,_opts,n)=>{
    if(n===1)throw new Error('synthetic disconnected transport');
    return response(good.stream);
  });
  const e=new env.EventSource('/flaky');
  let errors=0;
  e.onerror=()=>errors++;
  await until(()=>env.timers.size===1,'retry after network failure');
  assert.equal(e.readyState,0);assert.equal(errors,1);
  env.fireTimers();
  await until(()=>e.readyState===1,'reconnected');
  assert.equal(env.calls.length,2);
  good.push('retry: 0\ndata: ready\n\n');
  await until(()=>e._esRetry===0,'retry field');
  good.end();
  await until(()=>env.timers.size===1,'second reconnect');
  assert.equal([...env.timers.values()][0].ms,250);
  e.close();assert.equal(env.timers.size,0);
});
test('HTTP 204 terminates permanently, never emits an open',async()=>{
  const env=environment(async()=>response(null,204,''));
  const e=new env.EventSource('/gone');
  const events=[];
  e.addEventListener('open',()=>events.push('open'));
  e.onerror=()=>events.push('error');
  await until(()=>e.readyState===2,'204 closed');
  assert.deepEqual(events,['error']);
  assert.equal(env.calls.length,1);assert.equal(env.timers.size,0);
});
test('invalid content type and non-200 are terminal failures',async()=>{
  for(const [status,mime] of [[200,'text/plain'],[500,'text/event-stream']]){
    const env=environment(async()=>response(null,status,mime));
    const e=new env.EventSource('/invalid');
    let errors=0,opens=0;
    e.onerror=()=>errors++;
    e.onopen=()=>opens++;
    await until(()=>e.readyState===2,'invalid response');
    assert.equal(errors,1);assert.equal(opens,0);
    assert.equal(env.timers.size,0);
  }
});
test('close while fetch is pending aborts and ignores late response',async()=>{
  let finish,usedSignal;
  const wire=controlledStream();
  const env=environment((_url,options)=>{
    usedSignal=options.signal;
    return new Promise(resolve=>{finish=()=>resolve(response(wire.stream))});
  });
  const e=new env.EventSource('/slow');
  let opens=0,errors=0;
  e.onopen=()=>opens++;e.onerror=()=>errors++;
  await until(()=>Boolean(finish),'fetch started');
  e.close();
  assert.equal(usedSignal.aborted,true);
  finish();
  await new Promise(resolve=>setImmediate(resolve));
  assert.equal(e.readyState,2);assert.equal(opens,0);assert.equal(errors,0);
  assert.equal(env.timers.size,0);
});
test('close after open cancels pending reader, no reconnect',async()=>{
  const wire=controlledStream();
  const env=environment(async()=>response(wire.stream));
  const e=new env.EventSource('/forever');
  await until(()=>e.readyState===1,'open');
  e.close();
  await until(()=>wire.canceled,'cancel');
  assert.equal(e.readyState,2);assert.equal(env.timers.size,0);
});
test('line framing and UTF8 split over transport chunks',async()=>{
  const wire=controlledStream();
  const env=environment(async()=>response(wire.stream));
  const e=new env.EventSource('/utf8');
  const events=[];
  e.addEventListener('message',ev=>events.push(ev.data));
  await until(()=>e.readyState===1,'open');
  const bytes=encoder.encode('data: Привет 🦝\n\n');
  wire.push(bytes.slice(0,13));
  wire.push(bytes.slice(13,18));
  wire.push(bytes.slice(18));
  await until(()=>events.length===1,'unicode event');
  assert.deepEqual(events,['Привет 🦝']);
  e.close();
});
