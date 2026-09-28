const $ = (id) => document.getElementById(id);
let credential = '';
let refreshing = false;
let selectedCode = '';

function element(tag, text, className) {
  const node = document.createElement(tag);
  if (text !== undefined) node.textContent = text;
  if (className) node.className = className;
  return node;
}

function notice(message, error = false) {
  $('notice').textContent = message;
  $('notice').className = `notice${error ? ' error' : ''}`;
  $('notice').hidden = !message;
}

async function api(path, options = {}) {
  const response = await fetch(path, {
    ...options,
    cache: 'no-store',
    headers: { Authorization: `Bearer ${credential}`, 'Content-Type': 'application/json', ...options.headers },
    signal: AbortSignal.timeout(15000),
  });
  const body = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(body.error_description || `Request failed (${response.status})`);
  return body;
}

function renderDevices(devices) {
  $('device-count').textContent = String(devices.length);
  $('device-list').replaceChildren();
  if (!devices.length) {
    const empty = element('div', undefined, 'empty');
    empty.append(element('h3', 'Your next workspace starts here.'), element('p', 'Run the agent on your computer, then use its pairing code to connect it. Your devices will appear here once pairing is complete.'));
    $('device-list').append(empty);
    return;
  }
  for (const device of devices) {
    const card = element('article', undefined, 'device-card');
    const icon = element('div', '▱', 'computer-icon');
    icon.setAttribute('aria-hidden', 'true');
    card.append(icon, element('h3', device.name), element('p', device.device_id, 'identifier'));
    const bottom = element('div', undefined, 'device-footer');
    bottom.append(element('span', device.online ? '● Online' : '○ Offline', `badge${device.online ? '' : ' offline'}`));
    const revoke = element('button', 'Disconnect', 'secondary danger');
    revoke.type = 'button';
    revoke.setAttribute('aria-label', `Disconnect ${device.name}`);
    revoke.addEventListener('click', async () => {
      if (!confirm(`Revoke access for ${device.name}? The agent will stop, and this computer must be paired again.`)) return;
      revoke.disabled = true;
      try {
        await api(`/api/devices/${encodeURIComponent(device.device_id)}`, { method: 'DELETE' });
        notice(`${device.name} was disconnected.`);
        await refresh();
      } catch (error) { notice(error.message, true); }
      finally { revoke.disabled = false; }
    });
    bottom.append(revoke);
    card.append(bottom);
    $('device-list').append(card);
  }
}

function renderActivity(activity, devices) {
  const names = new Map(devices.map((d) => [d.device_id, d.name]));
  $('activity-list').replaceChildren();
  if (!activity.length) {
    const cell = element('td', 'No operations yet. Activity appears after pairing or using a tool.');
    cell.colSpan = 4;
    const row = element('tr'); row.append(cell); $('activity-list').append(row);
  }
  for (const event of [...activity].reverse()) {
    const row = element('tr');
    row.append(element('td', event.tool), element('td', names.get(event.device_id) || (event.device_id ? `${event.device_id.slice(0, 10)}…` : 'Workspace')), element('td', event.status), element('td', new Date(event.at * 1000).toLocaleString()));
    $('activity-list').append(row);
  }
}

async function refresh() {
  if (!credential || refreshing) return;
  refreshing = true;
  try {
    const [directory, log, connection] = await Promise.all([api('/api/devices'), api('/api/activity'), api('/api/connection')]);
    renderDevices(directory.devices);
    renderActivity(log.activity, directory.devices);
    $('connection-status').textContent = !connection.chatgpt_only ? 'Local development: generic MCP clients are enabled.'
      : connection.status === 'linked' ? `One ChatGPT connection is approved. Permissions: ${connection.scope}. Additional connections are blocked.`
      : connection.status === 'awaiting_chatgpt' ? `Linking is open until ${new Date(connection.linking_expires_at * 1000).toLocaleTimeString()}. Connect from your ChatGPT account and approve it with your administrator key.`
      : 'Access is locked. Open a linking window, then connect from your ChatGPT account.';
    $('open-connection').hidden = !connection.chatgpt_only;
    $('open-connection').textContent = connection.status === 'linked' ? 'Replace ChatGPT connection' : 'Allow ChatGPT connection';
  } finally { refreshing = false; }
}

