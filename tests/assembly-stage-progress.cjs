#!/usr/bin/env node
'use strict';
const assert=require('node:assert/strict');
const fs=require('node:fs/promises');
const os=require('node:os');
const path=require('node:path');
const {CliClient,delay,waitFor,fileHash}=require('./helpers/cli-client.cjs');
const {createHttpFixture,MiB}=require('./helpers/http-fixture.cjs');

(async()=>{
 const root=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-assembly-visible-'));
 const dir=path.join(root,'files'),data=path.join(root,'state');
 await fs.mkdir(dir,{recursive:true});
 const fixture=await createHttpFixture({sizeBytes:48*MiB+17,delayMs:1});
 const bin=path.resolve(__dirname,'../target/release/QDM.exe');
 process.env.QDM_TEST_ASSEMBLY_DELAY_MS='25';
 const client=new CliClient(bin,data,dir);
 const observed=[];
 try{
  await client.settings({connections:32,max_concurrent:1});
  const id=await client.add({url:fixture.url,filename:'merge-progress.bin',expected_sha256:fixture.hash()});
  const end=Date.now()+90000;
  let completed=null;
  while(Date.now()<end){
   const task=await client.task(id);
   assert.ok(task,'Task missing during merge');
   assert.ok(task.lanes.length<=64,'More than 64 connections exposed');
   assert.ok(task.lanes.every((lane,index)=>lane.id===index+1),'Connection slots are not stable');
   if(task.status==='assembling'){
      assert.equal(task.assembly_total_bytes,fixture.behavior.sizeBytes);
      assert.ok(task.assembly_bytes>=0&&task.assembly_bytes<=task.assembly_total_bytes);
      const last=observed.at(-1);
      if(last) assert.ok(task.assembly_bytes>=last.bytes,'Assembly bytes must be monotonic');
      if(!last || task.assembly_bytes!==last.bytes){
        observed.push({bytes:task.assembly_bytes,total:task.assembly_total_bytes,lanes:task.lanes.length});
      }
   }
   if(task.status==='completed'){completed=task;break;}
   if(task.status==='failed'||task.status==='error'){throw new Error(task.error||task.status);}
   await delay(12);
  }
  assert.ok(completed,'Download did not finish within timeout');
  assert.ok(observed.some(x=>x.bytes>0 && x.bytes<x.total),
    'Assembly phase did not report an intermediate physical-copy byte count');
  assert.equal(completed.assembly_bytes,completed.assembly_total_bytes);
  assert.equal(completed.assembly_bytes,fixture.behavior.sizeBytes);
  assert.equal(await fileHash(completed.file_path),fixture.hash());
  const summary={
    result:'PASS',
    assemblyObservedStages:observed.length,
    firstIntermediate:observed.find(x=>x.bytes>0&&x.bytes<x.total),
    lastStage:observed.at(-1),
    segments:completed.segments.length,
    connectionSlots:completed.lanes.length,
    totalBytes:completed.assembly_bytes,
    hashVerified:true
  };
  console.log(JSON.stringify(summary,null,2));
 }finally{
  try{await client.shutdown()}catch(_){await client.crash()}
  await fixture.close();
  await fs.rm(root,{recursive:true,force:true,maxRetries:8,retryDelay:100});
 }
})().catch(error=>{console.error('FAIL '+(error.stack||error));process.exitCode=1;});
