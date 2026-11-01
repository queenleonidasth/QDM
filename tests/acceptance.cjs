#!/usr/bin/env node
'use strict';

// Run the shipped executable, not engine internals. Node's standard library is
// the only prerequisite, on Windows or Linux.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { CliClient, waitFor, delay, fileHash } = require('./helpers/cli-client.cjs');
const { createHttpFixture, KiB, MiB } = require('./helpers/http-fixture.cjs');
const { openState, journalMode } = require('./helpers/sqlite-state.cjs');

function argument(name, fallback) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : fallback;
}
const binary = path.resolve(argument('--binary', process.env.QDM_BINARY || (process.platform === 'win32' ? 'target/release/QDM.exe' : 'target/release/qdm')));
const reportPath = argument('--report', null);
const filter = argument('--only', null);
const preserve = process.argv.includes('--keep');
const cases = [];
function test(name, run) { cases.push({ name, run }); }

async function setup(behavior = {}, settings = {}) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-rust-acceptance-'));
  const dataDir = path.join(root, 'state');
  const downloadDir = path.join(root, 'downloads');
  await fs.mkdir(downloadDir, { recursive: true });
  const fixture = await createHttpFixture(behavior);
  const clients = [];
  const context = {
    root, dataDir, downloadDir, fixture, clients, client: null, detail: {},
    async start() {
      const client = new CliClient(binary, dataDir, downloadDir);
      clients.push(client);
      await client.snapshot();
      this.client = client;
      return client;
    },
    async cleanup() {
      for (const client of clients) {
        if (!client.exited) {
          try { await client.shutdown(); } catch { await client.crash(); }
        }
      }
      await fixture.close();
      if (!preserve) await fs.rm(root, { recursive: true, force: true, maxRetries: 10, retryDelay: 100 });
    },
  };
  try {
    await context.start();
    await context.client.settings({ max_concurrent: 1, retries: 2, ...settings });
    return context;
  } catch (error) {
    await context.cleanup();
    throw error;
  }
}

async function withFixture(behavior, settings, run) {
  const context = await setup(behavior, settings);
  try { await run(context); return context.detail; }
  catch (error) {
    error.evidence = {
      ...context.detail, workDir: preserve ? context.root : '(temporary files cleaned)',
      fixture: context.fixture.metrics,
      requests: context.fixture.requests.slice(-80),
      stderr: context.clients.map((client) => client.stderr).filter(Boolean),
    };
    throw error;
  } finally { await context.cleanup(); }
}

async function complete(context, id, version = context.fixture.behavior.version, timeoutMs = 45000) {
  const task = await context.client.terminal(id, timeoutMs);
  assert.equal(task.status, 'completed', task.error || `Unexpected status ${task.status}`);
  const actualPath = task.file_path || path.join(task.directory, task.filename);
  const stat = await fs.stat(actualPath);
  assert.equal(stat.size, context.fixture.behavior.sizeBytes, 'Published file length differs from the origin');
  const hash = await fileHash(actualPath);
  assert.equal(hash, context.fixture.hash(version), 'Independent SHA-256 found corrupt or mixed-version output');
  assert.equal(task.sha256, hash, 'QDM-reported SHA-256 differs from the independently hashed file');
  context.detail = {
    ...context.detail, bytes: stat.size, sha256: hash,
    peakOriginTcpConnections: context.fixture.metrics.peakRangeSockets,
    peakOriginBodyTcpConnections: context.fixture.metrics.peakBodySockets,
    originBytes: context.fixture.metrics.bodyBytes,
    enginePeakConnections: task.peak_connections,
    dynamicSplits: task.dynamic_splits, resumedBytes: task.resumed_bytes,
    integrity: task.integrity,
  };
  return { ...task, actualPath };
}

async function assertNoPublishedFile(context, task, filename) {
  assert.ok(task.error, 'Failed operation must explain its error');
  if (task.file_path) assert.equal(await fs.stat(task.file_path).then(() => true, () => false), false, 'A failed operation exposed a final file');
  if (filename) assert.equal(await fs.stat(path.join(context.downloadDir, filename)).then(() => true, () => false), false, 'A failed operation left the requested final filename');
}

async function pauseAfter(context, id, targetBytes = 8 * MiB) {
  await waitFor(async () => {
    const task = await context.client.task(id);
    if (['completed', 'error', 'failed'].includes(task?.status)) throw new Error(`Transfer ended before pause: ${JSON.stringify(task)}`);
    return task?.downloaded_bytes >= targetBytes ? task : null;
  }, `at least ${targetBytes} bytes before pause`);
  await context.client.pause(id);
  const task = await waitFor(async () => {
    const current = await context.client.task(id);
    return current?.status === 'paused' ? current : null;
  }, 'paused checkpoint');
  assert.ok(task.downloaded_bytes >= targetBytes, 'Pause lost already received bytes');
  assert.ok(task.downloaded_bytes < context.fixture.behavior.sizeBytes, 'Pause did not happen during the transfer');
  await delay(100);
  return task;
}

