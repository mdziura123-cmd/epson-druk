// Epson Druk – logika okienka
const { invoke } = window.__TAURI__.core;
const dialog = window.__TAURI__.dialog;
const webview = window.__TAURI__.webview;

const $ = (id) => document.getElementById(id);
const state = {
  ip: null,
  status: null,
  files: [],        // { path, name, ext, state: '', cls: '' }
  color: true,
  printing: false,
};

// ---------------------------------------------------------------- pomocnicze
function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function toast(msg, kind = '') {
  const t = el('div', 'toast ' + kind, msg);
  $('toasts').appendChild(t);
  setTimeout(() => t.remove(), kind === 'bad' ? 9000 : 4500);
}

function errText(e) {
  return typeof e === 'string' ? e : (e && e.message) || String(e);
}

function setPill(text, cls) {
  $('pill-text').textContent = text;
  $('pill-dot').className = 'dot ' + (cls || '');
}

const REASONS = {
  'media-empty': 'Brak papieru',
  'media-empty-error': 'Brak papieru',
  'media-needed': 'Włóż papier',
  'media-jam': 'Papier się zaciął',
  'media-jam-error': 'Papier się zaciął',
  'marker-supply-low': 'Kończy się tusz',
  'marker-supply-empty': 'Brak tuszu',
  'cover-open': 'Otwarta pokrywa',
  'door-open': 'Otwarta pokrywa',
  'offline-report': 'Drukarka offline',
  'paused': 'Wstrzymana',
  'toner-low': 'Kończy się tusz',
};
function reasonText(r) {
  const key = r.replace(/-(report|warning)$/, '');
  return REASONS[r] || REASONS[key] || r;
}

