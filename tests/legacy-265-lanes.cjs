#!/usr/bin/env node
'use strict';

// Read one completed-history snapshot, then test only isolated copies. A real
// installation may already have accurate worker history, so it is not itself
// evidence of the old pre-lane schema.
const fs = require('node:fs/promises');
const path = require('node:path');
const os = require('node:os');
const assert = require('node:assert/strict');
const crypto = require('node:crypto');
const { CliClient, fileHash } = require('./helpers/cli-client.cjs');
function option(name, fallback) {
  const index = process.argv.indexOf(name);
  return index >= 0 ? process.argv[index + 1] : fallback;
}
const binary = path.resolve(option('--binary', path.join(__dirname, '../target/release/QDM.exe')));
const sourcePath = option('--state', process.env.APPDATA ? path.join(process.env.APPDATA, 'QDM', 'state-v2.json') : null);
const reportPath = option('--report', null);

(async () => {
  assert.ok(sourcePath, 'Supply --state with a read-only completed-history source.');
  const sourceBytes = await fs.readFile(sourcePath);
  const saved = JSON.parse(sourceBytes.toString('utf8'));
  const completed = saved.tasks.find(task => task.view.status === 'completed' && task.parts.length >= 200);
  assert.ok(completed, 'A completed history record with at least 200 raw chunks is required; live history is never modified.');
  assert.equal(completed.lane_activity?.length, 64, 'The current-format control needs 64 persisted worker records.');
  assert.equal(completed.view.lane_history_estimated, false, 'The current-format control needs accurate stored worker attribution.');
  const chunks = completed.parts.length;
  const total = completed.view.total_bytes;
  assert.equal(completed.parts.reduce((sum, part) => sum + part.downloaded, 0), total);
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'qdm-legacy-lanes-'));
  const report = {
    schema: 1, suite: 'Completed history attribution and explicit legacy-shape migration',
    startedAt: new Date().toISOString(), binary, binarySha256: await fileHash(binary),
    sourceSnapshotSha256: crypto.createHash('sha256').update(sourceBytes).digest('hex'),
    sourceAccess: 'One read-only snapshot; all test mutations and engine processes use isolated copies.',
    rawChunks: chunks, totalBytes: total, cases: [], status: 'running',
  };
  try {
    for (const legacy of [false, true]) {
      const name = legacy ? 'explicit-legacy-without-worker-records-is-estimated' : 'accurate-persisted-worker-attribution-is-preserved';
      const data = path.join(root, legacy ? 'legacy-state' : 'current-state');
      const out = path.join(root, legacy ? 'legacy-downloads' : 'current-downloads');
      await fs.mkdir(data, { recursive: true });
      await fs.mkdir(out, { recursive: true });
      const task = structuredClone(completed);
      task.view.directory = out;
      task.view.file_path = path.join(out, task.view.filename);
      task.pending_output = null;
      if (legacy) {
        delete task.lane_activity;
        delete task.view.lanes;
        delete task.view.lane_history_estimated;
        for (const part of task.parts) delete part.owner_lane;
      }
      const fixture = {
        ...saved,
        settings: { ...saved.settings, connections: 64, download_dir: out },
        tasks: [task],
      };
      await fs.writeFile(path.join(data, 'state-v2.json'), JSON.stringify(fixture));
      const client = new CliClient(binary, data, out);
      try {
        const snapshot = await client.snapshot();
        const restored = snapshot.downloads.find(value => value.id === task.view.id);
        assert.ok(restored, 'Completed task disappeared');
        assert.equal(restored.status, 'completed');
        assert.equal(restored.downloaded_bytes, total);
        assert.equal(restored.part_count, chunks);
        assert.equal(restored.segments.length, chunks, 'All raw chunk metadata must remain');
        assert.deepEqual(restored.segments.map(part => [part.id, part.start, part.end, part.downloaded_bytes]),
          [...completed.parts].sort((a, b) => a.start - b.start).map(part => [part.id, part.start, part.end, part.downloaded]));
        assert.equal(restored.lanes.length, 64, 'UI must expose exactly 64 configured worker slots');
        assert.deepEqual(restored.lanes.map(lane => lane.id), Array.from({ length: 64 }, (_, index) => index + 1));
        assert.equal(restored.lanes.reduce((sum, lane) => sum + lane.transferred_bytes, 0), total);
        assert.equal(restored.lane_history_estimated, legacy);
        if (!legacy) {
          assert.deepEqual(restored.lanes.map(lane => lane.transferred_bytes),
            completed.lane_activity.map(lane => lane.transferred_bytes), 'Exact historical worker attribution changed');
        }
        assert.equal(snapshot.active_count, 0, 'Completed history must not start a network transfer');
        report.cases.push({ name, status: 'passed', chunks, lanes: 64, bytes: total, estimated: legacy });
        console.log(`PASS ${name}: ${chunks} chunks, 64 slots, ${total} bytes`);
      } finally {
        try { await client.shutdown(); } catch (error) { await client.crash(); throw error; }
      }
    }
    report.binarySha256After = await fileHash(binary);
    assert.equal(report.binarySha256After, report.binarySha256, 'Executable changed during history tests');
    report.status = 'passed';
  } catch (error) {
    report.status = 'failed';
    report.error = error.stack || String(error);
    throw error;
  } finally {
    report.finishedAt = new Date().toISOString();
    report.passed = report.cases.length;
    report.failed = report.status === 'passed' ? 0 : 1;
    if (reportPath) {
      await fs.mkdir(path.dirname(path.resolve(reportPath)), { recursive: true });
      await fs.writeFile(reportPath, JSON.stringify(report, null, 2) + '\n');
    }
    await fs.rm(root, { recursive: true, force: true, maxRetries: 5, retryDelay: 100 });
  }
})().catch(error => { console.error('FAIL ' + (error.stack || error)); process.exitCode = 1; });