for (const connections of [32, 64]) {
  test(`tcp-${connections}-simultaneous-${connections === 64 ? 'no-validator' : 'etag'}`, () => withFixture({
    sizeBytes: 64 * MiB + 37, validators: connections === 64 ? 'none' : 'etag',
    barrierTarget: connections, delayMs: 3,
  }, connections === 32 ? {} : { connections }, async (context) => {
    const snapshot = await context.client.snapshot();
    assert.equal(snapshot.settings.connections, connections, 'Default must be 32 and selected maximum must permit 64');
    const id = await context.client.add({
      url: context.fixture.url, filename: `tcp-${connections}.bin`,
      ...(connections === 32 ? { expected_sha256: context.fixture.hash() } : {}),
    });
    const task = await complete(context, id);
    assert.equal(context.fixture.metrics.barrierReached, true, `Origin never saw ${connections} simultaneous large Range transfers`);
    assert.ok(context.fixture.metrics.peakRangeSockets >= connections, 'Reported settings did not produce distinct real TCP connections');
    assert.ok(context.fixture.metrics.peakBodySockets >= connections, 'Connections must overlap while sending response bodies');
    assert.ok(task.peak_connections >= connections, 'Engine connection telemetry disagrees with the observed origin');
    context.detail.validator = context.fixture.behavior.validators;
  }));
}

test('dynamic-reassigns-an-unfinished-slow-range', () => withFixture({
  sizeBytes: 32 * MiB + 41, slowFirstRange: true, slowDelayMs: 40, delayMs: 1,
}, { connections: 4 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'dynamic.bin' });
  const task = await complete(context, id);
  const reassigned = context.fixture.requests.filter((request) => request.tailOfUnfinished.length);
  assert.ok(task.dynamic_splits > 0, 'Idle workers did not split a remaining slow segment');
  assert.ok(reassigned.length, 'No new interior range was observed inside an unfinished prior transfer');
  context.detail.reassignmentRequests = reassigned.map((request) => ({
    id: request.id, start: request.requestedStart, end: request.requestedEnd,
    prior: request.tailOfUnfinished,
  }));
}));

test('pause-process-restart-reuses-confirmed-bytes', () => withFixture({
  sizeBytes: 64 * MiB + 43, delayMs: 18,
}, { connections: 4 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'restart.bin' });
  const saved = await pauseAfter(context, id);
  await context.client.shutdown();
  const requestIndex = context.fixture.requests.length;
  await context.start();
  const restored = await context.client.task(id);
  assert.equal(restored.status, 'paused');
  assert.ok(restored.downloaded_bytes >= saved.downloaded_bytes, 'Confirmed checkpoint bytes disappeared on restart');
  context.fixture.behavior.delayMs = 1;
  await context.client.resume(id);
  const task = await complete(context, id);
  const resumedNetworkBytes = context.fixture.bytesSince(requestIndex);
  assert.ok(resumedNetworkBytes < context.fixture.behavior.sizeBytes - saved.downloaded_bytes / 2,
    `Resume fetched ${resumedNetworkBytes} bytes after saving ${saved.downloaded_bytes}; this resembles a full restart`);
  assert.ok(task.resumed_bytes > 0, 'Resume telemetry did not report reused bytes');
  context.detail.savedBytes = saved.downloaded_bytes;
  context.detail.resumedNetworkBytes = resumedNetworkBytes;
}));

test('abrupt-process-death-recovers-a-valid-checkpoint', () => withFixture({
  sizeBytes: 64 * MiB + 47, delayMs: 20,
}, { connections: 4 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'crash.bin' });
  await waitFor(async () => (await context.client.task(id))?.downloaded_bytes >= 8 * MiB, 'bytes before abrupt process death');
  // Permit a periodic durable checkpoint, while the slow origin keeps the task
  // far from completion. This is not a graceful pause or shutdown.
  await delay(1300);
  const before = await context.client.task(id);
  assert.ok(before.downloaded_bytes < context.fixture.behavior.sizeBytes);
  await context.client.crash();
  await delay(150);
  const requestIndex = context.fixture.requests.length;
  await context.start();
  const restored = await context.client.task(id);
  assert.ok(restored, 'Crash removed the task');
  assert.ok(restored.downloaded_bytes > 0, 'No durable bytes survived abrupt termination');
  context.fixture.behavior.delayMs = 1;
  if (['paused', 'error', 'failed'].includes(restored.status)) await context.client.resume(id);
  const task = await complete(context, id);
  const resumedNetworkBytes = context.fixture.bytesSince(requestIndex);
  assert.ok(resumedNetworkBytes < context.fixture.behavior.sizeBytes - Math.min(restored.downloaded_bytes, before.downloaded_bytes) / 2,
    'Crash recovery redownloaded the whole object instead of reusing a checkpoint');
  assert.ok(task.resumed_bytes > 0);
  context.detail.beforeCrashBytes = before.downloaded_bytes;
  context.detail.restoredBytes = restored.downloaded_bytes;
  context.detail.resumedNetworkBytes = resumedNetworkBytes;
}));

test('no-validator-resume-compares-overlapping-old-bytes', () => withFixture({
  sizeBytes: 32 * MiB + 51, validators: 'none', delayMs: 20,
}, { connections: 4 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'overlap-resume.bin' });
  const saved = await pauseAfter(context, id, 4 * MiB);
  const requestIndex = context.fixture.requests.length;
  context.fixture.behavior.delayMs = 1;
  await context.client.resume(id);
  const task = await complete(context, id);
  const resumedRanges = context.fixture.requests.slice(requestIndex).filter((request) => request.range && request.requestedLength > 64 * KiB);
  const overlaps = saved.segments.filter((segment) => segment.downloaded_bytes > 64 * KiB &&
    segment.start + segment.downloaded_bytes < segment.end).map((segment) => {
      const savedEnd = segment.start + segment.downloaded_bytes;
      const request = resumedRanges.find((candidate) =>
        candidate.requestedStart < savedEnd && candidate.requestedStart >= savedEnd - 64 * KiB &&
        candidate.requestedEnd >= savedEnd);
      return request ? { segment: segment.id, savedEnd, resumedStart: request.requestedStart, overlapBytes: savedEnd - request.requestedStart } : null;
    }).filter(Boolean);
  assert.ok(overlaps.length, 'No resumed request re-read a saved segment tail before continuing');
  assert.ok(task.resumed_bytes > 0, 'No-validator resume should reuse data after overlap checks');
  assert.ok(context.fixture.bytesSince(requestIndex) < context.fixture.behavior.sizeBytes - saved.downloaded_bytes / 2,
    'No-validator resume silently restarted the whole file');
  context.detail.overlaps = overlaps;
  context.detail.savedBytes = saved.downloaded_bytes;
}));

