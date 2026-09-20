import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtempSync, writeFileSync, chmodSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { ownerAccessHeaders } from '../scripts/owner-access.mjs';

test('Operator scripts reject missing, unsafe, expired, unrelated and header-injecting Access credentials', () => {
  const directory = mkdtempSync(path.join(tmpdir(), 'commander-owner-access-'));
  const file = path.join(directory, 'access.jwt');
  const keys = ['REMOTE_COMMANDER_ACCESS_AUD', 'REMOTE_COMMANDER_ACCESS_TOKEN_FILE'];
  const before = new Map(keys.map(key => [key, process.env[key]]));
  const aud = 'a'.repeat(64);
  const token = (claims) => `e30.${Buffer.from(JSON.stringify(claims)).toString('base64url')}.testsignature`;
  try {
    process.env.REMOTE_COMMANDER_ACCESS_AUD = aud;
    process.env.REMOTE_COMMANDER_ACCESS_TOKEN_FILE = file;
    assert.throws(() => ownerAccessHeaders(), /login required/);
    const jwt = token({ aud: [aud], exp: Date.now() / 1000 + 3600 });
    writeFileSync(file, jwt, { mode: 0o600 });
    assert.deepEqual(ownerAccessHeaders(), { Cookie: `CF_Authorization=${jwt}` });
    // Client-side parsing is only an operator safeguard. Forged credentials are
    // rejected by the real Access context check, covered in access.test.mjs.
    chmodSync(file, 0o644);
    assert.throws(() => ownerAccessHeaders(), /login required/);
    chmodSync(file, 0o600);
    for (const claims of [{ aud: [aud], exp: 1 }, { aud: ['other-app'], exp: Date.now() / 1000 + 3600 }, { aud: aud, exp: Date.now() / 1000 + 3600 }]) {
      writeFileSync(file, token(claims));
      assert.throws(() => ownerAccessHeaders(), /expired or belongs/);
    }
    writeFileSync(file, jwt+'\r\nInjected: value');
    assert.throws(() => ownerAccessHeaders(), /only a JWT/);
  } finally {
    for (const [key, value] of before) { if (value === undefined) delete process.env[key]; else process.env[key] = value; }
    rmSync(directory, { recursive: true, force: true });
  }
});
