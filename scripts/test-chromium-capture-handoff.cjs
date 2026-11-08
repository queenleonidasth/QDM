'use strict';
const fs=require('fs'),vm=require('vm'),assert=require('node:assert/strict');
const source=fs.readFileSync('C:/Users/QUEEN/Projects/QDM/extensions/chromium/background.js','utf8');
const token='a'.repeat(64);
const delay=ms=>new Promise(resolve=>setTimeout(resolve,ms));
async function run(options={}){
 const events=[]; const state={autoCapture:options.enabled!==false,port:17876,token,forwardSession:false};
 let listener,createdListener,lastMessage='';
 const register=() => ({addListener(fn){this.fn=fn}});
 const api={
  storage:{local:{get:async()=>{if(options.settingsFail)throw Error('settings failed');return {...state}},set:async data=>{Object.assign(state,data);if(data.lastMessage)lastMessage=data.lastMessage}}},
  downloads:{
   onDeterminingFilename:{addListener(fn){listener=fn}},onCreated:{addListener(fn){createdListener=fn}},
   pause:async()=>{events.push('pause');if(options.pauseFail)throw Error('pause not possible')},
   resume:async()=>{events.push('resume')},
   cancel:async()=>{events.push('cancel');if(options.cancelFail)throw Error('cancel denied')},
   erase:async()=>{events.push('erase')},
  },
  runtime:{onInstalled:register(),onStartup:register(),onMessage:register(),sendNativeMessage:async()=>({ok:false})},
  contextMenus:{onClicked:register(),removeAll:async()=>{},create:()=>{}},
  action:{setBadgeText:async()=>{},setBadgeBackgroundColor:async()=>{}},
  tabs:{query:async()=>[]}
 };
 const context={chrome:api,importScripts:()=>{},URL,AbortSignal,setTimeout,clearTimeout,console,fetch:async(url,req)=>{
  if(url.includes('/health'))return {ok:true,status:200,json:async()=>({name:'QDM'})};
  if(url.endsWith('/downloads')){events.push('handoff');if(options.handoffFail)throw Error('unreachable');return {ok:true,status:200,json:async()=>({accepted:1,ids:['id42']})};}
  if(url.includes('/task-status')){events.push('confirmed-status');return {ok:true,status:200,json:async()=>({status:options.taskError?'error':'completed',downloadedBytes:100})};}
  throw Error('unexpected fetch '+url);
 }};
 vm.createContext(context);vm.runInContext(source,context);
 assert.equal(typeof listener,'function');assert.equal(createdListener,undefined,'onCreated is no longer primary');
 const item={id:42,state:'in_progress',url:'https://cdn.example/test.zip',finalUrl:'https://cdn.example/test.zip',mime:options.mime||'application/zip',filename:'test.zip',...(options.item||{})};
 let suggestCount=0;
 let finished;
 const done=new Promise((resolve,reject)=>{finished=resolve;setTimeout(()=>reject(Error('suggest never called')),2200).unref()});
 const result=listener(item,()=>{suggestCount++;events.push('suggest');finished()});
 if(result!==true)assert.equal(suggestCount,1);
 await done;
 assert.equal(suggestCount,1);
 return {events,lastMessage,result};
}
(async()=>{
 for(const [name,opts,expected] of [
  ['success',{},['pause','handoff','confirmed-status','cancel','erase','suggest']],
  ['paused-denied',{pauseFail:true},['pause','handoff','confirmed-status','cancel','erase','suggest']],
  ['no-QDM',{handoffFail:true},['pause','handoff','resume','suggest']],
  ['QDM error',{taskError:true},['pause','handoff','confirmed-status','resume','suggest']],
  ['browser cancel fails',{cancelFail:true},['pause','handoff','confirmed-status','cancel','resume','suggest']],
  ['disabled',{enabled:false},['suggest']],
  ['HTML MIME',{mime:'text/html'},['suggest']],
  ['invalid HTTP',{item:{url:'ftp://invalid/test.zip',finalUrl:'ftp://invalid/test.zip'}},['suggest']],
  ['settings exception',{settingsFail:true},['suggest']]
 ]) {
  const {events}=await run(opts);assert.deepEqual(events,expected,name);console.log('PASS',name);
 }
 console.log('PASS filename completion gate: suggest exactly once on every tested branch');
})().catch(e=>{console.error(e);process.exit(1)});
