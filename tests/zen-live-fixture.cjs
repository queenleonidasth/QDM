#!/usr/bin/env node
'use strict';
// Temporary local HTTP test origin for QDM's Zen click-to-download integration.
const http = require('node:http'), crypto = require('node:crypto');
const size = 4 * 1024 * 1024 + 79;
const data = Buffer.allocUnsafe(size);
for (let i = 0; i < size; i++) data[i] = (i * 29 + (i >>> 9) * 17) & 255;
const sha256 = crypto.createHash('sha256').update(data).digest('hex');
const requests = [];
const port = 19731;
function sendRange(req, res) {
  const m = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || '');
  let lo = 0, hi = size - 1;
  if (m) { lo = Number(m[1]); hi = m[2] ? Math.min(size-1,Number(m[2])) : size - 1; }
  if (lo >= size || hi < lo) { res.writeHead(416, {'Content-Range': 'bytes */'+size});res.end();return; }
  const sliced = m !== null;
  const payload = data.subarray(lo,hi+1);
  requests.push({path:req.url,method:req.method,range:req.headers.range || '',bytes:payload.length,at:Date.now()});
  res.writeHead(sliced ? 206 : 200,{
    'Content-Type':'application/octet-stream',
    'Content-Disposition':'attachment; filename="QDM-Zen-live-click-test.bin"',
    'Cache-Control':'no-store',
    'Accept-Ranges':'bytes',
    'Content-Length':String(payload.length),
    'ETag':'"zen-test-4mb-v1"',
    ...(sliced ? {'Content-Range':'bytes '+lo+'-'+hi+'/'+size} : {}),
  });
  if (req.method === 'HEAD') {res.end();return;}
  let pos=0;
  const chunk=16384;
  const timer=setInterval(()=>{
    if(res.destroyed){clearInterval(timer);return;}
    if(pos>=payload.length){clearInterval(timer);res.end();return;}
    res.write(payload.subarray(pos,Math.min(pos+chunk,payload.length)));
    pos+=chunk;
  },15);
  res.on('close',()=>clearInterval(timer));
}
http.createServer((req,res)=>{
  if(req.url==='/' || req.url==='/index.html'){
    res.writeHead(200, {'Content-Type':'text/html; charset=utf-8','Cache-Control':'no-store'});
    res.end('<!doctype html><html><head><title>QDM Zen integration acceptance</title></head><body><h1>QDM Zen integration test</h1><p>Local synthetic binary: 4 MiB</p><a id="test-download" href="/download.bin">Download QDM test file</a></body></html>');
  }else if(req.url==='/download.bin')sendRange(req,res);
  else if(req.url==='/metrics'){res.writeHead(200, {'Content-Type':'application/json'});res.end(JSON.stringify({size,sha256,requests},null,2));}
  else {res.writeHead(404);res.end('not found');}
}).listen(port,'127.0.0.1',()=>console.log('QDM_ZEN_TEST_ORIGIN_READY port='+port+' sha256='+sha256));
