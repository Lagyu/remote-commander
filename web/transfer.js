const $ = (id) => document.getElementById(id);
const base = location.pathname.replace(/\/$/, '');
let metadata, running = false, cancelled = false, active;
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function request(suffix, options = {}) {
  const controller = new AbortController();
  active = controller;
  const timeout = setTimeout(() => controller.abort(), 90000);
  try {
    const response = await fetch(`${base}${suffix}`, { ...options, signal: controller.signal, redirect: 'error', cache: 'no-store' });
    const value = await response.json();
    if (!response.ok) {
      const error = new Error(value.error_description || 'Transfer request failed.');
      error.retryable = response.status === 429 || response.status >= 500;
      throw error;
    }
    return value;
  } finally { clearTimeout(timeout); }
}
async function retry(suffix, options) {
  for (let attempt = 0; ; attempt++) {
    if (cancelled) throw new Error('Transfer cancelled.');
    try { return await request(suffix, options); }
    catch (error) {
      if (cancelled || error.retryable === false || attempt >= 3) throw error;
      $('status').textContent = 'Connection interrupted. Retrying the same chunk safely…';
      await sleep(1000 * 2 ** attempt);
    }
  }
}
function complete(value) {
  $('progress').value = 100;
  $('status').textContent = 'Upload complete. The file is now available on your computer.';
  $('checksum').textContent = `SHA-256\n${value.sha256}`;
  $('checksum').hidden = false;
  $('file').disabled = $('start').disabled = true;
}
async function load() {
  metadata = await request('/status');
  $('destination').textContent = metadata.path;
  $('size').textContent = `${metadata.size.toLocaleString()} bytes`;
  $('expires').textContent = new Date(metadata.expires_at * 1000).toLocaleString();
  $('cancel').disabled = false;
  if (metadata.complete) { complete(metadata); return; }
  $('file').disabled = $('start').disabled = false;
  $('status').textContent = metadata.bytes_received ? `${metadata.bytes_received.toLocaleString()} bytes already received. Select the same file to verify and continue.` : 'Choose a file with the required size, then start the upload.';
}
$('upload-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  if (running || cancelled || !metadata) return;
  const file = $('file').files[0];
  if (!file || file.size !== metadata.size) { $('status').textContent = `Choose a file of exactly ${metadata.size.toLocaleString()} bytes.`; return; }
  running = true;
  $('start').disabled = $('file').disabled = true;
  try {
    metadata = await retry('/status');
    if (metadata.complete) { complete(metadata); return; }
    // Replay any existing prefix so selecting a different same-sized file cannot silently corrupt a resumed upload.
    for (let offset = 0; offset < file.size; offset += metadata.chunk_bytes) {
      const body = await file.slice(offset, Math.min(offset + metadata.chunk_bytes, file.size)).arrayBuffer();
      await retry(`/chunk?offset=${offset}`, { method: 'POST', headers: { 'Content-Type': 'application/octet-stream' }, body });
      const sent = Math.min(offset + body.byteLength, file.size);
      $('progress').value = 100 * sent / file.size;
      $('status').textContent = `${sent.toLocaleString()} / ${file.size.toLocaleString()} bytes verified and transferred.`;
    }
    const result = await retry('/complete', { method: 'POST' });
    complete(result);
  } catch (error) {
    if (!cancelled) {
      // A lost completion response does not imply that publication failed.
      try {
        const state = await request('/status');
        if (state.complete) { complete(state); return; }
      } catch { /* Keep an explicit unknown-status message when disconnected. */ }
      $('start').disabled = $('file').disabled = false;
    }
    $('status').textContent = cancelled ? 'Transfer cancelled.' : `${error.message} Select Start upload to check status and retry safely. Completion may have succeeded if its reply was lost.`;
  } finally { running = false; }
});
$('cancel').addEventListener('click', async () => {
  cancelled = true;
  active?.abort();
  $('start').disabled = $('file').disabled = $('cancel').disabled = true;
  try {
    const result = await request('', { method: 'DELETE' });
    $('status').textContent = result.agent_notified ? 'Transfer cancelled. Incomplete temporary data was removed.' : 'Link revoked. The offline computer will discard incomplete temporary data when its session expires.';
  } catch (error) { $('status').textContent = `Cancellation could not be confirmed: ${error.message}`; }
});
window.addEventListener('beforeunload', (event) => { if (running && !cancelled) { event.preventDefault(); event.returnValue = ''; } });
load().catch((error) => { $('status').textContent = error.message; });