for (const validators of ['etag', 'last-modified', 'none']) {
  test(`resume-changed-content-${validators}-never-mixes-versions`, () => withFixture({
    sizeBytes: 24 * MiB + 53, validators, delayMs: 20,
  }, { connections: 4 }, async (context) => {
    const id = await context.client.add({ url: context.fixture.url, filename: `changed-${validators}.bin` });
    await pauseAfter(context, id, 4 * MiB);
    const requestIndex = context.fixture.requests.length;
    context.fixture.behavior.version = 2;
    context.fixture.behavior.delayMs = 1;
    await context.client.resume(id);
    const task = await complete(context, id, 2);
    assert.ok(context.fixture.requests.slice(requestIndex).some((request) => request.version === 2));
    context.detail.validator = validators;
    context.detail.recoveryNotice = task.notice;
  }));
}

test('no-validator-boundary-disagreement-recovers-one-whole-version', () => withFixture({
  sizeBytes: 16 * MiB, validators: 'none', mixedVersions: true, delayMs: 2,
}, { connections: 4 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'mixed-origin.bin' });
  await complete(context, id, 1);
  const versions = new Set(context.fixture.requests.filter((request) => request.sentBytes > 128 * KiB).map((request) => request.version));
  assert.ok(versions.has(1) && versions.has(2), 'Fixture did not actually send conflicting representations');
  context.detail.originRepresentations = [...versions];
}));

for (const rangeMode of ['ignore', 'ignore-after-probe']) {
  test(`range-${rangeMode}-recovers-as-an-exact-file`, () => withFixture({ rangeMode }, { connections: 8 }, async (context) => {
    const id = await context.client.add({ url: context.fixture.url, filename: `range-${rangeMode}.bin` });
    await complete(context, id);
  }));
}

test('malformed-content-range-is-rejected-or-recovered-safely', () => withFixture({
  rangeMode: 'malformed',
}, { connections: 4, retries: 1 }, async (context) => {
  const filename = 'malformed.bin';
  const id = await context.client.add({ url: context.fixture.url, filename });
  const task = await context.client.terminal(id);
  if (task.status === 'completed') {
    await complete(context, id);
    assert.ok(context.fixture.requests.some((request) => request.status === 200 && request.sentBytes === context.fixture.behavior.sizeBytes),
      'Malformed range response was accepted without a valid whole-file recovery');
    context.detail.outcome = 'safe full-stream recovery';
  } else { await assertNoPublishedFile(context, task, filename); context.detail.outcome = 'explicit safe refusal'; }
}));

test('transient-truncation-recovers-without-missing-bytes', () => withFixture({
  truncateCount: 1,
}, { connections: 4, retries: 3 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'transient.bin' });
  await complete(context, id);
  assert.ok(context.fixture.metrics.abortedRequests > 0, 'Fixture did not interrupt a response');
}));

test('permanent-truncation-does-not-publish-a-partial-file', () => withFixture({
  permanentTruncate: true,
}, { connections: 1, retries: 1 }, async (context) => {
  const filename = 'truncated.bin';
  const id = await context.client.add({ url: context.fixture.url, filename });
  const task = await context.client.terminal(id);
  assert.notEqual(task.status, 'completed', 'Permanently truncated response was declared complete');
  await assertNoPublishedFile(context, task, filename);
  context.detail.error = task.error;
}));

test('retry-429-and-503-honors-retry-after', () => withFixture({
  failures: [429, 503], retryAfterSeconds: 1,
}, { connections: 1, retries: 3 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'retry.bin' });
  await complete(context, id);
  const failures = context.fixture.requests.filter((request) => [429, 503].includes(request.status));
  assert.deepEqual(failures.map((request) => request.status), [429, 503]);
  const waits = failures.map((failure) => {
    const next = context.fixture.requests.find((request) => request.id > failure.id && request.method !== 'HEAD' && request.requestedLength > 64 * KiB);
    assert.ok(next, 'No transfer retry followed the temporary error');
    const waited = next.at - failure.at;
    assert.ok(waited >= 900, `Retry-After: 1 was ignored (${waited} ms)`);
    return { status: failure.status, waitedMs: waited };
  });
  context.detail.retryWaits = waits;
}));

test('expired-url-can-be-replaced-and-resumed', () => withFixture({
  sizeBytes: 32 * MiB + 59, delayMs: 20,
}, { connections: 4 }, async (context) => {
  const id = await context.client.add({ url: `${context.fixture.url}?token=old`, filename: 'refreshed.bin' });
  const saved = await pauseAfter(context, id, 4 * MiB);
  context.fixture.behavior.denied = true;
  await context.client.resume(id);
  const expired = await context.client.terminal(id);
  assert.notEqual(expired.status, 'completed');
  assert.ok(expired.error, 'Expired URL did not give an error');
  context.fixture.behavior.denied = false;
  context.fixture.behavior.delayMs = 1;
  const requestIndex = context.fixture.requests.length;
  await context.client.replaceUrl(id, `${context.fixture.url}?token=fresh`);
  const current = await context.client.task(id);
  if (!['downloading', 'probing', 'queued', 'verifying', 'completed'].includes(current.status)) await context.client.resume(id);
  const task = await complete(context, id);
  assert.ok(context.fixture.requests.slice(requestIndex).some((request) => request.url.includes('token=fresh')));
  assert.ok(context.fixture.bytesSince(requestIndex) < context.fixture.behavior.sizeBytes - saved.downloaded_bytes / 2,
    'Replacing an expired URL for the same verified object discarded reusable bytes');
  context.detail.refreshedUrl = task.url;
  context.detail.savedBytes = saved.downloaded_bytes;
}));

