#!/usr/bin/env node
'use strict';
// Causal diagnostic, not a speed benchmark: retain production engine traces for
// three jobs with controlled idle gaps, without changing the executable.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const syncFs = require('node:fs');
const path = require('node:path');
const os = require('node:os');
const readline = require('node:readline');
const { createHash, randomUUID } = require('node:crypto');
const { CliClient, delay, fileHash } = require('./helpers/cli-client.cjs');

const option = (name, fallback) => {
  const index = process.argv.indexOf(name);
  return index < 0 ? fallback : process.argv[index + 1];
};
const TRACE_PREFIX = 'QDM_TRANSFER_TRACE ';
const TRACE_LIMIT = 16 * 1024 * 1024;

function distribution(values) {
  const sorted = values.filter(Number.isFinite).sort((a, b) => a - b);
  if (!sorted.length) return null;
  const middle = Math.floor(sorted.length / 2);
  return { count: sorted.length, min: sorted[0],
    median: sorted.length % 2 ? sorted[middle] : (sorted[middle - 1] + sorted[middle]) / 2,
    p95: sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * 0.95) - 1)],
    max: sorted.at(-1) };
}

function summarizeTrace(records, taskId, dispatchAt) {
  const events = records.filter(record => record.task === taskId);
  const eventCounts = {}, responses = {}, remotes = {}, caches = {};
  const requests = [], latest = new Map(), checkpoints = [], phases = {};
  const addCount = (map, value) => { const key = String(value ?? '(absent)'); map[key] = (map[key] || 0) + 1; };
  for (const event of events) {
    addCount(eventCounts, event.event);
    if (['probe-send', 'probe-complete', 'final-verification-start', 'assembly-start', 'assembly-end', 'published'].includes(event.event)) {
      phases[event.event] = { atMs: event.atMs, sinceDispatchMs: event.atMs - dispatchAt };
    }
    if (event.event === 'request-send') {
      const request = { part: event.part, ordinal: requests.length + 1, slot: event.slot,
        start: event.start, endInclusive: event.end, savedBytes: event.savedBytes,
        sentAtMs: event.atMs, sentSinceDispatchMs: event.atMs - dispatchAt };
      requests.push(request); latest.set(event.part, request);
    }
    const request = latest.get(event.part);
    if (event.event === 'headers') {
      addCount(responses, event.status); addCount(remotes, event.remote); addCount(caches, event.cache);
      if (request) Object.assign(request, { headersAtMs: event.atMs, headersMs: event.setupMs,
        status: event.status, remote: event.remote, server: event.server,
        cache: event.cache, retryAfter: event.retryAfter, contentRange: event.contentRange });
    } else if (request && event.event === 'first-body') {
      Object.assign(request, { firstBodyAtMs: event.atMs, firstBodyMs: event.elapsedMs });
    } else if (request && event.event === 'first-payload') {
      Object.assign(request, { firstPayloadAtMs: event.atMs, firstPayloadMs: event.elapsedMs });
    } else if (request && event.event === 'last-owned-payload') {
      Object.assign(request, { lastOwnedPayloadAtMs: event.atMs,
        lastOwnedPayloadMs: event.elapsedMs, finalOwnedBytes: event.downloaded });
    } else if (request && event.event === 'attempt-finished') {
      Object.assign(request, { workerFinishedAtMs: event.atMs,
        workerAttemptMs: event.elapsedMs, result: event.result, fault: event.fault });
    }
    if (event.event === 'checkpoint-end') checkpoints.push({ part: event.part,
      atMs: event.atMs, bytesDurableAfterCheckpoint: event.bytes, elapsedMs: event.elapsedMs });
  }
  const phaseDelta = (end, start) => phases[end] && phases[start] ? phases[end].atMs - phases[start].atMs : null;
  const lastPayload = events.filter(e => e.event === 'last-owned-payload').at(-1);
  return { eventCounts, responseStatuses: responses, remoteEndpoints: remotes, cacheHeaders: caches,
    phases, probeMs: phaseDelta('probe-complete', 'probe-send'),
    validationMs: phaseDelta('assembly-start', 'final-verification-start'),
    assemblyMs: phaseDelta('assembly-end', 'assembly-start'),
    publishAfterAssemblyMs: phaseDelta('published', 'assembly-end'),
    lastOwnedPayloadSinceDispatchMs: lastPayload ? lastPayload.atMs - dispatchAt : null,
    requestHeadersMs: distribution(requests.map(r => r.headersMs)),
    requestFirstBodyMs: distribution(requests.map(r => r.firstBodyMs)),
    requestFirstPayloadMs: distribution(requests.map(r => r.firstPayloadMs)),
    checkpointMs: distribution(checkpoints.map(c => c.elapsedMs)),
    originBackoffs: events.filter(e => e.event === 'origin-backoff'),
    splits: events.filter(e => e.event === 'split'),
    zeroPayloadSplits: events.filter(e => e.event === 'split' && !(e.parentResponsePayload > 0)).length,
    requests, checkpoints,
    limits: 'Trace identifies response endpoints but not local TCP ports or connection IDs; reuse is not directly established. Worker attempts can contain multiple HTTP requests.' };
}

