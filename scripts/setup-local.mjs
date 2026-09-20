import { randomBytes } from 'node:crypto';
import { writeFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const file = fileURLToPath(new URL('../.dev.vars', import.meta.url));
if (existsSync(file)) {
  console.log('Existing .dev.vars preserved.');
} else {
  writeFileSync(file, `ADMIN_TOKEN=${randomBytes(32).toString('base64url')}\n`, { mode: 0o600, flag: 'wx' });
  console.log('Created private .dev.vars. Read its ADMIN_TOKEN locally to unlock the development dashboard.');
}
