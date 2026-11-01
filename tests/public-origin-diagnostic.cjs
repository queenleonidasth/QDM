#!/usr/bin/env node
'use strict';
// One bounded, real-origin transfer through the actual QDM executable. No
// prerequisite reference GET or use of a production profile/browser/IDM.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const { CliClient, delay, fileHash } = require('./helpers/cli-client.cjs');

const option = (name, fallback) => {
  const index = process.argv.indexOf(name);
  return index < 0 ? fallback : process.argv[index + 1];
};
const binary = path.resolve(option('--binary', 'target/release/QDM.exe'));
const reportPath = path.resolve(option('--report', 'docs/evidence/slint-idm-20261027/public-origin-diagnostic.json'));
const url = option('--url', 'https://bom1-ynm1.speedtest.network.ovh.net/files/100Mb.dat');
const expectedSha256 = option('--sha256', '20492a4d0d84f8beb1767f6616229f85d44c2827b64bdbfb260ee12fa1109e0e');
const expectedBytes = Number(option('--bytes', '104857600'));
const timeoutMs = Number(option('--timeout-ms', '90000'));
const connections = Number(option('--connections', '32'));
const tracing = process.argv.includes('--trace');
delete process.env.QDM_TEST_ASSEMBLY_DELAY_MS;

function adaptiveNotice(notice) {
  const match = /reducing connections to (\d+)|HTTP 429; (\d+) connections|testing (\d+) connections/i.exec(notice || '');
  return match ? Number(match[1] || match[2] || match[3]) : null;
}

