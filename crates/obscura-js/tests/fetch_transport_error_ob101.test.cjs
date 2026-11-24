'use strict';
// OBMAIN101: direct source-attested fetch() rejection taxonomy.
// Dummy native operation only. No network, browser profile, credentials or auth.
const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const source=fs.readFileSync(path.resolve(__dirname,'../js/bootstrap.js'),'utf8');
const begin='const _fetch = (async function _fetch(';
const end='globalThis.fetch = (input, init = {}) => _fetch(input, init);';
assert.equal(source.split(begin).length,2);
assert.equal(source.split(end).length,2);
const fragment=source.slice(source.indexOf(begin),source.indexOf(end)+end.length);
function environment(outcome) {
 const state={fetchCalls:0,abortRegistration:0,abortUnregistration:0,handleClosed:[],mode:null};
 const sandbox={
  Request:class Request{},URL,Error,TypeError,DOMException,JSON,Uint8Array,
  _resolveUrl:x=>x,
  _filterRequestHeaders:()=>({}),
  _serializeBody:()=>new Uint8Array(),
  _domParse:()=> 'http://127.0.0.1:19601/',
  _abortSignalState:x=>x,
  _createDependentAbortSignal:signals=>signals[0],
  _addAbortAlgorithm:()=>{state.abortRegistration++;return ()=>{state.abortUnregistration++}},
  _cancelFetchHandle:handle=>{state.handleClosed.push(handle.rid)},
  __obscuraCore:{ops:{
    op_fetch_cancel_handle:()=>1101,
    op_fetch_url:async (...args)=>{
      state.fetchCalls++;state.mode=args.at(-1);
      if(typeof outcome==='function')return outcome();
      throw outcome;
    },
  }},
 };
 const ctx=vm.createContext(sandbox);
 const bridge=vm.runInContext(fragment+'\n({fetch:globalThis.fetch,internal:_fetch})',ctx);
 return {state,...bridge};
}
test('native transport Error becomes Fetch API TypeError with message',async()=>{
 const env=environment(new Error('dummy connection refused'));
 await assert.rejects(env.fetch('/no-listener'),e=>{
  assert.equal(e.name,'TypeError');assert.ok(e instanceof TypeError);
  assert.equal(e.message,'dummy connection refused');return true;
 });
 assert.equal(env.state.fetchCalls,1);
 assert.equal(env.state.mode,'Fetch');
});
test('transport already TypeError is not double-wrapped',async()=>{
 const original=new TypeError('dummy TLS protocol error'),env=environment(original);
 await assert.rejects(env.fetch('/broken'),e=>{assert.equal(e,original);return true});
});
test('native primitive rejection is normalized without inventing a network retry',async()=>{
 const env=environment('socket refused by local fixture');
 await assert.rejects(env.fetch('/refused'),e=>{
  assert.equal(e.name,'TypeError');assert.equal(e.message,'socket refused by local fixture');return true;
 });
 assert.equal(env.state.fetchCalls,1);
});
test('failed transport with aborted signal preserves exact AbortError reason',async()=>{
 const reason=new DOMException('synthetic owner abort','AbortError');
 const state={aborted:false,reason:null};
 const env=environment(()=>{state.aborted=true;state.reason=reason;throw new Error('native canceled')});
 await assert.rejects(env.fetch('/pending',{signal:state}),e=>{assert.equal(e,reason);return true});
 assert.equal(env.state.fetchCalls,1);
 assert.equal(env.state.abortRegistration,1);
 assert.equal(env.state.abortUnregistration,1);
 assert.deepEqual(env.state.handleClosed,[1101]);
});
test('signal already aborted never creates an operation or network request',async()=>{
 const reason=new DOMException('aborted before fetch','AbortError');
 const sig={aborted:true,reason};
 const env=environment(new Error('should never run'));
 await assert.rejects(env.fetch('/never',{signal:sig}),e=>{assert.equal(e,reason);return true});
 assert.equal(env.state.fetchCalls,0);
 assert.equal(env.state.abortRegistration,0);
});
test('internal resourceType caller preserves native error class and identity',async()=>{
 const original=new Error('internal script loader failure'),env=environment(original);
 await assert.rejects(env.internal('/script',{},'Script'),e=>{assert.equal(e,original);return true});
 assert.equal(env.state.mode,'Script');
 assert.equal(env.state.fetchCalls,1);
});
test('CORS denial is still a TypeError, independently of native rejection',async()=>{
 const env=environment(()=>JSON.stringify({corsBlocked:true,corsError:'dummy denied cross-origin'}));
 await assert.rejects(env.fetch('/cors-denied'),e=>{
  assert.equal(e.name,'TypeError');assert.match(e.message,/dummy denied cross-origin/);return true;
 });
 assert.equal(env.state.fetchCalls,1);
});
test('native blocked response retains AbortError compatibility metadata',async()=>{
 const env=environment(()=>JSON.stringify({blocked:true}));
 await assert.rejects(env.fetch('/blocked'),e=>{
  assert.equal(e.name,'AbortError');assert.equal(e.__aborted,true);return true;
 });
 assert.equal(env.state.fetchCalls,1);
});
