// Non-destructive hosted checks. Does not create/revoke OAuth clients or touch files.
import assert from 'node:assert/strict';
import { randomBytes } from 'node:crypto';
import { mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { loadDeploymentEnv, projectRoot } from './deployment-env.mjs';

process.umask(0o077);
loadDeploymentEnv();
const origin = process.env.REMOTE_COMMANDER_PUBLIC_URL;
if (!origin || new URL(origin).protocol !== 'https:') throw new Error('Set REMOTE_COMMANDER_PUBLIC_URL to the deployed HTTPS origin.');
const results = [];
async function request(route, expected, options = {}) {
  const response = await fetch(new URL(route, origin), { ...options, redirect: 'manual', signal: AbortSignal.timeout(30000) });
  const text = await response.text();
  assert.ok(expected.includes(response.status), `${route.split('/').slice(0,2).join('/')}: unexpected HTTP ${response.status}`);
  results.push({ route: route.replace(/\/[A-Za-z0-9_-]{43}(?=\/|\?|$)/, '/<invalid-test-token>'), method: options.method ?? 'GET', status: response.status });
  return { response, text };
}
let health;
for (let attempt = 0; attempt < 30; attempt++) {
  health = JSON.parse((await request('/health', [200])).text);
  if (health.file_transfers?.max_bytes === 1073741824) break;
  await new Promise(resolve => setTimeout(resolve, 1000));
}
assert.equal(health.ok, true);
assert.equal(health.file_transfers?.max_bytes, 1073741824, 'Deployed transfer limits are not yet visible; inspect Worker deployment propagation.');
assert.equal(health.file_transfers.chunk_bytes, 1048576);
assert.equal(health.file_transfers.expires_in, 3600);
const js = await request('/transfer.js', [200]);
assert.match(js.text, /file\.slice/);
assert.match(js.text, /Completion may have succeeded/);
assert.equal(js.response.headers.get('cache-control'), 'no-store');
await request('/transfer.css', [200]);
const token = randomBytes(32).toString('base64url');
await request(`/download/${token}`, [404]);
await request(`/download/${token}`, [404], { method: 'HEAD' });
await request(`/upload/${token}`, [404]);
await request(`/upload/${token}/status`, [404]);
await request(`/upload/${token}/chunk?offset=0`, [404], { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body: new Uint8Array([0]) });
await request(`/upload/${token}/complete`, [404], { method: 'POST' });
await request(`/upload/${token}`, [404], { method: 'DELETE' });
await request('/mcp', [401]);
await request('/api/devices', [302,303,401,403]);
await request('/', [302,303,401,403]);
const directory = path.join(projectRoot, '.deploy');
mkdirSync(directory, { recursive: true, mode: 0o700 });
writeFileSync(path.join(directory, 'file-transfer-live-validation.json'), JSON.stringify({ checked_at: new Date().toISOString(), origin, health, results, note: 'Anonymous hosted route/security checks only. Full binary transfer integrity is exercised separately against the local Worker and real agent.' }, null, 2), { mode: 0o600 });
console.log(`Hosted transfer routes, declared 1 GiB limit, invalid-token rejection, and existing authentication verified: ${origin}`);
console.log(`${results.length} checks passed. Existing device pairing and OAuth authorization were not changed.`);
