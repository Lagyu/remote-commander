import test from 'node:test';
import assert from 'node:assert/strict';
import { harness, call } from './harness.mjs';
import { chromium } from 'playwright';
import { existsSync } from 'node:fs';

const callback = 'https://chatgpt.com/connector_platform_oauth_redirect';

test('Owner-pinned ChatGPT connection, safe reconnect, revocation and recovery', { timeout: 150000 }, async (t) => {
  const h = await harness({ chatgptOnly: true });
  t.after(() => h.cleanup());
  t.diagnostic(`Lifecycle logs: ${h.logDirectory}`);
  await h.startWorker();
  const register = (uri = callback) => h.request('/oauth/register', {
    json: { client_name: 'ChatGPT', redirect_uris: [uri], token_endpoint_auth_method: 'none' },
  });

  await t.test('Default is locked; only the owner can open initial linking', async () => {
    assert.equal((await register()).data.error, 'linking_closed');
    assert.equal((await h.request('/api/connection')).response.status, 401);
    assert.equal((await h.request('/api/connection/open', { json: {} })).response.status, 401);
    assert.equal((await h.admin('/api/connection')).data.status, 'locked');
    assert.equal((await h.admin('/api/connection/open', 'POST', {})).response.status, 200);
    const state = (await h.admin('/api/connection')).data;
    assert.equal(state.status, 'awaiting_chatgpt');
    assert.ok(state.linking_expires_at > Date.now() / 1000 + 580);
    assert.equal((await h.request('/mcp')).response.status, 401);
  });

  await t.test('Callback allowlist uses exact comparison and issuer identification', async () => {
    for (const bad of [
      'https://attacker.example/callback', `${callback}?next=elsewhere`,
      'https://chatgpt.com.attacker.example/connector_platform_oauth_redirect',
      'https://chatgpt.com/connector_platform_oauth_redirect/',
      'https://chatgpt.com:443/connector_platform_oauth_redirect',
    ]) assert.equal((await register(bad)).response.status, 400);
    const discovery = await h.request('/.well-known/oauth-authorization-server');
    assert.equal(discovery.data.authorization_response_iss_parameter_supported, true);
  });

  // Two user authorizations can share one public OAuth client ID. Neither may
  // mint another family after the first succeeds, even if both were approved.
  const first = await h.authorize('commander:read commander:write commander:execute commander:read commander:write commander:execute', { deferExchange: true });
  const competing = await h.authorize(undefined, { clientId: first.clientId, deferExchange: true });

  await t.test('Browser consent denial follows the allowed callback and includes issuer identification', async () => {
    const chrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
    const browser = await chromium.launch({ headless: true, ...(existsSync(chrome) ? { executablePath: chrome } : {}) });
    h.browsers.push(browser);
    const page = await browser.newPage();
    // Intercept only the destination: the form and 303 come from real workerd,
    // and Chromium still enforces our CSP before following the redirect.
    await page.route(`${callback}*`, (route) => route.fulfill({ contentType: 'text/html', body: '<p>Test callback</p>' }));
    await page.goto(`${h.origin}/oauth/authorize?${new URLSearchParams(first.params)}`);
    await page.getByLabel('Administrator key').fill(h.adminToken);
    const [approval] = await Promise.all([
      page.waitForResponse((response) => response.url() === `${h.origin}/oauth/approve`),
      page.getByRole('button', { name: 'Deny', exact: true }).click(),
    ]);
    assert.equal(approval.status(), 303, `Origin: ${await approval.request().headerValue('origin')}; rejection: ${approval.status() === 303 ? '' : await approval.text()}`);
    await page.waitForURL(`${callback}*`, { timeout: 10000 });
    const result = new URL(page.url());
    assert.equal(result.searchParams.get('error'), 'access_denied');
    assert.equal(result.searchParams.get('iss'), h.origin);
    assert.equal(result.searchParams.get('state'), first.params.state);
    await browser.close();
  });

  let tokens;
  let activeClientId = first.clientId;
  await t.test('Concurrent initial authorization-code exchanges admit exactly one family', async () => {
    const results = await Promise.all([
      h.request('/oauth/token', { form: first.exchange }),
      h.request('/oauth/token', { form: competing.exchange }),
    ]);
    assert.deepEqual(results.map((r) => r.response.status).sort(), [200, 400]);
    tokens = results.find((r) => r.response.status === 200).data;
    const state = (await h.admin('/api/connection')).data;
    assert.equal(state.status, 'linked');
    assert.equal(state.scope, 'commander:read commander:write commander:execute');
    assert.equal(state.linking_expires_at, null);
    assert.equal('family' in state, false);
  });

  await t.test('Reconnect can register and replace a linked connection without dashboard reset', async () => {
    const registration = await register();
    assert.equal(registration.response.status, 201);

    const reconnects = [
      await h.authorize(undefined, { deferExchange: true }),
      await h.authorize(undefined, { deferExchange: true }),
    ];

    // Reconnect consent alone is non-destructive. The old family remains valid
    // until a replacement code is successfully exchanged.
    assert.equal((await h.request('/mcp', { token: tokens.access_token })).response.status, 405);

    const results = await Promise.all(reconnects.map((pending) =>
      h.request('/oauth/token', { form: pending.exchange })));
    assert.deepEqual(results.map((r) => r.response.status).sort(), [200, 400]);
    const winner = results.findIndex((r) => r.response.status === 200);
    assert.notEqual(winner, -1);
    const oldTokens = tokens;
    tokens = results[winner].data;
    activeClientId = reconnects[winner].clientId;

    // Successful compare-and-swap replacement revokes the previous family.
    assert.equal((await h.request('/mcp', { token: oldTokens.access_token })).response.status, 401);
    assert.equal((await h.request('/oauth/token', { form: {
      grant_type: 'refresh_token',
      client_id: first.clientId,
      refresh_token: oldTokens.refresh_token,
      resource: `${h.origin}/mcp`,
    } })).response.status, 400);
    assert.equal((await h.admin('/api/connection')).data.status, 'linked');
  });

  const client = await h.sdk(tokens);
  const device = await h.pair('Private home fixture');
  await h.startAgent(device);
  const args = (extra) => ({ device_id: device.device_id, ...extra });

  await t.test('The approved connection reads, writes and runs a shell', async () => {
    await call(client, 'write_file', args({ path: 'approved.txt', content: 'owner-authorized\n' }));
    assert.equal((await call(client, 'read_file', args({ path: 'approved.txt' }))).content, 'owner-authorized\n');
    const process = await call(client, 'start_process', args({ command: 'pwd', timeout_ms: 3000 }));
    assert.ok(process.session_id);
    const config = await call(client, 'get_config', args({}));
    assert.equal(config.allow_write, true);
    assert.equal(config.allow_shell, true);
  });

  let rotated;
  await t.test('Token rotation and Worker restarts preserve the same connection', async () => {
    rotated = await h.request('/oauth/token', { form: {
      grant_type: 'refresh_token', client_id: activeClientId,
      refresh_token: tokens.refresh_token, resource: `${h.origin}/mcp`,
    } });
    assert.equal(rotated.response.status, 200);
    await h.restartWorker();
    assert.equal((await h.admin('/api/connection')).data.status, 'linked');
    const refreshedClient = await h.sdk(rotated.data);
    assert.equal((await call(refreshedClient, 'list_devices')).devices.length, 1);
  });

  await t.test('Owner revocation closes initial linking and rejects old tokens, refreshes and codes', async () => {
    assert.equal((await h.admin('/api/clients/revoke', 'POST', {})).response.status, 200);
    assert.equal((await h.request('/mcp', { token: rotated.data.access_token })).response.status, 401);
    assert.equal((await h.request(`/oauth/authorize?${new URLSearchParams(first.params)}`)).data.error, 'linking_closed');
    await h.admin('/api/connection/open', 'POST', {});
    for (const pending of [first, competing]) {
      assert.equal((await h.request('/oauth/token', { form: pending.exchange })).response.status, 400);
    }
    assert.equal((await h.request('/oauth/token', { form: {
      grant_type: 'refresh_token', client_id: activeClientId,
      refresh_token: rotated.data.refresh_token, resource: `${h.origin}/mcp`,
    } })).response.status, 400);
    const replacement = await h.authorize(undefined, { clientId: activeClientId });
    const replacementClient = await h.sdk(replacement);
    assert.equal((await call(replacementClient, 'list_devices')).devices.length, 1);
    assert.equal((await h.request('/mcp', { token: tokens.access_token })).response.status, 401);
    await h.admin('/api/clients/revoke', 'POST', {});
    assert.equal((await h.admin('/api/connection')).data.status, 'locked');
  });
});