test('expected-sha256-mismatch-never-publishes-output', () => withFixture({}, { connections: 4 }, async (context) => {
  const filename = 'wrong-hash.bin';
  const id = await context.client.add({ url: context.fixture.url, filename, expected_sha256: '0'.repeat(64) });
  const task = await context.client.terminal(id);
  assert.notEqual(task.status, 'completed');
  await assertNoPublishedFile(context, task, filename);
  assert.match(task.error, /sha|checksum|hash|integrity/i, 'Checksum failure must be understandable');
  context.detail.error = task.error;
}));

test('collisions-preserve-user-files-and-concurrent-jobs', () => withFixture({
  sizeBytes: 2 * MiB + 61,
}, { connections: 2, max_concurrent: 2 }, async (context) => {
  const filename = 'same.bin';
  const existingPath = path.join(context.downloadDir, filename);
  await fs.writeFile(existingPath, 'existing user data\n');
  const firstId = await context.client.add({ url: `${context.fixture.url}?job=one`, filename });
  const secondId = await context.client.add({ url: `${context.fixture.url}?job=two`, filename });
  const first = await complete(context, firstId);
  const second = await complete(context, secondId);
  assert.equal(await fs.readFile(existingPath, 'utf8'), 'existing user data\n');
  assert.notEqual(first.actualPath.toLowerCase(), second.actualPath.toLowerCase());
  assert.notEqual(first.actualPath.toLowerCase(), existingPath.toLowerCase());
  assert.notEqual(second.actualPath.toLowerCase(), existingPath.toLowerCase());
  context.detail.filenames = [path.basename(first.actualPath), path.basename(second.actualPath)];
}));

test('server-filename-is-safe-and-utf8-filename-star-is-preserved', async () => {
  const unsafe = await withFixture({
    sizeBytes: 12345, contentDisposition: 'attachment; filename="../../NUL.txt"',
  }, { connections: 1 }, async (context) => {
    const id = await context.client.add({ url: context.fixture.url });
    const task = await complete(context, id);
    assert.equal(path.dirname(path.resolve(task.actualPath)), path.resolve(context.downloadDir));
    assert.doesNotMatch(path.basename(task.actualPath), /^(con|prn|aux|nul|com[1-9¹²³]|lpt[1-9¹²³])(?:\.|$)/i);
    assert.doesNotMatch(path.basename(task.actualPath), /[<>:"/\\|?*\x00-\x1f]/);
    context.detail.filename = path.basename(task.actualPath);
  });
  const thaiName = 'ทดสอบ-ควีน.bin';
  const utf8 = await withFixture({
    sizeBytes: 23456,
    contentDisposition: `attachment; filename="fallback.bin"; filename*=UTF-8''${encodeURIComponent(thaiName)}`,
  }, { connections: 1 }, async (context) => {
    const id = await context.client.add({ url: context.fixture.url });
    const task = await complete(context, id);
    assert.equal(path.basename(task.actualPath), thaiName);
    context.detail.filename = path.basename(task.actualPath);
  });
  return { unsafe, utf8 };
});

for (const sizeBytes of [0, 1024 * 1024 + 67]) {
  test(sizeBytes === 0 ? 'zero-byte-file-completes' : 'unknown-length-stream-completes', () => withFixture({
    sizeBytes, unknownLength: sizeBytes > 0,
  }, { connections: 4 }, async (context) => {
    const id = await context.client.add({ url: context.fixture.url, filename: sizeBytes ? 'chunked.bin' : 'empty.bin' });
    await complete(context, id);
  }));
}

test('explicit-request-header-reaches-the-origin', () => withFixture({
  requireHeader: { name: 'x-qdm-fixture', value: 'authorized-test' }, sizeBytes: MiB + 71,
}, { connections: 4 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'headers.bin', headers: { 'x-qdm-fixture': 'authorized-test' } });
  await complete(context, id);
}));

test('weak-etag-never-becomes-an-if-range-validator', () => withFixture({
  validators: 'weak', sizeBytes: 4 * MiB + 73,
}, { connections: 4 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'weak-etag.bin' });
  await complete(context, id);
  const ranges = context.fixture.requests.filter((request) => request.range && request.requestedLength > 64 * KiB);
  assert.ok(ranges.length > 1, 'Weak ETag should not disable correctly checked parallel Range downloads');
  assert.ok(ranges.every((request) => request.ifRange === null),
    'RFC 9110 forbids weak ETag If-Range and date If-Range when an entity tag is present');
}));

