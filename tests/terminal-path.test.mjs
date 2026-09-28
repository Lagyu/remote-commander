import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync, existsSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { BOOTSTRAP_PATH, resolveTerminalPath, validateTerminalPath } from '../scripts/terminal-path.mjs';

function fixture(t) {
  const home = mkdtempSync(path.join(tmpdir(), 'commander-terminal-path-'));
  t.after(() => rmSync(home, { recursive: true, force: true }));
  return home;
}

for (const shell of ['/bin/bash', '/bin/zsh'].filter(existsSync)) {
  test(`${shell} startup PATH reaches a noninteractive command without importing other variables`, async (t) => {
    const home = fixture(t);
    const tools = path.join(home, 'tool bin');
    mkdirSync(tools);
    const expected = `${tools}:/usr/bin:/bin`;
    const startup = 'printf "startup chatter\\n"\n' +
      'printf x >> "$HOME/startup-count"\n' +
      'export PROFILE_SECRET=private-profile-value\n' +
      'printf "%s" "${DEPLOYMENT_SECRET-unset}" > "$HOME/inherited-secret"\n' +
      'export PATH="$HOME/tool bin:/usr/bin:/bin"\n';
    if (shell.endsWith('bash')) {
      writeFileSync(path.join(home, '.bash_profile'), '. "$HOME/.bashrc"\n');
      writeFileSync(path.join(home, '.bashrc'), startup);
    } else writeFileSync(path.join(home, '.zshrc'), startup);
    writeFileSync(path.join(tools, 'terminal-tool'), '#!/bin/sh\nprintf "tool-ok:%s" "${PROFILE_SECRET-unset}"\n', { mode: 0o700 });
    const previous = process.env.DEPLOYMENT_SECRET;
    process.env.DEPLOYMENT_SECRET = 'must-not-reach-startup';
    let terminal;
    try { terminal = await resolveTerminalPath({ shell, home, timeoutMs: 10000 }); }
    finally {
      if (previous === undefined) delete process.env.DEPLOYMENT_SECRET;
      else process.env.DEPLOYMENT_SECRET = previous;
    }
    assert.deepEqual(terminal, { shell, path: expected });
    assert.equal(readFileSync(path.join(home, 'inherited-secret'), 'utf8'), 'unset');
    for (let attempt = 0; attempt < 2; attempt++) {
      const result = spawnSync('/bin/sh', ['-c', 'terminal-tool'], {
        cwd: home, env: { HOME: home, PATH: terminal.path, LANG: 'en_US.UTF-8' }, encoding: 'utf8', timeout: 5000,
      });
      assert.equal(result.status, 0, result.stderr);
      assert.equal(result.stdout, 'tool-ok:unset');
    }
    assert.equal(readFileSync(path.join(home, 'startup-count'), 'utf8'), 'x');
  });
}

test('PATH validation preserves order, spaces, duplicates and XML-significant characters', () => {
  const value = '/a b:/a&b:/a<b:/usr/bin:/a b';
  assert.equal(validateTerminalPath(value), value);
  assert.equal(validateTerminalPath(BOOTSTRAP_PATH), BOOTSTRAP_PATH);
  for (const bad of ['', '/bin:', ':/bin', '/bin::/usr/bin', '.:/bin', 'relative:/bin', '/bin\n/evil', '/bin\0/evil', '/' + 'x'.repeat(32768)]) {
    assert.throws(() => validateTerminalPath(bad), /Terminal PATH/);
  }
});

function failingShell(t, source) {
  const home = fixture(t);
  const shell = path.join(home, 'zsh');
  writeFileSync(shell, `#!/bin/sh\n${source}\n`, { mode: 0o700 });
  return { shell, home };
}

test('failed or missing startup suppresses profile output and inherited credentials', async (t) => {
  const options = failingShell(t, 'printf "sensitive-output\\n"; printf "sensitive-error\\n" >&2; exit 7');
  await assert.rejects(resolveTerminalPath(options), (error) => {
    assert.match(error.message, /Login shell failed/);
    assert.doesNotMatch(error.message, /sensitive/);
    return true;
  });
  await assert.rejects(resolveTerminalPath({ ...options, shell: path.join(options.home, 'missing', 'bash') }), /Could not start/);
});

test('missing result, relative path and excessive output never become a successful discovery', async (t) => {
  await assert.rejects(resolveTerminalPath(failingShell(t, 'printf unrelated')), /unambiguous/);
  const home = fixture(t);
  writeFileSync(path.join(home, '.bash_profile'), 'export PATH=.:/bin\n');
  await assert.rejects(resolveTerminalPath({ shell: '/bin/bash', home }), /Terminal PATH/);
  await assert.rejects(resolveTerminalPath(failingShell(t, '/usr/bin/yes startup-noise')), /output exceeded/);
});

test('startup hangs are bounded and the waiting child process group is terminated', async (t) => {
  const options = failingShell(t, '/bin/sleep 30 &\nchild=$!\nprintf "%s" "$child" > "$HOME/child-pid"\nwait "$child"');
  const start = Date.now();
  // Process launch itself can exceed 500 ms under the full browser suite's load.
  // Observe actual child readiness before asserting descendant cleanup.
  const timedOut = assert.rejects(resolveTerminalPath({ ...options, timeoutMs: 5000 }), /timed out/);
  const pidFile = path.join(options.home, 'child-pid');
  let pid;
  while (Date.now() - start < 4000) {
    if (existsSync(pidFile)) {
      const value = readFileSync(pidFile, 'utf8');
      if (/^\d+$/.test(value)) { pid = Number(value); break; }
    }
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  await timedOut;
  assert.ok(pid > 0, 'Fixture child must start before the deadline to exercise cleanup');
  assert.ok(Date.now() - start < 10000);
  for (let i = 0; i < 50; i++) {
    const status = spawnSync('/bin/ps', ['-p', String(pid), '-o', 'stat='], { encoding: 'utf8' });
    if (status.status !== 0 || /^\s*Z/.test(status.stdout)) return;
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
  assert.fail('Startup descendant remained running after the deadline');
});

test('invalid probe options are rejected before a shell is spawned', async () => {
  for (const shell of ['bash', '/bin/fish', null]) await assert.rejects(resolveTerminalPath({ shell }), /login shell/);
  for (const timeoutMs of [0, 99, NaN, 60001]) await assert.rejects(resolveTerminalPath({ shell: '/bin/bash', timeoutMs }), /options/);
});
