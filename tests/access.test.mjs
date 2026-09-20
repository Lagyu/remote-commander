import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import path from 'node:path';
import { harness, call, until } from './harness.mjs';

const aud = 'a'.repeat(64);
const owner = 'owner@example.com';
const context = { aud, identity: { email: owner } };

test('Access protects administration without interrupting an existing ChatGPT and Mac session', async (t) => {
  const h = await harness({ chatgptOnly: true, access: { aud, owner, context } });
  t.after(() => h.cleanup());
  await h.startWorker();
  let tokens;
  let device;
  await t.test('Verified owner identity still requires the administrator credential', async () => {
    assert.equal((await h.request('/')).response.status, 200);
    assert.equal((await h.request('/api/devices')).response.status, 401);
    device = await h.pair('Access test Mac');
    await h.startAgent(device);
    assert.equal((await h.admin('/api/connection/open', 'POST', {})).response.status, 200);
    tokens = await h.authorize();
    assert.equal((await h.admin('/api/connection')).data.status, 'linked');
  });
  await t.test('Removing edge authentication fails closed, including forged identity headers and path variants', async () => {
    h.configureAccess({ aud, owner });
    await h.restartWorker();
    const headers = { 'Cf-Access-Authenticated-User-Email': owner, 'Cf-Access-Jwt-Assertion': 'forged', Cookie: 'CF_Authorization=forged' };
    for (const route of ['/', '/app.js', '/app.css', '/api/devices', '/api/connection', '/api/activity', '/oauth/authorize', '/unknown', '/mcp/../api/devices', '/oauth/token/../approve', '/mcp/anything']) {
      const r = await h.request(route, { token: h.adminToken, headers });
      assert.equal(r.response.status, 403, route);
      assert.equal(r.data.error, 'access_required', route);
      assert.equal(r.response.headers.get('cache-control'), 'no-store');
    }
    assert.equal((await h.request('/oauth/approve', { form: { admin_token: h.adminToken }, headers })).response.status, 403);
    assert.equal((await h.request('/api/devices', { method: 'OPTIONS', headers })).response.status, 403);
    assert.equal((await h.request('/oauth/token', { method: 'GET', headers })).response.status, 403);
  });
  await t.test('Discovery, PKCE rejection, refresh and full Mac permissions survive with no Access session', async () => {
    assert.equal((await h.request('/health')).response.status, 200);
    for (const route of ['/.well-known/oauth-authorization-server', '/.well-known/oauth-protected-resource', '/.well-known/oauth-protected-resource/mcp']) assert.equal((await h.request(route)).response.status, 200);
    assert.equal((await h.request('/mcp')).response.status, 401);
    assert.equal((await h.request('/mcp', { method: 'POST', json: { jsonrpc: '2.0', method: 'tools/list', id: 1 } })).response.status, 401);
    const invalid = await h.request('/oauth/token', { form: { grant_type: 'authorization_code', code: 'invalid' } });
    assert.ok([400, 401].includes(invalid.response.status));
    const refreshed = await h.request('/oauth/token', { form: { grant_type: 'refresh_token', refresh_token: tokens.refresh_token, client_id: tokens.clientId, resource: `${h.origin}/mcp` } });
    assert.equal(refreshed.response.status, 200);
    const client = await h.sdk(refreshed.data);
    assert.equal((await client.listTools()).tools.length, 24);
    const config = await until(async () => {
      try { return await call(client, 'get_config', { device_id: device.device_id }); } catch { return false; }
    }, 'agent reconnect through public authenticated WebSocket');
    assert.equal(config.allow_write, true); assert.equal(config.allow_shell, true);
    await call(client, 'write_file', { device_id: device.device_id, path: 'access-test.txt', content: 'access-test-ok' });
    assert.equal(readFileSync(path.join(h.files, 'access-test.txt'), 'utf8'), 'access-test-ok');
    assert.equal((await call(client, 'read_file', { device_id: device.device_id, path: 'access-test.txt' })).content, 'access-test-ok');
    const process = await call(client, 'start_process', { device_id: device.device_id, command: "printf 'access-shell-ok'", timeout_ms: 3000 });
    const output = await until(async () => {
      const result = await call(client, 'read_process_output', { device_id: device.device_id, session_id: process.session_id });
      return result.running ? false : result;
    }, 'shell completion');
    assert.match(JSON.stringify(output), /access-shell-ok/);
  });
  await t.test('Wrong owner, wrong audience, absent identity, and missing configuration cannot open administration', async () => {
    for (const context of [
      { aud, identity: { email: 'other@example.com' } },
      { aud: 'b'.repeat(64), identity: { email: owner } },
      { aud },
    ]) {
      h.configureAccess({ aud, owner, context }); await h.restartWorker();
      assert.equal((await h.admin('/api/connection')).response.status, 403);
      assert.equal((await h.request('/')).response.status, 403);
    }
    h.configureAccess({ context }); await h.restartWorker();
    assert.equal((await h.request('/')).response.status, 503);
    assert.equal((await h.request('/health')).response.status, 200);
    h.configureAccess({ aud, owner, context }); await h.restartWorker();
    assert.equal((await h.admin('/api/connection')).data.status, 'linked');
  });
});
