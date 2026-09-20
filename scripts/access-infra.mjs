import { spawnSync } from 'node:child_process';
import { writeFileSync, renameSync } from 'node:fs';
import path from 'node:path';
import { loadDeploymentEnv, projectRoot, readPrivateFile } from './deployment-env.mjs';

process.umask(0o077);
loadDeploymentEnv();
const action = process.argv[2] ?? 'plan';
const variables = path.join(projectRoot, '.deploy/access.tfvars.json');
const plan = path.join(projectRoot, '.deploy/access.tfplan');
const directory = path.join(projectRoot, 'infra/access');

function terraform(args, capture = false) {
  const result = spawnSync('terraform', args, { cwd: directory, encoding: 'utf8', stdio: capture ? 'pipe' : 'inherit' });
  if (result.error || result.status !== 0) throw new Error('Terraform failed; no later step was run. Inspect its output.');
  return result.stdout;
}

async function main() {
  if (!['bootstrap', 'plan', 'apply'].includes(action)) throw new Error('Use bootstrap, plan, or apply.');
  const vars = JSON.parse(readPrivateFile(variables));
  if (!process.env.CLOUDFLARE_API_TOKEN || vars.account_id !== process.env.CLOUDFLARE_ACCOUNT_ID
      || !/^[a-f0-9]{32}$/.test(vars.account_id) || !/^[a-z0-9][a-z0-9-]{2,62}$/.test(vars.team_name)
      || vars.hostname !== new URL(process.env.REMOTE_COMMANDER_PUBLIC_URL).hostname
      || !/^[^@\s]+@[^@\s]+\.[^@\s]+$/.test(vars.owner_email)) throw new Error('Access variables must match this project’s configured account and service.');
  terraform(['init', '-input=false']);
  if (action === 'bootstrap') {
    const endpoint = `https://api.cloudflare.com/client/v4/accounts/${vars.account_id}/access/organizations`;
    const headers = { Authorization: `Bearer ${process.env.CLOUDFLARE_API_TOKEN}`, 'Content-Type': 'application/json' };
    const authDomain = `${vars.team_name}.cloudflareaccess.com`;
    let response = await fetch(endpoint, { headers, redirect: 'error', signal: AbortSignal.timeout(20000) });
    let body = await response.json();
    if (response.status === 403 && body.errors?.some(e => e.code === 9999 && e.message?.includes('not_enabled'))) {
      // Provider 5.25 uses PUT even for create; a fresh org needs the documented
      // POST first. This does not subscribe to a paid plan or supply billing.
      response = await fetch(endpoint, { method: 'POST', headers, redirect: 'error', signal: AbortSignal.timeout(20000), body: JSON.stringify({
        auth_domain: authDomain, name: 'Remote Commander', session_duration: '1h',
        mfa_config: { allowed_authenticators: ['totp', 'biometrics', 'security_key'], session_duration: '0m' }, mfa_required_for_all_apps: false,
      }) });
      body = await response.json();
    }
    if (!response.ok || !body.success) throw new Error(`Access bootstrap failed (HTTP ${response.status}; codes ${body.errors?.map(e => e.code).join(',')}).`);
    if (body.result.auth_domain !== authDomain) throw new Error('Existing Zero Trust team differs; it was not changed. Import and review the existing organization manually.');
    const state = terraform(['state', 'list'], true);
    if (!state.split('\n').includes('cloudflare_zero_trust_organization.commander')) {
      terraform(['import', '-input=false', `-var-file=${variables}`, 'cloudflare_zero_trust_organization.commander', vars.account_id]);
    }
    console.log('Access organization bootstrapped. Next run plan, inspect it, then apply.');
    return;
  }
  if (action === 'plan') {
    terraform(['validate']);
    terraform(['plan', '-input=false', `-var-file=${variables}`, `-out=${plan}`]);
    return;
  }
  const planned = JSON.parse(terraform(['show', '-json', plan], true));
  if (Object.entries(vars).some(([key, value]) => planned.variables?.[key]?.value !== value)) {
    throw new Error('Access variables changed after planning. Run plan again before apply.');
  }
  terraform(['apply', '-input=false', plan]);
  const outputs = JSON.parse(terraform(['output', '-json'], true));
  const audience = outputs.admin_audience?.value;
  if (!/^[a-f0-9]{64}$/.test(audience)) throw new Error('Terraform did not return an Access application audience.');
  let contents = readPrivateFile(path.join(projectRoot, '.env'));
  for (const [key, value] of Object.entries({ REMOTE_COMMANDER_ACCESS_AUD: audience, REMOTE_COMMANDER_OWNER_EMAIL: vars.owner_email })) {
    contents = contents.replace(new RegExp(`^${key}=.*\\n?`, 'm'), '').trimEnd() + `\n${key}=${value}\n`;
  }
  const temporary = path.join(projectRoot, '.deploy/access-env.tmp');
  writeFileSync(temporary, contents, { mode: 0o600, flag: 'wx' });
  renameSync(temporary, path.join(projectRoot, '.env'));
  console.log('Access configuration saved to private .env. Deploy the Worker with npm run deploy.');
}

main().catch(error => { console.error(error.message); process.exitCode = 1; });
