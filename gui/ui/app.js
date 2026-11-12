// Epson Druk – logika okienka
const { invoke } = window.__TAURI__.core;
const dialog = window.__TAURI__.dialog;
const webview = window.__TAURI__.webview;
const events = window.__TAURI__.event;
const notif = window.__TAURI__.notification;

const $ = (id) => document.getElementById(id);
const state = {
  ip: null,
  status: null,
  files: [],        // { path, name, ext, state: '', cls: '' }
  color: true,
  printing: false,
  printOpts: {},
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
  'media-low': 'Kończy się papier',
  'waste-ink-full': 'Pełny pojemnik na zużyty tusz – potrzebny serwis',
  'fatal-error': 'Błąd drukarki – wyłącz ją i włącz ponownie',
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
    checkAlerts(st);
    refreshJobs();
    loadPrintOptions();
    if (!driverInfo) checkDriver();
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
  updateMaintButtons();
  renderSysinfo();
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

  // Tryb przez sterownik (np. L3150 bez IPP)
  const setup = $('p-setup');
  const needSetup = st.mode === 'cups' && !st.queue_ready;
  setup.classList.toggle('hidden', !needSetup);
  $('p-ip').textContent = st.ip + (st.mode === 'cups' ? ' · przez sterownik Epson' : '');
  if (needSetup) {
    const t = $('setup-text');
    t.replaceChildren();
    if (st.driver_missing) {
      t.append(
        el('b', '', 'Potrzebny sterownik Epson. '),
        document.createTextNode('Ta drukarka nie drukuje bez sterownika. Pobierz pakiet RPM ze strony Epsona, zainstaluj poleceniem '),
        el('code', '', 'rpm-ostree install ~/Pobrane/epson-inkjet-printer-escpr-*.rpm'),
        document.createTextNode(', uruchom komputer ponownie i kliknij „Sprawdź ponownie”.'));
      $('btn-setup').textContent = 'Sprawdź ponownie';
      $('btn-driver-page').classList.remove('hidden');
    } else {
      t.append(
        el('b', '', 'Prawie gotowe. '),
        document.createTextNode('Kliknij przycisk, a aplikacja doda drukarkę do systemu (raz). System może poprosić o hasło.'));
      $('btn-setup').textContent = 'Skonfiguruj drukarkę';
      $('btn-driver-page').classList.add('hidden');
    }
    setPill(`${st.model.replace(/ Series$/, '')} – wymaga konfiguracji`, 'busy');
  }

  // Czarno-biało tylko gdy drukarka nie umie koloru
  const colorBtn = document.querySelector('#color-seg [data-color="1"]');
  colorBtn.disabled = !st.color_supported;
  if (!st.color_supported) setColor(false);
}

// ---------------------------------------------------------------- tusz
const DEFAULT_INKS = ['Black', 'Cyan', 'Magenta', 'Yellow'];
let manualInks = null;   // { levels: {Black: 80, ...}, date: '2026-11-12' }
let inkEditing = false;

function inkList(st) {
  if (st && st.inks.length) return st.inks;
  return DEFAULT_INKS.map((name) => ({ name, level: null, color: null }));
}

function renderInks() {
  const box = $('inks');
  box.replaceChildren();
  const card = $('card-ink');
  card.querySelector('.ink-note')?.remove();
  const st = state.status;
  const manualBox = $('ink-manual');
  if (!st) {
    manualBox.classList.add('hidden');
    box.appendChild(el('div', 'empty', 'Połącz się z drukarką, aby zobaczyć tusz.'));
    return;
  }
  const live = st.inks.some((i) => i.level !== null && i.level !== undefined);
  const canManual = !live;
  manualBox.classList.toggle('hidden', !canManual);
  const useManual = canManual && (manualInks || inkEditing);

  for (const ink of inkList(st)) {
    const wrap = el('div', 'ink');
    const tank = el('div', 'tank');
    let level = ink.level;
    if (useManual) {
      level = manualInks && manualInks.levels[ink.name] !== undefined ? manualInks.levels[ink.name] : (inkEditing ? 100 : null);
      tank.classList.add('manual');
    }
    if (level === null || level === undefined) {
      tank.classList.add('unknown');
      tank.textContent = '?';
      wrap.append(tank, el('div', 'pct', '—'));
    } else {
      const fill = el('div', 'fill');
      fill.style.background = inkColor(ink);
      fill.style.height = level + '%';
      tank.appendChild(fill);
      wrap.append(tank, el('div', 'pct', level + '%'));
    }
    if (inkEditing) {
      tank.classList.add('editable');
      const r = el('input');
      r.type = 'range'; r.min = 0; r.max = 100; r.step = 5;
      r.value = level ?? 100;
      r.title = 'Ustaw tak, jak widzisz w okienku zbiornika';
      r.oninput = () => {
        manualInks = manualInks || { levels: {}, date: '' };
        manualInks.levels[ink.name] = parseInt(r.value, 10);
        const f = tank.querySelector('.fill');
        if (f) { f.style.height = r.value + '%'; wrap.querySelector('.pct').textContent = r.value + '%'; }
        else renderInks();
      };
      wrap.appendChild(r);
    }
    wrap.appendChild(el('div', 'name', inkName(ink.name)));
    box.appendChild(wrap);
  }

  let note = '';
  if (live) {
    note = st.ink_live && /L3\d{3}|EcoTank|ET-/i.test(st.model)
      ? 'Poziom szacowany przez drukarkę.' : '';
  } else if (inkEditing) {
    note = 'Przesuń suwaki tak, jak wygląda tusz w okienkach zbiorników z przodu drukarki.';
  } else if (manualInks) {
    note = 'Poziomy ustawione ręcznie – ta drukarka nie mierzy tuszu sama. Zaktualizuj je po uzupełnieniu.';
  } else {
    note = st.ink_unmeasured
      ? 'Ta drukarka nie mierzy tuszu (zgłasza zawsze 100%). Sprawdź okienka zbiorników lub ustaw poziomy ręcznie.'
      : 'Drukarka nie podaje poziomu tuszu. Sprawdź okienka zbiorników lub ustaw poziomy ręcznie.';
  }
  if (note) box.after(el('div', 'ink-note', note));
  $('ink-manual-date').textContent = manualInks && manualInks.date && !inkEditing ? `ostatnio: ${manualInks.date}` : '';
  $('btn-ink-edit').classList.toggle('hidden', inkEditing);
  $('ink-edit-actions').classList.toggle('hidden', !inkEditing);
  $('btn-ink-edit').textContent = manualInks ? 'Zmień poziomy' : 'Ustaw poziomy ręcznie';
}

