import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { resolve, extname } from 'node:path';
import { execFileSync, spawnSync } from 'node:child_process';
import { chromium } from 'playwright';

const root = resolve(import.meta.dirname, '..');
const cargo = JSON.parse(await readFile(resolve(root, 'web/package.json'), 'utf8'));
assert(cargo.private);
const version = execFileSync(resolve(root, 'target/debug/bts'), ['--version'], { encoding: 'utf8' }).trim().split(' ')[1];
const packagePath = `target/wasm/bts-v${version}-wasm`;
const options = { count: 7, seed: '18446744073709551615', start: '2026-09-01T00:00:00Z', end: '2026-09-01T01:00:00Z' };
const temporary = await mkdtemp(resolve(tmpdir(), 'bts-browser-'));
const server = createServer(async (request, response) => {
  try {
    const path = request.url === '/' ? '/web/smoke.html' : request.url;
    const file = resolve(root, `.${path}`);
    if (!file.startsWith(`${root}/`)) throw new Error('outside root');
    const content = await readFile(file);
    const mime = { '.html': 'text/html', '.js': 'text/javascript', '.mjs': 'text/javascript', '.wasm': 'application/wasm' };
    response.writeHead(200, { 'Content-Type': mime[extname(file)] ?? 'text/plain' });
    response.end(content);
  } catch {
    response.writeHead(404).end();
  }
});
await new Promise(done => server.listen(0, '127.0.0.1', done));
let browser;
try {
  browser = await chromium.launch({ headless: true });
  const context = await browser.newContext({ timezoneId: 'Pacific/Honolulu' });
  const page = await context.newPage();
  const requests = [];
  const failures = [];
  context.on('request', request => requests.push(request.url()));
  page.on('pageerror', error => failures.push(String(error)));
  await page.goto(`http://127.0.0.1:${server.address().port}/`);
  const metadata = await page.evaluate(async ({ packagePath }) => {
    window.bts = await import(`/${packagePath}/bts.js`);
    await window.bts.default();
    return { version: bts.version(), reference: bts.languageReference() };
  }, { packagePath });
  assert.equal(metadata.version, version);
  assert(metadata.reference.includes(`BTS release ${version}`));
  execFileSync(resolve(root, 'target/debug/bts'), ['setup', 'skill', 'codex'], { cwd: temporary });
  const nativeReference = await readFile(resolve(temporary, '.agents/skills/bts/SKILL.md'), 'utf8');
  assert.equal(metadata.reference.split('## Authoring for realism')[1], nativeReference.split('## Authoring for realism')[1]);

  function normalize(batch) {
    const ids = new Map(batch.events.map((event, index) => [event.span_id, `span-${index}`]));
    return batch.events.map(({ id, span_id, root_span_id, span_parents, ...event }) => ({
      ...event, span_id: ids.get(span_id), root_span_id: ids.get(root_span_id),
      span_parents: span_parents.map(parent => ids.get(parent)),
    }));
  }
  function equivalent(actual, expected, path = '') {
    if (typeof expected === 'number') {
      assert.equal(typeof actual, 'number', path);
      assert(Math.abs(actual - expected) <= Math.max(1e-9, Math.abs(expected) * 1e-14), `${path}: ${actual} != ${expected}`);
    } else if (expected && typeof expected === 'object') {
      assert.deepEqual(Object.keys(actual).sort(), Object.keys(expected).sort(), path);
      for (const key of Object.keys(expected)) equivalent(actual[key], expected[key], `${path}.${key}`);
    } else assert.equal(actual, expected, path);
  }
  // Existing fixtures cover dynamic repeats/choices/maybe, scorers, and tokens().
  for (const fixture of ['simple', 'dynamic', 'scorers']) {
    let source = await readFile(resolve(root, `tests/fixtures/${fixture}.bt`), 'utf8');
    // scorers.bt declares reusable scorers only; add a trace exercising one.
    if (fixture === 'scorers') source += '\ntrace "scored" { output = "answer" scorer "answer-quality" { score = 0.75 } }';
    const fixturePath = resolve(temporary, `${fixture}.bt`);
    await writeFile(fixturePath, source);
    for (const distribution of ['linear', 'sine']) {
      const args = ['write', '--from', fixturePath, '--count', String(options.count), '--seed', options.seed,
        '--start', options.start, '--end', options.end, '--dist', distribution, '--dry-run'];
      const native = JSON.parse(execFileSync(resolve(root, 'target/debug/bts'), args, { cwd: root, env: { ...process.env, BTS_LOG: 'off' } }));
      const result = await page.evaluate(({ source, options }) => {
        if (bts.check(source).length) throw new Error('fixture rejected');
        return [bts.generate(source, options), bts.generate(source, options)];
      }, { source, options: { ...options, distribution } });
      equivalent(normalize(result[0]), normalize(native));
      assert.deepEqual(normalize(result[0]), normalize(result[1]));
      for (const event of result[0].events) {
        assert.match(event.id, /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
      }
      console.log(`PASS native/browser parity and seeded repeatability: ${fixture}, ${distribution}`);
    }
  }

  const checks = await page.evaluate(({ options }) => {
    const errors = [];
    const invalid = [
      { count: 0 }, { count: -1 }, { count: 1.5 }, { count: 4294967296 }, { count: NaN },
      { seed: '18446744073709551616' }, { seed: '-1' }, { seed: 42 },
      { start: '2026-09-01' }, { end: options.start }, { end: '1960-01-01T00:00:00Z' },
      { distribution: 'other' }, { extra: true }, { end: '10000-01-01T00:00:00Z' },
    ];
    for (const override of invalid) {
      try { bts.generate('trace "t" {}', { ...options, ...override }); errors.push(`accepted invalid option: ${JSON.stringify(override)}`); }
      catch (error) { if (!error.message || !Array.isArray(error.diagnostics)) errors.push('unstructured error'); }
    }
    try { bts.generate('trace "t" { input = attachment("report.pdf", "application/pdf") }', options); errors.push('accepted attachment'); }
    catch (error) { if (!error.message.includes('attachments')) errors.push('missing attachment error'); }
    const syntax = 'trace "t" {\n input = }\n}';
    const diagnostics = bts.check(syntax);
    try { bts.generate(syntax, options); errors.push('accepted syntax error'); }
    catch (error) { if (JSON.stringify(error.diagnostics) !== JSON.stringify(diagnostics)) errors.push('diagnostics differ'); }
    for (const source of ['trace "unterminated', 'trace "t" { input = unknown() }']) {
      if (!bts.check(source).length) errors.push('missing lexing/validation error');
    }
    const unicode = bts.check('trace "é😀" {\n input = }\n}')[0];
    const generationSource = 'trace "t" { input = 1 / range(0, 0) }';
    try { bts.generate(generationSource, options); errors.push('accepted runtime error'); }
    catch (error) { if (error.diagnostics[0]?.phase !== 'generation') errors.push('missing generation location'); }
    const zoneA = bts.generate('trace "t" { output = uuid() }', options);
    const zoneB = bts.generate('trace "t" { output = uuid() }', { ...options, start: '2026-08-31T17:00:00-07:00', end: '2026-08-31T18:00:00-07:00' });
    return { errors, diagnostics, unicode, zoneA, zoneB };
  }, { options });
  assert.deepEqual(checks.errors, []);
  const invalidSource = 'trace "t" {\n input = }\n}';
  const invalidPath = resolve(temporary, 'invalid.bt');
  await writeFile(invalidPath, invalidSource);
  const nativeDiagnostic = spawnSync(resolve(root, 'target/debug/bts'), ['check', 'syntax', invalidPath], { encoding: 'utf8' });
  assert.equal(nativeDiagnostic.status, 1);
  const { line, column } = checks.diagnostics[0].location;
  assert(nativeDiagnostic.stderr.includes(`${invalidPath}:${line}:${column}: ${checks.diagnostics[0].phase} error: ${checks.diagnostics[0].message}`));
  assert.equal(checks.diagnostics[0].location.line, 2);
  assert.equal(checks.diagnostics[0].location.column, 10);
  assert.equal(checks.unicode.location.line, 2);
  assert.equal(checks.unicode.location.column, 10);
  assert.equal(checks.unicode.location.start, Buffer.byteLength('trace "é😀" {\n input = '));
  assert.deepEqual(normalize(checks.zoneA), normalize(checks.zoneB));
  console.log('PASS diagnostics, invalid options, seeded uuid(), and offset/timezone equivalence');

  const worker = await page.evaluate(async ({ packagePath, options }) => {
    const worker = new Worker('/web/smoke-worker.mjs', { type: 'module' });
    try {
      return await new Promise((resolve, reject) => {
        worker.onmessage = ({ data }) => data.error ? reject(data.error) : resolve(data);
        worker.onerror = reject;
        worker.postMessage({ packagePath, options });
      });
    } finally { worker.terminate(); }
  }, { packagePath, options });
  assert.equal(worker.version, version);
  assert.equal(worker.events, options.count);
  assert.equal(worker.diagnostics, 0);
  assert(worker.reference.includes(`BTS release ${version}`));
  assert.deepEqual(failures, []);
  assert(requests.every(url => url.startsWith(`http://127.0.0.1:${server.address().port}/`)));
  console.log('PASS real Chromium WASM initialization, all four methods, module worker, and no external requests');
} finally {
  await browser?.close();
  await new Promise(done => server.close(done));
  await rm(temporary, { recursive: true, force: true });
}
