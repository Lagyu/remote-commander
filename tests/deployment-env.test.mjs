import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, chmodSync, symlinkSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { readPrivateFile, loadDeploymentEnv, ensureWorkersSubdomain } from '../scripts/deployment-env.mjs';

test('Deployment credentials require a private regular file and cannot set arbitrary process variables', () => {
  const directory = mkdtempSync(path.join(tmpdir(), 'remote-commander-env-'));
  const file = path.join(directory, '.env');
  const keys = ['CLOUDFLARE_ACCOUNT_ID', 'CLOUDFLARE_API_TOKEN', 'NODE_OPTIONS'];
  const before = new Map(keys.map((key) => [key, process.env[key]]));
  try {
    delete process.env.CLOUDFLARE_API_TOKEN;
    process.env.CLOUDFLARE_ACCOUNT_ID = 'existing-account';
    writeFileSync(file, 'CLOUDFLARE_ACCOUNT_ID=other-account\nCLOUDFLARE_API_TOKEN=fake-test-token\nNODE_OPTIONS=must-not-be-loaded\n', { mode: 0o600 });
    loadDeploymentEnv(file);
    assert.equal(process.env.CLOUDFLARE_API_TOKEN, 'fake-test-token');
    assert.equal(process.env.CLOUDFLARE_ACCOUNT_ID, 'existing-account');
    assert.equal(process.env.NODE_OPTIONS, before.get('NODE_OPTIONS'));
    chmodSync(file, 0o644);
    assert.throws(() => readPrivateFile(file), /private regular file/);
    chmodSync(file, 0o600);
    symlinkSync(file, path.join(directory, 'link'));
    assert.throws(() => readPrivateFile(path.join(directory, 'link')));
  } finally {
    for (const [key, value] of before) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
    rmSync(directory, { recursive: true, force: true });
  }
});

test('Subdomain bootstrap creates once, verifies, and never renames an existing account subdomain', async () => {
  const original = globalThis.fetch;
  const calls = [];
  let missing = true;
  globalThis.fetch = async (_url, options) => {
    calls.push(options.method ?? 'GET');
    if (options.method === 'PUT') {
      assert.deepEqual(JSON.parse(options.body), { subdomain: 'chosen-name' });
      missing = false;
    }
    return Response.json(missing ? { success: false, errors: [{ code: 10007 }] }
      : { success: true, result: { subdomain: 'chosen-name' } }, { status: missing ? 404 : 200 });
  };
  try {
    assert.equal(await ensureWorkersSubdomain('fake-account', 'chosen-name', 'fake-token'), 'chosen-name');
    assert.deepEqual(calls, ['GET', 'PUT', 'GET']);
    calls.length = 0;
    await ensureWorkersSubdomain('fake-account', 'chosen-name', 'fake-token');
    assert.deepEqual(calls, ['GET']);
    calls.length = 0;
    await assert.rejects(ensureWorkersSubdomain('fake-account', 'different-name', 'fake-token'), /never renamed/);
    assert.deepEqual(calls, ['GET']);
  } finally { globalThis.fetch = original; }
});