async function saveManualInks() {
  manualInks = manualInks || { levels: {}, date: '' };
  for (const ink of inkList(state.status)) {
    if (manualInks.levels[ink.name] === undefined) manualInks.levels[ink.name] = 100;
  }
  manualInks.date = new Date().toLocaleDateString('pl-PL');
  inkEditing = false;
  await invoke('save_setting', { key: 'tusz-reczny', value: JSON.stringify(manualInks) }).catch((e) => toast(errText(e), 'bad'));
  renderInks();
  toast('Zapisano poziomy tuszu', 'ok');
}

async function scan() {
  const btn = $('btn-scan');
  btn.disabled = true;
  btn.textContent = 'Szukam…';
  const box = $('scan-results');
  box.classList.remove('hidden');
  box.replaceChildren(el('div', 'empty', 'Przeszukuję sieć – to potrwa kilka sekund…'));
  try {
    const extra = state.ip || $('manual-ip').value.trim() || null;
    const found = await invoke('scan', { extra });
    box.replaceChildren();
    const printers = found.filter((f) => f.model || f.raw);
    if (!printers.length) {
      box.appendChild(el('div', 'empty',
        'Nie znaleziono drukarki. Sprawdź, czy jest włączona i połączona z tą samą siecią Wi-Fi.'));
      return;
    }
    for (const f of printers) {
      const b = el('button', 'scan-item');
      const name = el('span', '', f.model || 'Drukarka');
      name.appendChild(el('span', 'mode-tag', f.ipp ? 'bez sterownika' : 'przez sterownik'));
      b.append(name, el('span', 'ip', f.ip));
      b.onclick = async () => {
        box.classList.add('hidden');
        await connect(f.ip);
      };
      box.appendChild(b);
    }
    // Jedna drukarka Epson? Połącz od razu.
    const epsons = printers.filter((f) => /epson/i.test(f.model || ''));
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

// ---------------------------------------------------------------- ustawienia wydruku
const OPT_LABELS = {
  PageSize: 'Rozmiar papieru', MediaType: 'Rodzaj papieru', Quality: 'Jakość druku',
  PrintQuality: 'Jakość druku', cupsPrintQuality: 'Jakość druku', escprQuality: 'Jakość druku',
  OutputMode: 'Jakość druku', Resolution: 'Rozdzielczość', InputSlot: 'Podajnik', Duplex: 'Druk dwustronny',
  Borderless: 'Bez marginesów', Brightness: 'Jasność', Contrast: 'Kontrast', Saturation: 'Nasycenie',
};
const OPT_ORDER = ['PageSize', 'MediaType', 'Quality', 'PrintQuality', 'cupsPrintQuality', 'escprQuality', 'OutputMode'];
// Kolor ustawia przełącznik Kolorowo/Czarno-biało. Jasność, kontrast, nasycenie itp. robi edytor zdjęć –
// opcje sterownika o tych nazwach filtry CUPS czytają jako procenty (Brightness=0 → czarna strona!).
const OPT_HIDDEN = ['Ink', 'ColorModel', 'ColorMode', 'PageRegion',
  'Brightness', 'Contrast', 'Saturation', 'Hue', 'Gamma', 'Sharpness'];
const CHOICE_PL = {
  PLAIN: 'Papier zwykły', PMPHOTO: 'Fotograficzny błyszczący Premium', PSPHOTO: 'Fotograficzny półbłyszczący Premium',
  PMMATT: 'Matowy Premium', PHOTO: 'Fotograficzny błyszczący', GLOSSY: 'Błyszczący', PLATINA: 'Fotograficzny Ultra Premium',
  MATTE: 'Matowy', ENV: 'Koperta', ENVELOPE: 'Koperta', BSMATT: 'Matowy dwustronny', PMLABEL: 'Etykiety',
  GPHOTO: 'Fotograficzny błyszczący', PLAINHIGH: 'Zwykły – wysoka jakość', LETTERHEAD: 'Papier firmowy',
  Draft: 'Szkic (szybko, mniej tuszu)', Economy: 'Ekonomiczna', Normal: 'Standardowa', Standard: 'Standardowa',
  High: 'Wysoka', Fine: 'Wysoka', Best: 'Najwyższa', Photo: 'Fotograficzna', Super: 'Najwyższa',
  A4: 'A4 (21×29,7 cm)', A5: 'A5 (14,8×21 cm)', A6: 'A6 (10,5×14,8 cm)', B5: 'B5', Letter: 'Letter (USA)',
  Legal: 'Legal (USA)', Postcard: 'Pocztówka (10×14,8 cm)', Hagaki: 'Pocztówka (10×14,8 cm)',
  KG: '10×15 cm', T4X6: '10×15 cm', '4x6': '10×15 cm', '4X6': '10×15 cm', L: '9×13 cm', '2L': '13×18 cm',
  '5x7': '13×18 cm', '8x10': '20×25 cm', None: 'Brak', True: 'Tak', False: 'Nie',
  Auto: 'Automatycznie', Upper: 'Tylny podajnik', Rear: 'Tylny podajnik',
};
const QUALITY_PL = {
  DRAFT: 'Szkic (szybko, mniej tuszu)', ECONOMY: 'Ekonomiczna', NORMAL: 'Standardowa', STANDARD: 'Standardowa',
  HIGH: 'Wysoka', FINE: 'Wysoka', BEST: 'Najwyższa', SUPERFINE: 'Najwyższa', PHOTO: 'Fotograficzna', FAST: 'Szybka',
};
const QUALITY_ORDER = ['DRAFT', 'FAST', 'ECONOMY', 'NORMAL', 'STANDARD', 'HIGH', 'FINE', 'PHOTO', 'BEST', 'SUPERFINE'];
function qualityLabel(q) { return QUALITY_PL[q.toUpperCase()] || q; }

/** MediaType w sterowniku Epson łączy papier i jakość: PLAIN_NORMAL → [PLAIN, NORMAL] */
function splitMedia(v) {
  const i = v.lastIndexOf('_');
  if (i <= 0) return null;
  const q = v.slice(i + 1).toUpperCase();
  return QUALITY_PL[q] ? [v.slice(0, i), v.slice(i + 1)] : null;
}

function choiceLabel(v) {
  if (CHOICE_PL[v]) return CHOICE_PL[v];
  if (/^[-+]?\d+$/.test(v)) return v === '0' ? 'Standardowo (0)' : (v.startsWith('-') ? v : '+' + v.replace('+', ''));
  const borderless = /full ?bleed|borderless|\.fb$/i.test(v);
  const base = v.replace(/\.(fullbleed|borderless|fb)$/i, '').replace(/_?(full ?bleed|borderless)$/i, '');
  let text = CHOICE_PL[base] || CHOICE_PL[base.toUpperCase()] || base;
  const m = /^(\d+(?:\.\d+)?)x(\d+(?:\.\d+)?)(in|mm)?$/i.exec(base);
  if (!CHOICE_PL[base] && m) {
    const inch = (m[3] || 'in').toLowerCase() === 'in';
    const f = (x) => inch ? Math.round(parseFloat(x) * 2.54) : Math.round(parseFloat(x) / 10);
    text = `${f(m[1])}×${f(m[2])} cm`;
  }
  return borderless ? `${text} – bez marginesów` : text;
}

/** Proporcje papieru (krótszy/dłuższy bok) dla edytora zdjęć. */
function paperAspect() {
  const v = (state.printOpts.PageSize || 'A4').replace(/\.(fullbleed|borderless|fb)$/i, '');
  const MM = { A4: [210, 297], A5: [148, 210], A6: [105, 148], B5: [182, 257], LETTER: [216, 279],
    LEGAL: [216, 356], POSTCARD: [100, 148], HAGAKI: [100, 148], KG: [102, 152], T4X6: [102, 152],
    L: [89, 127], '2L': [127, 178] };
  let d = MM[v.toUpperCase()];
  const m = /^(\d+(?:\.\d+)?)x(\d+(?:\.\d+)?)/i.exec(v);
  if (!d && m) d = [parseFloat(m[1]), parseFloat(m[2])];
  if (!d) d = MM.A4;
  return Math.min(d[0], d[1]) / Math.max(d[0], d[1]);
}

let driverOptions = [];
async function loadPrintOptions() {
  if (!state.ip) return;
  try { driverOptions = await invoke('printer_options', { ip: state.ip }); } catch (_) { driverOptions = []; }
  const saved = await invoke('load_setting', { key: 'opcje-druku' }).catch(() => null);
  let savedObj = {};
  try { savedObj = saved ? JSON.parse(saved) : {}; } catch (_) {}
  const grid = $('adv-grid');
  grid.replaceChildren();
  const opts = driverOptions
    .filter((o) => !OPT_HIDDEN.includes(o.key))
    .sort((a, b) => {
      const ia = OPT_ORDER.indexOf(a.key), ib = OPT_ORDER.indexOf(b.key);
      return (ia < 0 ? 99 : ia) - (ib < 0 ? 99 : ib);
    });
  state.printOpts = {};
  const save = () => invoke('save_setting', { key: 'opcje-druku', value: JSON.stringify(state.printOpts) }).catch(() => {});
  const makeSelect = (labelText, values, current, labelFn, onChange) => {
    const lab = el('label', '', labelText);
    const sel = el('select');
    for (const v of values) {
      const op = el('option', '', labelFn(v));
      op.value = v;
      if (v === current) op.selected = true;
      sel.appendChild(op);
    }
    sel.onchange = () => onChange(sel.value, sel);
    lab.appendChild(sel);
    grid.appendChild(lab);
    return sel;
  };

  for (const o of opts) {
    const val = o.choices.includes(savedObj[o.key]) ? savedObj[o.key] : o.default;
    state.printOpts[o.key] = val;

    // Papier + jakość w jednej opcji (Epson ESC/P-R) → dwie listy
    const split = o.key === 'MediaType' && o.choices.filter((c) => splitMedia(c)).length >= o.choices.length / 2;
    if (split) {
      const combos = {};          // papier → [jakości]
      const order = [];
      for (const c of o.choices) {
        const s = splitMedia(c) || [c, ''];
        if (!combos[s[0]]) { combos[s[0]] = []; order.push(s[0]); }
        combos[s[0]].push(s[1]);
      }
      for (const p of order) {
        combos[p].sort((x, y) => QUALITY_ORDER.indexOf(x.toUpperCase()) - QUALITY_ORDER.indexOf(y.toUpperCase()));
      }
      let [paper, quality] = splitMedia(val) || [val, ''];
      const combine = () => (quality ? `${paper}_${quality}` : paper);
      let qSel = null;
      const fillQuality = () => {
        const qs = combos[paper];
        if (!qs.includes(quality)) quality = qs.find((q) => /^(NORMAL|STANDARD)$/i.test(q)) || qs[0];
        qSel.replaceChildren();
        for (const q of qs) {
          const op = el('option', '', q ? qualityLabel(q) : 'Standardowa');
          op.value = q;
          if (q === quality) op.selected = true;
          qSel.appendChild(op);
        }
        qSel.disabled = qs.length < 2;
        state.printOpts.MediaType = combine();
      };
      makeSelect('Rodzaj papieru', order, paper, (p) => choiceLabel(p), (v) => {
        paper = v; fillQuality(); save();
      });
      qSel = makeSelect('Jakość druku', [], '', (q) => q, (v) => {
        quality = v; state.printOpts.MediaType = combine(); save();
      });
      fillQuality();
      continue;
    }

    makeSelect(OPT_LABELS[o.key] || o.label, o.choices, val, choiceLabel, (v) => {
      state.printOpts[o.key] = v; save();
    });
  }
  $('adv').classList.toggle('hidden', !opts.length);
}

// ---------------------------------------------------------------- edytor zdjęć
const PHOTO_EXT = ['jpg', 'jpeg', 'png', 'webp', 'bmp', 'gif'];
const PHOTO_DEFAULT = { rotate: 0, brightness: 0, contrast: 0, saturation: 0, fill: false };
let editing = null;     // plik
let editParams = null;
let previewTimer = null;
let previewSeq = 0;

function openEditor(f) {
  editing = f;
  editParams = { ...PHOTO_DEFAULT, ...(f.photo || {}) };
  $('pe-name').textContent = '– ' + f.name;
  syncEditorControls();
  $('photo-modal').classList.remove('hidden');
  schedulePreview(0);
}

function syncEditorControls() {
  $('pe-b').value = editParams.brightness; $('pe-b-v').textContent = editParams.brightness;
  $('pe-c').value = editParams.contrast; $('pe-c-v').textContent = editParams.contrast;
  $('pe-s').value = editParams.saturation; $('pe-s-v').textContent = editParams.saturation;
  document.querySelectorAll('#pe-fit button').forEach((b) =>
    b.classList.toggle('active', (b.dataset.fill === '1') === editParams.fill));
  const paper = choiceLabel(state.printOpts.PageSize || 'A4');
  const mt = state.printOpts.MediaType;
  const sp = mt && splitMedia(mt);
  const media = !mt ? '' : sp ? ` · ${choiceLabel(sp[0])} · ${qualityLabel(sp[1])}` : ' · ' + choiceLabel(mt);
  $('pe-paper').textContent = `Papier: ${paper}${media}${state.color ? '' : ' · czarno-białe'}`;
}

function schedulePreview(delay = 140) {
  clearTimeout(previewTimer);
  previewTimer = setTimeout(renderPreview, delay);
}

async function renderPreview() {
  if (!editing) return;
  const seq = ++previewSeq;
  $('pe-loading').classList.remove('hidden');
  try {
    const url = await invoke('photo_preview', {
      path: editing.path,
      photo: { ...editParams, aspect: paperAspect(), color: state.color },
    });
    if (seq !== previewSeq) return;
    const img = $('pe-img');
    img.onload = () => {
      const a = paperAspect();
      const landscape = img.naturalWidth >= img.naturalHeight;
      const page = $('pe-page');
      page.style.aspectRatio = landscape ? `${1 / a}` : `${a}`;
      page.style.width = landscape ? 'min(100%, 560px)' : 'auto';
      page.style.height = landscape ? 'auto' : '440px';
      page.classList.toggle('fill', editParams.fill);
      page.classList.toggle('margin', !editParams.fill);
    };
    img.src = url;
  } catch (e) {
    toast(errText(e), 'bad');
  } finally {
    if (seq === previewSeq) $('pe-loading').classList.add('hidden');
  }
}

function closeEditor(save) {
  if (save && editing) {
    const changed = Object.keys(PHOTO_DEFAULT).some((k) => editParams[k] !== PHOTO_DEFAULT[k]);
    editing.photo = changed ? { ...editParams } : null;
    renderFiles();
  }
  editing = null;
  $('photo-modal').classList.add('hidden');
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
    li.append(el('span', 'tag', f.ext.toUpperCase()), el('span', 'fname', f.name));
    if (PHOTO_EXT.includes(f.ext)) {
      const adj = el('button', 'btn secondary adj' + (f.photo ? ' done' : ''), f.photo ? 'Dostosowano ✓' : 'Dostosuj');
      adj.title = 'Obrót, kadrowanie, jasność, kontrast, kolory';
      adj.disabled = state.printing;
      adj.onclick = () => openEditor(f);
      li.appendChild(adj);
    }
    li.append(el('span', 'state ' + f.cls, f.state), rm);
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
  const notReady = state.status && state.status.mode === 'cups' && !state.status.queue_ready;
  b.disabled = state.printing || !state.ip || !state.files.length || notReady;
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
      const usePhoto = PHOTO_EXT.includes(f.ext) && (f.photo || !state.color);
      const r = await invoke('print_file', {
        ip: state.ip, path: f.path, copies: copies(), color: state.color, fit: $('fit').checked,
        options: state.printOpts,
        photo: usePhoto ? { ...PHOTO_DEFAULT, ...(f.photo || {}), aspect: paperAspect(), color: state.color } : null,
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
let jobsFast = false;

async function jobAction(job, action) {
  if (action === 'cancel') {
    const ok = await dialog.ask(
      job.active
        ? `Przerwać drukowanie „${job.name || 'dokument'}”?\n\nDrukarka może jeszcze dokończyć kartkę, która jest w środku.`
        : `Anulować zadanie „${job.name || 'dokument'}”?`,
      { title: job.active ? 'Przerwij wydruk' : 'Anuluj zadanie', kind: 'warning', okLabel: job.active ? 'Przerwij' : 'Anuluj zadanie', cancelLabel: 'Wróć' }
    ).catch(() => false);
    if (!ok) return;
  }
  try {
    await invoke('job_action', { ip: state.ip, id: job.id, action });
    toast({ cancel: job.active ? 'Wydruk przerwany' : 'Zadanie anulowane', hold: 'Zadanie wstrzymane', release: 'Zadanie wznowione' }[action], 'ok');
  } catch (e) { toast(errText(e), 'bad'); }
  refreshJobs();
}

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
      const main = el('div', 'job-main');
      const top = el('div', 'job-top');
      top.append(el('span', 'jid', '#' + j.id), el('span', 'jname', j.name || 'dokument'),
                 el('span', 'jstate ' + j.state.replace(/\s/g, '-'), j.state));
      if (j.progress !== null && j.progress !== undefined && (j.active || j.state === 'gotowe')) {
        top.appendChild(el('span', 'jid', j.progress + '%'));
      }
      main.appendChild(top);
      if (j.active || j.state === 'gotowe') {
        const bar = el('div', 'job-bar' + (j.active && (j.progress === null || j.progress === undefined) ? ' indet' : ''));
        const fill = el('div');
        fill.style.width = (j.progress ?? 0) + '%';
        if (j.state === 'gotowe') fill.style.background = 'var(--ok)';
        bar.appendChild(fill);
        main.appendChild(bar);
      }
      if (j.detail) main.appendChild(el('div', 'job-detail', j.detail));
      const actions = el('div', 'job-actions');
      if (j.can_hold) {
        const b = el('button', 'btn secondary', 'Wstrzymaj');
        b.title = 'Zatrzymaj to zadanie w kolejce – wydrukujesz je później';
        b.onclick = () => jobAction(j, 'hold');
        actions.appendChild(b);
      }
      if (j.can_release) {
        const b = el('button', 'btn secondary', 'Wznów');
        b.onclick = () => jobAction(j, 'release');
        actions.appendChild(b);
      }
      if (j.state !== 'gotowe' && j.state !== 'anulowane' && j.state !== 'przerwane') {
        const b = el('button', 'btn ' + (j.active ? 'danger' : 'secondary'), j.active ? 'Przerwij' : 'Anuluj');
        b.onclick = () => jobAction(j, 'cancel');
        actions.appendChild(b);
      }
      li.append(main, actions);
      ul.appendChild(li);
    }
    // Gdy coś się drukuje – odświeżamy co 2 s
    const fast = list.some((j) => j.active || j.state === 'w kolejce');
    if (fast !== jobsFast) {
      jobsFast = fast;
      clearInterval(jobsTimer);
      jobsTimer = setInterval(refreshJobs, fast ? 2000 : 10000);
    }
  } catch (_) {
    /* drukarka może nie udostępniać kolejki – pomijamy */
  } finally {
    btn.classList.remove('spin');
  }
}

// ---------------------------------------------------------------- alarmy
const ALERTS = {
  'media-empty': 'Brak papieru w drukarce – włóż papier, aby kontynuować.',
  'media-jam': 'Papier się zaciął – wyjmij go delikatnie z drukarki.',
  'cover-open': 'Pokrywa drukarki jest otwarta.',
  'marker-supply-empty': 'Skończył się tusz – uzupełnij zbiornik.',
  'waste-ink-full': 'Pojemnik na zużyty tusz jest pełny – potrzebny serwis.',
};
const alerted = new Set();

async function notify(body) {
  try {
    let ok = await notif.isPermissionGranted();
    if (!ok) ok = (await notif.requestPermission()) === 'granted';
    if (ok) notif.sendNotification({ title: 'Epson Druk', body });
  } catch (_) { /* brak powiadomień systemowych – zostaje komunikat w oknie */ }
}

function checkAlerts(st) {
  const now = new Set(st.reasons);
  for (const [key, msg] of Object.entries(ALERTS)) {
    if (now.has(key) && !alerted.has(key)) {
      alerted.add(key);
      toast(msg, 'bad');
      notify(msg);
    } else if (!now.has(key) && alerted.has(key)) {
      alerted.delete(key);
      if (key === 'media-empty') toast('Papier włożony – drukarka gotowa', 'ok');
    }
  }
  for (const ink of st.inks) {
    const key = 'ink-low:' + ink.name;
    if (ink.level !== null && ink.level !== undefined && ink.level <= 10 && ink.level > 0) {
      if (!alerted.has(key)) {
        alerted.add(key);
        const msg = `Kończy się tusz: ${inkName(ink.name)} (${ink.level}%). Warto go uzupełnić.`;
        toast(msg);
        notify(msg);
      }
    } else if (ink.level > 20) {
      alerted.delete(key);
    }
  }
}

// ---------------------------------------------------------------- sterownik i firmware
let driverInfo = null;

function renderSysinfo() {
  const st = state.status;
  const showDriver = st && st.mode === 'cups' && !st.driver_missing;
  $('row-driver').classList.toggle('hidden', !showDriver);
  $('row-fw').classList.toggle('hidden', !st);
  $('sysinfo').classList.toggle('hidden', !st);
  if (!st) return;
  $('fw-val').textContent = st.firmware || 'nieznana';
  if (showDriver && driverInfo) {
    const v = $('driver-val');
    if (driverInfo.update_available) {
      v.textContent = `${driverInfo.installed} → dostępna ${driverInfo.latest}`;
      v.className = 'sys-val upd';
      $('btn-driver-update').classList.remove('hidden');
    } else if (driverInfo.installed) {
      v.textContent = driverInfo.latest ? `${driverInfo.installed} (aktualny)` : driverInfo.installed;
      v.className = 'sys-val' + (driverInfo.latest ? ' ok' : '');
      $('btn-driver-update').classList.add('hidden');
    } else {
      v.textContent = 'zainstalowany';
      v.className = 'sys-val';
    }
  }
}

async function checkDriver() {
  if (!state.status || state.status.mode !== 'cups') return;
  try {
    const prev = driverInfo;
    driverInfo = await invoke('driver_info');
    renderSysinfo();
    if (driverInfo.update_available && !(prev && prev.update_available)) {
      const msg = `Dostępna nowa wersja sterownika Epson: ${driverInfo.latest} (masz ${driverInfo.installed}).`;
      toast(msg);
      notify(msg);
    }
  } catch (_) { /* brak internetu – pomijamy */ }
}

// ---------------------------------------------------------------- konserwacja
async function runMaintenance(action) {
  if (!state.ip) return;
  const isClean = action === 'clean';
  const ok = await dialog.ask(
    isClean
      ? 'Czyszczenie głowicy zużywa trochę tuszu i trwa około 1–2 minut. W tym czasie nie wyłączaj drukarki.\n\nRozpocząć czyszczenie?'
      : 'Drukarka wydrukuje wzór testu dysz na jednej kartce. Upewnij się, że jest włożony papier A4.\n\nWydrukować test?',
    { title: isClean ? 'Czyszczenie głowicy' : 'Test dysz', kind: isClean ? 'warning' : 'info', okLabel: isClean ? 'Wyczyść' : 'Drukuj', cancelLabel: 'Anuluj' }
  ).catch(() => false);
  if (!ok) return;
  const btn = $(isClean ? 'btn-clean' : 'btn-nozzle');
  btn.disabled = true;
  try {
    await invoke('maintenance', { ip: state.ip, action });
    toast(isClean
      ? 'Czyszczenie rozpoczęte – poczekaj, aż kontrolka zasilania przestanie migać. Potem zrób test dysz.'
      : 'Wysłano test dysz. Jeśli we wzorze są przerwy – uruchom czyszczenie głowicy.', 'ok');
  } catch (e) {
    toast(errText(e), 'bad');
  } finally {
    setTimeout(() => { btn.disabled = !state.ip; }, isClean ? 60000 : 5000);
  }
}

function updateMaintButtons() {
  const on = !!(state.ip && state.status);
  $('btn-nozzle').disabled = !on;
  $('btn-clean').disabled = !on;
}

// ---------------------------------------------------------------- aktualizacje
let update = null;

async function checkUpdate({ manual = false } = {}) {
  const btn = $('btn-version');
  btn.classList.add('checking');
  if (manual) $('btn-version-text').textContent = 'Sprawdzam…';
  try {
    update = await invoke('check_update');
    if (update.available) {
      $('upd-version').textContent = update.latest;
      $('upd-current').textContent = update.current;
      $('update-bar').classList.remove('hidden');
      btn.classList.add('has-update');
      btn.title = `Dostępna wersja ${update.latest} – kliknij, aby zobaczyć`;
      $('btn-version-text').textContent = 'Nowa wersja!';
      if (!update.can_install) {
        $('btn-upd-install').textContent = 'Pobierz ze strony';
        $('upd-sub').textContent = 'Ta kopia nie działa jako AppImage – pobierz nową wersję ręcznie.';
      }
    } else if (manual) {
      toast(`Masz najnowszą wersję (${update.current})`, 'ok');
    }
  } catch (e) {
    if (manual) toast(errText(e), 'bad');
  } finally {
    btn.classList.remove('checking');
    if (!(update && update.available)) $('btn-version-text').textContent = 'Aktualizacje';
    if (update) $('app-version').textContent = update.current;
  }
}

async function installUpdate() {
  if (!update) return;
  if (!update.can_install) {
    invoke('open_page', { url: update.page }).catch((e) => toast(errText(e), 'bad'));
    return;
  }
  const b = $('btn-upd-install');
  if (b.dataset.ready === '1') {
    try { await invoke('restart_app'); } catch (e) { toast(errText(e), 'bad'); }
    return;
  }
  b.disabled = true;
  b.textContent = 'Pobieram…';
  $('btn-upd-close').disabled = true;
  $('upd-progress').classList.remove('hidden');
  $('upd-sub').textContent = 'Pobieranie aktualizacji…';
  const unlisten = await events.listen('update-progress', (e) => {
    $('upd-progress-fill').style.width = e.payload + '%';
    $('upd-sub').textContent = `Pobieranie aktualizacji… ${e.payload}%`;
  });
  try {
    const v = await invoke('install_update');
    $('upd-progress-fill').style.width = '100%';
    $('upd-sub').textContent = `Wersja ${v} zainstalowana. Uruchom aplikację ponownie, aby z niej korzystać.`;
    b.textContent = 'Uruchom ponownie';
    b.dataset.ready = '1';
    b.disabled = false;
  } catch (e) {
    $('upd-sub').textContent = 'Aktualizacja nie powiodła się – możesz spróbować ponownie.';
    b.textContent = 'Spróbuj ponownie';
    b.disabled = false;
    $('btn-upd-close').disabled = false;
    toast(errText(e), 'bad');
  } finally {
    unlisten();
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
  $('btn-driver-page').onclick = () =>
    invoke('open_page', { url: 'https://support.epson.net/linux/Printer/LSB_less_distribution_pages/en/escpr.php' })
      .catch((e) => toast(errText(e), 'bad'));
  $('btn-setup').onclick = async () => {
    const st = state.status;
    if (!st) return;
    if (st.driver_missing) { connect(st.ip); return; }
    const b = $('btn-setup');
    b.disabled = true;
    b.textContent = 'Konfiguruję…';
    try {
      await invoke('setup_printer', { ip: st.ip, model: st.model });
      toast('Drukarka dodana do systemu – możesz drukować', 'ok');
      await connect(st.ip, { silent: true });
    } catch (e) {
      const msg = errText(e);
      if (msg === 'BRAK_STEROWNIKA') { await connect(st.ip, { silent: true }); }
      else toast(msg, 'bad');
    } finally {
      b.disabled = false;
    }
  };
  $('btn-print').onclick = printAll;
  $('btn-jobs').onclick = refreshJobs;
  $('btn-version').onclick = () => {
    if (update && update.available) {
      $('update-bar').classList.remove('hidden');
      $('update-bar').scrollIntoView({ behavior: 'smooth' });
    } else {
      checkUpdate({ manual: true });
    }
  };
  $('btn-nozzle').onclick = () => runMaintenance('nozzle');
  // Tusz ręcznie
  $('btn-ink-edit').onclick = () => { inkEditing = true; renderInks(); };
  $('btn-ink-save').onclick = saveManualInks;
  $('btn-ink-full').onclick = () => {
    manualInks = { levels: {}, date: '' };
    for (const ink of inkList(state.status)) manualInks.levels[ink.name] = 100;
    saveManualInks();
  };
  // Edytor zdjęć
  $('pe-close').onclick = () => closeEditor(false);
  $('pe-ok').onclick = () => closeEditor(true);
  $('pe-reset').onclick = () => { editParams = { ...PHOTO_DEFAULT }; syncEditorControls(); schedulePreview(0); };
  $('pe-rot-l').onclick = () => { editParams.rotate = (editParams.rotate + 270) % 360; schedulePreview(0); };
  $('pe-rot-r').onclick = () => { editParams.rotate = (editParams.rotate + 90) % 360; schedulePreview(0); };
  for (const [id, key] of [['pe-b', 'brightness'], ['pe-c', 'contrast'], ['pe-s', 'saturation']]) {
    $(id).oninput = () => {
      editParams[key] = parseInt($(id).value, 10);
      $(id + '-v').textContent = editParams[key];
      schedulePreview();
    };
  }
  document.querySelectorAll('#pe-fit button').forEach((b) => {
    b.onclick = () => { editParams.fill = b.dataset.fill === '1'; syncEditorControls(); schedulePreview(0); };
  });
  document.addEventListener('keydown', (e) => {
    if (e.key === 'Escape' && editing) closeEditor(false);
  });
  $('btn-driver-update').onclick = () =>
    invoke('open_page', { url: (driverInfo && driverInfo.page) || 'https://support.epson.net/linux/Printer/LSB_less_distribution_pages/en/escpr.php' })
      .catch((e) => toast(errText(e), 'bad'));
  $('btn-panel').onclick = () => {
    if (!state.ip) return;
    invoke('open_page', { url: `http://${state.ip}/` }).catch((e) => toast(errText(e), 'bad'));
  };
  $('btn-clean').onclick = () => runMaintenance('clean');
  $('btn-upd-install').onclick = installUpdate;
  $('btn-upd-close').onclick = () => { $('update-bar').classList.add('hidden'); $('update-notes').classList.add('hidden'); };
  $('btn-upd-notes').onclick = () => {
    const n = $('update-notes');
    n.textContent = (update && update.notes.trim()) || 'Brak opisu zmian dla tej wersji.';
    n.classList.toggle('hidden');
  };
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
  invoke('app_version').then((v) => { $('app-version').textContent = v; }).catch(() => {});
  try {
    const mi = await invoke('load_setting', { key: 'tusz-reczny' });
    if (mi) manualInks = JSON.parse(mi);
  } catch (_) {}
  setTimeout(() => checkUpdate(), 3000); // sprawdź aktualizacje chwilę po starcie
  setInterval(() => { checkUpdate(); checkDriver(); }, 6 * 60 * 60 * 1000); // i co 6 godzin
  const saved = await invoke('load_printer').catch(() => null);
  if (saved) {
    $('manual-ip').value = saved;
    await connect(saved, { silent: true });
    if (!state.status) scan(); // zapisana drukarka zmieniła adres? szukamy
  } else {
    scan();
  }
  // Kolejka: co 10 s (co 2 s, gdy coś się drukuje – patrz refreshJobs)
  jobsTimer = setInterval(refreshJobs, 10000);
  // Stan drukarki co 10 s
  setInterval(() => {
    if (state.ip && !state.printing && !inkEditing) {
      invoke('status', { ip: state.ip })
        .then((st) => { state.status = st; renderPrinter(); renderInks(); checkAlerts(st); })
        .catch(() => setPill('Brak połączenia', 'bad'));
    }
  }, 10000);
}

start();
