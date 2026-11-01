'use strict';

// End-to-end Firefox/Zen extension QA. Uses a NEW headless browser profile and
// the W3C WebDriver HTTP protocol; no Selenium/npm dependency or user profile.
// Required: --browser ABSOLUTE_BROWSER_EXE --driver ABSOLUTE_GECKODRIVER_EXE
const fs = require('node:fs/promises');
const path = require('node:path');
const http = require('node:http');
const net = require('node:net');
const crypto = require('node:crypto');
const { spawn } = require('node:child_process');
const os = require('node:os');
const { CliClient, fileHash } = require('./helpers/cli-client.cjs');

const root = path.resolve(__dirname, '..');
const flags = new Map();
for (let i = 2; i < process.argv.length; i += 2) flags.set(process.argv[i], process.argv[i + 1]);
const binary = path.resolve(flags.get('--binary') || path.join(root, 'target', 'release', 'QDM.exe'));
const browserPath = flags.get('--browser');
const driverPath = flags.get('--driver');
if (!browserPath || !driverPath || !path.isAbsolute(browserPath) || !path.isAbsolute(driverPath)) {
  console.error('Usage: node scripts/browser-qa.cjs --browser ABSOLUTE_FIREFOX_OR_ZEN_EXE --driver ABSOLUTE_GECKODRIVER_EXE [--output ABSOLUTE_JSON] [--extensions ABSOLUTE_EXTENSION_DIRECTORY]');
  process.exit(2);
}
const extensionDirectory = path.resolve(flags.get('--extensions') || path.join(root, 'extensions', 'firefox'));
const qaParent = path.resolve(flags.get('--workdir') || path.join(os.tmpdir(), 'qdm-zen-native-qa'));
const reportPath = path.resolve(flags.get('--output') || path.join(qaParent, 'browser-validation.json'));
const startedAt = new Date().toISOString();
const report = { suite: 'QDM native executable and actual Zen extension end-to-end QA', startedAt, passed: false, browserPath, driverPath, extensionDirectory, checks: [], limitations: [
  'Uses a newly created headless browser profile, not the user browser profile.',
  'Exercises the real options form and popup-to-background-to-QDM bridge path.',
  'Native operating-system context-menu clicking is not simulated; menu registration is separately queried through the extension API.',
  'The fixture is a controlled local HTTP server; this does not establish compatibility with authenticated websites.'
] };
let manager, bridge, fixture, driver, driverBase, sessionId, runDir, profileDir, driverExit = false, pairingToken = '';
let driverOutput = '';
let fixtureRequests = [];

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const hash = bytes => crypto.createHash('sha256').update(bytes).digest('hex');
function log(phase, detail = {}) { console.log(JSON.stringify({ phase, at: new Date().toISOString(), ...detail })); }
function check(name, evidence) { report.checks.push({ name, passed: true, evidence }); log(name, typeof evidence === 'object' ? evidence : { detail: evidence }); }
async function until(fn, description, timeout = 15000) {
  const end = Date.now() + timeout;
  let lastError;
  while (Date.now() < end) {
    try { const value = await fn(); if (value) return value; } catch (e) { lastError = e; }
    await delay(100);
  }
  throw new Error(`Timed out waiting for ${description}${lastError ? `: ${lastError.message}` : ''}`);
}

