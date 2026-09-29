import test from 'node:test';
import assert from 'node:assert/strict';
import { once } from 'node:events';
import { readFileSync, writeFileSync, existsSync, symlinkSync, statSync, mkdirSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { request as httpRequest } from 'node:http';
import path from 'node:path';
import WebSocket from 'ws';
import { chromium } from 'playwright';
import { harness, root, call, until, delay, secret, isAlive } from './harness.mjs';

test('Rust agent → Cloudflare Durable Object → official MCP client', { timeout: 240000 }, async (t) => {
  const h = await harness();
  t.after(() => h.cleanup());
  t.diagnostic(`Lifecycle logs: ${h.logDirectory}`);
  await h.startWorker();
  const full = await h.authorize();
  const client = await h.sdk(full);
  const device = await h.pair('Integration <computer>');
  const agent = await h.startAgent(device);
  const args = (value = {}) => ({ device_id: device.device_id, ...value });

  await t.test('OAuth discovery, JSON-RPC initialization, and device pairing are real', async () => {
    const metadata = await h.request('/.well-known/oauth-protected-resource/mcp');
    assert.equal(metadata.data.resource, `${h.origin}/mcp`);
    const discovery = await h.request('/.well-known/oauth-authorization-server');
    assert.deepEqual(discovery.data.code_challenge_methods_supported, ['S256']);
    const catalog = await client.listTools();
    assert.equal(catalog.tools.length, 26);
    assert.ok(catalog.tools.every((tool) => tool.inputSchema.additionalProperties === false));
    const screenshot = catalog.tools.find((tool) => tool.name === 'get_screenshot');
    assert.equal(screenshot.inputSchema.properties.display.default, 1);
    assert.equal(screenshot.inputSchema.properties.max_dimension.default, 1600);
    const devices = await call(client, 'list_devices');
    assert.equal(devices.devices.find((d) => d.device_id === device.device_id).online, true);
    assert.equal(JSON.stringify(devices).includes(device.token), false);
    assert.equal((await call(client, 'ping_device', args())).ok, true);
    const info = await call(client, 'get_config', args());
    assert.equal(info.allow_write, true); assert.equal(info.allow_shell, true); assert.equal(info.allow_screenshot, true); assert.equal(info.shell_is_sandboxed, false);
    const replay = await h.request('/pair/token', { json: { device_code: device.device_code } });
    assert.equal(replay.response.status, 400); assert.equal(replay.data.error, 'expired_token');
    const anonymous = await h.request('/api/devices');
    assert.equal(anonymous.response.status, 401);
  });

  await t.test('File reads, atomic edits, moves, and paginated searches operate on disk', async () => {
    const page = await call(client, 'read_file', args({ path: 'hello.txt', length: 1 }));
    assert.equal(page.content, 'first line\n'); assert.equal(page.next_offset, 1);
    assert.equal((await call(client, 'read_file', args({ path: 'hello.txt', offset: page.next_offset, length: 1 }))).content, 'second line\n');
    await call(client, 'create_directory', args({ path: 'notes/nested' }));
    await call(client, 'write_file', args({ path: 'notes/nested/a.txt', content: 'alpha\n' }));
    await call(client, 'write_file', args({ path: 'notes/nested/a.txt', content: 'beta\n', mode: 'append' }));
    await call(client, 'edit_block', args({ path: 'notes/nested/a.txt', old_string: 'alpha', new_string: 'gamma' }));
    assert.equal(readFileSync(path.join(h.files, 'notes/nested/a.txt'), 'utf8'), 'gamma\nbeta\n');
    await call(client, 'edit_block', args({ path: 'notes/nested/a.txt', old_string: 'absent', new_string: 'bad' }), true);
    assert.equal(readFileSync(path.join(h.files, 'notes/nested/a.txt'), 'utf8'), 'gamma\nbeta\n');
    await call(client, 'move_file', args({ source: 'notes/nested/a.txt', destination: 'notes/nested/b.txt' }));
    assert.equal(existsSync(path.join(h.files, 'notes/nested/a.txt')), false);
    const listing = await call(client, 'list_directory', args({ path: 'notes', depth: 2 }));
    assert.ok(listing.entries.some((entry) => entry.path.endsWith('b.txt')));
    const multiple = await call(client, 'read_multiple_files', args({ paths: ['hello.txt', 'notes/nested/b.txt', 'missing.txt'] }));
    assert.equal(multiple.files[1].content, 'gamma\nbeta\n'); assert.ok(multiple.files[2].error);
    assert.equal((await call(client, 'get_file_info', args({ path: 'notes/nested/b.txt' }))).size, 11);
    const search = await call(client, 'start_search', args({ path: '.', pattern: 'gamma', search_type: 'content' }));
    assert.equal(search.total, 1);
    assert.equal((await call(client, 'get_more_search_results', args({ search_id: search.search_id }))).results[0].line, 1);
    assert.equal((await call(client, 'list_searches', args())).searches.length, 1);
    assert.equal((await call(client, 'stop_search', args({ search_id: search.search_id }))).removed, true);

    const largeBytes = Buffer.alloc(3 * 1024 * 1024 + 17, 0x5a);
    writeFileSync(path.join(h.files, 'large.bin'), largeBytes);
    await call(client, 'read_file', args({ path: 'large.bin' }), true);
    const download = await client.callTool({ name: 'download_file', arguments: args({ path: 'large.bin' }) });
    assert.notEqual(download.isError, true, JSON.stringify(download));
    assert.equal(download.content[0].type, 'resource_link');
    assert.equal(download.content[0].name, 'large.bin');
    assert.equal(download.content[0].size, largeBytes.length);
    const downloaded = await fetch(download.content[0].uri);
    assert.equal(downloaded.status, 200);
    assert.equal(downloaded.headers.get('content-length'), String(largeBytes.length));
    assert.equal(downloaded.headers.get('cache-control'), 'no-store');
    assert.deepEqual(Buffer.from(await downloaded.arrayBuffer()), largeBytes);
  });

  await t.test('Traversal, symlink escapes, special files, and malformed tool arguments fail safely', async () => {
    symlinkSync(h.outside, path.join(h.files, 'escape'));
    await call(client, 'read_file', args({ path: '../outside/private.txt' }), true);
    await call(client, 'read_file', args({ path: 'escape/private.txt' }), true);
    await call(client, 'write_file', args({ path: 'escape/private.txt', content: 'overwritten' }), true);
    await call(client, 'create_directory', args({ path: 'escape/new' }), true);
    assert.equal(readFileSync(path.join(h.outside, 'private.txt'), 'utf8'), 'outside-root-sentinel');
    const fifo = path.join(h.files, 'named-pipe');
    assert.equal(spawnSync('mkfifo', [fifo]).status, 0);
    const started = Date.now();
    await call(client, 'read_file', args({ path: 'named-pipe' }), true);
    assert.ok(Date.now() - started < 3000, 'A named pipe must not block the file reader');
    await assert.rejects(client.callTool({ name: 'read_file', arguments: args({ path: 'hello.txt', unexpected: true }) }), /schema|arguments/i);
    await assert.rejects(client.callTool({ name: 'read_file', arguments: args({ path: 'hello.txt', offset: -1 }) }), /schema|arguments/i);
  });

  await t.test('Process sessions accept stdin, bound output, enforce deadlines, and terminate descendants', async () => {
    const started = await call(client, 'start_process', args({ command: 'read value; printf "received:%s" "$value"', timeout_ms: 3000 }));
    await call(client, 'interact_with_process', args({ session_id: started.session_id, input: 'hello\n', close_stdin: true }));
    const output = await until(async () => {
      const value = await call(client, 'read_process_output', args({ session_id: started.session_id }));
      return !value.running && value;
    }, 'interactive process completion');
    assert.equal(output.output, 'received:hello'); assert.equal(output.exit_code, 0);
    const large = await call(client, 'start_process', args({ command: "head -c 200000 /dev/zero | tr '\\000' x", timeout_ms: 3000 }));
    const bounded = await until(async () => {
      const value = await call(client, 'read_process_output', args({ session_id: large.session_id }));
      return !value.running && value;
    }, 'bounded output process');
    assert.equal(bounded.truncated, true); assert.ok(bounded.output.length <= 32768); assert.ok(bounded.output_start > 0);
    const timed = await call(client, 'start_process', args({ command: 'sleep 30 & wait', timeout_ms: 150 }));
    const timeout = await until(async () => {
      const value = await call(client, 'read_process_output', args({ session_id: timed.session_id }));
      return !value.running && value;
    }, 'process deadline');
    assert.equal(timeout.timed_out, true); assert.equal(isAlive(timed.pid), false);
    const running = await call(client, 'start_process', args({ command: 'sleep 30', timeout_ms: 30000 }));
    await call(client, 'force_terminate', args({ session_id: running.session_id }));
    await until(() => !isAlive(running.pid), 'explicit process termination');
    assert.ok((await call(client, 'list_sessions', args())).sessions.length >= 4);
  });

  await t.test('PKCE, exact redirects, consent replay, scope limits, and refresh replay are enforced', async () => {
    const pending = await h.authorize('commander:read', { deferExchange: true });
    const wrong = await h.request('/oauth/token', { form: { ...pending.exchange, code_verifier: secret() } });
    assert.equal(wrong.response.status, 400); assert.equal(wrong.data.error, 'invalid_grant');
    const audience = await h.request('/oauth/token', { form: { ...pending.exchange, resource: 'https://elsewhere.example/mcp' } });
    assert.equal(audience.response.status, 400);
    const correct = await h.request('/oauth/token', { form: pending.exchange });
    assert.equal(correct.response.status, 200);
    assert.equal((await h.request('/oauth/token', { form: pending.exchange })).response.status, 400);
    assert.equal((await h.request('/oauth/approve', { origin: h.origin, form: { ticket: pending.ticket, admin_token: h.adminToken, decision: 'allow' } })).response.status, 400);
    const badRedirect = await h.request(`/oauth/authorize?${new URLSearchParams({ ...pending.params, redirect_uri: `${h.origin}/callback/other` })}`);
    assert.equal(badRedirect.response.status, 400); assert.equal(badRedirect.response.headers.has('location'), false);
    const readClient = await h.sdk(correct.data);
    const denied = await readClient.callTool({ name: 'write_file', arguments: args({ path: 'denied.txt', content: 'no' }) });
    assert.equal(denied.isError, true); assert.ok(denied._meta['mcp/www_authenticate']);
    assert.equal(existsSync(path.join(h.files, 'denied.txt')), false);
    const refreshForm = { grant_type: 'refresh_token', client_id: pending.clientId, refresh_token: correct.data.refresh_token, resource: `${h.origin}/mcp` };
    assert.equal((await h.request('/oauth/token', { form: { ...refreshForm, scope: 'commander:read commander:write' } })).response.status, 400);
    const rotated = await h.request('/oauth/token', { form: refreshForm });
    assert.equal(rotated.response.status, 200);
    const replay = await h.request('/oauth/token', { form: refreshForm });
    assert.equal(replay.response.status, 400); assert.equal(replay.data.error, 'invalid_grant');
    const revoked = await h.request('/mcp', { token: rotated.data.access_token });
    assert.equal(revoked.response.status, 401);
    assert.match(revoked.response.headers.get('www-authenticate'), /resource_metadata/);
  });

  await t.test('HTTP transport rejects bad origins, invalid versions, oversized bodies, and execution notifications', async () => {
    const headers = { Accept: 'application/json, text/event-stream' };
    const rpc = { jsonrpc: '2.0', id: 77, method: 'ping' };
    assert.equal((await h.request('/mcp', { json: rpc, headers })).response.status, 401);
    assert.equal((await h.request('/mcp', { token: h.adminToken, json: rpc, headers })).response.status, 401);
    assert.equal((await h.request('/mcp', { token: full.access_token, origin: 'https://attacker.example', json: rpc, headers })).response.status, 403);
    assert.equal((await h.request('/mcp', { token: full.access_token, json: rpc, headers: { ...headers, 'MCP-Protocol-Version': '1900-01-01' } })).response.status, 400);
    assert.equal((await h.request('/mcp', { token: full.access_token })).response.status, 405);
    const oversized = await h.request('/mcp', { token: full.access_token, json: { ...rpc, padding: 'x'.repeat(193 * 1024) }, headers });
    assert.equal(oversized.response.status, 413);
    const batch = await h.request('/mcp', { token: full.access_token, json: [rpc], headers });
    assert.equal(batch.response.status, 200, 'A rejected upload must not disrupt the following request');
    assert.equal(batch.data.error.code, -32600);
    // Fetch's half-duplex body waits for upload completion before exposing the
    // response. Use the HTTP client to observe an early server rejection.
    const slowStatus = await new Promise((resolve, reject) => {
      const request = httpRequest(`${h.origin}/mcp`, {
        method: 'POST', headers: { ...headers, Authorization: `Bearer ${full.access_token}`, 'Content-Type': 'application/json' },
      });
      const finishUpload = setTimeout(() => request.end(), 6500);
      const deadline = setTimeout(() => request.destroy(new Error('Slow-upload response deadline exceeded')), 10000);
      const cleanup = () => { clearTimeout(finishUpload); clearTimeout(deadline); };
      request.once('error', (error) => { cleanup(); reject(error); });
      request.once('response', (response) => {
        response.resume();
        response.once('end', () => { cleanup(); request.destroy(); resolve(response.statusCode); });
      });
      request.write('{');
    });
    assert.equal(slowStatus, 408, 'Slow uploads must time out before entering the Durable Object lock');
    assert.equal((await h.request('/health')).response.status, 200);
    const notification = await h.request('/mcp', { token: full.access_token, json: { jsonrpc: '2.0', method: 'tools/call', params: { name: 'write_file', arguments: args({ path: 'notification.txt', content: 'must not happen' }) } }, headers });
    assert.equal(notification.response.status, 202); assert.equal(notification.text, '');
    assert.equal(existsSync(path.join(h.files, 'notification.txt')), false);
    const log = await h.admin('/api/activity');
    assert.equal(JSON.stringify(log.data).includes('received:%s'), false);
    assert.equal(JSON.stringify(log.data).includes(device.token), false);
  });

  await t.test('Browser pairing drives the native CLI; local permission flags remain authoritative', async () => {
    const chrome = '/Applications/Google Chrome.app/Contents/MacOS/Google Chrome';
    const browser = await chromium.launch({ headless: true, ...(existsSync(chrome) ? { executablePath: chrome } : {}) });
    h.browsers.push(browser);
    const page = await browser.newPage({ viewport: { width: 1440, height: 1100 } });
    const errors = []; page.on('pageerror', (error) => errors.push(error.message));
    await page.goto(h.origin);
    await page.getByLabel('Administrator key').fill(h.adminToken);
    await page.getByRole('button', { name: 'Unlock workspace' }).click();
    await page.locator('#workspace').waitFor({ state: 'visible' });
    assert.equal(await page.getByRole('heading', { name: 'Integration <computer>' }).count(), 1);
    assert.deepEqual(await page.evaluate(() => [localStorage.length, sessionStorage.length]), [0, 0]);
    const config = path.join(h.directory, 'browser-paired.json');
    const pairing = h.launch('pair-cli', path.join(root, 'target/debug/remote-commander'), ['pair', '--server', h.origin, '--name', 'Browser-paired computer', '--config', config, '--no-browser', '--insecure-localhost']);
    const code = await until(() => readFileSync(pairing.logfile, 'utf8').match(/Pairing code: ([A-Z0-9]{8})/)?.[1], 'native pairing code');
    await page.getByRole('button', { name: '+ Pair a computer' }).click();
    await page.getByLabel('Pairing code').fill(code);
    await page.getByRole('button', { name: 'Find computer' }).click();
    await page.getByRole('heading', { name: 'Browser-paired computer' }).waitFor();
    await page.getByRole('button', { name: 'Approve this computer' }).click();
    await until(() => pairing.status !== 'running', 'native pairing completion');
    assert.equal(pairing.status.code, 0); assert.equal(statSync(config).mode & 0o777, 0o600);
    const paired = JSON.parse(readFileSync(config, 'utf8'));
    const readonly = h.agent(config, ['--no-screenshot']);
    await until(async () => (await h.admin('/api/devices')).data.devices.some((d) => d.device_id === paired.device_id && d.online), 'read-only agent connection');
    const secondary = { device_id: paired.device_id };
    await call(client, 'write_file', { ...secondary, path: 'readonly-denied', content: 'no' }, true);
    await call(client, 'start_process', { ...secondary, command: 'printf unexpected' }, true);
    await call(client, 'get_screenshot', secondary, true);
    assert.equal((await call(client, 'read_file', { ...secondary, path: 'hello.txt', length: 1 })).content, 'first line\n');
    await page.getByRole('button', { name: 'Refresh', exact: true }).click();
    await page.getByRole('heading', { name: 'Browser-paired computer' }).waitFor();
    mkdirSync(path.join(root, 'test-results'), { recursive: true });
    await page.screenshot({ path: path.join(root, 'test-results/dashboard-desktop.png'), fullPage: true });
    await page.setViewportSize({ width: 390, height: 844 });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
    await page.screenshot({ path: path.join(root, 'test-results/dashboard-mobile.png'), fullPage: true });
    await page.getByRole('button', { name: 'Lock workspace' }).click();
    await page.locator('#login-panel').waitFor({ state: 'visible' });
    assert.equal(await page.getByLabel('Administrator key').inputValue(), '');
    assert.deepEqual(errors, []);
    await call(client, 'shutdown_device', secondary);
    await until(() => readonly.status !== 'running', 'remote agent shutdown');
    assert.equal(readonly.status.code, 0);
    await browser.close();
  });

  await t.test('WebSocket replies are device-bound, disconnects resolve callers, and lost replies are not retried', async () => {
    const fixture = await h.pair('Relay fixture');
    const other = await h.pair('Other relay fixture');
    const connect = async (device) => {
      const socket = new WebSocket(`${h.origin.replace('http:', 'ws:')}/agent/${device.device_id}`, { headers: { Authorization: `Bearer ${device.token}` } });
      h.sockets.push(socket); await once(socket, 'open'); return socket;
    };
    const socket = await connect(fixture);
    const unrelated = await connect(other);
    const received = once(socket, 'message');
    let settled = false;
    const pending = client.callTool({ name: 'ping_device', arguments: { device_id: fixture.device_id } }).then((value) => { settled = true; return value; });
    const command = JSON.parse((await received)[0].toString());
    unrelated.send(JSON.stringify({ id: command.id, result: { content: [{ type: 'text', text: 'forged' }], isError: false } }));
    await delay(150); assert.equal(settled, false, 'A second device must not resolve the first device request');
    socket.terminate();
    const ended = await pending;
    assert.equal(ended.isError, true); assert.match(ended.content[0].text, /connection|disconnect/i);
    const silent = await connect(fixture);
    let deliveries = 0; silent.on('message', () => deliveries++);
    const timeout = await call(client, 'ping_device', { device_id: fixture.device_id }, true);
    assert.equal(timeout.error, 'unknown_execution_state'); assert.equal(deliveries, 1);
    silent.terminate(); unrelated.terminate();
  });

  await t.test('Durable state survives a Worker restart and the native agent reconnects', async () => {
    await h.restartWorker();
    await until(async () => (await h.admin('/api/devices')).data?.devices.some((d) => d.device_id === device.device_id && d.online), 'agent reconnection', 20000);
    assert.equal((await call(client, 'ping_device', args())).ok, true);
    assert.equal((await call(client, 'read_file', args({ path: 'notes/nested/b.txt' }))).content, 'gamma\nbeta\n');
  });

  await t.test('Revocation stops the agent and its process group, then invalidates client grants', async () => {
    const outstanding = await h.authorize('commander:read', { deferExchange: true });
    const process = await call(client, 'start_process', args({ command: 'sleep 30 & echo $! > descendant.pid; wait', timeout_ms: 30000 }));
    const descendant = await until(() => {
      const file = path.join(h.files, 'descendant.pid');
      return existsSync(file) && Number(readFileSync(file, 'utf8').trim());
    }, 'descendant PID');
    assert.equal((await h.admin(`/api/devices/${device.device_id}`, 'DELETE')).response.status, 200);
    await until(() => agent.status !== 'running', 'revoked agent exit');
    await until(() => !isAlive(process.pid) && !isAlive(descendant), 'revocation process cleanup');
    assert.equal((await call(client, 'ping_device', args(), true)).error, 'device_not_found');
    assert.equal((await h.admin('/api/clients/revoke', 'POST', {})).response.status, 200);
    assert.equal((await h.request('/oauth/token', { form: outstanding.exchange })).response.status, 400, 'Revocation also invalidates an outstanding authorization code');
    assert.equal((await h.request('/mcp', { token: full.access_token })).response.status, 401);
  });
});
