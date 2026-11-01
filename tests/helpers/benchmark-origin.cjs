'use strict';
const http = require('node:http');
const crypto = require('node:crypto');
const { payload } = require('./http-fixture.cjs');

async function createBenchmarkOrigin(bytes) {
  const data = payload(bytes, 27);
  const expectedSha256 = crypto.createHash('sha256').update(data).digest('hex');
  const runs = new Map(), sockets = new Set(), timers = new Set();
  let nextSocket = 0, closing = false;
  const server = http.createServer((req, res) => {
    const key = new URL(req.url, 'http://127.0.0.1').searchParams.get('run');
    const run = runs.get(key);
    if (!run) {res.writeHead(404);res.end();return;}
    const range = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || '');
    let start = range ? Number(range[1]) : 0;
    let end = range && range[2] ? Math.min(Number(range[2]), bytes-1) : bytes-1;
    if(start > end || start >= bytes){res.writeHead(416,{'Content-Range':`bytes */${bytes}`});res.end();return;}
    const request={id:run.requests.length+1,method:req.method,range:req.headers.range||null,
      start,end,socketId:req.socket.benchmarkId,at:Date.now(),firstSendAt:null,lastSendAt:null,
      endedAt:null,sentBytes:0,aborted:false,status:range?206:200};
    run.requests.push(request);run.firstRequestAt ??= request.at;
    const headers={'Content-Type':'application/octet-stream','Content-Length':end-start+1,
      'Accept-Ranges':'bytes','ETag':'"qdm-benchmark-v1"','Cache-Control':'no-store',
      'Last-Modified':'Mon, 26 Oct 2026 08:00:00 GMT'};
    if(range)headers['Content-Range']=`bytes ${start}-${end}/${bytes}`;
    res.writeHead(request.status,headers);
    if(req.method==='HEAD'){res.end();request.endedAt=Date.now();return;}
    const large=end-start+1>64*1024;
    if(large){run.active.set(request.id,request.socketId);run.peakBodyRequests=Math.max(run.peakBodyRequests,run.active.size);run.peakBodySockets=Math.max(run.peakBodySockets,new Set(run.active.values()).size);}
    let position=start,timer,finished=false;
    const finish=(aborted)=>{if(request.endedAt!==null)return;request.endedAt=Date.now();request.aborted=aborted;run.active.delete(request.id);if(timer){clearTimeout(timer);timers.delete(timer);}};
    res.once('finish',()=>{finished=true;finish(false);});res.once('close',()=>finish(!finished));
    const send=()=>{
      if(timer)timers.delete(timer);
      if(closing||res.destroyed||res.writableEnded)return;
      const next=Math.min(position+(run.delayMs&&large?16*1024:256*1024),end+1);
      const now=Date.now();request.firstSendAt ??= now;request.lastSendAt=now;run.firstSendAt ??=now;run.lastSendAt=now;
      const chunk=data.subarray(position,next);request.sentBytes+=chunk.length;run.sentBytes+=chunk.length;
      const okay=res.write(chunk);position=next;
      if(position>end){res.end();return;}
      const again=()=>{if(run.delayMs&&large){timer=setTimeout(send,run.delayMs);timers.add(timer);}else setImmediate(send);};
      if(okay)again();else res.once('drain',again);
    };
    send();
  });
  server.on('connection',socket=>{socket.benchmarkId=++nextSocket;sockets.add(socket);socket.once('close',()=>sockets.delete(socket));});
  server.on('clientError',(_error,socket)=>socket.destroy());
  await new Promise((resolve,reject)=>{server.once('error',reject);server.listen(0,'127.0.0.1',resolve);});
  return {
    bytes,expectedSha256,
    createRun(id,delayMs){const run={id,delayMs,firstRequestAt:null,firstSendAt:null,lastSendAt:null,sentBytes:0,peakBodyRequests:0,peakBodySockets:0,active:new Map(),requests:[]};runs.set(id,run);return {url:`http://127.0.0.1:${server.address().port}/benchmark.bin?run=${id}`,run};},
    snapshot(run){const intervals=run.requests.filter(r=>r.sentBytes>0).map(r=>[r.start,r.start+r.sentBytes]).sort((a,b)=>a[0]-b[0]);let covered=0,last=0;for(const [start,end]of intervals){if(end>last){covered+=end-Math.max(last,start);last=end;}}
      return{requestedBodyChunkBytes:run.delayMs?16*1024:256*1024,requestedInterChunkDelayMs:run.delayMs,firstRequestAt:run.firstRequestAt,firstOriginSendAt:run.firstSendAt,lastOriginSendAt:run.lastSendAt,originBytesSent:run.sentBytes,uniqueByteCoverage:covered,peakBodyRequests:run.peakBodyRequests,peakBodyTcpSockets:run.peakBodySockets,activeBodyRequestsAtFinish:run.active.size,requests:run.requests};},
    async close(){closing=true;for(const timer of timers)clearTimeout(timer);for(const socket of sockets)socket.destroy();await new Promise(resolve=>server.close(resolve));}
  };
}
module.exports={createBenchmarkOrigin};
