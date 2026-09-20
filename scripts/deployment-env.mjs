import { constants, openSync, closeSync, fstatSync, readFileSync, existsSync } from 'node:fs';
import { parseEnv } from 'node:util';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

export const projectRoot = fileURLToPath(new URL('../', import.meta.url));

export function readPrivateFile(file, maximum = 65536) {
  const fd = openSync(file, constants.O_RDONLY | (constants.O_NOFOLLOW ?? 0));
  try {
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.size > maximum || (stat.mode & 0o077) !== 0 || (process.getuid && stat.uid !== process.getuid())) {
      throw new Error(`${file} must be an owned, private regular file (mode 0600).`);
    }
    return readFileSync(fd, 'utf8');
  } finally { closeSync(fd); }
}

export function loadDeploymentEnv(file = path.join(projectRoot, '.env')) {
  if (!existsSync(file)) return;
  const allowed = new Set(['CLOUDFLARE_ACCOUNT_ID', 'CLOUDFLARE_API_TOKEN', 'REMOTE_COMMANDER_PUBLIC_URL', 'REMOTE_COMMANDER_ADMIN_SECRET_FILE', 'REMOTE_COMMANDER_ACCESS_AUD', 'REMOTE_COMMANDER_OWNER_EMAIL', 'REMOTE_COMMANDER_ACCESS_TOKEN_FILE']);
  for (const [name, value] of Object.entries(parseEnv(readPrivateFile(file)))) {
    if (allowed.has(name) && process.env[name] === undefined) process.env[name] = value;
  }
}

export async function ensureWorkersSubdomain(account, expected, token) {
  const endpoint = `https://api.cloudflare.com/client/v4/accounts/${account}/workers/subdomain`;
  const headers = { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' };
  const get = () => fetch(endpoint, { headers, redirect: 'error', signal: AbortSignal.timeout(20000) });
  let response = await get();
  let body = await response.json();
  if (response.status === 404 && body.errors?.some((error) => error.code === 10007)) {
    response = await fetch(endpoint, { method: 'PUT', headers, body: JSON.stringify({ subdomain: expected }), redirect: 'error', signal: AbortSignal.timeout(20000) });
    body = await response.json();
    if (!response.ok || !body.success) throw new Error(`Could not create workers.dev subdomain (HTTP ${response.status}; codes: ${body.errors?.map((error) => error.code).join(', ')}).`);
    response = await get();
    body = await response.json();
  }
  if (!response.ok || !body.success) throw new Error(`Could not verify workers.dev subdomain (HTTP ${response.status}).`);
  if (body.result?.subdomain !== expected) throw new Error('The account already uses a different workers.dev subdomain; set REMOTE_COMMANDER_PUBLIC_URL to that address. Existing account subdomains are never renamed automatically.');
  return body.result.subdomain;
}
