import path from 'node:path';
import { projectRoot, readPrivateFile } from './deployment-env.mjs';

// A short-lived human session, obtained by cloudflared after the owner's login
// and any configured MFA. No service-token policy or administrator bypass is provisioned.
export function ownerAccessHeaders() {
  if (!process.env.REMOTE_COMMANDER_ACCESS_AUD) return {};
  const file = path.resolve(projectRoot, process.env.REMOTE_COMMANDER_ACCESS_TOKEN_FILE ?? '.deploy/access.jwt');
  let token;
  try { token = readPrivateFile(file, 16384).trim(); }
  catch { throw new Error('Owner Access login required. See docs/OPERATIONS.md for the cloudflared login command.'); }
  if (!/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(token)) throw new Error('The owner Access token file must contain only a JWT.');
  let claims;
  try { claims = JSON.parse(Buffer.from(token.split('.')[1], 'base64url')); }
  catch { throw new Error('Invalid owner Access token file.'); }
  if (!Array.isArray(claims.aud) || !claims.aud.includes(process.env.REMOTE_COMMANDER_ACCESS_AUD)
      || typeof claims.exp !== 'number' || claims.exp <= Date.now() / 1000 + 30) {
    throw new Error('Owner Access session expired or belongs to another application. Sign in again.');
  }
  // The local claims check only prevents mistakes; Cloudflare verifies the
  // signature, issuer, audience, expiry and policy on every protected request.
  return { Cookie: `CF_Authorization=${token}` };
}