(async () => {
  assert.ok(/^https?:\/\//.test(url), 'Use an HTTP(S) origin URL');
  assert.match(expectedSha256, /^[a-f\d]{64}$/i);
  assert.ok(Number.isInteger(expectedBytes) && expectedBytes > 0);
  assert.ok(timeoutMs >= 1000 && timeoutMs <= 90000, 'Transfer budget must be between1s and90s');
  assert.ok(Number.isInteger(connections) && connections >= 1 && connections <= 64);
  const workdir = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-public-origin-diagnostic-'));
  const downloadDir = path.join(workdir, 'files');
  await fs.mkdir(downloadDir, { recursive: true });
  await fs.mkdir(path.dirname(reportPath), { recursive: true });
  const report = {
    schema: 1, suite: 'Bounded actual QDM public-origin recovery diagnostic',
    status: 'running', startedAt: new Date().toISOString(), binary,
    binarySha256: await fileHash(binary), url, expectedBytes, expectedSha256,
    transferBudgetMs: timeoutMs, configuredConnections: connections, workdir,
    samples: [], transitions: [],
    method: {
      reference: 'Supplied expected SHA-256; no additional reference GET before the transfer',
      executable: 'Actual QDM CLI with a new isolated profile; normal TLS validation and transfer engine',
      timeout: 'At most90s observation budget from adding the task, followed by bounded graceful shutdown of only this owned process',
      telemetry: 'Snapshot every100ms; periodic sample each1s and immediate status/notice/worker-count transitions. Active counts are engine worker ownership, not packet-captured TCP sockets.',
      adaptiveLimit: 'Only inferred from explicit engine notices, because TaskView does not serialize the internal adaptive_limit; null before an observed notice',
      preservation: 'The report, isolated state, partial files, and completed output are retained on success and failure',
    },
  };
  let client, taskId, dispatchAt, lastSampleAt = 0, lastPersistAt = 0, lastTransition = '', lastAdaptive = null;
  const traceChunks = [];
  let traceBytes = 0;
  const persist = async () => {
    await fs.writeFile(reportPath, JSON.stringify(report, null, 2) + '\n');
    lastPersistAt = Date.now();
  };
  const capture = (task, at) => {
    const fromNotice = adaptiveNotice(task.notice);
    if (fromNotice !== null) lastAdaptive = fromNotice;
    const observation = {
      at: new Date(at).toISOString(), elapsedMs: at - dispatchAt,
      status: task.status, downloadedBytes: task.downloaded_bytes, totalBytes: task.total_bytes,
      networkBytes: task.network_bytes, speedBytesPerSecond: task.speed_bps,
      activeWorkers: task.connections_active, configuredLimit: task.connections_limit,
      adaptiveLimitInferredFromNotice: lastAdaptive, peakWorkers: task.peak_connections,
      parts: task.part_count, dynamicSplits: task.dynamic_splits,
      assemblyBytes: task.assembly_bytes, assemblyTotalBytes: task.assembly_total_bytes,
      notice: task.notice, error: task.error,
    };
    const transition = JSON.stringify([task.status, task.connections_active, task.notice, task.error, lastAdaptive]);
    const changed = transition !== lastTransition;
    if (changed) {
      report.transitions.push(observation);
      lastTransition = transition;
      console.log(JSON.stringify({ event: 'transition', ...observation }));
    }
    if (!lastSampleAt || at - lastSampleAt >= 1000 || ['completed', 'error', 'failed'].includes(task.status)) {
      report.samples.push(observation); lastSampleAt = at;
    }
    report.lastTask = task;
    return changed;
  };
  try {
    await persist();
    const previousTrace = process.env.QDM_TRANSFER_TRACE;
    if (tracing) process.env.QDM_TRANSFER_TRACE = '1';
    try { client = new CliClient(binary, path.join(workdir, 'state'), downloadDir); }
    finally {
      if (previousTrace === undefined) delete process.env.QDM_TRANSFER_TRACE;
      else process.env.QDM_TRANSFER_TRACE = previousTrace;
    }
    if (tracing) client.child.stderr.on('data', bytes => {
      traceBytes += bytes.length;
      if (traceBytes <= 16 * 1024 * 1024) traceChunks.push(bytes);
      else report.traceTruncated = true;
    });
    report.processId = client.child.pid;
    const initial = await client.command('snapshot', {}, 5000);
    await client.command('settings', { settings: { ...initial.settings, connections,
      max_concurrent: 1, speed_limit_kib: 0, close_to_tray: false } }, 5000);
    dispatchAt = Date.now(); report.dispatchAt = new Date(dispatchAt).toISOString();
    taskId = await client.command('add', { request: {
      url, filename: 'qdm-public-origin-diagnostic.bin', start: true,
      expected_sha256: expectedSha256,
    } }, Math.min(5000, timeoutMs));
    report.taskId = taskId;
    const deadline = dispatchAt + timeoutMs;
    let finished = false;
    while (Date.now() < deadline) {
      const snapshot = await client.command('snapshot', {}, Math.max(1, Math.min(2000, deadline - Date.now())));
      const task = snapshot.downloads.find(value => value.id === taskId);
      assert.ok(task, 'QDM lost the diagnostic task');
      const changed = capture(task, Date.now());
      if (task.total_bytes !== null) assert.equal(task.total_bytes, expectedBytes, 'Origin size changed');
      assert.ok(task.downloaded_bytes <= expectedBytes, 'File progress exceeded expected size');
      if (changed || Date.now() - lastPersistAt >= 1000) await persist();
      if (['error', 'failed'].includes(task.status)) throw new Error(task.error || task.notice || `QDM ended ${task.status}`);
      if (task.status === 'completed') {
        report.completionObservedAt = new Date().toISOString();
        report.dispatchToCompletionObservedMs = Date.now() - dispatchAt;
        assert.equal((await fs.stat(task.file_path)).size, expectedBytes);
        report.actualSha256 = await fileHash(task.file_path);
        assert.equal(report.actualSha256, expectedSha256, 'Independent SHA-256 mismatch');
        report.output = task.file_path;
        report.independentHashVerified = true;
        report.status = 'passed'; finished = true; break;
      }
      await delay(Math.min(100, Math.max(0, deadline - Date.now())));
    }
    if (!finished) {
      report.timedOut = true;
      throw new Error(`QDM did not complete within the${timeoutMs}ms transfer observation budget`);
    }
  } catch (error) {
    report.status = 'failed'; report.error = error.stack || String(error); process.exitCode = 1;
    console.error(report.error);
  } finally {
    await persist();
    if (client) {
      report.stderr = client.stderr;
      try { report.shutdown = await client.shutdown(); }
      catch (error) {
        report.shutdownError = error.stack || String(error);
        await client.crash(); report.status = 'failed'; process.exitCode = 1;
      }
      try {
        const saved = JSON.parse(await fs.readFile(path.join(workdir, 'state', 'state-v2.json'), 'utf8'));
        const ownedTask = saved.tasks.find(value => value.view.id === taskId);
        report.savedTaskAfterShutdown = ownedTask?.view || null;
        report.savedDurableBytes = ownedTask?.parts?.reduce((sum, part) => sum + part.durable, 0) ?? null;
      } catch (error) { report.savedStateReadError = String(error); }
    }
    if (tracing) {
      report.traceFile = reportPath.replace(/\.json$/i, '') + '.trace.jsonl';
      report.traceBytes = traceBytes;
      await fs.writeFile(report.traceFile, Buffer.concat(traceChunks));
    }
    report.binarySha256After = await fileHash(binary);
    if (report.binarySha256After !== report.binarySha256) { report.status = 'failed'; report.binaryChanged = true; process.exitCode = 1; }
    report.finishedAt = new Date().toISOString();
    await persist();
    console.log(JSON.stringify({ status: report.status, report: reportPath, workdir,
      elapsedMs: report.dispatchToCompletionObservedMs, independentHashVerified: report.independentHashVerified || false,
      transitions: report.transitions.length, lastTask: report.lastTask?.status }, null, 2));
  }
})().catch(error => { console.error(error.stack || error); process.exitCode = 1; });
