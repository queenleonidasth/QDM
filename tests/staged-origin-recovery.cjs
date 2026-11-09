#!/usr/bin/env node
'use strict';
// Origin-side proof of gradual startup and retaining already accepted streams.
const assert = require('node:assert/strict');
const http = require('node:http');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { createHash } = require('node:crypto');
const { CliClient, delay, waitFor, fileHash } = require('./helpers/cli-client.cjs');
const { payload, MiB, KiB } = require('./helpers/http-fixture.cjs');
const option = (name, fallback) => { const i = process.argv.indexOf(name); return i < 0 ? fallback : process.argv[i + 1]; };
const binary = path.resolve(option('--binary', 'target/release/QDM.exe'));
const reportPath = path.resolve(option('--report', 'docs/evidence/slint-idm-20261027/staged-origin-recovery.json'));
const data = payload(32 * MiB + 137, 41);
const expected = createHash('sha256').update(data).digest('hex');
delete process.env.QDM_TEST_ASSEMBLY_DELAY_MS;

async function testCase(mode, pauseDuringCooldown) {
  const workdir = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-staged-origin-'));
  const downloadDir = path.join(workdir, 'files');
  await fs.mkdir(downloadDir, { recursive: true });
  const result = { mode, pauseDuringCooldown, status: 'running', requests: [], workdir };
  const retryAfterSeconds = mode === '429' && !pauseDuringCooldown ? 8 : 2;
  result.retryAfterSeconds = mode === '429' ? retryAfterSeconds : null;
  const sockets = new Set(), active = new Map(), timers = new Set();
  let nextSocket = 0, nextRequest = 0, firstPressureAt = null, firstPressureId = null, peakSockets = 0, client;
  const schedule = (fn, milliseconds) => { const timer = setTimeout(() => { timers.delete(timer); fn(); }, milliseconds); timers.add(timer); return timer; };
  const server = http.createServer((request, response) => {
    const match = /^bytes=(\d+)-(\d+)$/.exec(request.headers.range || '');
    if (!match) { response.writeHead(400); response.end(); return; }
    const start = Number(match[1]), end = Number(match[2]);
    if (start > end || end >= data.length) { response.writeHead(416); response.end(); return; }
    const bytes = end - start + 1, body = bytes > 64 * KiB;
    const record = { id: ++nextRequest, socketId: request.socket.testSocketId, at: Date.now(),
      start, end, body, status: null, sent: 0, sentAfterPressure: 0, closedAt: null };
    result.requests.push(record);
    if (body && active.size >= 2) {
      firstPressureAt ??= Date.now();
      firstPressureId ??= record.id;
      if (mode === '429') {
        record.status = 429;
        response.writeHead(429, { 'Retry-After': String(retryAfterSeconds), 'Content-Length': '0' }); response.end();
      } else { record.status = 'transport-reset'; request.socket.destroy(); }
      return;
    }
    record.status = 206;
    if (body) { active.set(record.id, record); peakSockets = Math.max(peakSockets, new Set([...active.values()].map(r => r.socketId)).size); }
    response.writeHead(206, { 'Content-Type': 'application/octet-stream', 'Accept-Ranges': 'bytes',
      'ETag': '"staged-origin-v41"', 'Content-Length': bytes, 'Content-Range': `bytes ${start}-${end}/${data.length}` });
    response.flushHeaders();
    if (!body) { record.sent = bytes; response.end(data.subarray(start, end + 1)); return; }
    let position = start, timer;
    const closed = () => { record.closedAt ??= Date.now(); active.delete(record.id); if (timer) { clearTimeout(timer); timers.delete(timer); } };
    response.once('close', closed); response.once('finish', closed);
    const send = () => {
      if (response.destroyed || response.writableEnded) return;
      const next = Math.min(position + 64 * KiB, end + 1);
      const count = next - position;
      const accepted = response.write(data.subarray(position, next));
      record.sent += count;
      if (firstPressureAt !== null && Date.now() >= firstPressureAt) record.sentAfterPressure += count;
      position = next;
      if (position > end) { response.end(); return; }
      const again = () => { timer = schedule(send, 8); };
      if (accepted) again(); else response.once('drain', again);
    };
    send();
  });
  server.on('connection', socket => { socket.testSocketId = ++nextSocket; sockets.add(socket); socket.once('close', () => sockets.delete(socket)); });
  server.on('clientError', (_error, socket) => socket.destroy());
  try {
    await new Promise(resolve => server.listen(0, '127.0.0.1', resolve));
    client = new CliClient(binary, path.join(workdir, 'state'), downloadDir);
    await client.settings({ connections: 32, retries: 2, max_concurrent: 1 });
    const id = await client.add({ url: `http://127.0.0.1:${server.address().port}/limited.bin`,
      filename: `${mode}-${pauseDuringCooldown ? 'pause' : 'continue'}.bin`, expected_sha256: expected });
    await waitFor(() => firstPressureAt !== null, 'origin rejects an attempted growth above two sockets', 15000);
    const original = result.requests.filter(r => r.body && r.status === 206 && r.id < firstPressureId);
    assert.equal(original.length, 2, 'Startup must begin with two accepted body streams');
    assert.ok(original.every(r => r.end - r.start + 1 >= 15 * MiB), 'Startup did not retain large initial ranges');
    const firstAt = original[0].at;
    assert.equal(result.requests.filter(r => r.body && r.status === 206 && r.at < firstAt + 75).length, 2, 'The initial request wave exceeded two streams');
    await delay(300);
    assert.ok(original.every(r => r.closedAt === null), 'An accepted stream was canceled immediately after origin pressure');
    assert.ok(original.reduce((sum, r) => sum + r.sentAfterPressure, 0) >= 128 * KiB,
      'Accepted streams stopped delivering bytes during shared backoff');
    result.acceptedStreamsAliveAfterPressure = true;
    result.acceptedBytesDuringFirst300Ms = original.reduce((sum, r) => sum + r.sentAfterPressure, 0);
    if (pauseDuringCooldown) {
      const started = Date.now(); await client.pause(id); result.pauseMs = Date.now() - started;
      assert.ok(result.pauseMs < 2000, 'Pause waited for the full Retry-After interval');
      const paused = await client.task(id); assert.equal(paused.status, 'paused');
      result.savedBytesAtPause = paused.downloaded_bytes;
      assert.ok(result.savedBytesAtPause > 0);
      await client.resume(id);
    }
    const completed = await client.terminal(id, 45000);
    const completedObservedAt = Date.now();
    assert.equal(completed.status, 'completed', completed.error || completed.notice);
    assert.equal(await fileHash(completed.file_path), expected);
    assert.equal(completed.notice, null, 'Completed UI retained a stale assembly or backoff notice');
    if (mode === '429') {
      const tooEarly = result.requests.filter(r => r.at >= firstPressureAt + 100 && r.at < firstPressureAt + retryAfterSeconds * 1000 - 100);
      assert.equal(tooEarly.length, 0, 'A new request bypassed Retry-After, including after Resume');
      if (!pauseDuringCooldown) {
        assert.ok(completedObservedAt < firstPressureAt + retryAfterSeconds * 1000,
          'Untouched rejected splits were not returned to already accepted responses');
        assert.equal(completed.part_count, 2, 'Rejected zero-byte splits still left holes in the original two ranges');
        assert.equal(result.requests.filter(r => r.body && r.status === 206).length, 2,
          'Completion required an extra accepted response instead of the original two streams');
        result.completedWithinRetryAfterWithoutNewRequests = true;
      }
    }
    if (pauseDuringCooldown) assert.ok(completed.resumed_bytes >= result.savedBytesAtPause);
    Object.assign(result, { status: 'passed', firstPressureAt, peakAcceptedBodyTcpSockets: peakSockets,
      rejectedRequests: result.requests.filter(r => r.status !== 206).length,
      actualSha256: expected, independentHashVerified: true, totalBytes: data.length,
      resumedBytes: completed.resumed_bytes, finalParts: completed.part_count,
      dynamicSplits: completed.dynamic_splits, completedObservedAt });
  } catch (error) { result.status = 'failed'; result.error = error.stack || String(error); }
  finally {
    if (client) { try { await client.shutdown(); } catch (error) { await client.crash(); result.shutdownError = String(error); result.status = 'failed'; } }
    for (const timer of timers) clearTimeout(timer);
    for (const socket of sockets) socket.destroy();
    await new Promise(resolve => server.close(resolve));
    if (result.status === 'passed') await fs.rm(workdir, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
  return result;
}

(async () => {
  const report = { schema: 1, suite: 'Staged startup and accepted-stream recovery', startedAt: new Date().toISOString(),
    binary, binarySha256: await fileHash(binary), cases: [], status: 'running' };
  await fs.mkdir(path.dirname(reportPath), { recursive: true });
  for (const [mode, pause] of [['429', false], ['429', true], ['reset', false]]) {
    const result = await testCase(mode, pause); report.cases.push(result);
    await fs.writeFile(reportPath, JSON.stringify(report, null, 2) + '\n');
    console.log(`${result.status.toUpperCase()} ${mode} ${pause ? 'pause/resume' : 'continue'} ${result.error || ''}`);
  }
  report.binarySha256After = await fileHash(binary);
  report.status = report.cases.every(value => value.status === 'passed') && report.binarySha256After === report.binarySha256 ? 'passed' : 'failed';
  report.finishedAt = new Date().toISOString();
  await fs.writeFile(reportPath, JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({ status: report.status, report: reportPath, cases: report.cases.map(({mode, pauseDuringCooldown, status, rejectedRequests, acceptedBytesDuringFirst300Ms, pauseMs}) => ({mode, pauseDuringCooldown, status, rejectedRequests, acceptedBytesDuringFirst300Ms, pauseMs})) }, null, 2));
  if (report.status !== 'passed') process.exitCode = 1;
})().catch(error => { console.error(error.stack || error); process.exitCode = 1; });
