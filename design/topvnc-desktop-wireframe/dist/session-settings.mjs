// Interactive design examples only. No transport, host commands or measurements.
export function createDemoSettings({ $, el, close, fullscreen }) {
  const state = { offered: true, scale: 100, display: '1', quality: '6', stats: false, connected: true };
  const stats = el('div', 'SIMULATED DIAGNOSTICS · EXAMPLE VALUES\n60 updates/s · 60 drawn/s · 12.0 Mbit/s · RTT 8 ms', 'demo-stats');
  stats.id = 'demo-stats'; stats.hidden = true;
  const message = el('div', 'Demo disconnected. Open Settings to reconnect the simulated session.', 'demo-session-message');
  message.id = 'demo-session-message'; message.hidden = true; message.setAttribute('role', 'status');
  $('#remote').append(stats, message);
  function refresh() {
    stats.hidden = !state.stats || !state.connected;
    message.hidden = state.connected;
    $('#device').dataset.demoConnected = String(state.connected);
    $('#remote-output').contentEditable = state.connected ? 'plaintext-only' : 'false';
    $('.desktop-session-status span').textContent = state.connected ? 'SIMULATED SESSION · NO LIVE HOST' : 'DEMO DISCONNECTED · NO LIVE HOST';
    $('.desktop-session-status span:last-child').textContent = state.offered ? `${state.scale}% served · display ${state.display === '0' ? 'all' : state.display} · example` : 'Host sizing unavailable · example';
  }
  function build(content) {
    content.replaceChildren(el('p', 'Simulated settings · no live host, credentials or network connection. Changes affect this preview only.', 'sheet-description'));
    const capabilityLabel = el('label', 'Host capability example', 'demo-setting');
    const capabilities = el('select'); capabilities.id = 'demo-capabilities';
    for (const [value, label] of [['offered', 'Display and sizing offered'], ['unavailable', 'Display and sizing unavailable']]) {
      const option = el('option', label); option.value = value; capabilities.append(option);
    }
    capabilities.value = state.offered ? 'offered' : 'unavailable'; capabilityLabel.append(capabilities); content.append(capabilityLabel);
    capabilities.addEventListener('change', () => { state.offered = capabilities.value === 'offered'; refresh(); build(content); $('#demo-capabilities').focus(); });

    const displayLabel = el('label', 'Display', 'demo-setting');
    const display = el('select'); display.id = 'demo-serve-display'; display.disabled = !state.offered || !state.connected;
    for (const [value, label] of [['1', 'Display 1'], ['2', 'Display 2'], ['0', 'All displays']]) { const option = el('option', label); option.value = value; display.append(option); }
    display.value = state.display; displayLabel.append(display);
    if (!state.offered) displayLabel.append(el('small', 'Not offered by this host · example'));
    display.addEventListener('change', () => { state.display = display.value; refresh(); }); content.append(displayLabel);

    const scaleLabel = el('label', 'Served size', 'demo-setting'); scaleLabel.htmlFor = 'demo-serve-scale';
    const scaleValue = el('output'); scaleValue.id = 'demo-serve-scale-value'; scaleValue.htmlFor = 'demo-serve-scale';
    const scale = el('input'); scale.id = 'demo-serve-scale'; scale.type = 'range'; scale.min = '25'; scale.max = '100'; scale.step = '1'; scale.value = String(state.scale); scale.disabled = !state.offered || !state.connected;
    const showScale = () => { scaleValue.textContent = state.offered ? `${state.scale}% · ${Math.round(1920 * state.scale / 100)} × ${Math.round(1080 * state.scale / 100)} example` : 'Unavailable'; };
    showScale(); scale.addEventListener('input', () => { state.scale = Number(scale.value); showScale(); refresh(); });
    scaleLabel.append(scaleValue, scale, el('small', state.offered ? '25% – 100% · In production, host display and served size affect every viewer.' : 'Requires an updated TopVNC host · example')); content.append(scaleLabel);

    const quality = el('fieldset', undefined, 'demo-setting'); quality.append(el('legend', 'Picture quality'));
    const choices = el('div', undefined, 'demo-quality');
    for (const [value, label] of [['2','Low'],['4','Med'],['6','Good'],['8','High'],['9','Best']]) {
      const choice = el('label'); const radio = el('input'); radio.type = 'radio'; radio.name = 'demo-quality'; radio.value = value; radio.checked = state.quality === value; radio.disabled = !state.connected;
      radio.addEventListener('change', () => { state.quality = value; }); choice.append(radio, el('span', label)); choices.append(choice);
    }
    quality.append(choices); content.append(quality);
    const full = el('button', 'Toggle fullscreen preview', 'wide-button'); full.id = 'demo-fullscreen'; full.addEventListener('click', () => { close(); fullscreen(); }); content.append(full);

    const debug = el('label', undefined, 'demo-debug'); const toggle = el('input'); toggle.id = 'demo-stats-toggle'; toggle.type = 'checkbox'; toggle.checked = state.stats; toggle.disabled = !state.connected;
    toggle.addEventListener('change', () => { state.stats = toggle.checked; refresh(); }); debug.append(toggle, el('span', 'Debug / Stats')); content.append(debug, el('p', 'Shows example update rate, draw rate, bandwidth and RTT. These are simulated values, not performance measurements.', 'sheet-description'));
    for (const [key, value] of [['Machine', 'Studio PC · demo'], ['Security', 'Simulated · not connected'], ['Authentication', 'No credentials accepted by this prototype'], ['Transport design', 'Encrypted channel via WebSocket gateway']]) {
      const row = el('div', undefined, 'setting-row'); row.append(el('span', key), el('span', value)); content.append(row);
    }
    const disconnect = el('button', state.connected ? 'Disconnect demo' : 'Reconnect demo', 'wide-button'); disconnect.id = 'demo-disconnect';
    disconnect.addEventListener('click', () => { state.connected = !state.connected; close(); refresh(); $('#session-feedback').textContent = state.connected ? 'Simulated · physical keyboard and mouse' : 'Demo disconnected · no live host'; }); content.append(disconnect);
    refresh();
  }
  refresh();
  return { build };
}