// Small deterministic store-only ZIP writer; sufficient for the extension's
// ordinary files and avoids downloading another build dependency for QA.
const crcTable = Array.from({ length: 256 }, (_, n) => {
  for (let i = 0; i < 8; i++) n = n & 1 ? (0xedb88320 ^ (n >>> 1)) : (n >>> 1);
  return n >>> 0;
});
function crc32(bytes) { let crc = 0xffffffff; for (const b of bytes) crc = crcTable[(crc ^ b) & 255] ^ (crc >>> 8); return (crc ^ 0xffffffff) >>> 0; }
async function zipExtension(directory, destination) {
  const files = [];
  async function walk(at, prefix = '') {
    for (const entry of (await fs.readdir(at, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      const relative = prefix ? `${prefix}/${entry.name}` : entry.name;
      if (entry.isDirectory()) await walk(path.join(at, entry.name), relative);
      else if (entry.isFile()) files.push({ name: relative, bytes: await fs.readFile(path.join(at, entry.name)) });
      else throw new Error(`Unexpected nonregular extension entry: ${relative}`);
    }
  }
  await walk(directory);
  const local = [], central = [];
  let offset = 0;
  for (const file of files) {
    const name = Buffer.from(file.name, 'utf8');
    const crc = crc32(file.bytes);
    const header = Buffer.alloc(30);
    header.writeUInt32LE(0x04034b50, 0); header.writeUInt16LE(20, 4); header.writeUInt16LE(0x0800, 6);
    header.writeUInt16LE(33, 12); header.writeUInt32LE(crc, 14); header.writeUInt32LE(file.bytes.length, 18); header.writeUInt32LE(file.bytes.length, 22); header.writeUInt16LE(name.length, 26);
    local.push(header, name, file.bytes);
    const directoryHeader = Buffer.alloc(46);
    directoryHeader.writeUInt32LE(0x02014b50, 0); directoryHeader.writeUInt16LE(20, 4); directoryHeader.writeUInt16LE(20, 6); directoryHeader.writeUInt16LE(0x0800, 8);
    directoryHeader.writeUInt16LE(33, 14); directoryHeader.writeUInt32LE(crc, 16); directoryHeader.writeUInt32LE(file.bytes.length, 20); directoryHeader.writeUInt32LE(file.bytes.length, 24); directoryHeader.writeUInt16LE(name.length, 28); directoryHeader.writeUInt32LE(offset, 42);
    central.push(directoryHeader, name);
    offset += header.length + name.length + file.bytes.length;
  }
  const centralBytes = Buffer.concat(central);
  const end = Buffer.alloc(22);
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(files.length, 8); end.writeUInt16LE(files.length, 10); end.writeUInt32LE(centralBytes.length, 12); end.writeUInt32LE(offset, 16);
  const zip = Buffer.concat([...local, centralBytes, end]);
  await fs.writeFile(destination, zip);
  return { fileCount: files.length, sha256: hash(zip), files: files.map(f => ({ path: f.name, sha256: hash(f.bytes), bytes: f.bytes.length })) };
}

async function freePort() {
  const server = net.createServer();
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const port = server.address().port;
  await new Promise(resolve => server.close(resolve));
  return port;
}

async function wire(method, endpoint, body, timeout = 30000) {
  const response = await fetch(`${driverBase}${endpoint}`, { method, headers: body === undefined ? {} : { 'Content-Type': 'application/json' }, body: body === undefined ? undefined : JSON.stringify(body), signal: AbortSignal.timeout(timeout) });
  const result = await response.json();
  if (!response.ok || result.value?.error) throw new Error(`WebDriver ${endpoint}: ${result.value?.message || result.value?.error || response.status}`);
  return result.value;
}
const command = (method, endpoint, body, timeout) => wire(method, `/session/${sessionId}${endpoint}`, body, timeout);
const execute = (script, args = []) => command('POST', '/execute/sync', { script, args });
async function element(selector) {
  const found = await command('POST', '/element', { using: 'css selector', value: selector });
  return found['element-6066-11e4-a52e-4f735466cecf'];
}
async function type(selector, text) {
  const id = await element(selector);
  await command('POST', `/element/${id}/clear`, {});
  await command('POST', `/element/${id}/value`, { text: String(text) });
}
async function click(selector) { await command('POST', `/element/${await element(selector)}/click`, {}); }
async function text(selector) { return execute('return document.querySelector(arguments[0])?.textContent || "";', [selector]); }

async function openExtensionPage(extensionId, relativePath) {
  // Firefox restricts content-context navigation to moz-extension: URLs. Use its
  // documented UI-test context on this owned profile, obtain the real extension
  // URL from the installed policy, and open a trusted tab through gBrowser.
  const oldHandles = new Set(await command('GET', '/window/handles'));
  let url;
  await command('POST', '/moz/context', { context: 'chrome' });
  try {
    url = await execute(`
      const policy = WebExtensionPolicy.getByID(arguments[0]);
      if (!policy) throw new Error('The installed extension has no active policy.');
      const url = policy.getURL(arguments[1]);
      const browserWindow = window.gBrowser ? window : Services.wm.getMostRecentWindow('navigator:browser');
      if (!browserWindow?.gBrowser) throw new Error('No owned browser tab manager is available.');
      browserWindow.gBrowser.selectedTab = browserWindow.gBrowser.addTrustedTab(url);
      return url;
    `, [extensionId, relativePath]);
  } finally { await command('POST', '/moz/context', { context: 'content' }); }
  const handle = await until(async () => (await command('GET', '/window/handles')).find(h => !oldHandles.has(h)), `new ${relativePath} test tab`);
  await command('POST', '/window', { handle });
  await until(async () => (await command('GET', '/url')) === url, `loaded ${relativePath}`);
  return url;
}

async function setupFixture() {
  const bytes = Buffer.alloc(4 * 1024 * 1024 + 79);
  for (let i = 0; i < bytes.length; i++) bytes[i] = (i * 31 + (i >>> 9)) & 255;
  const etag = '"qdm-browser-qa-v1"';
  const server = http.createServer((req, res) => {
    if (req.url === '/') {
      res.writeHead(200, {'Content-Type':'text/html; charset=utf-8'});
      res.end('<!doctype html><html><title>QDM owned extension acceptance</title><a id="automatic" href="/automatic.bin">Download automatic fixture</a><a id="fallback" href="/fallback.bin">Download fallback fixture</a></html>');
      return;
    }
    const range = /^bytes=(\d+)-(\d*)$/.exec(req.headers.range || '');
    fixtureRequests.push({ method: req.method, path: req.url, range: req.headers.range || null, ifRange: req.headers['if-range'] || null });
    let start = 0, end = bytes.length - 1, status = 200;
    if (range && (!req.headers['if-range'] || req.headers['if-range'] === etag)) {
      start = Number(range[1]); end = range[2] ? Math.min(Number(range[2]), end) : end; status = 206;
    }
    if (start > end || start >= bytes.length) { res.writeHead(416, { 'Content-Range': `bytes */${bytes.length}` }); res.end(); return; }
    const headers = { 'Content-Type': 'application/octet-stream', 'Content-Length': end - start + 1, 'Content-Disposition': 'attachment; filename="QUEEN-browser-qa.bin"', ETag: etag, 'Accept-Ranges': 'bytes', Connection: 'close' };
    if (status === 206) headers['Content-Range'] = `bytes ${start}-${end}/${bytes.length}`;
    res.writeHead(status, headers);
    if(req.method === 'HEAD'){res.end();return;}
    let position = start;
    const timer=setInterval(()=>{
      if(res.destroyed){clearInterval(timer);return;}
      if(position > end){clearInterval(timer);res.end();return;}
      res.write(bytes.subarray(position, Math.min(end+1,position+16384)));
      position+=16384;
    },10);
    res.on('close',()=>clearInterval(timer));
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  return { server, bytes, sha256: hash(bytes), url: `http://127.0.0.1:${server.address().port}/fixture.bin` };
}

async function stopDriver() {
  if (sessionId) {
    try { await command('DELETE', '', undefined, 15000); report.cleanup.sessionDeleted = true; }
    catch (error) { report.cleanup.sessionDeleteError = error.message; }
  }
  if (driver && !driverExit) {
    if (process.platform === 'win32') {
      await new Promise(resolve => {
        const killer = spawn('taskkill.exe', ['/PID', String(driver.pid), '/T', '/F'], { windowsHide: true, stdio: 'ignore' });
        killer.once('error', resolve); killer.once('exit', resolve);
      });
    } else driver.kill('SIGTERM');
    await until(() => driverExit, 'owned geckodriver exit', 5000).catch(() => {});
  }
  report.cleanup.driverExited = driverExit;
}

async function main() {
  await Promise.all([fs.access(browserPath), fs.access(driverPath), fs.access(path.join(extensionDirectory, 'manifest.json'))]);
  await fs.mkdir(qaParent, { recursive: true });
  runDir = await fs.mkdtemp(path.join(qaParent, 'run-'));
  profileDir = path.join(runDir, 'profile');
  await fs.mkdir(profileDir);
  report.runDirectory = runDir;
  report.profileDirectory = profileDir;
  report.binary = binary;
  report.binarySha256 = await fileHash(binary);
  const extensionManifest = JSON.parse(await fs.readFile(path.join(extensionDirectory, 'manifest.json'), 'utf8'));
  const extensionId = extensionManifest.browser_specific_settings?.gecko?.id || extensionManifest.applications?.gecko?.id;
  if (!extensionId) throw new Error('Firefox extension requires a manifest Gecko ID for this isolated QA harness.');
  const extensionZip = path.join(runDir, 'qdm-test-extension.xpi');
  report.extensionArchive = await zipExtension(extensionDirectory, extensionZip);
  await fs.mkdir(path.join(runDir, 'downloads'), {recursive:true});
  await fs.mkdir(path.join(runDir, 'browser-downloads'), {recursive:true});
  manager = new CliClient(binary, path.join(runDir, 'manager-data'), path.join(runDir, 'downloads'), ['--bridge']);
  await manager.settings({ connections:32, max_concurrent:1 });
  bridge = await manager.command('integration');
  const token = bridge.token;
  pairingToken = token;
  fixture = await setupFixture();
  check('controlled_services_ready', { bridgePort: bridge.port, expectedBytes: fixture.bytes.length, expectedSha256: fixture.sha256 });

  const port = await freePort();
  driverBase = `http://127.0.0.1:${port}`;
  driver = spawn(driverPath, ['--host', '127.0.0.1', '--port', String(port), '--log', 'warn', '--allow-system-access'], { cwd: runDir, windowsHide: true, stdio: ['ignore', 'pipe', 'pipe'] });
  report.automation = { systemAccess: true, scope: 'New owned headless profile only; used to open installed extension pages through browser UI APIs.' };
  report.driverPid = driver.pid;
  driver.on('exit', (code, signal) => { driverExit = true; report.driverExit = { code, signal }; });
  driver.on('error', error => { driverExit = true; driverOutput += `\nDriver spawn failed: ${error.message}`; });
  for (const stream of [driver.stdout, driver.stderr]) stream.on('data', chunk => { if (driverOutput.length < 100000) driverOutput += chunk.toString(); });
  log('geckodriver_spawned', { pid: driver.pid, port });
  await until(async () => { if (driverExit) throw new Error(driverOutput || 'Geckodriver exited.'); return (await wire('GET', '/status', undefined, 1000)).ready; }, 'geckodriver ready', 15000);
  const created = await wire('POST', '/session', { capabilities: { alwaysMatch: {
    browserName: 'firefox', 'moz:firefoxOptions': {
      binary: browserPath,
      args: ['-headless', '-no-remote', '-profile', profileDir],
      prefs: {
        'browser.shell.checkDefaultBrowser': false,
        'browser.startup.page': 0,
        'browser.startup.homepage': 'about:blank',
        'browser.startup.homepage_override.mstone': 'ignore',
        'browser.aboutwelcome.enabled': false,
        'datareporting.healthreport.uploadEnabled': false,
        'datareporting.policy.dataSubmissionEnabled': false,
        'toolkit.telemetry.enabled': false,
        'app.update.auto': false,
        'browser.download.folderList': 2,
        'browser.download.dir': path.join(runDir, 'browser-downloads'),
        'browser.download.useDownloadDir': true,
        'browser.helperApps.neverAsk.saveToDisk': 'application/octet-stream',
        'browser.download.always_ask_before_handling_new_types': false,
        'browser.download.manager.showWhenStarting': false
      }, log: { level: 'warn' }
    }
  } } }, 120000);
  sessionId = created.sessionId;
  report.browser = { name: created.capabilities?.browserName, version: created.capabilities?.browserVersion, platform: created.capabilities?.platformName, pid: created.capabilities?.['moz:processID'], profile: created.capabilities?.['moz:profile'], driverVersion: created.capabilities?.['moz:geckodriverVersion'] };
  if (!sessionId) throw new Error('Geckodriver returned no session ID.');
  await command('POST', '/timeouts', { script: 30000, pageLoad: 30000, implicit: 0 });
  await command('POST', '/window/rect', { width: 1100, height: 850 });
  check('fresh_headless_browser_started', report.browser);
  const installedId = await command('POST', '/moz/addon/install', { path: extensionZip, temporary: true });
  if (installedId !== extensionId) throw new Error(`Unexpected installed extension identity: ${installedId}`);
  check('temporary_extension_installed', { id: installedId, version: extensionManifest.version });

  report.optionsUrl = await openExtensionPage(extensionId, 'options.html');
  await until(async () => (await text('h1')).includes('Integration'), 'QDM pairing page');
  await type('#token', token);
  await type('#port', bridge.port);
  await click('#pair-form button[type="submit"]');
  const pairingText = await until(async () => {
    const message = await text('#message');
    if (message.includes('เชื่อมต่อไม่สำเร็จ')) throw new Error(message);
    return message.startsWith('เชื่อมต่อแล้ว') && message;
  }, 'successful extension pairing', 15000);
  check('options_form_pairing_succeeded', { message: pairingText });

  // A successful update proves onInstalled created the actual menu item. This
  // query operates only on our temporary extension, not the user's menus.
  const menu = await command('POST', '/execute/async', { script: `
    const done = arguments[arguments.length - 1];
    const view = window.wrappedJSObject || window;
    const api = view.browser || view.chrome || globalThis.browser || globalThis.chrome;
    if (!api?.contextMenus?.update) { done({ok:false,error:'Extension API unavailable in WebDriver script context'}); return; }
    Promise.resolve(api.contextMenus.update('qdm-download', {title:'ดาวน์โหลดด้วย QDM'}))
      .then(() => done({ok:true}), error => done({ok:false,error:String(error.message || error)}));
  `, args: [] });
  if (!menu.ok) throw new Error(`Context-menu registration could not be confirmed: ${menu.error}`);
  check('context_menu_registered', { id: 'qdm-download' });

  report.popupUrl = await openExtensionPage(extensionId, 'popup.html');
  await until(async () => Boolean(await element('#urls')), 'QDM popup UI');
  await type('#urls', fixture.url);
  await click('#send');
  const callbackText = await until(async () => {
    const message = await text('#message');
    return message.includes('เพิ่ม 1 รายการใน QDM แล้ว') && message;
  }, 'popup/background handoff callback', 15000);
  check('popup_background_handoff_succeeded', { message: callbackText });
  const completed = await until(async () => {
    const task = (await manager.snapshot()).downloads[0];
    if (task?.status === 'error') throw new Error(task.error);
    return task?.status === 'completed' && task;
  }, 'QDM file completion', 20000);
  const actual = await fs.readFile(completed.file_path);
  if (actual.length !== fixture.bytes.length || hash(actual) !== fixture.sha256 || completed.sha256 !== fixture.sha256) throw new Error('The file delivered through the browser extension failed the byte/hash check.');
  if ((await manager.snapshot()).downloads.length !== 1) throw new Error('The extension created an unexpected number of downloads.');
  check('downloaded_file_verified', { filename: completed.filename, filePath: completed.file_path, bytes: actual.length, sha256: completed.sha256, segments: completed.segments.length, status: completed.status });
  await command('POST', '/url', {url: new URL('/', fixture.url).href});
  await click('#automatic');
  const automatic = await until(async () => {
    const tasks = (await manager.snapshot()).downloads;
    const task = tasks.find(t => t.url.endsWith('/automatic.bin'));
    if(task?.status === 'error') throw new Error(task.error);
    return task?.status === 'completed' && task;
  }, 'actual clicked browser download is intercepted', 30000);
  if(await fileHash(automatic.file_path)!==fixture.sha256)throw new Error('Automatic capture file hash mismatch.');
  if((await manager.snapshot()).downloads.length!==2)throw new Error('Automatic capture duplicated the QDM task.');
  check('real_browser_link_click_autocapture', {filename:automatic.filename,bytes:fixture.bytes.length,sha256:automatic.sha256});
  await openExtensionPage(extensionId,'popup.html');
  const readBrowserDownloads=async(url)=>command('POST','/execute/async',{script:`
    const done=arguments[arguments.length-1];
    const view=window.wrappedJSObject||window;
    const api=view.browser||view.chrome||globalThis.browser||globalThis.chrome;
    Promise.all([api.downloads.search(arguments[0]?{url:arguments[0]}:{}),api.storage.local.get('lastMessage')]).then(([items,stored])=>done({ok:true,lastMessage:stored.lastMessage,items:items.map(x=>({id:x.id,url:x.url,state:x.state,filename:x.filename,bytesReceived:x.bytesReceived,paused:x.paused,canResume:x.canResume,error:x.error}))}),e=>done({ok:false,error:e.message}));
  `,args:[url]});
  const capturedHistory=await readBrowserDownloads(new URL('/automatic.bin',fixture.url).href);
  if(!capturedHistory.ok||capturedHistory.items.length!==0)throw new Error('Browser original was not cancelled and removed after QDM acceptance.');
  check('captured_browser_transfer_cancelled_after_acceptance',{browserHistoryMatches:0});
  await manager.shutdown();
  await command('POST','/url',{url:new URL('/',fixture.url).href});
  await click('#fallback');
  await openExtensionPage(extensionId,'popup.html');
  const fallback=await until(async()=>{
    const result=await readBrowserDownloads(null);
    report.lastFallbackObservation=result;
    if(!result.ok)throw new Error(result.error);
    return result.items.find(x=>x.url.endsWith('/fallback.bin')&&x.state==='complete');
  },'browser continues download when QDM is offline',30000);
  if(await fileHash(fallback.filename)!==fixture.sha256)throw new Error('Fallback browser file hash mismatch.');
  const fallbackSize=(await fs.stat(fallback.filename)).size;
  if(fallbackSize!==fixture.bytes.length)throw new Error('Fallback browser file size mismatch.');
  check('offline_QDM_keeps_original_browser_request_running',{bytes:fallbackSize,browserReportedBytes:fallback.bytesReceived,sha256:fixture.sha256});
  report.fixtureRequests = fixtureRequests;
  report.passed = true;
  report.status = 'passed';
  report.failed = 0;
}

(async () => {
  try { await main(); }
  catch (error) { report.status='failed';report.failed=1; report.failure = { message: error.message, stack: error.stack }; log('failed', { message: error.message }); process.exitCode = 1; }
  finally {
    report.cleanup = {};
    report.fixtureRequests=fixtureRequests;
    await stopDriver().catch(error => { report.cleanup.driverError = error.message; });
    await manager?.shutdown().catch(error => { report.cleanup.managerError = error.message; });
    if (fixture) { fixture.server.closeAllConnections(); await new Promise(resolve => fixture.server.close(resolve)); }
    if (profileDir) {
      try { await fs.rm(profileDir, { recursive: true, force: true, maxRetries: 3, retryDelay: 200 }); report.cleanup.profileRemoved = true; }
      catch (error) { report.cleanup.profileRemoved = false; report.cleanup.profileRemoveError = error.message; }
    }
    report.finishedAt = new Date().toISOString();
    report.driverLog = pairingToken ? driverOutput.replaceAll(pairingToken,'<redacted>') : driverOutput;
    await fs.mkdir(path.dirname(reportPath), { recursive: true });
    await fs.writeFile(reportPath, pairingToken ? JSON.stringify(report,null,2).replaceAll(pairingToken,'<redacted>') : JSON.stringify(report,null,2), 'utf8');
    if(report.passed && runDir)await fs.rm(runDir,{recursive:true,force:true,maxRetries:5,retryDelay:200});
    log('report_written', { passed: report.passed, reportPath, checks: report.checks.length, cleanup: report.cleanup });
  }
})();

