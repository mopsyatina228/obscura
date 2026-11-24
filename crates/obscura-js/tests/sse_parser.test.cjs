'use strict';
// Source-attested unit tests for OBMAIN89's incremental SSE parser.
// The real EventSource transport is deliberately NOT claimed implemented here.
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const path=require('node:path');
const vm=require('node:vm');

const source=fs.readFileSync(path.resolve(__dirname,'../js/bootstrap.js'),'utf8');
const begin='// OBMAIN89_SSE_PARSER_BEGIN';
const end='// OBMAIN89_SSE_PARSER_END';
assert.equal(source.split(begin).length,2,'parser must occur exactly once');
assert.equal(source.split(end).length,2,'parser must have one end marker');
const body=source.split(begin)[1].split(end)[0];
assert.match(body,/function _createSseEventParser\(/);
const make=vm.runInNewContext(body+'\n_createSseEventParser',{TextDecoder});
const encoder=new TextEncoder();
const reader=(chunks,finish=true)=>{
 const events=[],retries=[];
 const p=make(e=>events.push(JSON.parse(JSON.stringify(e))),n=>retries.push(n));
 for(const c of chunks)p.push(c);
 if(finish)p.finish();
 return {events,retries,parser:p};
};

test('two separate lines framed into one multiline message across split CRLF',()=>{
 const {events}=reader(['data: first\r','\ndata: second\r\n\r','\n']);
 assert.deepEqual(events,[{type:'message',data:'first\nsecond',lastEventId:''}]);
});
test('named event, id and retry parsed before dispatch',()=>{
 const out=reader(['id: 207\nevent: delta\nretry: 1500\ndata: ready\n\n']);
 assert.deepEqual(out.events,[{type:'delta',data:'ready',lastEventId:'207'}]);
 assert.deepEqual(out.retries,[1500]);
});
test('UTF-8 split at each byte decodes Unicode without U+FFFD',()=>{
 const bytes=encoder.encode('data: Привет 🦝 ✓\n\n');
 const r=reader([...bytes].map(byte=>Uint8Array.of(byte)));
 assert.equal(r.events[0].data,'Привет 🦝 ✓');
 assert.ok(!r.events[0].data.includes('�'));
});
test('BOM split across input boundaries is ignored only at beginning',()=>{
 const bytes=encoder.encode('\uFEFFdata: ok\n\n');
 const out=reader([bytes.slice(0,1),bytes.slice(1,2),bytes.slice(2)]);
 assert.equal(out.events[0].data,'ok');
});
test('comments, unknown fields, and id-only records do not dispatch',()=>{
 const out=reader([': comment\nx-vendor: ignore\nid: update\n\n','data: record\n\n']);
 assert.deepEqual(out.events,[{type:'message',data:'record',lastEventId:'update'}]);
});
test('id field containing NUL is rejected, old id persists',()=>{
 const out=reader(['id: old\ndata: before\n\nid: bad\0id\ndata: after\n\n']);
 assert.equal(out.events[0].lastEventId,'old');
 assert.equal(out.events[1].lastEventId,'old');
});
test('empty id clears the last event id',()=>{
 const out=reader(['id: 1\ndata: a\n\nid\ndata: b\n\n']);
 assert.deepEqual(out.events.map(e=>e.lastEventId),['1','']);
});
test('bare data line sends empty string event',()=>{
 const out=reader(['data\n\n']);
 assert.equal(out.events.length,1);
 assert.equal(out.events[0].data,'');
});
test('bare event field falls back to message',()=>{
 const out=reader(['event\ndata: foo\n\n']);
 assert.equal(out.events[0].type,'message');
});
test('CR-only line endings work, without duplicate LF frames',()=>{
 const a=reader(['data: one\r\rdata: two\r\r']);
 assert.deepEqual(a.events.map(e=>e.data),['one','two']);
});
test('leading spaces in field value strip exactly one ASCII space',()=>{
 const a=reader(['data:  two\n\n']);
 assert.equal(a.events[0].data,' two');
});
test('retry accepts non-negative ASCII digits including zero, rejects garbage',()=>{
 const out=reader(['retry: 0\nretry: 00025\nretry: +1\nretry: -1\nretry: 1.5\nretry: x\nretry: 9007199254740992\n']);
 assert.deepEqual(out.retries,[0,25]);
});
test('EOF discards unterminated pending data even after newline',()=>{
 const out=reader(['data: pending\n']);
 assert.deepEqual(out.events,[]);
});
test('EOF closes and further push is rejected',()=>{
 const {parser}=reader(['data: one\n\n']);
 assert.throws(()=>parser.push('data: two\n\n'),/closed/);
 assert.doesNotThrow(()=>parser.finish());
});
test('cancel stops further processing',()=>{
 const events=[];const p=make(e=>events.push(e));
 p.push('data: pending\n');p.cancel();
 assert.throws(()=>p.push('data: new\n\n'),/closed/);
 assert.equal(events.length,0);
});
test('separate parser instances do not share last-event-id or data',()=>{
 const a=reader(['id: A\ndata: a\n\n']);
 const b=reader(['data: b\n\n']);
 assert.equal(a.events[0].lastEventId,'A');
 assert.equal(b.events[0].lastEventId,'');
});
test('line length is bounded against unlimited peer input',()=>{
 assert.throws(()=>reader(['z'.repeat(262145)]),/maximum size/);
});
test('aggregate event length is bounded across many valid lines',()=>{
 assert.throws(()=>reader(['data: '+'x'.repeat(262140)+'\n', 'data: '+'x'.repeat(262140)+'\n','data: '+'x'.repeat(262140)+'\n','data: '+'x'.repeat(262140)+'\n','data: plus\n']),/maximum size/);
});
test('callback types validated',()=>{
 assert.throws(()=>make(null),/callbacks must be functions/);
 assert.throws(()=>make(()=>{},null),/callbacks must be functions/);
});
test('chunk boundary after CR before LF consumes exactly one newline',()=>{
 const r=reader(['data: a\r','\n\r','\ndata: b\r\n\r\n']);
 assert.deepEqual(r.events.map(e=>e.data),['a','b']);
});
