'use strict';
// Real CLI download benchmark against a local deterministic HTTP Range server.
// Streams zero-filled ranges without allocating the full 1 GiB in RAM.
const http = require('node:http');
const os = require('node:os');
const fs = require('node:fs/promises');
const path = require('node:path');
const crypto = require('node:crypto');
const assert = require('node:assert/strict');
const { CliClient, fileHash } = require('./helpers/cli-client.cjs');
const options = process.argv.slice(2);
const arg = (flag, value) => { const i = options.indexOf(flag); return i >= 0 ? options[i + 1] : value; };
const MiB = 1024 * 1024;
const mode = arg('--scenario', 'small');
const size = mode === '1gb' ? 1024 * MiB : mode === 'unstable' ? 64 * MiB + 37 : mode === 'shed' ? 128 * MiB + 19 : 192 * 1024 + 17;
const binary = path.resolve(arg('--binary', 'target/release/QDM.exe'));
const reportPath = arg('--report', '');
const zeros = Buffer.alloc(64 * 1024);
const expected = () => { const h = crypto.createHash('sha256'); for(let n = 0; n < size; n += zeros.length) h.update(zeros.subarray(0, Math.min(zeros.length, size - n)));return h.digest('hex'); };
async function benchmark() {
  const root = await fs.mkdtemp(path.join(os.tmpdir(),'qdm-speed-plan-bench-'));
  let client,server;
  const observations = { requests: 0, slowRequests: 0, aborted:0, sentBytes:0 };
  try {
    let firstBodyRange = null;
    server = http.createServer((req,res) => {
      if(!['GET','HEAD'].includes(req.method)){res.writeHead(405);res.end();return;}
      const match = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range||'');
      let start = match ? Number(match[1]) : 0;
      let end = match ? (match[2] ? Number(match[2]) : size - 1) : size - 1;
      if(!Number.isSafeInteger(start)||!Number.isSafeInteger(end)||start<0||end>=size||start>end){res.writeHead(416, {'Content-Range':`bytes */${size}`});res.end();return;}
      const range = match !== null;
      const length = end - start + 1;
      const headers = {'Content-Type':'application/octet-stream','Accept-Ranges':'bytes',ETag:'"qdm-speedbench-zero-v1"','Content-Length':String(length),'Content-Disposition':'attachment; filename="speedbench.bin"'};
      if(range)headers['Content-Range']=`bytes ${start}-${end}/${size}`;
      res.writeHead(range?206:200,headers);
      if(req.method==='HEAD'){res.end();return;}
      observations.requests++;
      const slow = (mode==='unstable' || mode==='shed') && length > 512*1024 && (firstBodyRange===null || firstBodyRange===start);
      if(slow){firstBodyRange=start;observations.slowRequests++;}
      let pos=start;
      let finished=false;
      res.on('finish',()=>{finished=true;});
      res.on('close',()=>{if(!finished)observations.aborted++;});
      const send=()=>{
        if(res.destroyed)return;
        if(pos>end){res.end();return;}
        const len=Math.min(zeros.length,end-pos+1);
        const accepted=res.write(zeros.subarray(0,len));
        observations.sentBytes+=len;pos+=len;
        const next=()=>{if(slow)setTimeout(send,mode==='shed'?80:28);else if(mode==='shed')setTimeout(send,3);else setImmediate(send);};
        if(!accepted)res.once('drain',next);else next();
      };
      send();
    });
    await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',resolve)});
    const port = server.address().port;
    const state = path.join(root,'state');const download = path.join(root,'downloads');
    await fs.mkdir(download,{recursive:true});
    if(mode === 'shed') process.env.QDM_TRANSFER_TRACE = '1';
    client = new CliClient(binary,state,download);
    await client.snapshot();
    await client.settings({connections:mode==='shed'?2:8,max_concurrent:1,speed_limit_kib:0,retries:3,shed_slow:true});
    let shedEvents=0, shedDebug=[];
    client.child.stderr.on('data',data=>{
      const text=data.toString();
      shedEvents+=(text.match(/\"event\":\"shed-slow\"/g)||[]).length;
      for(const line of text.split(/\r?\n/)){
        if(line.includes('\"event\":\"shed-eval\"') || line.includes('\"event\":\"shed-skip\"')){
          try { shedDebug.push(JSON.parse(line.slice(line.indexOf('{')))); } catch(_){}
          if(shedDebug.length>120)shedDebug.shift();
        }
      }
    });
    const begun=Date.now();
    const id=await client.add({url:`http://127.0.0.1:${port}/speedbench.bin`,filename:`benchmark-${mode}.bin`});
    const task=await client.terminal(id,360000);
    const elapsedMs=Date.now()-begun;
    assert.equal(task.status,'completed',task.error||task.status);
    assert.equal(await fileHash(task.file_path),expected(),'output SHA-256 mismatch');
    const report={scenario:mode,sizeBytes:size,binary,elapsedMs,
      throughputMiBps:+(size/MiB/(elapsedMs/1000)).toFixed(2),sha256:expected(),
      dynamicSplits:task.dynamic_splits,peakConnections:task.peak_connections,shedTraceEvents:shedEvents,
      http:observations,status:'pass',at:new Date().toISOString(),shedDebug:mode==='shed'?shedDebug:undefined};
    if(reportPath){await fs.mkdir(path.dirname(reportPath),{recursive:true});await fs.writeFile(reportPath,JSON.stringify(report,null,2));}
    console.log(JSON.stringify(report));
  } finally {
    if(client){try{await client.shutdown();}catch(_){try{await client.crash();}catch(_){}}}
    if(server)await new Promise(resolve=>{server.close(()=>resolve());server.closeAllConnections?.();});
    await fs.rm(root,{recursive:true,force:true,maxRetries:5,retryDelay:200});
  }
}
benchmark().catch(err=>{console.error(err.stack||err);process.exitCode=1});
