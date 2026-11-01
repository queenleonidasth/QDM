#!/usr/bin/env node
'use strict';
// Independent origin-side proof that a throttled task probes higher concurrency
// after pause/resume. Only localhost traffic and isolated temporary state.
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const crypto = require('node:crypto');
const { CliClient, waitFor, delay, fileHash } = require('./helpers/cli-client.cjs');
const MiB = 1024 * 1024;
const getArg = (name, fallback) => {
  const index = process.argv.indexOf(name);
  return index < 0 ? fallback : process.argv[index + 1];
};
const binary = path.resolve(getArg('--binary', process.env.QDM_BINARY || 'target/release/QDM.exe'));
const reportPath = getArg('--report', null);
const payload = Buffer.allocUnsafe(96 * MiB + 137);
for (let offset = 0; offset < payload.length; offset += 4) {
  const value = (Math.imul(offset >>> 2, 0x9e3779b1) ^ (offset >>> 12)) >>> 0;
  for (let index = 0; index < 4 && offset + index < payload.length; index++) {
    payload[offset + index] = value >>> (index * 8) & 255;
  }
}
const expected = crypto.createHash('sha256').update(payload).digest('hex');
process.env.QDM_RANGE_RECOVERY_TEST_MS = '200';
let serverLimit = 8;
let refused = 0;
let peakAfterRefusal = 0;
let chunkDelayMs = 25;
let rejected = 0;
let accepted = 0;
let peakBeforeResume = 0;
let peakAfterResume = 0;
let resumed = false;
let nextRequestId = 0;
let nextSocketId = 0;
const active = new Map();
const sockets = new Set();
const timers = new Set();
const server = http.createServer((request, response) => {
  const match = /^bytes=(\d+)-(\d+)$/.exec(request.headers.range || '');
  if (!match) { response.writeHead(400); response.end(); return; }
  const start = Number(match[1]);
  const end = Number(match[2]);
  if (start > end || end >= payload.length) { response.writeHead(416); response.end(); return; }
  const length = end - start + 1;
  const body = length > 64 * 1024;
  if (body && active.size >= 1 && refused === 0) {
    refused++;
    response.writeHead(200, { 'Content-Length': payload.length, 'ETag': '"qdm-adaptive-recovery-v1"' });
    response.end(); return;
  }
  if (body && active.size >= serverLimit) {
    rejected++;
    response.writeHead(429, { 'Retry-After': '0', 'Content-Length': '0' });
    response.end(); return;
  }
  const requestId = ++nextRequestId;
  if (body) {
    active.set(requestId, request.socket.recoverySocketId); accepted++;
    const activeTcpSockets = new Set(active.values()).size;
    if (refused) peakAfterRefusal = Math.max(peakAfterRefusal, activeTcpSockets);
    if (resumed) peakAfterResume = Math.max(peakAfterResume, activeTcpSockets);
    else peakBeforeResume = Math.max(peakBeforeResume, activeTcpSockets);
  }
  response.writeHead(206, {
    'Content-Type': 'application/octet-stream',
    'Accept-Ranges': 'bytes', 'ETag': '"qdm-adaptive-recovery-v1"',
    'Content-Range': `bytes ${start}-${end}/${payload.length}`,
    'Content-Length': length,
  });
  if (!body) { response.end(payload.subarray(start, end + 1)); return; }
  let position = start;
  let timer;
  const cleanup = () => {
    active.delete(requestId);
    if (timer) { clearTimeout(timer); timers.delete(timer); }
  };
  response.once('close', cleanup);
  response.once('finish', cleanup);
  const send = () => {
    if (timer) timers.delete(timer);
    if (response.destroyed || response.writableEnded) return;
    const next = Math.min(position + (chunkDelayMs ? 16 * 1024 : 256 * 1024), end + 1);
    const okay = response.write(payload.subarray(position, next));
    position = next;
    if (position > end) { response.end(); return; }
    const again = () => { timer = setTimeout(send, chunkDelayMs); timers.add(timer); };
    if (okay) again(); else response.once('drain', again);
  };
  send();
});
server.on('connection', (socket) => { socket.recoverySocketId = ++nextSocketId; sockets.add(socket); socket.once('close', () => sockets.delete(socket)); });
server.on('clientError', (_error, socket) => socket.destroy());

(async () => {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-adaptive-recovery-'));
  const downloadDir = path.join(root, 'files');
  await fs.mkdir(downloadDir, { recursive: true });
  const report = { suite: 'transient range refusal recovery', startedAt: new Date().toISOString(),
    binary, binarySha256: await fileHash(binary), result: 'RUNNING' };
  let client;
  try {
    await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
    client = new CliClient(binary, path.join(root, 'state'), downloadDir);
    await client.settings({ connections: 8, max_concurrent: 1, retries: 2 });
    const id = await client.add({
      url: `http://127.0.0.1:${server.address().port}/adaptive-recovery.bin`,
      filename: 'adaptive-recovery.bin', expected_sha256: expected,
    });
    let last = 0;
    await waitFor(async () => {
      const task = await client.task(id);
      assert.ok(task.downloaded_bytes >= last, `progress reset: ${last} -> ${task.downloaded_bytes}`);
      last = task.downloaded_bytes;
      if (['error', 'failed'].includes(task.status)) throw new Error(JSON.stringify(task));
      return refused && peakAfterRefusal >= 2;
    }, 'recovered parallel range bodies after HTTP 200', 40000);
    chunkDelayMs = 0;
    const completed = await client.terminal(id, 45000);
    assert.equal(completed.status, 'completed', completed.error);
    assert.equal(await fileHash(completed.file_path), expected);
    Object.assign(report, {result: 'PASS', refused, peakAfterRefusal, sha256: expected});
  } catch (error) {
    Object.assign(report, { result: 'FAIL', error: error.stack || String(error), rejected,
      accepted, peakBeforeResume, peakAfterResume });
    process.exitCode = 1;
  } finally {
    if (client) { try { await client.shutdown(); } catch { await client.crash(); } }
    for (const timer of timers) clearTimeout(timer);
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => server.close(resolve));
    await fs.rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    report.finishedAt = new Date().toISOString();
    report.binarySha256After = await fileHash(binary);
    assert.equal(report.binarySha256After, report.binarySha256, 'Executable changed during the run');
    if (reportPath) {
      await fs.mkdir(path.dirname(path.resolve(reportPath)), { recursive: true });
      await fs.writeFile(reportPath, JSON.stringify(report, null, 2) + '\n');
    }
    console.log(JSON.stringify(report, null, 2));
  }
})().catch((error) => { console.error(error.stack || error); process.exitCode = 1; });