async function boundedHash(filename, deadline) {
  const controller = new AbortController();
  const remaining = deadline - Date.now();
  if (remaining <= 0) throw new Error('Sequence observation budget expired before independent hash');
  const timer = setTimeout(() => controller.abort(), remaining);
  try {
    const hash = createHash('sha256');
    for await (const bytes of syncFs.createReadStream(filename, { signal: controller.signal })) hash.update(bytes);
    return hash.digest('hex');
  } finally { clearTimeout(timer); }
}

async function main() {
  const binary = path.resolve(option('--binary', 'target/release/QDM.exe'));
  const reportPath = path.resolve(option('--report', 'docs/evidence/slint-idm-20261027/public-origin-sequence.json'));
  const baseUrl = new URL(option('--url', 'https://dl-cdn.alpinelinux.org/alpine/v3.24/releases/x86_64/alpine-virt-3.24.3-x86_64.iso'));
  const expectedSha256 = option('--sha256', 'cdc1bc0efc64d8d611bf3733f54868172a545888dfd726f518bf279bcb0ff1af').toLowerCase();
  const expectedBytes = Number(option('--bytes', '70254592'));
  const budgetMs = Number(option('--timeout-ms', '90000'));
  const connections = Number(option('--connections', '32'));
  const gapsMs = option('--gaps-ms', '8000,35000').split(',').map(Number);
  const freshProcess = process.argv.includes('--fresh-process-between-jobs');
  assert.ok(['http:', 'https:'].includes(baseUrl.protocol) && !baseUrl.username && !baseUrl.password);
  assert.match(expectedSha256, /^[a-f\d]{64}$/);
  assert.ok(Number.isSafeInteger(expectedBytes) && expectedBytes > 0);
  assert.ok(Number.isInteger(budgetMs) && budgetMs >= 1000 && budgetMs <= 90000);
  assert.ok(Number.isInteger(connections) && connections >= 1 && connections <= 64);
  assert.ok(gapsMs.length === 2 && gapsMs.every(n => Number.isInteger(n) && n >= 0 && n <= 60000));
  assert.ok(gapsMs.reduce((a, b) => a + b, 0) < budgetMs, 'Gaps consume the observation budget');
  const workdir = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-origin-sequence-'));
  const dataDir = path.join(workdir, 'state'), downloadDir = path.join(workdir, 'files');
  await fs.mkdir(downloadDir, { recursive: true });
  await fs.mkdir(path.dirname(reportPath), { recursive: true });
  const tracePath = reportPath.replace(/\.json$/i, '') + '.trace.jsonl';
  const report = { schema: 1, suite: 'Three-job actual QDM public-origin causal diagnostic',
    status: 'running', startedAt: new Date().toISOString(), binary,
    binarySha256: await fileHash(binary), baseUrl: baseUrl.href, expectedBytes, expectedSha256,
    configuredConnections: connections, observationBudgetMs: budgetMs, requestedGapsMs: gapsMs,
    isolation: freshProcess ? 'fresh-process-between-jobs' : 'same-process-shared-client',
    workdir, dataDir, downloadDir, traceFile: tracePath, plannedJobs: 3, processes: [], jobs: [],
    method: {
      purpose: 'Causal trace only; tracing and snapshot polling can affect timing. Not an IDM benchmark or a replacement for any slow measured run.',
      executable: 'Unchanged actual executable in CLI mode; same transfer engine and normal TLS; isolated profile, one active job, no bandwidth limit.',
      urls: 'Each job adds a unique qdm_benchmark query nonce; no prerequisite reference GET.',
      continuity: freshProcess ? 'Restart only this owned process between jobs, preserving its isolated completed history. This resets the client pool; origin/IP state may persist.' : 'All jobs share one Engine and its existing reqwest client/pool. The source currently configures a 30 s idle timeout.',
      gaps: 'Minimum requested gap starts at internal completed_at when available. Independent hashing and process startup can lengthen it; actual gaps are recorded.',
      budget: 'At most90s from first dispatch, including gaps, job observations and independent file hashing; bounded owned-process cleanup is additional.',
      bytes: 'Snapshots show accepted payload including bounded write buffers. Checkpoint-end trace records show durable bytes for that part at that time. Assembly counters show output writes. These are distinct measures.',
      telemetry: 'Snapshots requested every100ms; scalar samples every250ms/status transition, with up to256 unfinished part geometries. Complete final view, raw trace and saved state retained.',
      limits: 'Remote endpoint/cache/status and timing traces do not expose TCP connection IDs. A warm-pool association alone does not prove reuse caused a delay.',
      preservation: 'Only this newly created profile/process/output is used. No IDM, browser or production state is read or modified.' },
  };
  let client, session, deadline, lastPersistAt = 0, traceBytes = 0, storedTraceBytes = 0;
  const traceChunks = [], traceRecords = [];
  const persist = async () => { await fs.writeFile(reportPath, JSON.stringify(report, null, 2) + '\n'); lastPersistAt = Date.now(); };
  const remaining = () => deadline ? Math.max(0, deadline - Date.now()) : 5000;
  const command = (operation, fields = {}) => {
    const time = Math.min(5000, remaining());
    if (!time) throw new Error('Sequence observation budget expired');
    return client.command(operation, fields, time);
  };
  const closeClient = async () => {
    if (!client) return;
    const owned = client; client = null;
    try { session.shutdown = await owned.shutdown(); }
    catch (error) { session.shutdownError = String(error); await owned.crash(); throw error; }
    finally { session.closedAt = new Date().toISOString(); session.stderrTail = owned.stderr; }
  };
  const startClient = async () => {
    const priorTrace = process.env.QDM_TRANSFER_TRACE;
    const priorDelay = process.env.QDM_TEST_ASSEMBLY_DELAY_MS;
    process.env.QDM_TRANSFER_TRACE = '1'; delete process.env.QDM_TEST_ASSEMBLY_DELAY_MS;
    try { client = new CliClient(binary, dataDir, downloadDir); }
    finally {
      if (priorTrace === undefined) delete process.env.QDM_TRANSFER_TRACE; else process.env.QDM_TRANSFER_TRACE = priorTrace;
      if (priorDelay === undefined) delete process.env.QDM_TEST_ASSEMBLY_DELAY_MS; else process.env.QDM_TEST_ASSEMBLY_DELAY_MS = priorDelay;
    }
    session = { processId: client.child.pid, startedAt: new Date().toISOString() };
    report.processes.push(session);
    client.child.stderr.on('data', bytes => {
      traceBytes += bytes.length;
      const take = Math.min(bytes.length, TRACE_LIMIT - storedTraceBytes);
      if (take > 0) { traceChunks.push(bytes.subarray(0, take)); storedTraceBytes += take; }
      if (take < bytes.length) report.traceTruncated = true;
    });
    const lines = readline.createInterface({ input: client.child.stderr });
    lines.on('line', line => {
      if (!line.startsWith(TRACE_PREFIX) || report.traceTruncated) return;
      try { traceRecords.push(JSON.parse(line.slice(TRACE_PREFIX.length))); }
      catch { report.traceParseErrors = (report.traceParseErrors || 0) + 1; }
    });
    const initial = await command('snapshot');
    await command('settings', { settings: { ...initial.settings, connections,
      max_concurrent: 1, speed_limit_kib: 0, close_to_tray: false } });
    return initial;
  };
  try {
    await persist();
    await startClient();
    for (let index = 0; index < 3; index++) {
      const previous = report.jobs.at(-1);
      if (previous) {
        if (freshProcess) { await closeClient(); await startClient(); }
        const notBefore = previous.internalCompletedAtMs + gapsMs[index - 1];
        while (Date.now() < notBefore) {
          if (remaining() <= 0) throw new Error('Sequence budget expired during the planned idle gap');
          await delay(Math.min(500, notBefore - Date.now(), remaining()));
        }
      }
      if (!deadline) { report.firstDispatchAtMs = Date.now(); deadline = report.firstDispatchAtMs + budgetMs; }
      if (remaining() <= 0) throw new Error('Sequence budget expired before the next dispatch');
      const url = new URL(baseUrl); url.searchParams.set('qdm_benchmark', 'sequence-' + randomUUID());
      const row = { index: index + 1, status: 'running', processId: client.child.pid, url: url.href,
        dispatchAtMs: Date.now(), requestedGapMs: index ? gapsMs[index - 1] : null,
        gapAfterPriorInternalCompletionMs: previous ? Date.now() - previous.internalCompletedAtMs : null,
        samples: [], transitions: [] };
      report.jobs.push(row); await persist();
      try {
        row.taskId = await command('add', { request: { url: row.url,
          filename: 'qdm-sequence-' + (index + 1) + '.bin', start: true, expected_sha256: expectedSha256 } });
        let lastSampleAt = 0, lastTransition = '', completed = false;
        while (remaining() > 0) {
          const snapshot = await command('snapshot');
          const task = snapshot.downloads.find(value => value.id === row.taskId);
          assert.ok(task, 'QDM lost the sequence task');
          const at = Date.now();
          assert.ok(task.downloaded_bytes <= expectedBytes, 'Progress exceeded expected size');
          if (task.total_bytes !== null) assert.equal(task.total_bytes, expectedBytes, 'Origin length changed');
          const sample = { atMs: at, elapsedMs: at - row.dispatchAtMs, status: task.status,
            acceptedBytes: task.downloaded_bytes, networkBytes: task.network_bytes,
            speedBytesPerSecond: task.speed_bps, activeWorkers: task.connections_active,
            peakWorkers: task.peak_connections, partCount: task.part_count, splits: task.dynamic_splits,
            assemblyBytes: task.assembly_bytes, assemblyTotalBytes: task.assembly_total_bytes,
            notice: task.notice, error: task.error };
          const signature = JSON.stringify([task.status, task.notice, task.error, task.connections_active]);
          if (signature !== lastTransition) { row.transitions.push(sample); lastTransition = signature; }
          if (!lastSampleAt || at - lastSampleAt >= 250 || ['completed', 'error', 'failed'].includes(task.status)) {
            const unfinished = task.segments.filter(part => part.active || part.downloaded_bytes < part.end - part.start);
            row.samples.push({ ...sample, unfinishedParts: unfinished.slice(0, 256), unfinishedPartsOmitted: Math.max(0, unfinished.length - 256) });
            lastSampleAt = at;
          }
          row.lastTask = task;
          if (at - lastPersistAt >= 1000) await persist();
          if (['error', 'failed'].includes(task.status)) throw new Error(task.error || task.notice || 'Task failed');
          if (task.status === 'completed') {
            row.completionObservedAtMs = at;
            row.dispatchToCompletionObservedMs = at - row.dispatchAtMs;
            row.internalCompletedAtMs = task.completed_at || at;
            row.output = task.file_path;
            assert.equal((await fs.stat(row.output)).size, expectedBytes);
            row.actualSha256 = await boundedHash(row.output, deadline);
            assert.equal(row.actualSha256, expectedSha256, 'Independent SHA-256 mismatch');
            row.independentHashVerified = true;
            // Do not let a still-cleaning owner confound the next job's admission.
            while ((await command('snapshot')).active_count !== 0) {
              if (remaining() <= 0) throw new Error('Owner did not release before sequence budget');
              await delay(Math.min(25, remaining()));
            }
            row.ownerReleasedObservedAtMs = Date.now();
            row.status = 'passed'; completed = true; break;
          }
          await delay(Math.min(100, remaining()));
        }
        if (!completed) { row.timedOut = true; throw new Error('Sequence observation budget expired during job'); }
      } catch (error) { row.status = 'failed'; row.error = error.stack || String(error); throw error; }
      finally { row.traceSummary = summarizeTrace(traceRecords, row.taskId, row.dispatchAtMs); await persist(); }
      console.log(JSON.stringify({ event: 'job-finished', index: row.index, status: row.status,
        elapsedMs: row.dispatchToCompletionObservedMs, gapMs: row.gapAfterPriorInternalCompletionMs,
        headersMs: row.traceSummary.requestHeadersMs, firstBodyMs: row.traceSummary.requestFirstBodyMs,
        parts: row.lastTask?.part_count, splits: row.lastTask?.dynamic_splits }));
    }
    report.status = 'passed';
  } catch (error) {
    report.status = 'failed'; report.error = error.stack || String(error); process.exitCode = 1;
  } finally {
    report.observationEndedAt = new Date().toISOString();
    try { await closeClient(); }
    catch (error) { report.shutdownError = String(error); report.status = 'failed'; process.exitCode = 1; }
    try {
      const saved = JSON.parse((await fs.readFile(path.join(dataDir, 'state-v2.json'), 'utf8')).replace(/^\uFEFF/, ''));
      for (const row of report.jobs) {
        const task = saved.tasks.find(value => value.view.id === row.taskId);
        row.savedDurableBytesAfterShutdown = task?.parts?.reduce((sum, part) => sum + part.durable, 0) ?? null;
        row.savedStatusAfterShutdown = task?.view.status || null;
        row.traceSummary = summarizeTrace(traceRecords, row.taskId, row.dispatchAtMs);
      }
    } catch (error) { report.savedStateReadError = String(error); }
    report.traceBytes = traceBytes; report.traceStoredBytes = storedTraceBytes;
    await fs.writeFile(tracePath, Buffer.concat(traceChunks));
    report.traceSha256 = await fileHash(tracePath);
    report.binarySha256After = await fileHash(binary);
    if (report.binarySha256After !== report.binarySha256 || report.traceTruncated || report.traceParseErrors) {
      report.status = 'failed'; report.diagnosticEvidenceIncomplete = true; process.exitCode = 1;
    }
    report.finishedAt = new Date().toISOString();
    report.passedJobs = report.jobs.filter(row => row.status === 'passed').length;
    report.failedJobs = report.jobs.filter(row => row.status === 'failed').length;
    report.notRunJobs = 3 - report.jobs.length;
    await persist();
    console.log(JSON.stringify({ status: report.status, report: reportPath, trace: tracePath,
      workdir, passed: report.passedJobs, failed: report.failedJobs, notRun: report.notRunJobs }));
  }
}

module.exports = { summarizeTrace, distribution };
if (require.main === module) main().catch(error => { console.error(error.stack || error); process.exitCode = 1; });