test('aggregate-speed-limit-is-shared-by-concurrent-downloads', () => withFixture({
  sizeBytes: MiB + 79, delayMs: 0, barrierTarget: 8,
}, { connections: 4, max_concurrent: 2, speed_limit_kib: 512 }, async (context) => {
  const started = Date.now();
  const first = await context.client.add({ url: `${context.fixture.url}?job=speed-one`, filename: 'speed-one.bin' });
  const second = await context.client.add({ url: `${context.fixture.url}?job=speed-two`, filename: 'speed-two.bin' });
  await Promise.all([complete(context, first), complete(context, second)]);
  const elapsedMs = Date.now() - started;
  const minimumMs = 2 * context.fixture.behavior.sizeBytes / (512 * KiB) * 1000;
  assert.equal(context.fixture.metrics.barrierReached, true, 'Both four-connection downloads must run concurrently');
  assert.ok(elapsedMs >= minimumMs * 0.75,
    `Two downloads completed in ${elapsedMs} ms, faster than an aggregate 512 KiB/s limit permits`);
  assert.ok(elapsedMs < 20000, 'Limited downloads stalled instead of making useful progress');
  context.detail.elapsedMs = elapsedMs;
  context.detail.minimumPayloadTransferMs = minimumMs;
  context.detail.aggregatePayloadKiBps = 2 * context.fixture.behavior.sizeBytes / KiB / (elapsedMs / 1000);
}));

test('per-download-speed-limit-is-enforced', () => withFixture({
  sizeBytes: 512 * KiB + 71, delayMs: 0, chunkSize: 32 * KiB,
}, { connections: 1, max_concurrent: 1, speed_limit_kib: 0 }, async (context) => {
  const started = Date.now();
  const id = await context.client.add({ url: context.fixture.url, filename: 'per-download-limit.bin', speed_limit_kib: 256 });
  await complete(context, id);
  const elapsedMs = Date.now() - started;
  const minimumMs = context.fixture.behavior.sizeBytes / (256 * KiB) * 1000;
  assert.ok(elapsedMs >= minimumMs * 0.65,
    `Per-download limit completed in ${elapsedMs} ms, faster than a 256 KiB/s budget permits`);
  context.detail.elapsedMs = elapsedMs;
  context.detail.perDownloadLimitKiBps = 256;
}));

test('queue-speed-limit-is-shared-across-concurrent-transfers', () => withFixture({
  sizeBytes: 512 * KiB + 89, delayMs: 0, chunkSize: 32 * KiB, barrierTarget: 2,
}, { connections: 1, max_concurrent: 2, speed_limit_kib: 0, queue_speed_limits: { shared: 256 } }, async (context) => {
  const started = Date.now();
  const first = await context.client.add({ url: `${context.fixture.url}?job=shared-one`, filename: 'shared-one.bin', queue: 'shared' });
  const second = await context.client.add({ url: `${context.fixture.url}?job=shared-two`, filename: 'shared-two.bin', queue: 'shared' });
  await Promise.all([complete(context, first), complete(context, second)]);
  const elapsedMs = Date.now() - started;
  const minimumMs = 2 * context.fixture.behavior.sizeBytes / (256 * KiB) * 1000;
  assert.equal(context.fixture.metrics.barrierReached, true, 'Both queue members must transfer concurrently');
  assert.ok(elapsedMs >= minimumMs * 0.65,
    `Shared queue completed in ${elapsedMs} ms, faster than its aggregate 256 KiB/s budget permits`);
  context.detail.elapsedMs = elapsedMs;
  context.detail.queueLimitKiBps = 256;
  context.detail.queueMembers = 2;
}));

test('default-queue-limit-applies-to-unassigned-downloads', () => withFixture({
  sizeBytes: 256 * KiB + 41, delayMs: 0, chunkSize: 32 * KiB,
}, { connections: 1, max_concurrent: 1, speed_limit_kib: 0, queue_speed_limits: { Default: 128 } }, async (context) => {
  const started = Date.now();
  const id = await context.client.add({ url: context.fixture.url, filename: 'default-queue-limit.bin' });
  await complete(context, id);
  const elapsedMs = Date.now() - started;
  const minimumMs = context.fixture.behavior.sizeBytes / (128 * KiB) * 1000;
  assert.ok(elapsedMs >= minimumMs * 0.6,
    `Default queue completed in ${elapsedMs} ms, faster than its 128 KiB/s budget permits`);
  context.detail.elapsedMs = elapsedMs;
  context.detail.defaultQueueLimitKiBps = 128;
}));

test('changing-an-active-per-download-limit-takes-effect-without-restart', () => withFixture({
  sizeBytes: MiB + 53, delayMs: 0, chunkSize: 32 * KiB,
}, { connections: 1, max_concurrent: 1, speed_limit_kib: 0 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'dynamic-limit.bin', speed_limit_kib: 1024 });
  const progress = await waitFor(async () => {
    const task = await context.client.task(id);
    return task?.downloaded_bytes >= 128 * KiB ? task : null;
  }, 'active transfer reaches its initial limit');
  const changedAt = Date.now();
  await context.client.limits(id, '', 128);
  await complete(context, id);
  const elapsedMs = Date.now() - changedAt;
  const remainingBytes = context.fixture.behavior.sizeBytes - progress.downloaded_bytes;
  const minimumMs = remainingBytes / (128 * KiB) * 1000;
  assert.ok(elapsedMs >= minimumMs * 0.6,
    `Updated limit completed in ${elapsedMs} ms, faster than the remaining 128 KiB/s budget permits`);
  context.detail.elapsedAfterUpdateMs = elapsedMs;
  context.detail.remainingBytesAtUpdate = remainingBytes;
}));

