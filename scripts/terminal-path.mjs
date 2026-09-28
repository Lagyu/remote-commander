import { spawn } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { userInfo } from 'node:os';
import path from 'node:path';

export const BOOTSTRAP_PATH = '/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin';
const OUTPUT_LIMIT = 64 * 1024;

export function validateTerminalPath(value) {
  if (typeof value !== 'string' || !value || Buffer.byteLength(value) > 32768 ||
      /[\x00-\x1f\x7f]/u.test(value) || value.split(':').some((entry) => !path.isAbsolute(entry))) {
    throw new Error('Terminal PATH must contain only nonempty absolute directories, without control characters.');
  }
  // Preserve ordering, duplicates and spaces exactly as configured by the owner.
  return value;
}

/** Run the OS account's login shell once during installation, not per command.
 * Only PATH is retained. Never inherit deployment credentials or print profile output.
 * This is intentionally a non-PTY probe: unattended startup must not require input. */
export async function resolveTerminalPath(options = {}) {
  const owner = userInfo();
  const { shell = owner.shell, home = owner.homedir, username = owner.username, timeoutMs = 20000 } = options;
  if (!shell || !path.isAbsolute(shell) || !['bash', 'zsh'].includes(path.basename(shell))) {
    throw new Error('Terminal PATH discovery requires an absolute Bash or zsh login shell.');
  }
  if (!path.isAbsolute(home) || !Number.isSafeInteger(timeoutMs) || timeoutMs < 100 || timeoutMs > 60000) {
    throw new Error('Invalid terminal PATH discovery options.');
  }
  const marker = `RDC_PATH_${randomUUID()}`;
  const bytes = await new Promise((resolve, reject) => {
    let completed = false;
    let size = 0;
    const chunks = [];
    const child = spawn(shell, ['-ilc', `builtin printf '\\0${marker}\\0%s\\0' "$PATH"`], {
      cwd: home,
      env: { HOME: home, USER: username, LOGNAME: username, SHELL: shell, PATH: BOOTSTRAP_PATH, LANG: 'en_US.UTF-8' },
      detached: true,
      stdio: ['ignore', 'pipe', 'ignore'],
    });
    const killGroup = () => {
      if (child.pid) {
        try { process.kill(-child.pid, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') child.kill('SIGKILL'); }
      }
    };
    const finish = (error) => {
      if (completed) return;
      completed = true;
      clearTimeout(timer);
      killGroup();
      child.stdout.destroy();
      if (error) reject(error);
      else resolve(Buffer.concat(chunks));
    };
    const timer = setTimeout(() => finish(new Error('Terminal PATH discovery timed out; no agent configuration was changed.')), timeoutMs);
    child.once('error', () => finish(new Error('Could not start the login shell for PATH discovery.')));
    child.stdout.on('data', (chunk) => {
      size += chunk.length;
      if (size > OUTPUT_LIMIT) finish(new Error('Terminal startup output exceeded the PATH discovery limit.'));
      else chunks.push(chunk);
    });
    // A startup script must not leave a child holding the capture pipe open.
    child.once('exit', killGroup);
    child.once('close', (code) => finish(code === 0 ? undefined : new Error('Login shell failed during PATH discovery; profile output was withheld.')));
  });
  const text = bytes.toString('utf8');
  const startMarker = `\0${marker}\0`;
  const start = text.indexOf(startMarker);
  const end = text.indexOf('\0', start + startMarker.length);
  if (start < 0 || end < 0 || text.indexOf(startMarker, start + startMarker.length) !== -1) {
    throw new Error('Login shell did not return an unambiguous PATH record.');
  }
  return { shell, path: validateTerminalPath(text.slice(start + startMarker.length, end)) };
}
