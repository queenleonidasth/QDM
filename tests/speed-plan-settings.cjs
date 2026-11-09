'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { CliClient } = require('./helpers/cli-client.cjs');
async function main(){
 const binary=path.resolve(process.argv[2]||'target/release/QDM.exe');
 const root=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-speed-settings-'));
 const state=path.join(root,'state'); const downloads=path.join(root,'downloads');
 let client;
 try{
  client=new CliClient(binary,state,downloads);
  const initial=(await client.snapshot()).settings;
  assert.equal(initial.connections,8);
  assert.equal(initial.shed_slow,true);
  await client.settings({connections:32,shed_slow:false});
  const changed=(await client.snapshot()).settings;
  assert.equal(changed.connections,32);
  assert.equal(changed.shed_slow,false);
  await client.shutdown();
  client=new CliClient(binary,state,downloads);
  const restored=(await client.snapshot()).settings;
  assert.equal(restored.connections,32,'legacy nondefault connection setting must persist');
  assert.equal(restored.shed_slow,false,'shed slow OFF must persist');
  await client.settings({connections:64,shed_slow:true});
  const enabled=(await client.snapshot()).settings;
  assert.equal(enabled.connections,64,'64 connections remain configurable');
  assert.equal(enabled.shed_slow,true,'shed slow can be turned back ON');
  await client.shutdown();
  client=null;
  console.log('PASS new default=8, shed=true; OFF persisted; legacy 32 preserved; 64 accepted; ON restored');
 } finally {
  if(client)try{await client.shutdown()}catch(_){try{await client.crash()}catch(_){}}
  await fs.rm(root,{recursive:true,force:true,maxRetries:5,retryDelay:200});
 }
}
main().catch(e=>{console.error(e.stack||e);process.exitCode=1});
