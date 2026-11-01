from pathlib import Path
p=Path(r'C:\Users\QUEEN\Projects\QDM\native\tests\acceptance.cjs')
s=p.read_text(encoding='utf-8')
def change(old,new):
 global s
 n=s.count(old)
 if n!=1: raise ValueError((n,old[:130]))
 s=s.replace(old,new,1)
change("const { createHttpFixture, KiB, MiB } = require('./helpers/http-fixture.cjs');",
       "const { createHttpFixture, KiB, MiB } = require('./helpers/http-fixture.cjs');\nconst { openState, journalMode } = require('./helpers/sqlite-state.cjs');")
change("  const persisted = JSON.parse(await fs.readFile(path.join(context.dataDir, 'state-v2.json'), 'utf8'));",
       "  const persisted = openState(context.dataDir);")
old_line=next(line for line in s.splitlines() if "const partPath = path.join(context.dataDir, 'parts-v2'" in line)
change(old_line+"\n  assert.equal((await fs.stat(partPath)).size, saved.downloaded_bytes);",
       "  const partPath = path.join(context.downloadDir, '.qdm-' + id + '.partial');\n  assert.equal((await fs.stat(partPath)).size, context.fixture.behavior.sizeBytes,\n    'Single direct-write file holds the full sparse logical size');")
change("async function duringAssembly(context, id) {",
       "async function duringVerification(context, id) {")
change("return task?.status === 'assembling' && task.assembly_bytes > 0 &&",
       "return task?.status === 'verifying' && task.assembly_bytes > 0 &&")
change("}, 'an intermediate assembly state', 30000, 10);",
       "}, 'an intermediate direct-file SHA-256 verification state', 30000, 10);")
if s.count('await duringAssembly(context, id);')!=2:raise ValueError('duringAssembly calls missing')
s=s.replace('await duringAssembly(context, id);','await duringVerification(context, id);')
change("assert.ok(['assembling', 'completed'].includes(task.status),",
       "assert.ok(['verifying', 'completed'].includes(task.status),")
change("  assert.deepEqual(await fs.readdir(context.downloadDir), [],\n    'Paused assembly left a temporary or partially published output');",
       "  assert.deepEqual(await fs.readdir(context.downloadDir), ['.qdm-' + id + '.partial'],\n    'Paused SHA validation must retain one private direct-write file, never a published output');")
needle="test('buffered-small-writes-survive-pause-and-process-restart', () => withFixture({"
if s.count(needle)!=1:raise ValueError('test start')
extra="""test('direct-write-uses-one-sparse-partial-and-sqlite-wal', () => withFixture({
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

"""
s=s.replace(needle,extra+needle)
p.write_text(s,encoding='utf-8')
print('Updated acceptance with SQLite WAL, direct write, hash-only verification and new scenario')
