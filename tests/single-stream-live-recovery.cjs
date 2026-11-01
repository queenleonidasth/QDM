'use strict';
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { CliClient, delay, fileHash } = require('./helpers/cli-client.cjs');
const binary = path.resolve(process.argv[2] || 'target/release/QDM.exe');
const payload = Buffer.allocUnsafe(16 * 1024 * 1024 + 137);
for (let i = 0; i < payload.length; i++) payload[i] = (i * 31 ^ i >>> 9 ^ i >>> 17) & 255;
const hash = crypto.createHash('sha256').update(payload).digest('hex');
process.env.QDM_RANGE_RECOVERY_TEST_MS = '200';
async function run(mode) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-live-range-'));
  const dir = path.join(root, 'downloads'); await fs.mkdir(dir);
  let started = Date.now(), probes = 0, peak = 0, active = 0, full = 0, fullCompleted = 0, accepted = 0;
  const sockets = new Set();
  const server = http.createServer((req, res) => {
    const match = /^bytes=(\d+)-(\d+)$/.exec(req.headers.range || '');
    const probe = match && +match[1] === 0 && +match[2] === 0;
    if (probe) probes++;
    const range = match && Date.now() - started >= 700 && mode !== 'persistent';
    const start = range ? +match[1] : 0, end = range ? +match[2] : payload.length - 1;
    const etag = range && mode === 'changed' ? '"changed"' : '"original"';
    const headers = {'Content-Length': end - start + 1, ETag: etag};
    if (range) headers['Content-Range'] = `bytes ${start}-${end}/${payload.length}`;
    res.writeHead(range ? 206 : 200, headers);
    if (range && end - start < 128 * 1024) return res.end(payload.subarray(start, end + 1));
    if (range) { active++; accepted++; peak = Math.max(peak, active); }
    else { full++; res.once('finish', () => fullCompleted++); }
    let pos = start, timer;
    const cleanup = () => { if (timer) clearTimeout(timer); if (range) { active--; } };
    res.once('close', cleanup);
    function send() {
      if (res.destroyed) return;
      const next = Math.min(pos + 32 * 1024, end + 1);
      const ok = res.write(payload.subarray(pos, next)); pos = next;
      if (pos > end) return res.end();
      if (ok) timer = setTimeout(send, 8); else res.once('drain', () => { timer = setTimeout(send, 8); });
    }
    send();
  });
  server.on('connection', s => { sockets.add(s); s.once('close', () => sockets.delete(s)); });
  server.on('clientError', (_, s) => s.destroy());
  let client;
  try {
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    started = Date.now();
    client = new CliClient(binary, path.join(root, 'state'), dir);
    await client.settings({ connections: 6, max_concurrent: 1, retries: 2 });
    const id = await client.add({url: `http://127.0.0.1:${server.address().port}/file`, filename: 'file.bin', expected_sha256: hash});
    let last = 0, terminal;
    const deadline = Date.now() + 40000;
    while (Date.now() < deadline) {
      const task = await client.task(id);
      assert.ok(task.downloaded_bytes >= last, `progress decreased ${last} -> ${task.downloaded_bytes}`);
      last = task.downloaded_bytes;
      if (['completed', 'failed', 'error'].includes(task.status)) { terminal = task; break; }
      await delay(40);
    }
    assert.ok(terminal, 'timed out'); assert.equal(terminal.status, 'completed', terminal.error);
    assert.equal(await fileHash(terminal.file_path), hash);
    assert.ok(probes >= 2, `no live probe: ${probes}`);
    if (mode === 'recover') { assert.ok(peak >= 2, `no parallel recovery: ${peak}`); assert.ok(accepted >= 2); }
    else { assert.equal(accepted, 0, 'unsafe promotion'); assert.equal(fullCompleted, 1, `completed full streams: ${fullCompleted}`); }
    assert.ok(probes <= 8, `probe flood: ${probes}`);
    console.log(JSON.stringify({mode, probes, peak, accepted, full, fullCompleted, sha256: hash, result: 'PASS'}));
  } finally {
    if (client) { try { await client.shutdown(); } catch { await client.crash(); } }
    for (const socket of sockets) socket.destroy();
    await new Promise(resolve => server.close(resolve));
    await fs.rm(root, {recursive: true, force: true, maxRetries: 10, retryDelay: 100});
  }
}
(async () => { for (const mode of ['recover', 'persistent', 'changed']) await run(mode); })().catch(e => { console.error(e.stack); process.exitCode = 1; });

