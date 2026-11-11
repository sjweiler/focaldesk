import {test} from 'node:test';
import assert from 'node:assert/strict';
import {decodeChats,appendReply} from '../src/conversations.ts';

test('saved history retains workspaces, attachments and citations',()=>{
 const chats=[{id:2,title:'Plan',project:'MSAIE',messages:[{role:'user',content:'Review this',attachments:[{name:'plan.md',content:'# Plan'}]},{role:'assistant',content:'Reviewed',citations:[{memory_id:12,source:'plan.md',excerpt:'Plan',distance:.3}]}]}];
 assert.deepEqual(decodeChats(JSON.stringify(chats)),chats);
});
test('malformed saved data is rejected before rendering',()=>{
 for(const value of [null,{},[{id:1,title:'Chat',project:'General',messages:[{role:'system',content:'bad'}]}],[{id:1,title:'Chat',project:'General',messages:[{role:'user',content:'test',attachments:[{name:'x'}]}]}]]){
  assert.throws(()=>decodeChats(JSON.stringify(value)));
 }
});
test('a delayed reply updates its original chat after workspace navigation',()=>{
 const chats=[{id:1,title:'New chat',project:'FocalDesk',messages:[]},{id:2,title:'Research',project:'Research',messages:[]}];
 const messages=[{role:'user',content:'Explain the compositor'},{role:'assistant',content:'Response'}];
 const result=appendReply(chats,1,messages);
 assert.equal(result[0].title,'Explain the compositor');
 assert.deepEqual(result[0].messages,messages);
 assert.equal(result[1],chats[1]);
 assert.deepEqual(chats[0].messages,[]);
});

test('import remaps conflicting IDs without overwriting existing messages',async()=>{
 const {importChats}=await import('../src/conversations.ts');
 const existing=[{id:4,title:'Original',project:'General',messages:[{role:'user',content:'keep'}]}];
 const incoming=[{id:4,title:'Imported',project:'Research',messages:[]},{id:4,title:'Second',project:'General',messages:[]}];
 const merged=importChats(existing,incoming);
 assert.equal(new Set(merged.map(c=>c.id)).size,3);
 assert.equal(merged[2],existing[0]);
});
test('legacy history preserves roles and does not turn summaries into fake messages',async()=>{
 const {convertLegacy}=await import('../src/conversations.ts');
 const chats=convertLegacy({conversations:[{title:'Old chat',summary:'Summary',messages:['User: Question','AI: Answer','unclassified log']}]});
 assert.deepEqual(chats[0].messages,[{role:'user',content:'Question'},{role:'assistant',content:'Answer'}]);
});
test('project source restrictions survive backup round trip',async()=>{
 const {decodeState}=await import('../src/state.ts');
 const state=decodeState({chats:[],projects:[{name:'General',sources:null},{name:'FocalDesk',sources:['/docs/current.md']}],settings:{temperature:.5,max_tokens:4096}});
 assert.deepEqual(state.projects[1].sources,['/docs/current.md']);
 assert.equal(state.settings.max_tokens,4096);
 assert.deepEqual(decodeState(JSON.parse(JSON.stringify(state))),state);
});
test('invalid imported workspace definitions cannot silently remove source restrictions',async()=>{
 const {decodeState}=await import('../src/state.ts');
 assert.throws(()=>decodeState({chats:[],projects:[{name:'FocalDesk',sources:'all'}]}));
});
