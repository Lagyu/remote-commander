import { parseArgs } from 'node:util';
import { homedir, hostname } from 'node:os';
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, copyFileSync, renameSync, chmodSync, writeFileSync, realpathSync, statSync, openSync, closeSync } from 'node:fs';
import path from 'node:path';
import { loadDeploymentEnv, projectRoot, readPrivateFile } from './deployment-env.mjs';
import { ownerAccessHeaders } from './owner-access.mjs';

process.umask(0o077);
const label = 'app.remote-commander.agent';
const xml = (value) => String(value).replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;').replaceAll('"', '&quot;').replaceAll("'", '&apos;');
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function main() {
  if (process.platform !== 'darwin') throw new Error('This installer is for macOS LaunchAgents. Use the Rust CLI directly on Linux.');
  loadDeploymentEnv();
  const { values } = parseArgs({ options: {
    root: { type: 'string' }, name: { type: 'string', default: hostname() },
    'allow-write': { type: 'boolean', default: false }, 'allow-shell': { type: 'boolean', default: false },
  }, strict: true });
  if (!values.root) throw new Error('--root is required. Pass --root "$HOME" to expose your home directory.');
  const root = realpathSync(values.root);
  if (!statSync(root).isDirectory()) throw new Error('--root must be a directory.');
  const origin = new URL(process.env.REMOTE_COMMANDER_PUBLIC_URL);
  if (origin.protocol !== 'https:' || origin.username || origin.password || origin.pathname !== '/' || origin.search || origin.hash) throw new Error('The deployment URL must be an HTTPS origin.');
  const adminFile = path.resolve(projectRoot, process.env.REMOTE_COMMANDER_ADMIN_SECRET_FILE ?? '.deploy/admin.key');
  const admin = readPrivateFile(adminFile, 4096).trim();
  const accessHeaders = ownerAccessHeaders();
  async function api(route, { body, owner = false, method } = {}) {
    const response = await fetch(new URL(route, origin), {
      method: method ?? (body ? 'POST' : 'GET'), redirect: 'error', signal: AbortSignal.timeout(15000),
      headers: { 'Content-Type': 'application/json', ...(owner ? { ...accessHeaders, Authorization: `Bearer ${admin}` } : {}) },
      body: body ? JSON.stringify(body) : undefined,
    });
    const data = await response.json();
    if (!response.ok) throw new Error(`${route} failed (HTTP ${response.status}, ${data.error ?? 'unknown'}).`);
    return data;
  }
  const source = path.join(projectRoot, 'target/release/remote-commander');
  if (!existsSync(source)) throw new Error('Build the agent first: cargo build --release -p rdc-agent');
  const directory = path.join(homedir(), 'Library/Application Support/Remote Commander');
  const logs = path.join(homedir(), 'Library/Logs/Remote Commander');
  const launchAgents = path.join(homedir(), 'Library/LaunchAgents');
  for (const dir of [directory, logs, launchAgents]) mkdirSync(dir, { recursive: true, mode: 0o700 });
  const configFile = path.join(directory, 'device.json');
  const installedBinary = path.join(directory, 'remote-commander');
  const plistFile = path.join(launchAgents, `${label}.plist`);
  if (existsSync(plistFile) && !readPrivateFile(plistFile).includes(xml(installedBinary))) {
    throw new Error('An unrelated LaunchAgent uses this label; it was left unchanged.');
  }
  let device;
  if (existsSync(configFile)) {
    device = JSON.parse(readPrivateFile(configFile));
    if (device.server !== origin.origin) throw new Error('The installed agent belongs to a different server. Revoke it before changing deployments.');
    const directory = await api('/api/devices', { owner: true });
    if (!directory.devices.some((d) => d.device_id === device.device_id)) throw new Error('The saved device was revoked. Remove its local credential before pairing again.');
  } else {
    const pairing = await api('/pair/start', { body: { name: values.name } });
    await api('/api/pair/approve', { owner: true, body: { user_code: pairing.user_code } });
    const redeemed = await api('/pair/token', { body: { device_code: pairing.device_code } });
    device = { server: origin.origin, device_id: redeemed.device_id, token: redeemed.token };
    writeFileSync(configFile, `${JSON.stringify(device, null, 2)}\n`, { mode: 0o600, flag: 'wx' });
  }
  const tempBinary = `${installedBinary}.next`;
  copyFileSync(source, tempBinary);
  chmodSync(tempBinary, 0o700);
  renameSync(tempBinary, installedBinary);
  const args = [installedBinary, 'run', '--config', configFile, '--root', root];
  if (values['allow-write']) args.push('--allow-write');
  if (values['allow-shell']) args.push('--allow-shell');
  const stdout = path.join(logs, 'agent.stdout.log');
  const stderr = path.join(logs, 'agent.stderr.log');
  for (const file of [stdout, stderr]) closeSync(openSync(file, 'a', 0o600));
  const plist = `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>${label}</string>
<key>ProgramArguments</key><array>${args.map((a) => `<string>${xml(a)}</string>`).join('')}</array>
<key>WorkingDirectory</key><string>${xml(root)}</string>
<key>RunAtLoad</key><true/>
<key>KeepAlive</key><false/>
<key>ProcessType</key><string>Background</string>
<key>ExitTimeOut</key><integer>15</integer>
<key>Umask</key><integer>63</integer>
<key>EnvironmentVariables</key><dict><key>PATH</key><string>/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin</string></dict>
<key>StandardOutPath</key><string>${xml(stdout)}</string>
<key>StandardErrorPath</key><string>${xml(stderr)}</string>
</dict></plist>\n`;
  writeFileSync(`${plistFile}.next`, plist, { mode: 0o600 });
  const lint = spawnSync('/usr/bin/plutil', ['-lint', `${plistFile}.next`], { encoding: 'utf8' });
  if (lint.status !== 0) throw new Error('Generated LaunchAgent did not pass plutil validation.');
  renameSync(`${plistFile}.next`, plistFile);
  const domain = `gui/${process.getuid()}`;
  const service = `${domain}/${label}`;
  if (spawnSync('/bin/launchctl', ['print', service], { stdio: 'ignore' }).status === 0) {
    if (spawnSync('/bin/launchctl', ['bootout', service], { stdio: 'inherit' }).status !== 0) throw new Error('Could not stop the existing agent.');
    // bootout returns before a terminating job has necessarily unloaded.
    // Bootstrapping immediately can fail with EIO while the old PID exits.
    for (let attempt = 0; attempt < 40; attempt++) {
      if (spawnSync('/bin/launchctl', ['print', service], { stdio: 'ignore' }).status !== 0) break;
      await delay(500);
    }
    if (spawnSync('/bin/launchctl', ['print', service], { stdio: 'ignore' }).status === 0) {
      throw new Error(`The previous agent has not unloaded; inspect launchctl print ${service} before restarting.`);
    }
  }
  if (spawnSync('/bin/launchctl', ['bootstrap', domain, plistFile], { stdio: 'inherit' }).status !== 0) throw new Error('Could not start the LaunchAgent.');
  let status;
  for (let attempt = 0; attempt < 20; attempt++) {
    status = spawnSync('/bin/launchctl', ['print', service], { encoding: 'utf8' });
    const directory = await api('/api/devices', { owner: true });
    if (/state = running/.test(status.stdout) && directory.devices.some((d) => d.device_id === device.device_id && d.online)) {
      const report = { service, root, allow_write: values['allow-write'], allow_shell: values['allow-shell'],
        pid: Number(status.stdout.match(/\bpid = (\d+)/)?.[1]), device_id: device.device_id,
        checked_at: new Date().toISOString(), online: true, keep_alive: false };
      writeFileSync(path.join(directoryPath(), 'agent-installation.json'), `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 });
      console.log(JSON.stringify(report, null, 2));
      console.log(`Credential: ${configFile}\nLaunchAgent: ${plistFile}\nLogs: ${logs}`);
      return;
    }
    await delay(1000);
  }
  throw new Error(`Agent did not come online; inspect ${stderr} and launchctl print ${service}.`);
}

function directoryPath() {
  const directory = path.join(projectRoot, '.deploy');
  mkdirSync(directory, { recursive: true, mode: 0o700 });
  return directory;
}

main().catch((error) => { console.error(error.message); process.exitCode = 1; });