test('pausing-one-queue-member-does-not-starve-another', () => withFixture({
  sizeBytes: 512 * KiB + 97, delayMs: 0, chunkSize: 32 * KiB,
}, { connections: 1, max_concurrent: 2, speed_limit_kib: 0, queue_speed_limits: { shared: 512 } }, async (context) => {
  const first = await context.client.add({ url: `${context.fixture.url}?job=pause-one`, filename: 'pause-one.bin', queue: 'shared' });
  const second = await context.client.add({ url: `${context.fixture.url}?job=pause-two`, filename: 'pause-two.bin', queue: 'shared' });
  await Promise.all([first, second].map((id) => waitFor(async () => {
    const task = await context.client.task(id);
    return task?.downloaded_bytes >= 64 * KiB ? task : null;
  }, `queue member ${id} receives initial data`)));
  const secondBeforePause = (await context.client.task(second)).downloaded_bytes;
  await context.client.pause(first);
  await waitFor(async () => (await context.client.task(second))?.downloaded_bytes >= secondBeforePause + 64 * KiB,
    'unpaused queue member keeps progressing');
  await context.client.resume(first);
  await Promise.all([complete(context, first), complete(context, second)]);
  context.detail.pausedMemberResumed = true;
  context.detail.unpausedMemberProgressed = true;
}));

test('queue-respects-max-concurrent-and-pause-releases-a-slot', () => withFixture({
  sizeBytes: 16 * MiB + 83, delayMs: 40,
}, { connections: 4, max_concurrent: 1 }, async (context) => {
  const first = await context.client.add({ url: `${context.fixture.url}?job=queue-one`, filename: 'queue-one.bin' });
  await waitFor(async () => (await context.client.task(first))?.downloaded_bytes > 128 * KiB, 'first queued job starts');
  const second = await context.client.add({ url: `${context.fixture.url}?job=queue-two`, filename: 'queue-two.bin' });
  await delay(250);
  const waiting = await context.client.snapshot();
  assert.equal(waiting.downloads.find((task) => task.id === second).status, 'queued');
  assert.equal(waiting.active_count, 1);
  assert.equal(context.fixture.requests.some((request) => request.url.includes('job=queue-two')), false,
    'Queued task contacted the origin before a concurrency slot was available');
  await context.client.pause(first);
  await waitFor(async () => (await context.client.task(second))?.downloaded_bytes > 128 * KiB, 'pausing first task releases a slot');
  assert.equal((await context.client.task(first)).status, 'paused');
  await context.client.resume(first);
  const resumed = await context.client.snapshot();
  assert.equal(resumed.downloads.find((task) => task.id === first).status, 'queued');
  assert.ok(resumed.active_count <= 1);
  context.fixture.behavior.delayMs = 1;
  await complete(context, second);
  await complete(context, first);
  context.detail.maxConcurrent = 1;
  context.detail.pausedTaskQueuedBehindActiveTask = true;
}));

test('scheduled-download-waits-for-time-and-survives-restart', () => withFixture({
  sizeBytes: MiB + 89, delayMs: 0,
}, { connections: 4 }, async (context) => {
  const scheduledAt = Date.now() + 4000;
  const id = await context.client.add({ url: context.fixture.url, filename: 'scheduled.bin', scheduled_at: scheduledAt });
  assert.equal((await context.client.task(id)).status, 'scheduled');
  await delay(700);
  assert.equal(context.fixture.requests.length, 0, 'Scheduled download contacted the origin early');
  await context.client.shutdown();
  await context.start();
  const restored = await context.client.task(id);
  assert.equal(restored.scheduled_at, scheduledAt, 'Schedule was lost across a process restart');
  await complete(context, id);
  const firstRequestAt = context.fixture.requests[0].at;
  assert.ok(firstRequestAt >= scheduledAt - 30, 'Scheduled transfer began before its requested start time');
  assert.ok(firstRequestAt < scheduledAt + 5000, 'Scheduler did not wake the task promptly');
  context.detail.scheduledAt = scheduledAt;
  context.detail.firstOriginRequestAt = firstRequestAt;
}));

for (const unknownLength of [false, true]) {
  test(`gzip-content-encoding-${unknownLength ? 'chunked' : 'length-known'}-decodes-one-exact-stream`, () => withFixture({
    sizeBytes: 2 * MiB + 97, contentEncoding: 'gzip', unknownLength,
  }, { connections: 8 }, async (context) => {
    const id = await context.client.add({ url: context.fixture.url, filename: 'decoded-gzip.bin' });
    const task = await complete(context, id);
    assert.ok(context.fixture.requests.some((request) => request.method === 'GET' && request.status === 200 && !request.range),
      'Content-encoded ranges were not retried as a coherent whole stream');
    assert.equal(task.supports_ranges, false, 'Encoded stream was incorrectly presented as byte-range resumable');
    context.detail.contentEncoding = 'gzip';
    context.detail.wireLengthKnown = !unknownLength;
  }));
}


test('direct-write-uses-one-sparse-partial-and-sqlite-wal', () => withFixture({
  sizeBytes: 16 * MiB + 137, chunkSize: 16 * KiB, delayMs: 5,
}, { connections: 8, max_concurrent: 1 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'single-direct.bin' });
  await waitFor(async () => {
    const task = await context.client.task(id);
    return task?.downloaded_bytes > 64 * KiB && task.downloaded_bytes < context.fixture.behavior.sizeBytes;
  }, 'directly written bytes and active HTTP connections');
  const observed = (await fs.readdir(context.downloadDir)).filter(x => x.startsWith('.qdm-'));
  assert.deepEqual(observed, ['.qdm-' + id + '.partial'], 'One direct-write temporary file per task');
  assert.equal((await fs.stat(path.join(context.downloadDir, observed[0]))).size,
    context.fixture.behavior.sizeBytes, 'Direct-write file logical size must be exact');
  assert.equal(await fs.stat(path.join(context.dataDir, 'parts-v2')).then(()=>true,()=>false), false,
    'Direct-write backend must never create legacy per-worker files');
  assert.equal(journalMode(context.dataDir).toLowerCase(), 'wal');
  const completed = await complete(context, id);
  assert.deepEqual((await fs.readdir(context.downloadDir)).sort(), ['single-direct.bin']);
  const db = openState(context.dataDir);
  const stored = db.tasks.find(t=>t.view.id === id);
  assert.ok(stored && stored.view.status === 'completed', 'SQLite must contain completed job');
  assert.equal(stored.view.sha256, completed.sha256);
  context.detail.sqliteWal = true;
  context.detail.noMergeCopy = true;
}));

