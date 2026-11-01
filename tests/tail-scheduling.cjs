#!/usr/bin/env node
'use strict';
// Independent origin proof: a waiting response is not a productive split
// source, and a new request can cost more than finishing a fast short tail.
const assert=require('node:assert/strict');
const http=require('node:http');
const fs=require('node:fs/promises');
const path=require('node:path');
const os=require('node:os');
const {createHash}=require('node:crypto');
const {CliClient,fileHash}=require('./helpers/cli-client.cjs');
const {payload,KiB,MiB}=require('./helpers/http-fixture.cjs');
const option=(name,fallback)=>{const index=process.argv.indexOf(name);return index<0?fallback:process.argv[index+1];};
const binary=path.resolve(option('--binary','target/release/QDM.exe'));
const reportPath=path.resolve(option('--report','docs/evidence/slint-idm-20261027/tail-scheduling.json'));
const tracing=process.argv.includes('--trace');

async function runCase(mode){
  const data=payload((mode==='setup-cost'?64:16)*MiB+137,57);
  const expected=createHash('sha256').update(data).digest('hex');
  const root=await fs.mkdtemp(path.join(os.tmpdir(),'qdm-tail-scheduling-'));
  const result={name:mode,status:'running',requests:[],zeroPayloadSubdivisions:[],workdir:root};
  const timers=new Set(),sockets=new Set();let client,firstPayloadRequest=null;
  const schedule=(fn,delay)=>{const timer=setTimeout(()=>{timers.delete(timer);fn();},delay);timers.add(timer);return timer;};
  const server=http.createServer((request,response)=>{
    const match=/^bytes=(\d+)-(\d+)$/.exec(request.headers.range||'');
    if(!match){response.writeHead(400);response.end();return;}
    const start=Number(match[1]),end=Number(match[2]),length=end-start+1;
    if(start>end||end>=data.length){response.writeHead(416);response.end();return;}
    const record={id:result.requests.length+1,start,end,length,at:Date.now(),headersAt:null,firstBodyAt:null,sent:0,closedAt:null};
    if(length>64*KiB){
      firstPayloadRequest??=record;
      for(const prior of result.requests){
        if(prior.length>128*KiB&&prior.sent===0&&prior.closedAt===null&&
          start>prior.start+64*KiB&&end<=prior.end){
          result.zeroPayloadSubdivisions.push({parent:prior.id,child:record.id,at:record.at,
            parentHeadersSent:prior.headersAt!==null,parentBytesSent:prior.sent});
        }
      }
    }
    result.requests.push(record);
    response.once('close',()=>{record.closedAt??=Date.now();});
    response.once('finish',()=>{record.closedAt??=Date.now();});
    const sendHeaders=()=>{
      if(response.destroyed)return;
      record.headersAt=Date.now();
      response.writeHead(206,{'Content-Type':'application/octet-stream','Content-Length':length,
        'Content-Range':`bytes ${start}-${end}/${data.length}`,'ETag':'"tail-scheduling-v57"','Accept-Ranges':'bytes'});
      response.flushHeaders();
    };
    const sendBody=()=>{
      let position=start;
      const next=()=>{
        if(response.destroyed||response.writableEnded)return;
        const until=Math.min(position+(mode==='setup-cost'?512*KiB:256*KiB),end+1);
        record.firstBodyAt??=Date.now();record.sent+=until-position;
        const accepted=response.write(data.subarray(position,until));position=until;
        if(position>end){response.end();return;}
        const again=()=>{if(mode==='setup-cost')schedule(next,8);else setImmediate(next);};
        if(accepted)again();else response.once('drain',again);
      };next();
    };
    if(length<=64*KiB){sendHeaders();sendBody();return;}
    if(mode==='setup-cost'){
      // Known setup cost plus a fast body makes speculative small tails an
      // observable regression, rather than depending on a public CDN's load.
      schedule(()=>{sendHeaders();sendBody();},250);return;
    }
    if(record===firstPayloadRequest){
      if(mode==='waiting-headers')schedule(()=>{sendHeaders();sendBody();},900);
      else {sendHeaders();schedule(sendBody,900);}
      return;
    }
    sendHeaders();sendBody();
  });
  server.on('connection',socket=>{sockets.add(socket);socket.once('close',()=>sockets.delete(socket));});
  server.on('clientError',(_error,socket)=>socket.destroy());
  const previousTrace=process.env.QDM_TRANSFER_TRACE;
  const traceChunks=[];
  try{
    await new Promise(resolve=>server.listen(0,'127.0.0.1',resolve));
    if(tracing)process.env.QDM_TRANSFER_TRACE='1';
    client=new CliClient(binary,path.join(root,'state'),path.join(root,'files'));
    if(tracing)client.child.stderr.on('data',bytes=>traceChunks.push(bytes));
    await client.settings({connections:4,max_concurrent:1,retries:2});
    const started=Date.now();
    const id=await client.add({url:`http://127.0.0.1:${server.address().port}/tail.bin`,
      filename:'tail.bin',expected_sha256:expected});
    const completed=await client.terminal(id,20000);
    result.elapsedMs=Date.now()-started;
    assert.equal(completed.status,'completed',completed.error||completed.notice);
    result.actualSha256=await fileHash(completed.file_path);
    assert.equal(result.actualSha256,expected);
    result.hashVerified=true;result.dynamicSplits=completed.dynamic_splits;
    result.peakConnections=completed.peak_connections;result.finalParts=completed.part_count;
    result.bodyRequests=result.requests.filter(row=>row.length>64*KiB).length;
    result.requestedBodyBytes=result.requests.filter(row=>row.length>64*KiB).reduce((sum,row)=>sum+row.length,0);
    if(mode==='setup-cost'){
      assert.equal(result.bodyRequests,2,'A fast remaining body was subdivided into extra latency-dominated requests');
      assert.equal(completed.dynamic_splits,0,'The known setup cost should keep the two productive initial responses');
    }else{
      assert.equal(result.zeroPayloadSubdivisions.length,0,'A zero-payload response was subdivided while still waiting');
    }
    result.status='passed';
  }catch(error){result.status='failed';result.error=error.stack||String(error);}
  finally{
    if(previousTrace===undefined)delete process.env.QDM_TRANSFER_TRACE;else process.env.QDM_TRANSFER_TRACE=previousTrace;
    if(client){try{await client.shutdown();}catch(error){await client.crash();result.status='failed';result.shutdownError=String(error);}}
    for(const timer of timers)clearTimeout(timer);
    for(const socket of sockets)socket.destroy();
    await new Promise(resolve=>server.close(resolve));
    if(tracing){
      result.traceFile=reportPath.replace(/\.json$/i,'')+`-${mode}.trace.jsonl`;
      await fs.writeFile(result.traceFile,Buffer.concat(traceChunks));
    }
    await fs.rm(root,{recursive:true,force:true,maxRetries:5,retryDelay:100});
    result.workdirRemoved=true;
  }
  return result;
}

