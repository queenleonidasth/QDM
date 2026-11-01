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

async function main() {
 await fs.mkdir(output,{recursive:true}); const workspace=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-bulk-ux-'));
 const dataDir=path.join(workspace,'state'),downloadDir=path.join(workspace,'downloads'); await fs.mkdir(downloadDir,{recursive:true});
 const report={workspace, binary, binarySha256:await fileHash(binary), steps:[]}; let fixture,client,gui,uia;
 const step=async(name,fn)=>{const value=await fn(); report.steps.push({name,value}); return value;};
 try {
   fixture=await createHttpFixture({sizeBytes:1024,delayMs:1,chunkSize:1024}); client=new CliClient(binary,dataDir,downloadDir);
   await client.settings({close_to_tray:false});
   for(const filename of ['bulk-one.txt','bulk-two.txt','bulk-missing.txt']) { const id=await client.add({url:fixture.url,filename}); const task=await client.terminal(id,30000); if(task.status!=='completed')throw Error('Fixture failed'); }
   await client.shutdown(); client=null; await fixture.close(); fixture=null;
   await fs.unlink(path.join(downloadDir,'bulk-missing.txt'));
   gui=launch(binary,['--data-dir',dataDir,'--download-dir',downloadDir],{windowsHide:false}); uia=new UiaClient(gui.child.pid);
   await uia.command('window',{},17000); await uia.command('place_within_work_area'); await delay(1000);
   const capture=async name=>{const target=path.join(output,name+'.png');const proc=launch('powershell.exe',['-NoProfile','-NonInteractive','-ExecutionPolicy','Bypass','-File',path.join(__dirname,'capture-window.ps1'),'-TargetProcessId',String(gui.child.pid),'-OutputPath',target]);const result=await proc.exited;if(result.code!==0)throw Error(proc.stderr);return target;};
   report.initial=await uia.command('inventory'); await step('initial screenshot',()=>capture('native-initial'));
   await delay(1600); await step('idle phase screenshot',()=>capture('native-idle-phase')); await step('select all',()=>uia.command('invoke',{name:'Select all'})); await delay(300); const all=await uia.command('inventory'); if(!all.result.some(x=>x.name==='3 selected')) throw Error('Select all missed eligible rows'); await step('clear all',()=>uia.command('invoke',{name:'Clear selection'})); await step('select first checkbox',()=>uia.command('toggle',{name:'Select bulk-one.txt'}));
   await step('select second checkbox',()=>uia.command('toggle',{name:'Select bulk-two.txt'}));
   await delay(400); report.selected=await uia.command('inventory'); await step('selected screenshot',()=>capture('native-selected'));
   if(!report.selected.result.some(x=>x.name==='2 selected')) throw Error('Selected count not found');
   await step('open confirmation',()=>uia.command('invoke',{name:'Remove from list'})); await delay(300); report.confirmation=await uia.command('inventory'); await step('confirmation screenshot',()=>capture('native-confirmation'));
   if(!report.confirmation.result.some(x=>x.name==='Remove download')) throw Error('Confirmation action not found');
   await step('cancel confirmation',()=>uia.command('invoke',{name:'Cancel'})); await delay(300);
   await step('open file',()=>uia.command('invoke',{name:'Open file'})); await delay(1200); report.openInventory=await uia.command('inventory');
   await step('reopen confirmation',()=>uia.command('invoke',{name:'Remove from list'})); await step('confirm removal',()=>uia.command('invoke',{name:'Remove download'})); await delay(1500);
   await fs.access(path.join(downloadDir,'bulk-one.txt')); await fs.access(path.join(downloadDir,'bulk-two.txt'));
   report.after=await uia.command('inventory'); report.filesPreserved=true; await step('after removal screenshot',()=>capture('native-after'));
   await uia.command('close'); await bounded(gui.exited,10000,'GUI did not exit'); gui=null;
   const state=JSON.parse(await fs.readFile(path.join(dataDir,'state-v2.json'),'utf8')); report.remaining=state.tasks.map(t=>t.view.filename);
   if(report.remaining.includes('bulk-one.txt')||report.remaining.includes('bulk-two.txt')) throw Error('Selected entries were not removed');
   report.status='passed';
 } catch(e) {report.status='failed';report.error=String(e.stack||e);process.exitCode=1;}
 finally {if(uia)await uia.close().catch(()=>{});if(gui)gui.child.kill();if(client)await client.shutdown().catch(()=>{});if(fixture)await fixture.close().catch(()=>{});await fs.writeFile(path.join(output,'bulk-test.json'),JSON.stringify(report,null,2));console.log(JSON.stringify({status:report.status,error:report.error,workspace}));}
}
main().catch(e=>{console.error(e);process.exitCode=1;});
