import test from 'node:test';
import assert from 'node:assert/strict';
import { constants, openSync, closeSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { harness, call, delay, until } from './harness.mjs';

test('Invalid directory paths cannot block subsequent shell commands', { timeout: 60000 }, async (t) => {
  const h = await harness();
  t.after(() => h.cleanup());
  t.diagnostic(`Lifecycle logs: ${h.logDirectory}`);
  await h.startWorker();
  const client = await h.sdk(await h.authorize());
  const device = await h.pair('Directory responsiveness');
  await h.startAgent(device);
  const args = (value = {}) => ({ device_id: device.device_id, ...value });
  const fifo = path.join(h.files, 'directory-pipe');
  assert.equal(spawnSync('mkfifo', [fifo]).status, 0);

  // Rescue a buggy blocking open so the regression still cleans up normally.
  for (const name of ['list_directory', 'start_search']) {
    const release = setTimeout(() => {
      closeSync(openSync(fifo, constants.O_RDWR | constants.O_NONBLOCK));
    }, 2000);
    try {
      const started = Date.now();
      await call(client, name, args({ path: 'directory-pipe', ...(name === 'start_search' ? { pattern: 'anything' } : {}) }), true);
      assert.ok(Date.now() - started < 1000, `${name} must reject a FIFO without waiting for a writer`);
    } finally {
      clearTimeout(release);
    }
  }
  assert.equal((await call(client, 'ping_device', args())).ok, true);
  const session = await call(client, 'start_process', args({ command: 'printf responsive', timeout_ms: 3000 }));
  const output = await until(async () => {
    const result = await call(client, 'read_process_output', args({ session_id: session.session_id }));
    return !result.running && result;
  }, 'shell completion after invalid paths');
  assert.equal(output.exit_code, 0);
  assert.equal(output.output, 'responsive');

  await t.test('A blocked stdin write does not hold up health checks or another launch', async () => {
    const sleeping = await call(client, 'start_process', args({ command: 'sleep 30', timeout_ms: 30000 }));
    const writing = (async () => {
      for (let i = 0; i < 64; i++) {
        const result = await client.callTool({ name: 'interact_with_process', arguments: args({ session_id: sleeping.session_id, input: 'x'.repeat(8192) }) });
        if (result.isError) return result;
      }
      throw new Error('The sleeping process unexpectedly consumed all stdin');
    })();
    // Fill the real OS pipe while the child deliberately does not read it.
    await delay(250);
    try {
      const started = Date.now();
      assert.equal((await call(client, 'ping_device', args())).ok, true);
      assert.equal((await call(client, 'get_config', args())).allow_shell, true);
      const launched = await call(client, 'start_process', args({ command: 'printf launched', timeout_ms: 3000 }));
      assert.ok(Date.now() - started < 1000, 'Health checks and launching must not wait for the blocked input write');
      const result = await until(async () => {
        const value = await call(client, 'read_process_output', args({ session_id: launched.session_id }));
        return !value.running && value;
      }, 'concurrent shell completion');
      assert.equal(result.exit_code, 0);
      assert.equal(result.output, 'launched');
      assert.match((await writing).content[0].text, /did not consume stdin/);
    } finally {
      await call(client, 'force_terminate', args({ session_id: sleeping.session_id }));
      await writing;
    }
    assert.equal((await call(client, 'ping_device', args())).ok, true);
  });
});