const INK_COLORS = {
  black: '#222831', 'photo black': '#222831', cyan: '#00a3e0',
  magenta: '#d4007f', yellow: '#f2c500',
};
function inkColor(ink) {
  if (ink.color && /^#[0-9a-f]{6}$/i.test(ink.color)) return ink.color;
  const n = ink.name.toLowerCase();
  for (const k of Object.keys(INK_COLORS)) if (n.includes(k)) return INK_COLORS[k];
  return '#7f8c9d';
}
const INK_PL = { black: 'Czarny', cyan: 'Błękitny', magenta: 'Purpurowy', yellow: 'Żółty' };
function inkName(n) {
  const l = n.toLowerCase();
  for (const k of Object.keys(INK_PL)) if (l.includes(k)) return INK_PL[k];
  return n;
}

// ---------------------------------------------------------------- drukarka
async function connect(ip, { silent = false } = {}) {
  const btn = $('btn-refresh');
  btn.classList.add('spin');
  setPill('Łączenie…', 'busy');
  try {
    const st = await invoke('status', { ip });
    state.ip = ip;
    state.status = st;
    await invoke('save_printer', { ip });
    renderPrinter();
    renderInks();
    refreshJobs();
    if (!silent) toast(`Połączono z ${st.model}`, 'ok');
  } catch (e) {
    setPill('Brak połączenia', 'bad');
    if (!silent || !state.status) toast('Nie mogę połączyć się z drukarką: ' + errText(e), 'bad');
  } finally {
    btn.classList.remove('spin');
    updatePrintButton();
  }
}

function renderPrinter() {
  const st = state.status;
  $('printer-empty').classList.toggle('hidden', !!st);
  $('printer-info').classList.toggle('hidden', !st);
  if (!st) return;
  $('p-model').textContent = st.model;
  $('p-ip').textContent = st.ip;
  const stateCls = { gotowa: 'ok', drukuje: 'busy', zatrzymana: 'bad' }[st.state] || '';
  $('p-state').textContent = st.state.charAt(0).toUpperCase() + st.state.slice(1);
  setPill(`${st.model.replace(/ Series$/, '')} – ${st.state}`, stateCls);

  const reasons = st.reasons.map(reasonText);
  if (st.message && !reasons.length) reasons.push(st.message);
  $('p-reasons').textContent = reasons.join(' · ');
  $('p-reasons').classList.toggle('hidden', !reasons.length);

  // Czarno-biało tylko gdy drukarka nie umie koloru
  const colorBtn = document.querySelector('#color-seg [data-color="1"]');
  colorBtn.disabled = !st.color_supported;
  if (!st.color_supported) setColor(false);
}

function renderInks() {
  const box = $('inks');
  box.replaceChildren();
  const st = state.status;
  if (!st || !st.inks.length) {
    box.appendChild(el('div', 'empty',
      st ? 'Drukarka nie podaje poziomu tuszu. W L3150 sprawdź go w okienkach zbiorników z przodu.'
         : 'Połącz się z drukarką, aby zobaczyć tusz.'));
    return;
  }
  let unknown = false;
  for (const ink of st.inks) {
    const wrap = el('div', 'ink');
    const tank = el('div', 'tank');
    if (ink.level === null || ink.level === undefined) {
      tank.classList.add('unknown');
      tank.textContent = '?';
      unknown = true;
      wrap.append(tank, el('div', 'pct', '—'));
    } else {
      const fill = el('div', 'fill');
      fill.style.background = inkColor(ink);
      fill.style.height = '0%';
      tank.appendChild(fill);
      requestAnimationFrame(() => { fill.style.height = ink.level + '%'; });
      wrap.append(tank, el('div', 'pct', ink.level + '%'));
    }
    wrap.appendChild(el('div', 'name', inkName(ink.name)));
    box.appendChild(wrap);
  }
  if (unknown) {
    const card = $('card-ink');
    card.querySelector('.ink-note')?.remove();
    card.appendChild(el('div', 'ink-note',
      '„?” – drukarka EcoTank nie mierzy tuszu. Sprawdź poziom w okienkach zbiorników.'));
  }
}

async function scan() {
  const btn = $('btn-scan');
  btn.disabled = true;
  btn.textContent = 'Szukam…';
  const box = $('scan-results');
  box.classList.remove('hidden');
  box.replaceChildren(el('div', 'empty', 'Przeszukuję sieć – to potrwa kilka sekund…'));
  try {
    const found = await invoke('scan');
    box.replaceChildren();
    const printers = found.filter((f) => f.model);
    if (!printers.length) {
      box.appendChild(el('div', 'empty',
        'Nie znaleziono drukarki. Sprawdź, czy jest włączona i połączona z tą samą siecią Wi-Fi.'));
      return;
    }
    for (const f of printers) {
      const b = el('button', 'scan-item');
      b.append(el('span', '', f.model), el('span', 'ip', f.ip));
      b.onclick = async () => {
        box.classList.add('hidden');
        await connect(f.ip);
      };
      box.appendChild(b);
    }
    // Jedna drukarka Epson? Połącz od razu.
    const epsons = printers.filter((f) => /epson/i.test(f.model));
    if (epsons.length === 1 && !state.ip) {
      box.classList.add('hidden');
      await connect(epsons[0].ip);
    }
  } catch (e) {
    box.replaceChildren(el('div', 'empty', 'Błąd wyszukiwania: ' + errText(e)));
  } finally {
    btn.disabled = false;
    btn.textContent = 'Szukaj w sieci';
  }
}

// ---------------------------------------------------------------- pliki
const OK_EXT = ['pdf', 'jpg', 'jpeg', 'png', 'webp', 'bmp', 'gif', 'txt', 'pwg', 'urf'];

function addFiles(paths) {
  let skipped = 0;
  for (const p of paths) {
    const name = p.split('/').pop();
    const ext = (name.includes('.') ? name.split('.').pop() : '').toLowerCase();
    if (!OK_EXT.includes(ext)) { skipped++; continue; }
    if (state.files.some((f) => f.path === p)) continue;
    state.files.push({ path: p, name, ext, state: '', cls: '' });
  }
  if (skipped) toast(`Pominięto ${skipped} plik(ów) w nieobsługiwanym formacie`, 'bad');
  renderFiles();
}

function renderFiles() {
  const ul = $('files');
  ul.replaceChildren();
  state.files.forEach((f, i) => {
    const li = el('li');
    const rm = el('button', 'icon-btn', '×');
    rm.title = 'Usuń z listy';
    rm.disabled = state.printing;
    rm.onclick = () => { state.files.splice(i, 1); renderFiles(); };
    li.append(el('span', 'tag', f.ext.toUpperCase()), el('span', 'fname', f.name),
              el('span', 'state ' + f.cls, f.state), rm);
    li.title = f.path;
    ul.appendChild(li);
  });
  updatePrintButton();
}

async function pickFiles() {
  try {
    const sel = await dialog.open({
      multiple: true,
      title: 'Wybierz pliki do druku',
      filters: [
        { name: 'Dokumenty i zdjęcia', extensions: OK_EXT },
        { name: 'Wszystkie pliki', extensions: ['*'] },
      ],
    });
    if (!sel) return;
    addFiles(Array.isArray(sel) ? sel : [sel]);
  } catch (e) {
    toast('Nie mogę otworzyć okna wyboru plików: ' + errText(e), 'bad');
  }
}

function updatePrintButton() {
  const b = $('btn-print');
  b.disabled = state.printing || !state.ip || !state.files.length;
  b.textContent = state.printing ? 'Drukuję…'
    : state.files.length > 1 ? `Drukuj (${state.files.length})` : 'Drukuj';
}

function setColor(c) {
  state.color = c;
  document.querySelectorAll('#color-seg button').forEach((b) =>
    b.classList.toggle('active', (b.dataset.color === '1') === c));
}

function copies() {
  const n = parseInt($('copies').value, 10);
  return Number.isFinite(n) ? Math.min(99, Math.max(1, n)) : 1;
}

async function printAll() {
  if (!state.ip || !state.files.length) return;
  state.printing = true;
  updatePrintButton();
  let ok = 0;
  for (const f of state.files) {
    if (f.cls === 'ok') continue;
    f.state = f.ext === 'pdf' ? 'przygotowuję…' : 'wysyłam…';
    f.cls = '';
    renderFiles();
    try {
      const r = await invoke('print_file', {
        ip: state.ip, path: f.path, copies: copies(), color: state.color, fit: $('fit').checked,
      });
      f.state = 'wysłano ✓';
      f.cls = 'ok';
      ok++;
      if (r.note) toast(`${f.name}: ${r.note}`);
    } catch (e) {
      f.state = 'błąd';
      f.cls = 'bad';
      toast(`${f.name}: ${errText(e)}`, 'bad');
    }
    renderFiles();
  }
  state.printing = false;
  updatePrintButton();
  if (ok) {
    toast(ok === 1 ? 'Wysłano do drukarki' : `Wysłano ${ok} pliki do drukarki`, 'ok');
    refreshJobs();
    // Wysłane pliki znikają z listy po chwili
    setTimeout(() => {
      state.files = state.files.filter((f) => f.cls !== 'ok');
      renderFiles();
    }, 2500);
  }
}

// ---------------------------------------------------------------- kolejka
let jobsTimer = null;
async function refreshJobs() {
  if (!state.ip) return;
  const btn = $('btn-jobs');
  btn.classList.add('spin');
  try {
    const list = await invoke('jobs', { ip: state.ip });
    const ul = $('jobs');
    ul.replaceChildren();
    if (!list.length) ul.appendChild(el('li', 'empty', 'Kolejka jest pusta.'));
    for (const j of list) {
      const li = el('li', 'job');
      const cancel = el('button', 'icon-btn', '×');
      cancel.title = 'Anuluj zadanie';
      cancel.onclick = async () => {
        try {
          await invoke('cancel', { ip: state.ip, id: j.id });
          toast(`Anulowano zadanie #${j.id}`, 'ok');
          refreshJobs();
        } catch (e) { toast(errText(e), 'bad'); }
      };
      li.append(el('span', 'jid', '#' + j.id), el('span', 'jname', j.name || 'dokument'),
                el('span', 'jstate ' + j.state.replace(/\s/g, '-'), j.state), cancel);
      ul.appendChild(li);
    }
  } catch (_) {
    /* drukarka może nie udostępniać kolejki – pomijamy */
  } finally {
    btn.classList.remove('spin');
  }
}

// ---------------------------------------------------------------- start
function bind() {
  $('btn-scan').onclick = scan;
  $('btn-refresh').onclick = () => (state.ip ? connect(state.ip) : scan());
  $('btn-manual').onclick = () => {
    const ip = $('manual-ip').value.trim();
    if (ip) connect(ip);
  };
  $('manual-ip').addEventListener('keydown', (e) => { if (e.key === 'Enter') $('btn-manual').click(); });
  $('btn-pick').onclick = pickFiles;
  $('btn-print').onclick = printAll;
  $('btn-jobs').onclick = refreshJobs;
  $('copies-minus').onclick = () => { $('copies').value = Math.max(1, copies() - 1); };
  $('copies-plus').onclick = () => { $('copies').value = Math.min(99, copies() + 1); };
  $('copies').onchange = () => { $('copies').value = copies(); };
  document.querySelectorAll('#color-seg button').forEach((b) => {
    b.onclick = () => { if (!b.disabled) setColor(b.dataset.color === '1'); };
  });

  // Przeciąganie plików na okno
  webview.getCurrentWebview().onDragDropEvent((ev) => {
    const p = ev.payload;
    if (p.type === 'enter' || p.type === 'over') $('drop').classList.add('over');
    else if (p.type === 'leave') $('drop').classList.remove('over');
    else if (p.type === 'drop') {
      $('drop').classList.remove('over');
      if (p.paths && p.paths.length) addFiles(p.paths);
    }
  });

  // Skróty: Ctrl+O – wybierz pliki, Ctrl+P – drukuj
  document.addEventListener('keydown', (e) => {
    if (!e.ctrlKey) return;
    if (e.key === 'o') { e.preventDefault(); pickFiles(); }
    if (e.key === 'p') { e.preventDefault(); if (!$('btn-print').disabled) printAll(); }
  });

  // Bez angielskiego menu kontekstowego przeglądarki
  document.addEventListener('contextmenu', (e) => {
    if (!(e.target instanceof HTMLInputElement)) e.preventDefault();
  });
}

async function start() {
  bind();
  updatePrintButton();
  const saved = await invoke('load_printer').catch(() => null);
  if (saved) {
    $('manual-ip').value = saved;
    await connect(saved, { silent: true });
    if (!state.status) scan(); // zapisana drukarka zmieniła adres? szukamy
  } else {
    scan();
  }
  // Odświeżanie stanu i kolejki co 10 s
  jobsTimer = setInterval(() => {
    if (state.ip && !state.printing) {
      refreshJobs();
      invoke('status', { ip: state.ip })
        .then((st) => { state.status = st; renderPrinter(); })
        .catch(() => setPill('Brak połączenia', 'bad'));
    }
  }, 10000);
}

start();
