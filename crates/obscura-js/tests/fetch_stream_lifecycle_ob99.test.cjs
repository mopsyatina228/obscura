'use strict';
// OBMAIN99: exact bootstrap helper exercised with weak abort targets and
// synthetic read/abort races. No network, third-party sessions or credentials.
const test=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs'),path=require('node:path'),vm=require('node:vm');
const source=fs.readFileSync(path.resolve(__dirname,'../js/bootstrap.js'),'utf8');
const begin='// OBMAIN93_STREAM_BRIDGE: one native pull per ReadableStream demand;';
const end='const _requestHeaderGuards = new WeakSet();';
assert.equal(source.split(begin).length,2);
assert.equal(source.split(end).length,2);
const helper=source.split(begin)[1].split(end)[0];
const delay=()=>new Promise(r=>setImmediate(r));
function deferred(){let resolve,reject;const promise=new Promise((r,j)=>{resolve=r;reject=j});return {promise,resolve,reject}}
function signal(){return {aborted:false,reason:null,listeners:new Set()}}
function abort(sig,reason=new Error('dummy signal cancelled')){
 sig.aborted=true;sig.reason=reason;
 for(const f of [...sig.listeners])f();
}
function environment(nativeNext){
 const state={reads:0,rids:[],cleanups:0,registered:0,removed:0,refs:[],live:new Set()};
 const ops={
  async op_fetch_stream_next(rid){state.reads++;return nativeNext(rid,state.reads)},
  op_try_close(rid){state.rids.push(rid)},
 };
 const context=vm.createContext({
  ReadableStream,WeakRef,__obscuraCore:{ops},
  _abortSignalState:s=>s,
  // Mimic production _addAbortAlgorithm: the callback only retains a WeakRef.
  _addAbortAlgorithm:(sig,target,callback)=>{
   const weak=new WeakRef(target);
   state.registered++;state.refs.push(weak);
   const listener=()=>{const value=weak.deref();if(value)callback(value,sig.reason)};
   if(sig.aborted){listener();return ()=>{}}
   sig.listeners.add(listener);state.live.add(listener);
   return ()=>{state.removed++;sig.listeners.delete(listener);state.live.delete(listener)};
  },
 });
 const create=vm.runInContext(helper+'\n_createNativeFetchStream',context);
 return {state,create:(sig)=>create(991,sig||null,()=>{state.cleanups++})};
}
test('weak abort target stays reachable while stream is live',async()=>{
 assert.match(helper,/let closed = false, controller = null, removeAbort = null, abortHandle = null/);
 assert.match(helper,/abortHandle = \{ rid \}/);
 assert.match(helper,/abortHandle = null;\s*cleanup\(\)/);
 const sig=signal(),env=environment(async()=>new Uint8Array([42]));
 const stream=env.create(sig);
 assert.equal(env.state.refs[0].deref()!==undefined,true);
 // Multiple event-loop turns exercise the weak-reference ownership boundary.
 for(let i=0;i<8;i++){await delay();if(global.gc)global.gc()}
 assert.ok(env.state.refs[0].deref(), 'weak abort handle must be strongly kept by live stream');
 abort(sig);
 assert.deepEqual(env.state.rids,[991]);
 assert.equal(env.state.cleanups,1);
 assert.equal(env.state.removed,1);
 assert.equal(sig.listeners.size,0);
 assert.equal(stream.locked,false);
});
test('abort without any reader closes RID and cleans up once',async()=>{
 const sig=signal(),env=environment(async()=>{throw Error('unexpected native call')});
 const stream=env.create(sig);
 await delay();abort(sig,new Error('abort without reader'));
 await delay();assert.equal(env.state.reads,0);
 assert.deepEqual(env.state.rids,[991]);
 assert.equal(env.state.cleanups,1);
 assert.equal(env.state.removed,1);
 assert.equal(sig.listeners.size,0);
 assert.equal(stream.locked,false);
});
test('cancel during withheld native pull discards late successful chunk',async()=>{
 const pending=deferred(),env=environment(async()=>pending.promise);
 const reader=env.create().getReader(),waiting=reader.read();
 await delay();assert.equal(env.state.reads,1);
 await reader.cancel('user stopped');
 const output=await waiting;
 assert.equal(output.done,true);
 pending.resolve(new TextEncoder().encode('late chunk'));
 await delay();await delay();
 assert.deepEqual(env.state.rids,[991]);
 assert.equal(env.state.cleanups,1);
});
test('transport failure rejects pending reader with one close',async()=>{
 const env=environment(async()=>{throw new TypeError('synthetic read failed')});
 const reader=env.create().getReader();
 await assert.rejects(reader.read(),/synthetic read failed/);
 await assert.rejects(reader.read(),/synthetic read failed/);
 assert.equal(env.state.reads,1);
 assert.deepEqual(env.state.rids,[991]);
 assert.equal(env.state.cleanups,1);
});
test('abort during pending native read returns exact abort reason',async()=>{
 const sig=signal(),pending=deferred(),env=environment(async()=>pending.promise);
 const reader=env.create(sig).getReader(),waiting=reader.read();
 await delay();
 const reason=new Error('synthetic explicit abort');
 abort(sig,reason);
 await assert.rejects(waiting,/synthetic explicit abort/);
 pending.reject(new Error('late transport error'));
 await delay();await delay();
 assert.deepEqual(env.state.rids,[991]);
 assert.equal(env.state.cleanups,1);
 assert.equal(env.state.removed,1);
 assert.equal(sig.listeners.size,0);
});
test('successful EOF removes listener, no RID close even after late abort',async()=>{
 let n=0;
 const sig=signal(),env=environment(async()=>++n===1?Uint8Array.of(10,20):new Uint8Array());
 const reader=env.create(sig).getReader();
 assert.deepEqual([...((await reader.read()).value)],[10,20]);
 assert.equal((await reader.read()).done,true);
 const reads=env.state.reads;
 abort(sig);
 await delay();
 assert.equal(env.state.reads,reads);
 assert.equal(env.state.cleanups,1);
 assert.equal(env.state.removed,1);
 assert.equal(sig.listeners.size,0);
 assert.deepEqual(env.state.rids,[]);
});
test('cancel after EOF does not double cleanup or close',async()=>{
 let n=0;
 const env=environment(async()=>++n===1?Uint8Array.of(5):new Uint8Array());
 const reader=env.create().getReader();
 await reader.read();await reader.read();await reader.cancel();
 assert.equal(env.state.cleanups,1);
 assert.deepEqual(env.state.rids,[]);
});
test('read pull backpressure no speculative extra native requests',async()=>{
 const waits=[deferred(),deferred()];
 const env=environment(async(_rid,n)=>n<=2?waits[n-1].promise:new Uint8Array());
 const reader=env.create().getReader();
 const first=reader.read();
 await delay();assert.equal(env.state.reads,1);
 waits[0].resolve(Uint8Array.of(1));
 assert.equal((await first).value[0],1);
 for(let i=0;i<5;i++)await delay();
 assert.equal(env.state.reads,1);
 const second=reader.read();
 await delay();assert.equal(env.state.reads,2);
 waits[1].resolve(Uint8Array.of(2));
 assert.equal((await second).value[0],2);
 assert.equal((await reader.read()).done,true);
 assert.equal(env.state.cleanups,1);
});
test('native oversize rejection closes resource and abort observer exactly once',async()=>{
 const sig=signal(),env=environment(async()=>{throw Error('Response stream exceeds maximum bytes')});
 const reader=env.create(sig).getReader();
 await assert.rejects(reader.read(),/exceeds maximum bytes/);
 abort(sig);
 assert.deepEqual(env.state.rids,[991]);
 assert.equal(env.state.removed,1);
 assert.equal(env.state.cleanups,1);
 assert.equal(sig.listeners.size,0);
});