test('buffered-small-writes-survive-pause-and-process-restart', () => withFixture({
  sizeBytes: 4 * MiB + 117, chunkSize: 4 * KiB, delayMs: 15,
}, { connections: 1 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'buffered-pause.bin' });
  const saved = await pauseAfter(context, id, 32 * KiB);
  // Pause before the 256 KiB userspace buffer can fill: the checkpoint has to
  // flush accepted bytes, rather than trusting the backing file's old size.
  assert.ok(saved.downloaded_bytes < 256 * KiB,
    `Fixture failed to pause before a buffer filled: ${saved.downloaded_bytes}`);
  const persisted = openState(context.dataDir);
  const stored = persisted.tasks.find((task) => task.view.id === id);
  assert.ok(stored.parts.length === 1);
  const part = stored.parts[0];
  const partPath = path.join(context.downloadDir, '.qdm-' + id + '.partial');
  assert.equal((await fs.stat(partPath)).size, context.fixture.behavior.sizeBytes,
    'Single direct-write file holds the full sparse logical size');
  assert.equal(part.durable, saved.downloaded_bytes);
  assert.ok(part.durable_tail_sha256);
  await context.client.shutdown();
  await context.start();
  const restored = await context.client.task(id);
  assert.equal(restored.downloaded_bytes, saved.downloaded_bytes);
  assert.equal(restored.lanes[0].current_bytes, saved.downloaded_bytes);
  context.fixture.behavior.delayMs = 0;
  await context.client.resume(id);
  await complete(context, id);
  context.detail.checkpointBytes = saved.downloaded_bytes;
}));

async function withAssemblyPacing(run) {
  const previous = process.env.QDM_TEST_ASSEMBLY_DELAY_MS;
  process.env.QDM_TEST_ASSEMBLY_DELAY_MS = '30';
  try { return await run(); }
  finally {
    if (previous === undefined) delete process.env.QDM_TEST_ASSEMBLY_DELAY_MS;
    else process.env.QDM_TEST_ASSEMBLY_DELAY_MS = previous;
  }
}

test('resume-validation-is-visible-and-pause-all-keeps-one-owner', () => withFixture({
  sizeBytes: 8 * MiB + 127, chunkSize: 8 * KiB, delayMs: 20,
}, { connections: 4 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'resume-validation.bin' });
  const saved = await pauseAfter(context, id, 512 * KiB);
  // Delay actual32KiB witness responses to make the real checking phase
  // observable. The one-byte metadata probe still completes in one chunk.
  context.fixture.behavior.sampleDelayMs = 100;
  await context.client.resume(id);
  await waitFor(async () => (await context.client.task(id))?.status === 'verifying',
    'visible verification of saved data before resumed writers', 5000);
  const probes = context.fixture.requests.filter(r => r.requestedLength === 1).length;
  for (let attempt = 0; attempt < 3; attempt++) {
    await context.client.resume(id);
    const snapshot = await context.client.snapshot();
    assert.equal(snapshot.active_count, 1, 'Resume duplicated the validation owner');
    assert.equal(snapshot.downloads.find(task => task.id === id).status, 'verifying');
  }
  const stopping = Date.now();
  await context.client.command('pause_all');
  const paused = await context.client.task(id);
  assert.equal(paused.status, 'paused');
  assert.equal(context.fixture.requests.filter(r => r.requestedLength === 1).length, probes,
    'Repeated Resume launched an extra metadata probe');
  assert.ok(paused.downloaded_bytes >= saved.downloaded_bytes, 'Validation pause discarded saved bytes');
  assert.ok(Date.now() - stopping < 1500, 'PauseAll waited for validation to finish');
  context.fixture.behavior.sampleDelayMs = 0;
  context.fixture.behavior.delayMs = 0;
  await context.client.resume(id);
  const completed = await complete(context, id);
  assert.ok(completed.resumed_bytes >= saved.downloaded_bytes);
  context.detail.validationStatusObserved = true;
  context.detail.repeatedResumeKeptOneOwner = true;
  context.detail.savedBytes = saved.downloaded_bytes;
}));

async function duringVerification(context, id) {
  return waitFor(async () => {
    const task = await context.client.task(id);
    assert.ok(!['error', 'failed'].includes(task?.status), task?.error || 'Transfer failed');
    return task?.status === 'verifying' && task.assembly_bytes > 0 &&
      task.assembly_bytes < task.assembly_total_bytes ? task : null;
  }, 'an intermediate direct-file SHA-256 verification state', 30000, 10);
}