$('login-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const button = event.submitter; button.disabled = true;
  credential = $('admin-key').value.trim();
  try {
    await refresh();
    $('admin-key').value = '';
    $('login-panel').hidden = true; $('workspace').hidden = false; $('logout').hidden = false;
    notice('Workspace unlocked for this tab.');
    if (selectedCode) { $('pair-code').value = selectedCode; $('pair-dialog').showModal(); }
  } catch (error) { credential = ''; notice(error.message, true); }
  finally { button.disabled = false; }
});

$('logout').addEventListener('click', () => {
  credential = ''; selectedCode = ''; $('admin-key').value = '';
  $('device-list').replaceChildren(); $('activity-list').replaceChildren();
  $('workspace').hidden = true; $('logout').hidden = true; $('login-panel').hidden = false;
  $('pair-dialog').close(); notice('Workspace locked.'); $('admin-key').focus();
});
$('refresh').addEventListener('click', () => refresh().catch((error) => notice(error.message, true)));
$('pair-open').addEventListener('click', () => {
  selectedCode = ''; $('pair-preview').hidden = true; $('pair-status').textContent = ''; $('pair-code').value = '';
  $('pair-dialog').showModal();
});
$('pair-close').addEventListener('click', () => $('pair-dialog').close());
$('pair-code').addEventListener('input', () => { selectedCode = ''; $('pair-preview').hidden = true; });
$('pair-form').addEventListener('submit', async (event) => {
  event.preventDefault();
  const button = event.submitter; button.disabled = true; $('pair-status').textContent = '';
  selectedCode = ''; $('pair-preview').hidden = true;
  try {
    const code = $('pair-code').value.trim().toUpperCase();
    const pairing = await api(`/api/pair/${encodeURIComponent(code)}`);
    selectedCode = pairing.user_code;
    $('pair-name').textContent = pairing.name; $('verified-code').textContent = pairing.user_code;
    $('pair-preview').hidden = false;
  } catch (error) { $('pair-status').textContent = error.message; }
  finally { button.disabled = false; }
});
$('pair-approve').addEventListener('click', async () => {
  if (!selectedCode) return;
  $('pair-approve').disabled = true;
  try {
    const result = await api('/api/pair/approve', { method: 'POST', body: JSON.stringify({ user_code: selectedCode }) });
    selectedCode = ''; $('pair-preview').hidden = true; $('pair-dialog').close();
    notice(`${result.name} was approved. Finish pairing in your terminal, then start the agent to bring it online.`);
    await refresh();
  } catch (error) { $('pair-status').textContent = error.message; }
  finally { $('pair-approve').disabled = false; }
});
$('revoke-clients').addEventListener('click', async () => {
  if (!confirm('Revoke client access and close linking? Paired computers will remain registered.')) return;
  try { await api('/api/clients/revoke', { method: 'POST', body: '{}' }); notice('All MCP client access was revoked.'); await refresh(); }
  catch (error) { notice(error.message, true); }
});
$('open-connection').addEventListener('click', async () => {
  if (!confirm('Revoke existing client access and open a ten-minute window to link one ChatGPT connection?')) return;
  try { await api('/api/connection/open', { method: 'POST', body: '{}' }); notice('Connect from your ChatGPT account within ten minutes, then approve its authorization request.'); await refresh(); }
  catch (error) { notice(error.message, true); }
});
$('mcp-endpoint').value = `${location.origin}/mcp`;
$('pair-command').textContent = `./target/release/remote-commander pair --server ${location.origin} --name "My computer"${location.protocol === 'http:' ? ' --insecure-localhost' : ''}`;
if (location.protocol === 'http:') {
  const runCommand = [...document.querySelectorAll('pre code')].find((node) => node.textContent.includes(' run --root '));
  if (runCommand) runCommand.textContent += ' --insecure-localhost';
}
$('copy-endpoint').addEventListener('click', async () => {
  try { await navigator.clipboard.writeText($('mcp-endpoint').value); notice('MCP server URL copied.'); }
  catch { $('mcp-endpoint').select(); notice('Select and copy the MCP server URL.'); }
});
const initialCode = new URL(location.href).searchParams.get('pair');
if (initialCode && /^[a-z0-9]{8}$/i.test(initialCode)) {
  selectedCode = initialCode.toUpperCase();
  history.replaceState(null, '', '/');
  notice('Unlock the workspace, then verify the pairing code shown in your local terminal.');
}
setInterval(() => {
  if (credential && !document.hidden) refresh().catch((error) => notice(`Refresh failed: ${error.message}`, true));
}, 10000);
