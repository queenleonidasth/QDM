#!/usr/bin/env node
'use strict';
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs/promises');
const os = require('node:os');
const path = require('node:path');
const crypto = require('node:crypto');
const { CliClient, delay, fileHash } = require('./helpers/cli-client.cjs');
const binary = path.resolve(process.argv[2] || 'target/release/QDM.exe');
const size = 8 * 1024 * 1024 + 137;
const payload = Buffer.allocUnsafe(size);
for (let i = 0; i < size; i++) payload[i] = (i * 31 ^ i >>> 9 ^ i >>> 17) & 255;
const expected = crypto.createHash('sha256').update(payload).digest('hex');

async function run(mode) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-tail-retry-'));
  let client, attempts = 0, aborts = 0;
  const sockets = new Set();
  const server = http.createServer((req, res) => {
    const match = /^bytes=(\d+)-(\d+)$/.exec(req.headers.range || '');
    if (!match) { res.writeHead(400); res.end(); return; }
    const start = Number(match[1]), end = Number(match[2]);
    if (start > end || end >= size) { res.writeHead(416); res.end(); return; }
    res.writeHead(206, { 'Accept-Ranges': 'bytes', ETag: '"qdm-tail-retry-v1"',
      'Content-Range': `bytes ${start}-${end}/${size}`, 'Content-Length': end - start + 1 });
    if (start === 0 && end === 0) { res.end(payload.subarray(0, 1)); return; }
    attempts++;
    const length = end - start + 1;
    const send = mode === 'progress' || attempts === 1 ? Math.min(1024 * 1024, length) : 0;
    if (send === length) { res.end(payload.subarray(start, end + 1)); return; }
    aborts++;
    res.flushHeaders();
    if (send) res.write(payload.subarray(start, start + send), () => setTimeout(() => res.destroy(), 100));
    else setTimeout(() => res.destroy(), 100);
  });
  server.on('connection', socket => { sockets.add(socket); socket.once('close', () => sockets.delete(socket)); });
  server.on('clientError', (_, socket) => socket.destroy());
  try {
    const dir = path.join(root, 'downloads'); await fs.mkdir(dir);
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    client = new CliClient(binary, path.join(root, 'state'), dir);
    await client.settings({ connections: 1, max_concurrent: 1, retries: 2 });
    const id = await client.add({ url: `http://127.0.0.1:${server.address().port}/file`, filename: 'file.bin', expected_sha256: expected });
    let last = 0, terminal;
    const deadline = Date.now() + 45000;
    while (Date.now() < deadline) {
      const task = await client.task(id);
      assert.ok(task.downloaded_bytes >= last, `progress decreased ${last} -> ${task.downloaded_bytes}`);
      last = task.downloaded_bytes;
      if (['completed', 'failed', 'error'].includes(task.status)) { terminal = task; break; }
      await delay(40);
    }
    assert.ok(terminal, `timed out; attempts=${attempts}, bytes=${last}`);
    if (mode === 'progress') {
      assert.equal(terminal.status, 'completed', JSON.stringify(terminal));
      assert.ok(aborts > 2, `expected more aborts than retries: ${aborts}`);
      assert.equal(await fileHash(terminal.file_path), expected);
    } else {
      assert.ok(['failed', 'error'].includes(terminal.status), JSON.stringify(terminal));
      assert.match(JSON.stringify(terminal), /Retry budget exhausted/);
      assert.equal(attempts, 3, `unexpected attempts: ${attempts}`);
      const partDir = path.join(root, 'state', 'parts-v2', id);
      const parts = await fs.readdir(partDir);
      const part = parts.find(name => name.endsWith('.part'));
      assert.ok(part, 'partial file missing');
      assert.equal((await fs.stat(path.join(partDir, part))).size, 1024 * 1024);
      assert.equal(await fileHash(path.join(partDir, part)), crypto.createHash('sha256').update(payload.subarray(0, 1024 * 1024)).digest('hex'));
      await delay(1500);
      assert.equal(attempts, 3, 'requests continued after terminal failure');
    }
    console.log(JSON.stringify({ mode, result: 'PASS', attempts, aborts, bytes: last, sha256: expected }));
  } finally {
    if (client) { try { await client.shutdown(); } catch { await client.crash(); } }
    for (const socket of sockets) socket.destroy();
    await new Promise(resolve => server.close(resolve));
    await fs.rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
  }
}
(async () => { for (const mode of ['progress', 'no-progress']) await run(mode); })().catch(error => { console.error(error.stack); process.exitCode = 1; });
