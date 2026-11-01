#!/usr/bin/env node
'use strict';
const http=require('node:http');
const fs=require('node:fs/promises');
const os=require('node:os');
const path=require('node:path');
const crypto=require('node:crypto');
const assert=require('node:assert/strict');
const {CliClient,fileHash}=require('./helpers/cli-client.cjs');
const BINARY=path.resolve(__dirname,'../target/release/QDM.exe');
const DATA=Buffer.alloc(12*1024*1024+71);
for(let i=0;i<DATA.length;i++)DATA[i]=(i*29+(i>>10))&255;
const EXPECTED=crypto.createHash('sha256').update(DATA).digest('hex');
let active=0,peak=0,rejected=0,accepted=0;
const server=http.createServer((req,res)=>{
 const range=req.headers.range||'';
 const match=/^bytes=(\d+)-(\d+)$/.exec(range);
 if(!match){res.writeHead(400);res.end();return;}
 const begin=Number(match[1]),end=Math.min(Number(match[2]),DATA.length-1);
 if(begin>=DATA.length||end<begin){res.writeHead(416);res.end();return;}
 const probe=begin===0&&end===0;
 if(!probe&&active>=4){rejected++;res.writeHead(429,{'Retry-After':'0','Content-Length':'0'});res.end();return;}
 const chunk=DATA.subarray(begin,end+1);
 if(!probe){active++;accepted++;peak=Math.max(peak,active);}
 res.writeHead(206,{
   'Content-Type':'application/octet-stream',
   'Accept-Ranges':'bytes',
   'ETag':'"qdm-429-adaptive-test"',
   'Content-Range':'bytes '+begin+'-'+end+'/'+DATA.length,
   'Content-Length':String(chunk.length)
 });
 if(probe){res.end(chunk);return;}
 let off=0;
 const timer=setInterval(()=>{
  if(res.destroyed){clearInterval(timer);return;}
  if(off>=chunk.length){clearInterval(timer);res.end();return;}
  const part=chunk.subarray(off,Math.min(off+16384,chunk.length));off+=part.length;
  res.write(part);
 },5);
 res.on('close',()=>{clearInterval(timer);active=Math.max(0,active-1);});
});
(async()=>{
 const root=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-429-resume-'));
 await fs.mkdir(path.join(root,'downloads'),{recursive:true});
 const client=new CliClient(BINARY,path.join(root,'state'),path.join(root,'downloads'));
 try{
  await new Promise(r=>server.listen(0,'127.0.0.1',r));
  const url='http://127.0.0.1:'+server.address().port+'/adaptive.bin';
  await client.settings({connections:32,retries:1});
  const id=await client.add({url,filename:'adaptive.bin',expected_sha256:EXPECTED});
  const task=await client.terminal(id,180000);
  assert.equal(task.status,'completed',task.error||task.notice||'unknown failure');
  assert.equal(await fileHash(task.file_path),EXPECTED);
  assert.ok(rejected>0,'fixture did not return 429');
  assert.ok(accepted>1,'fixture did not support ranged transfers');
  console.log(JSON.stringify({result:'PASS',http429:rejected,accepted,peak,bytes:DATA.length,sha256:EXPECTED,downloadId:id,completed:task.status},null,2));
 } finally {
  try{await client.shutdown();}catch(_) {await client.crash();}
  await new Promise(r=>server.close(r));
  await fs.rm(root,{recursive:true,force:true});
 }
})().catch(e=>{console.error(e.stack||e);process.exitCode=1;server.close();});