(async()=>{
  await fs.mkdir(path.dirname(reportPath),{recursive:true});
  const report={schema:1,suite:'Productive response ownership and latency-aware tail scheduling',
    startedAt:new Date().toISOString(),binary,binarySha256:await fileHash(binary),status:'running',cases:[]};
  for(const mode of ['waiting-headers','waiting-first-body','setup-cost']){
    const result=await runCase(mode);report.cases.push(result);
    await fs.writeFile(reportPath,JSON.stringify(report,null,2)+'\n');
    console.log(`${result.status.toUpperCase()} ${mode}: ${result.elapsedMs}ms, ${result.bodyRequests} body requests, ${result.zeroPayloadSubdivisions.length} zero-payload subdivisions`);
  }
  report.binarySha256After=await fileHash(binary);
  report.passed=report.cases.filter(row=>row.status==='passed').length;
  report.failed=report.cases.length-report.passed;
  report.status=report.failed===0&&report.binarySha256After===report.binarySha256?'passed':'failed';
  report.finishedAt=new Date().toISOString();
  await fs.writeFile(reportPath,JSON.stringify(report,null,2)+'\n');
  console.log(JSON.stringify({status:report.status,passed:report.passed,failed:report.failed,report:reportPath}));
  if(report.status!=='passed')process.exitCode=1;
})().catch(error=>{console.error(error.stack||error);process.exitCode=1;});
