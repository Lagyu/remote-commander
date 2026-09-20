import { randomBytes } from 'node:crypto';
import { writeFileSync } from 'node:fs';
import path from 'node:path';

const destination = process.argv[2];
if (process.argv.length !== 3 || !destination || !path.isAbsolute(destination)) {
  console.error('Usage: node scripts/create-admin-key.mjs /absolute/private/path/commander-admin.key');
  process.exit(1);
}
try {
  writeFileSync(destination, `${randomBytes(32).toString('base64url')}\n`, { flag: 'wx', mode: 0o600 });
  console.log(`Created private administrator credential: ${destination}`);
} catch (error) {
  console.error(`Could not create the key (${error.code ?? 'unknown error'}). The parent directory must exist; existing files are never overwritten.`);
  process.exitCode = 1;
}
