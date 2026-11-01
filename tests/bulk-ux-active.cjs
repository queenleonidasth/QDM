#!/usr/bin/env node
'use strict';
// Capture only a QDM HWND created by this script. No desktop recording, global
// input injection, foreground assumption, user profile, or production files.
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const readline = require('node:readline');
const {spawn} = require('node:child_process');
const {performance} = require('node:perf_hooks');
const {CliClient,waitFor,delay,fileHash} = require('./helpers/cli-client.cjs');
const {createHttpFixture,MiB,KiB} = require('./helpers/http-fixture.cjs');

function option(name,fallback) {
  const i=process.argv.indexOf(name);
  if(i<0)return fallback;
  if(!process.argv[i+1]||process.argv[i+1].startsWith('--'))throw new Error(`${name} requires a value.`);
  return process.argv[i+1];
}
const binary=path.resolve(option('--binary','target/release/QDM.exe'));
const ffmpeg=path.resolve(option('--ffmpeg','C:\\Users\\QUEEN\\AppData\\Local\\Microsoft\\WinGet\\Links\\ffmpeg.exe'));
const backend=option('--backend','winit-femtovg');
const output=path.resolve(option('--output-dir',`docs/evidence/slint-motion-${new Date().toISOString().replace(/[:.]/g,'-')}`));
const durationSeconds=16;

function bounded(promise,milliseconds,message) {
  let timer;
  return Promise.race([promise,new Promise((_,reject)=>{timer=setTimeout(()=>reject(new Error(message)),milliseconds);})]).finally(()=>clearTimeout(timer));
}
function launch(file,args,options={}) {
  const child=spawn(file,args,{windowsHide:true,stdio:['pipe','pipe','pipe'],...options});
  let stdout='',stderr='';
  child.stdout.on('data',b=>{stdout=(stdout+b.toString('utf8')).slice(-100000);});
  child.stderr.on('data',b=>{stderr=(stderr+b.toString('utf8')).slice(-100000);});
  const exited=new Promise(resolve=>{child.once('error',error=>resolve({code:-1,error:String(error)}));child.once('exit',(code,signal)=>resolve({code,signal}));});
  return {child,exited,get stdout(){return stdout;},get stderr(){return stderr;}};
}
class UiaClient {
  constructor(processId) {
    this.process=launch('powershell.exe',['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',path.join(__dirname,'helpers','bulk-ux-uia.ps1'),'-TargetProcessId',String(processId)]);
    this.pending=new Map();this.sequence=0;
    readline.createInterface({input:this.process.child.stdout}).on('line',line=>{
      let reply;try{reply=JSON.parse(line.replace(/^\uFEFF/,''));}catch{return;}
      const request=this.pending.get(reply.id);if(!request)return;
      clearTimeout(request.timer);this.pending.delete(reply.id);
      if(reply.ok)request.resolve(reply);else request.reject(new Error(reply.error||'UIA action failed.'));
    });
    this.process.exited.then(result=>{for(const request of this.pending.values()){clearTimeout(request.timer);request.reject(new Error(`Owned UIA helper exited: ${JSON.stringify(result)} ${this.process.stderr}`));}this.pending.clear();});
    this.process.child.stdin.on('error',()=>{});
  }
  command(action,fields={},timeoutMs=8000) {
    const id=++this.sequence;
    return new Promise((resolve,reject)=>{
      const timer=setTimeout(()=>{this.pending.delete(id);reject(new Error(`UIA ${action} timed out. ${this.process.stderr}`));},timeoutMs);
      this.pending.set(id,{resolve,reject,timer});
      this.process.child.stdin.write(JSON.stringify({id,action,...fields})+'\n');
    });
  }
  async close() {
    this.process.child.stdin.end();
    try{await bounded(this.process.exited,3000,'Owned UIA helper did not stop.');}
    catch{this.process.child.kill();await bounded(this.process.exited,3000,'Owned UIA helper did not terminate.');}
  }
}

async function main(){const ws=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-active-'));const data=path.join(ws,'data'),dir=path.join(ws,'files');await fs.mkdir(dir,{recursive:true});let fixture,client,gui,uia;let report={workspace:ws};try{
 fixture=await createHttpFixture({sizeBytes:96*MiB,delayMs:100,chunkSize:8*KiB});client=new CliClient(binary,data,dir);await client.settings({close_to_tray:false,connections:1});let id=await client.add({url:fixture.url,filename:'active-demo.bin',start:false});await client.shutdown();client=null;
 gui=launch(binary,['--data-dir',data,'--download-dir',dir],{windowsHide:false});uia=new UiaClient(gui.child.pid);await uia.command('window',{},17000);await delay(500);await uia.command('invoke',{name:'Resume all'});
 await waitFor(async()=>{try{let s=JSON.parse(await fs.readFile(path.join(data,'state-v2.json'),'utf8'));let t=s.tasks.find(t=>t.view.id===id).view;return t.status==='downloading'&&t.downloaded_bytes>0;}catch{return false;}},'active',20000,100);
 await delay(600);report.active=await uia.command('inventory');let cap=launch('powershell.exe',['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',path.join(__dirname,'capture-window.ps1'),'-TargetProcessId',String(gui.child.pid),'-OutputPath',path.join(output,'native-active.png')]);await cap.exited;
 if(!report.active.result.some(x=>x.name==='TRANSFER RATE'))throw Error('Transfer rate absent during download');
 await uia.command('invoke',{name:'Pause all'});await delay(1000);report.paused=await uia.command('inventory');if(report.paused.result.some(x=>x.name==='TRANSFER RATE'))throw Error('Transfer rate remains after pause');
 report.status='passed';await uia.command('close');await bounded(gui.exited,10000,'exit');gui=null;
}catch(e){report.status='failed';report.error=String(e.stack||e);process.exitCode=1;}finally{if(uia)await uia.close().catch(()=>{});if(gui)gui.child.kill();if(client)await client.shutdown().catch(()=>{});if(fixture)await fixture.close().catch(()=>{});await fs.writeFile(path.join(output,'active-test.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({status:report.status,error:report.error}));}}
main();