test('resume-during-assembly-never-starts-a-second-owner', () => withAssemblyPacing(() => withFixture({
  sizeBytes: 48 * MiB + 119, delayMs: 0,
}, { connections: 8, max_concurrent: 2 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'one-owner.bin' });
  await duringVerification(context, id);
  const requestIndex = context.fixture.requests.length;
  for (let index = 0; index < 8; index++) {
    await context.client.resume(id);
    const snapshot = await context.client.snapshot();
    const task = snapshot.downloads.find((value) => value.id === id);
    assert.ok(['verifying', 'completed'].includes(task.status),
      `Resume changed an owned assembly to ${task.status}`);
    assert.ok(snapshot.active_count <= 1, 'Resume created a second active owner');
  }
  await complete(context, id);
  await delay(250);
  assert.equal(context.fixture.requests.length, requestIndex,
    'Resume during assembly issued new origin requests');
  const published = (await fs.readdir(context.downloadDir)).filter((name) => !name.startsWith('.'));
  assert.deepEqual(published, ['one-owner.bin'], 'One task published duplicate files');
  context.detail.repeatResumeCommands = 8;
  context.detail.newRequestsDuringAssembly = context.fixture.requests.length - requestIndex;
})));

test('pause-all-stops-assembly-and-resume-reuses-complete-parts', () => withAssemblyPacing(() => withFixture({
  sizeBytes: 48 * MiB + 121, delayMs: 0,
}, { connections: 8, max_concurrent: 2 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'pause-assembly.bin' });
  await duringVerification(context, id);
  const startPause = Date.now();
  await context.client.command('pause_all');
  const pauseAssemblyMs = Date.now() - startPause;
  const snapshot = await context.client.snapshot();
  const task = snapshot.downloads.find((value) => value.id === id);
  assert.equal(task.status, 'paused', 'Pause all left assembly running');
  assert.equal(snapshot.active_count, 0, 'Pause all returned with an active writer');
  assert.equal(task.downloaded_bytes, context.fixture.behavior.sizeBytes,
    'Pausing assembly lost completely downloaded parts');
  assert.deepEqual(await fs.readdir(context.downloadDir), ['.qdm-' + id + '.partial'],
    'Paused SHA validation must retain one private direct-write file, never a published output');
  const requestIndex = context.fixture.requests.length;
  await context.client.resume(id);
  await complete(context, id);
  const bodyRequests = context.fixture.requests.slice(requestIndex)
    .filter((request) => request.requestedLength > 64 * KiB);
  assert.equal(bodyRequests.length, 0, 'Resuming assembly fetched file bodies again');
  context.detail.pauseAssemblyMs = pauseAssemblyMs;
  context.detail.resumeBodyRequests = bodyRequests.length;
})));

test('single-stream-fallback-reports-one-real-lane', () => withFixture({
  sizeBytes: 4 * MiB + 123, rangeMode: 'ignore', chunkSize: 16 * KiB, delayMs: 2,
}, { connections: 64 }, async (context) => {
  const id = await context.client.add({ url: context.fixture.url, filename: 'single-lane.bin' });
  await waitFor(async () => {
    const task = await context.client.task(id);
    return task?.downloaded_bytes > 64 * KiB ? task : null;
  }, 'single-stream progress');
  const progress = await context.client.task(id);
  assert.equal(progress.lanes.length, 1);
  assert.equal(progress.lanes[0].id, 1);
  assert.ok(progress.lanes[0].current_bytes > 0, 'Single-stream worker progress is stale');
  assert.equal(progress.lane_history_estimated, false);
  const completed = await complete(context, id);
  assert.equal(completed.lanes[0].current_bytes, context.fixture.behavior.sizeBytes);
  assert.equal(completed.lanes[0].transferred_bytes, context.fixture.behavior.sizeBytes);
  assert.equal(completed.lanes[0].active, false);
}));

async function main() {
  const selected = filter ? cases.filter((item) => item.name.includes(filter)) : cases;
  assert.ok(selected.length, `No test names match ${filter}`);
  const report = {
    schema: 1, suite: 'QDM independent executable acceptance',
    startedAt: new Date().toISOString(), binary,
    binarySha256: await fileHash(binary), platform: process.platform,
    architecture: process.arch, node: process.version, os: os.release(),
    testedSubject: 'Actual QDM executable; origin fixture and test driver are a separate process',
    cases: [],
  };
  for (const item of selected) {
    const started = Date.now();
    process.stdout.write(`RUN  ${item.name}\n`);
    try {
      const detail = await item.run();
      const result = { name: item.name, status: 'passed', elapsedMs: Date.now() - started, detail };
      report.cases.push(result);
      process.stdout.write(`PASS ${item.name} (${result.elapsedMs} ms)\n`);
    } catch (error) {
      const result = { name: item.name, status: 'failed', elapsedMs: Date.now() - started, error: error.message, stack: error.stack, evidence: error.evidence };
      report.cases.push(result);
      process.stdout.write(`FAIL ${item.name}: ${error.message.slice(0, 3500)}\n`);
    }
    if (reportPath) {
      await fs.mkdir(path.dirname(path.resolve(reportPath)), { recursive: true });
      await fs.writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`);
    }
  }
  report.binarySha256After = await fileHash(binary);
  report.binaryUnchangedDuringRun = report.binarySha256After === report.binarySha256;
  if (!report.binaryUnchangedDuringRun) {
    report.cases.push({ name: 'executable-provenance', status: 'failed', elapsedMs: 0,
      error: 'The executable changed during this run. Results cannot certify one distributed build.' });
  }
  report.finishedAt = new Date().toISOString();
  report.passed = report.cases.filter((item) => item.status === 'passed').length;
  report.failed = report.cases.length - report.passed;
  if (reportPath) await fs.writeFile(reportPath, `${JSON.stringify(report, null, 2)}\n`);
  process.stdout.write(`RESULT ${report.passed}/${report.cases.length} passed; ${report.failed} failed\n`);
  if (report.failed) process.exitCode = 1;
}

main().catch((error) => { process.stderr.write(`${error.stack || error}\n`); process.exitCode = 1; });

