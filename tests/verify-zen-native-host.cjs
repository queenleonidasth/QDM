'use strict';
const assert = require('node:assert/strict');
const fs = require('node:fs');
const path = require('node:path');
const {spawn} = require('node:child_process');
const crypto = require('node:crypto');
const host = path.resolve(__dirname, '..', 'target', 'release', 'QDMHost.exe');
const tokenFile = path.join(process.env.APPDATA, 'QDM', 'browser-pairing-v2.bin');
const hash = b => crypto.createHash('sha256').update(b).digest('hex');

function request(extensionId, body) {
  return new Promise((resolve, reject) => {
    const child = spawn(host, [extensionId], {windowsHide:true, stdio:['pipe','pipe','pipe']});
    const output = [], errors = [];
    child.stdout.on('data', b => output.push(b));
    child.stderr.on('data', b => errors.push(b));
    child.once('error', reject);
    child.once('close', exitCode => {
      const raw = Buffer.concat(output);
      if (exitCode) return resolve({exitCode, raw, stderr:Buffer.concat(errors).toString('utf8')});
      try {
        assert(raw.length >= 4, 'missing native frame');
        const n = raw.readUInt32LE(0);
        assert(n > 0 && n <= 16384 && n + 4 === raw.length, 'invalid frame length');
        resolve({exitCode, data: JSON.parse(raw.subarray(4).toString('utf8'))});
      } catch (error) {reject(error);}
    });
    const bytes = Buffer.from(JSON.stringify(body), 'utf8');
    const frame = Buffer.alloc(4); frame.writeUInt32LE(bytes.length);
    child.stdin.end(Buffer.concat([frame, bytes]));
  });
}
(async () => {
  const before = hash(fs.readFileSync(tokenFile));
  const good = await request('qdm@queen.local',{action:'discover',start:false});
  assert.equal(good.exitCode,0);
  assert.equal(good.data.ok,true, JSON.stringify({error:good.data.error}));
  assert.match(good.data.token,/^[0-9a-f]{64}$/i);
  assert(Number.isInteger(good.data.port) && good.data.port>=17876 && good.data.port<=17885);
  const res = await fetch(`http://127.0.0.1:${good.data.port}/health`,{headers:{Authorization:'Bearer '+good.data.token}});
  assert.equal(res.status,200);
  assert.equal((await res.json()).name,'QDM');
  const again = await request('qdm@queen.local',{action:'discover',start:false});
  assert.equal(again.data.token,good.data.token);
  assert.equal(again.data.port,good.data.port);
  const denied = await request('other-extension@example.com',{action:'discover',start:false});
  assert.notEqual(denied.exitCode,0);
  assert.equal(denied.raw.length,0);
  assert.equal(hash(fs.readFileSync(tokenFile)),before);
  process.stdout.write(JSON.stringify({status:'passed',nativeMessages:3,port:good.data.port,pairingStable:true,originRejected:true,secretProtectedHashUnchanged:true})+'\n');
})().catch(error=>{console.error(error.message);process.exitCode=1;});
