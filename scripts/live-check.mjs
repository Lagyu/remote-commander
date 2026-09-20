import assert from 'node:assert/strict';
import { randomBytes, createHash } from 'node:crypto';
import { homedir } from 'node:os';
import { mkdtempSync, mkdirSync, rmSync, writeFileSync, readFileSync } from 'node:fs';
import path from 'node:path';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import { loadDeploymentEnv, readPrivateFile, projectRoot } from './deployment-env.mjs';
import { ownerAccessHeaders } from './owner-access.mjs';

process.umask(0o077);
loadDeploymentEnv();
const origin = process.env.REMOTE_COMMANDER_PUBLIC_URL;
const accessHeaders = ownerAccessHeaders();
const admin = readPrivateFile(path.resolve(projectRoot, process.env.REMOTE_COMMANDER_ADMIN_SECRET_FILE)).trim();
const device = JSON.parse(readPrivateFile(path.join(homedir(), 'Library/Application Support/Remote Commander/device.json')));
const secret = () => randomBytes(32).toString('base64url');
const digest = (value) => createHash('sha256').update(value).digest('base64url');
const client = new Client({ name: 'remote-commander-live-verification', version: '1.0.0' });
const results = [];
async function request(route, { token, json, form, originHeader } = {}) {
  const response = await fetch(new URL(route, origin), {
    method: json || form ? 'POST' : 'GET', redirect: 'manual', signal: AbortSignal.timeout(35000),
    headers: { ...((route.startsWith('/api/') || route.startsWith('/oauth/authorize') || route === '/oauth/approve') ? accessHeaders : {}), ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(json ? { 'Content-Type': 'application/json' } : {}),
      ...(form ? { 'Content-Type': 'application/x-www-form-urlencoded' } : {}),
      ...(originHeader ? { Origin: originHeader } : {}) },
    body: json ? JSON.stringify(json) : form ? new URLSearchParams(form) : undefined,
  });
  const text = await response.text();
  let data; try { data = JSON.parse(text); } catch { /* Consent HTML. */ }
  return { response, text, data };
}
const owner = (route, json) => request(route, { token: admin, json });
async function call(name, args = {}) {
  const result = await client.callTool({ name, arguments: { device_id: device.device_id, ...args } });
  assert.equal(Boolean(result.isError), false, `${name} failed`);
  return result.structuredContent ?? JSON.parse(result.content[0].text);
}
let opened = false;
let fixture;
try {
  assert.equal((await owner('/api/connection')).data.status, 'locked', 'Live verification refuses to replace an existing connection or linking window.');
  assert.equal((await request('/health')).data.ok, true);
  assert.equal((await request('/mcp')).response.status, 401);
  assert.equal((await request('/mcp', { token: admin })).response.status, 401);
  assert.equal((await request('/api/devices')).response.status, 401);
  results.push('Public health/TLS; anonymous and administrator-key MCP calls rejected');
  assert.equal((await owner('/api/connection/open', {})).response.status, 200);
  opened = true;
  const callback = 'https://chatgpt.com/connector_platform_oauth_redirect';
  const registration = await request('/oauth/register', { json: { client_name: 'Owner live verification', redirect_uris: [callback], token_endpoint_auth_method: 'none' } });
  assert.equal(registration.response.status, 201);
  const verifier = secret();
  const clientId = registration.data.client_id;
  const query = { client_id: clientId, redirect_uri: callback, response_type: 'code', code_challenge_method: 'S256',
    code_challenge: digest(verifier), resource: `${origin}/mcp`, scope: 'commander:read commander:write commander:execute', state: secret() };
  const consent = await request(`/oauth/authorize?${new URLSearchParams(query)}`);
  assert.equal(consent.response.status, 200);
  const ticket = consent.text.match(/name="ticket" value="([^"]+)"/)?.[1];
  assert.ok(ticket);
  const approved = await request('/oauth/approve', { originHeader: origin, form: { ticket, admin_token: admin, decision: 'allow' } });
  assert.equal(approved.response.status, 303);
  const callbackResult = new URL(approved.response.headers.get('location'));
  assert.equal(callbackResult.searchParams.get('iss'), origin);
  const tokens = await request('/oauth/token', { form: { grant_type: 'authorization_code', client_id: clientId,
    redirect_uri: callback, code: callbackResult.searchParams.get('code'), code_verifier: verifier, resource: `${origin}/mcp` } });
  assert.equal(tokens.response.status, 200);
  assert.equal((await request('/oauth/authorize?' + new URLSearchParams(query))).data.error, 'connection_locked');
  await client.connect(new StreamableHTTPClientTransport(new URL('/mcp', origin), { requestInit: { headers: { Authorization: `Bearer ${tokens.data.access_token}` } } }));
  assert.equal((await client.listTools()).tools.length, 23);
  const config = await call('get_config');
  assert.equal(config.root, homedir());
  assert.equal(config.allow_write, true);
  assert.equal(config.allow_shell, true);
  results.push('Actual installed macOS agent online; root is home; read/write/shell enabled; 23 tools');
  mkdirSync(path.join(projectRoot, '.local'), { recursive: true, mode: 0o700 });
  fixture = mkdtempSync(path.join(projectRoot, '.local/live-'));
  const relative = path.relative(homedir(), path.join(fixture, 'sentinel.txt'));
  const sentinel = `remote-commander-live-${secret()}`;
  await call('write_file', { path: relative, content: sentinel });
  assert.equal(readFileSync(path.join(fixture, 'sentinel.txt'), 'utf8'), sentinel);
  assert.equal((await call('read_file', { path: relative })).content, sentinel);
  results.push('Hosted MCP write and read verified against a disposable file on local disk');
  const process = await call('start_process', { command: "printf 'remote-commander-shell-ok\\n'; pwd", timeout_ms: 3000 });
  let output;
  for (let i = 0; i < 10; i++) {
    output = await call('read_process_output', { session_id: process.session_id });
    if (!output.running) break;
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  assert.equal(output.running, false);
  assert.equal(output.exit_code, 0);
  assert.equal(output.output.trim(), `remote-commander-shell-ok\n${homedir()}`);
  results.push('Hosted MCP shell executed and exited successfully with home as working directory');
  await owner('/api/clients/revoke', {});
  assert.equal((await request('/mcp', { token: tokens.data.access_token })).response.status, 401);
  assert.equal((await owner('/api/connection')).data.status, 'locked');
  results.push('Temporary verification authorization revoked and service locked for real ChatGPT linking');
  writeFileSync(path.join(projectRoot, '.deploy/live-validation.json'), JSON.stringify({ checked_at: new Date().toISOString(), origin, results }, null, 2), { mode: 0o600 });
  console.log(results.join('\n'));
} finally {
  await client.close().catch(() => {});
  if (opened) {
    const revoked = await owner('/api/clients/revoke', {});
    if (!revoked.response.ok) throw new Error('Could not revoke the verification connection. Inspect the owner dashboard.');
  }
  if (fixture) rmSync(fixture, { recursive: true, force: true });
}
