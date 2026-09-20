import { parseArgs } from 'node:util';
import { spawnSync } from 'node:child_process';
import { readFileSync, writeFileSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { loadDeploymentEnv, readPrivateFile, ensureWorkersSubdomain } from './deployment-env.mjs';

process.umask(0o077);
const root = fileURLToPath(new URL('../', import.meta.url));
const wrangler = path.join(root, 'node_modules/wrangler/bin/wrangler.js');

function run(command, args, options = {}) {
  const result = spawnSync(command, args, { cwd: root, stdio: 'inherit', ...options });
  if (result.error || result.status !== 0) throw new Error(`${path.basename(command)} failed; deployment stopped.`);
}

function secretFromFile(file) {
    const token = readPrivateFile(file, 4096).trim();
    if (!/^[A-Za-z0-9_-]{43,128}$/.test(token)) throw new Error('Secret file must contain only a random base64url or hexadecimal key, at least 43 characters long.');
    return token;
}

async function main() {
  loadDeploymentEnv();
  const { values } = parseArgs({ options: {
    'account-id': { type: 'string' }, 'public-url': { type: 'string' },
    'worker-name': { type: 'string', default: 'remote-commander' },
    'zone-id': { type: 'string' }, 'admin-secret-file': { type: 'string' },
    'dry-run': { type: 'boolean', default: false },
  }, strict: true });
  const account = values['account-id'] ?? process.env.CLOUDFLARE_ACCOUNT_ID;
  const name = values['worker-name'];
  if (!account || !/^[a-f0-9]{32}$/.test(account)) throw new Error('--account-id must be a 32-character Cloudflare account ID.');
  if (!/^[a-z][a-z0-9-]{2,62}$/.test(name)) throw new Error('Invalid --worker-name.');
  const publicUrl = values['public-url'] ?? process.env.REMOTE_COMMANDER_PUBLIC_URL;
  if (!publicUrl) throw new Error('--public-url or REMOTE_COMMANDER_PUBLIC_URL is required.');
  const url = new URL(publicUrl);
  if (url.protocol !== 'https:' || url.username || url.password || url.pathname !== '/' || url.search || url.hash || url.port) throw new Error('--public-url must be an HTTPS origin on port 443.');
  const workersDev = url.hostname.endsWith('.workers.dev');
  if (workersDev && (url.hostname.split('.').length !== 4 || url.hostname.split('.')[0] !== name)) throw new Error('workers.dev URL must match --worker-name and your account subdomain.');
  if (!workersDev && !/^[a-f0-9]{32}$/.test(values['zone-id'] ?? '')) throw new Error('--zone-id is required for a custom domain.');
  if (!workersDev && !values['dry-run'] && !process.env.CLOUDFLARE_API_TOKEN) throw new Error('CLOUDFLARE_API_TOKEN is required for Terraform domain provisioning.');
  const secretFile = values['admin-secret-file'] ?? process.env.REMOTE_COMMANDER_ADMIN_SECRET_FILE;
  if (!values['dry-run'] && !secretFile) throw new Error('--admin-secret-file or REMOTE_COMMANDER_ADMIN_SECRET_FILE is required for deployment.');
  const secret = values['dry-run'] ? undefined : secretFromFile(path.resolve(root, secretFile));
  const audience = process.env.REMOTE_COMMANDER_ACCESS_AUD || (values['dry-run'] ? '0'.repeat(64) : '');
  const owner = process.env.REMOTE_COMMANDER_OWNER_EMAIL || (values['dry-run'] ? 'owner@example.invalid' : '');
  if (!/^[a-f0-9]{64}$/i.test(audience) || !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(owner)) {
    throw new Error('Provision infra/access first, then set REMOTE_COMMANDER_ACCESS_AUD and REMOTE_COMMANDER_OWNER_EMAIL. Public deployments require owner-only Cloudflare Access.');
  }

  const directory = path.join(root, '.deploy');
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  const config = JSON.parse(readFileSync(path.join(root, 'wrangler.json'), 'utf8'));
  delete config.$schema; delete config.build; delete config.dev; delete config.access;
  Object.assign(config, {
    name, account_id: account, main: path.join(root, 'build/worker/shim.mjs'),
    workers_dev: workersDev, preview_urls: false,
    vars: { PUBLIC_URL: url.origin, ALLOW_LOCALHOST: 'false', CHATGPT_ONLY: 'true', ACCESS_REQUIRED: 'true', ACCESS_AUD: audience, ACCESS_OWNER_EMAIL: owner },
  });
  const configFile = path.join(directory, 'wrangler.json');
  writeFileSync(configFile, `${JSON.stringify(config, null, 2)}\n`, { mode: 0o600 });
  run(process.execPath, [path.join(root, 'scripts/build-worker.mjs')]);

  if (values['dry-run']) {
    run(process.execPath, [wrangler, 'deploy', '--config', configFile, '--dry-run', '--outdir', path.join(directory, 'dry-run')]);
    run('terraform', ['-chdir=infra', 'init', '-backend=false', '-input=false']);
    run('terraform', ['-chdir=infra', 'validate']);
    run('terraform', ['-chdir=infra/access', 'init', '-backend=false', '-input=false']);
    run('terraform', ['-chdir=infra/access', 'validate']);
    console.log('Worker packaging and Terraform validation completed. No cloud resources were changed.');
    return;
  }

  // A newly created Worker returns 503 until the secret is provisioned. Secrets
  // travel over stdin, never command arguments, Terraform variables, or state.
  if (workersDev && process.env.CLOUDFLARE_API_TOKEN) {
    const subdomain = await ensureWorkersSubdomain(account, url.hostname.split('.')[1], process.env.CLOUDFLARE_API_TOKEN);
    writeFileSync(path.join(directory, 'subdomain.json'), JSON.stringify({ account_id: account, subdomain }), { mode: 0o600 });
    console.log(`Verified account subdomain: ${subdomain}.workers.dev`);
  }
  run(process.execPath, [wrangler, 'deploy', '--config', configFile]);
  run(process.execPath, [wrangler, 'secret', 'put', 'ADMIN_TOKEN', '--config', configFile], {
    stdio: ['pipe', 'inherit', 'inherit'], input: `${secret}\n`,
  });
  if (!workersDev) {
    const variables = path.join(directory, 'terraform.tfvars.json');
    const plan = path.join(directory, 'domain.tfplan');
    writeFileSync(variables, JSON.stringify({ account_id: account, zone_id: values['zone-id'], worker_name: name, hostname: url.hostname }), { mode: 0o600 });
    run('terraform', ['-chdir=infra', 'init', '-input=false']);
    run('terraform', ['-chdir=infra', 'plan', '-input=false', `-var-file=${variables}`, `-out=${plan}`]);
    run('terraform', ['-chdir=infra', 'apply', '-input=false', plan]);
  }
  let healthy = false;
  for (let attempt = 0; attempt < 10; attempt++) {
    try {
      const response = await fetch(`${url.origin}/health`, { signal: AbortSignal.timeout(5000), redirect: 'error' });
      const health = await response.json();
      if (response.ok && health.ok && health.service === 'remote-commander') { healthy = true; break; }
    } catch { /* DNS and certificates can require propagation. */ }
    await new Promise((resolve) => setTimeout(resolve, 1000));
  }
  if (!healthy) throw new Error('Deployment commands completed, but the public health check did not pass. Inspect domain propagation and Wrangler before connecting clients.');
  console.log(`Deployment verified: ${url.origin}\nMCP endpoint: ${url.origin}/mcp`);
}

main().catch((error) => { console.error(error.message); process.exitCode = 1; });
