import assert from 'node:assert/strict';
import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import { randomBytes, createHash } from 'node:crypto';
import { readFileSync, writeFileSync, mkdirSync, mkdtempSync, rmSync, openSync, closeSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';

export const root = fileURLToPath(new URL('../', import.meta.url));
export const secret = () => randomBytes(32).toString('base64url');
export const digest = (value) => createHash('sha256').update(value).digest('base64url');
export const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));

export async function until(check, label, timeout = 15000) {
  const deadline = Date.now() + timeout;
  let error;
  while (Date.now() < deadline) {
    try { const value = await check(); if (value) return value; }
    catch (e) { error = e; }
    await delay(100);
  }
  throw new Error(`Timed out: ${label}${error ? ` (${error.message})` : ''}`);
}

export function isAlive(pid) {
  try { process.kill(pid, 0); return true; }
  catch (error) { return error.code === 'EPERM'; }
}

export async function harness({ chatgptOnly = false, access, agentBinary } = {}) {
  process.umask(0o077);
  mkdirSync(path.join(root, '.local'), { recursive: true, mode: 0o700 });
  const directory = mkdtempSync(path.join(root, '.local/e2e-'));
  const logDirectory = path.join(root, 'logs', path.basename(directory));
  mkdirSync(logDirectory, { recursive: true, mode: 0o700 });
  const files = path.join(directory, 'files'); mkdirSync(files);
  const outside = path.join(directory, 'outside'); mkdirSync(outside);
  writeFileSync(path.join(files, 'hello.txt'), 'first line\nsecond line\nthird line\n');
  writeFileSync(path.join(outside, 'private.txt'), 'outside-root-sentinel');
  const adminToken = secret();
  const reservation = createServer();
  await new Promise((resolve, reject) => { reservation.once('error', reject); reservation.listen(0, '127.0.0.1', resolve); });
  const port = reservation.address().port;
  await new Promise((resolve) => reservation.close(resolve));
  const origin = `http://127.0.0.1:${port}`;
  const config = JSON.parse(readFileSync(path.join(root, 'wrangler.json'), 'utf8'));
  delete config.build; delete config.dev; delete config.$schema;
  config.main = path.join(root, 'build/worker/shim.mjs');
  config.vars = { PUBLIC_URL: origin, ALLOW_LOCALHOST: 'true', CHATGPT_ONLY: String(chatgptOnly) };
  const configFile = path.join(directory, 'wrangler.json');
  function configureAccess(settings) {
    delete config.access;
    if (settings) {
      Object.assign(config.vars, { ACCESS_REQUIRED: 'true', ACCESS_AUD: settings.aud ?? '', ACCESS_OWNER_EMAIL: settings.owner ?? '' });
      if (settings.context) config.access = { dev: settings.context };
    }
    writeFileSync(configFile, JSON.stringify(config), { mode: 0o600 });
  }
  configureAccess(access);
  writeFileSync(configFile, JSON.stringify(config), { mode: 0o600 });
  writeFileSync(path.join(directory, '.dev.vars'), `ADMIN_TOKEN=${adminToken}\n`, { mode: 0o600 });
  const children = [];
  const clients = [];
  const sockets = [];
  const browsers = [];
  let worker;

  function journal() {
    writeFileSync(path.join(logDirectory, 'lifecycle.json'), JSON.stringify(children.map(({ name, child, status }) => ({ name, pid: child.pid, status })), null, 2), { mode: 0o600 });
  }

  function launch(name, command, args) {
    const logfile = path.join(logDirectory, `${name}-${children.length}.log`);
    const fd = openSync(logfile, 'w', 0o600);
    const child = spawn(command, args, { cwd: root, detached: true, stdio: ['ignore', fd, fd], env: { ...process.env, NO_COLOR: '1', WRANGLER_SEND_METRICS: 'false' } });
    closeSync(fd);
    const handle = { name, child, logfile, status: 'running' };
    handle.done = new Promise((resolve) => {
      child.once('exit', (code, signal) => { handle.status = { code, signal }; journal(); resolve(handle.status); });
      child.once('error', (error) => { handle.status = { error: error.message }; journal(); resolve(handle.status); });
    });
    children.push(handle); journal();
    return handle;
  }

  async function stop(handle) {
    if (!handle || handle.status !== 'running') return;
    try { process.kill(-handle.child.pid, 'SIGTERM'); } catch (error) { if (error.code !== 'ESRCH') throw error; }
    await Promise.race([handle.done, delay(5000)]);
    if (handle.status === 'running') {
      process.kill(-handle.child.pid, 'SIGKILL');
      await handle.done;
      throw new Error(`${handle.name} required forced termination; inspect ${handle.logfile}`);
    }
  }

  async function request(resource, options = {}) {
    const headers = { ...options.headers };
    if (options.token) headers.Authorization = `Bearer ${options.token}`;
    if (options.origin) headers.Origin = options.origin;
    let body = options.body;
    if (options.json !== undefined) { body = JSON.stringify(options.json); headers['Content-Type'] = 'application/json'; }
    if (options.form !== undefined) { body = new URLSearchParams(options.form); headers['Content-Type'] = 'application/x-www-form-urlencoded'; }
    const response = await fetch(new URL(resource, origin), {
      method: options.method || (body === undefined ? 'GET' : 'POST'), headers, body,
      redirect: 'manual', signal: AbortSignal.timeout(35000),
    });
    const text = await response.text();
    let data; try { data = JSON.parse(text); } catch { data = undefined; }
    return { response, data, text };
  }

  const admin = (resource, method = 'GET', json) => request(resource, { method, token: adminToken, json });

  async function startWorker() {
    worker = launch('worker', process.execPath, [path.join(root, 'node_modules/wrangler/bin/wrangler.js'), 'dev', '--local', '--config', configFile, '--ip', '127.0.0.1', '--port', String(port), '--persist-to', path.join(directory, 'state')]);
    await until(async () => {
      if (worker.status !== 'running') throw new Error(`Worker exited. See ${worker.logfile}`);
      const result = await request('/health');
      return result.response.ok && result.data?.ok;
    }, 'local Worker startup', 30000);
  }

  async function authorize(scope = 'commander:read commander:write commander:execute', options = {}) {
    const redirectUri = options.redirectUri ?? (chatgptOnly ? 'https://chatgpt.com/connector_platform_oauth_redirect' : `${origin}/callback`);
    let clientId = options.clientId;
    if (!clientId) {
      const registration = await request('/oauth/register', { json: { client_name: 'Integration test', redirect_uris: [redirectUri], token_endpoint_auth_method: 'none' } });
      assert.equal(registration.response.status, 201, `Registration failed: ${registration.data?.error_description}`);
      clientId = registration.data.client_id;
    }
    const verifier = secret();
    const params = { client_id: clientId, redirect_uri: redirectUri, response_type: 'code', code_challenge_method: 'S256', code_challenge: digest(verifier), resource: `${origin}/mcp`, scope, state: 'test-state' };
    const consent = await request(`/oauth/authorize?${new URLSearchParams(params)}`);
    assert.equal(consent.response.status, 200, `Consent failed: ${consent.data?.error_description}`);
    const ticket = consent.text.match(/name="ticket" value="([^"]+)"/)?.[1];
    assert.ok(ticket, 'Consent form must contain a ticket');
    const approval = await request('/oauth/approve', { origin, form: { ticket, admin_token: adminToken, decision: 'allow' } });
    assert.equal(approval.response.status, 303, `Approval failed: ${approval.data?.error_description}`);
    const redirect = new URL(approval.response.headers.get('location'));
    assert.equal(redirect.searchParams.get('state'), 'test-state');
    assert.equal(redirect.searchParams.get('iss'), origin);
    const code = redirect.searchParams.get('code');
    const exchange = { grant_type: 'authorization_code', client_id: clientId, redirect_uri: redirectUri, resource: `${origin}/mcp`, code_verifier: verifier, code };
    if (options.deferExchange) return { clientId, verifier, code, exchange, params, ticket };
    const tokens = await request('/oauth/token', { form: exchange });
    assert.equal(tokens.response.status, 200, `Token exchange failed: ${tokens.data?.error_description}`);
    assert.equal(tokens.data.token_type, 'Bearer');
    return { ...tokens.data, clientId, verifier, code, exchange, params, ticket };
  }

  async function sdk(tokens) {
    const client = new Client({ name: 'remote-commander-e2e', version: '1.0.0' });
    clients.push(client);
    await client.connect(new StreamableHTTPClientTransport(new URL('/mcp', origin), { requestInit: { headers: { Authorization: `Bearer ${tokens.access_token}` } } }));
    return client;
  }

  async function pair(name) {
    const started = await request('/pair/start', { json: { name } });
    assert.equal(started.response.status, 200, `Pairing failed: ${started.data?.error_description}`);
    const approved = await admin('/api/pair/approve', 'POST', { user_code: started.data.user_code });
    assert.equal(approved.response.status, 200);
    const redeemed = await request('/pair/token', { json: { device_code: started.data.device_code } });
    assert.equal(redeemed.response.status, 200, `Pairing redemption failed: ${redeemed.data?.error_description}`);
    return { server: origin, ...redeemed.data, device_code: started.data.device_code };
  }

  function agent(configPath, permissions = []) {
    return launch('agent', agentBinary ?? path.join(root, 'target/debug/remote-commander'), ['run', '--config', configPath, '--root', files, '--insecure-localhost', ...permissions]);
  }

  async function startAgent(device, permissions = ['--allow-write', '--allow-shell']) {
    const configPath = path.join(directory, `device-${secret().slice(0, 8)}.json`);
    writeFileSync(configPath, JSON.stringify({ server: origin, device_id: device.device_id, token: device.token }), { mode: 0o600 });
    const handle = agent(configPath, permissions);
    await until(async () => (await admin('/api/devices')).data?.devices.some((d) => d.device_id === device.device_id && d.online), 'agent connection');
    return handle;
  }

  async function cleanup() {
    let failure;
    for (const browser of browsers) await browser.close().catch(() => {});
    for (const socket of sockets) socket.terminate();
    for (const client of clients) await client.close().catch(() => {});
    for (const child of [...children].reverse()) {
      try { await stop(child); } catch (error) { failure ??= error; }
    }
    journal();
    rmSync(directory, { recursive: true, force: true });
    if (failure) throw failure;
  }

  return { directory, logDirectory, files, outside, origin, adminToken, request, admin, launch, stop, startWorker, configureAccess,
    restartWorker: async () => { await stop(worker); await startWorker(); },
    authorize, sdk, pair, agent, startAgent, sockets, browsers, cleanup };
}

export async function call(client, name, args = {}, expectError = false) {
  const result = await client.callTool({ name, arguments: args });
  const value = result.structuredContent ?? JSON.parse(result.content[0].text);
  assert.equal(Boolean(result.isError), expectError, `${name}: ${value.error ?? ''} ${value.message ?? ''}`);
  return value;
}
