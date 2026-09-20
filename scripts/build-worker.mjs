import { spawnSync } from 'node:child_process';
import { existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = fileURLToPath(new URL('../', import.meta.url));
const local = path.join(root, '.tools/bin/worker-build');
const binary = existsSync(local) ? local : 'worker-build';
const result = spawnSync(binary, ['--release', '--out-dir', '../../build'], {
  cwd: path.join(root, 'crates/worker'),
  stdio: 'inherit',
  env: { ...process.env, PATH: `${path.join(root, '.tools/bin')}${path.delimiter}${process.env.PATH ?? ''}` },
});
if (result.error) {
  console.error('worker-build is required. Run: cargo install worker-build --version 0.8.6 --locked --root .tools');
  process.exit(1);
}
if (result.status !== 0) process.exit(result.status ?? 1);
if (!existsSync(path.join(root, 'build/worker/shim.mjs'))) {
  console.error('Build did not produce build/worker/shim.mjs.');
  process.exit(1);
}
