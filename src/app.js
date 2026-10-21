let v = null, selected = null;
let cur = null; // 表示中のトレイ id
let favs = {}, msgs = []; // 表示中トレイのお気に入り / 新着
let editing = false; // タブ名を編集中は再描画しない
let hiddenTabs = new Set();
try { hiddenTabs = new Set(JSON.parse(localStorage.hiddenTabs || '[]')); } catch {}
const inboxId = (id) => id === 'inbox' || id === 'inbox-dm' || id === 'inbox-mix';
const isInbox = () => inboxId(cur); // 受信トレイ（サーバー用 / DM 用 / まとめ）は ★ なしで全件表示
// 受信トレイは設定でオンのときだけ
const usable = () => v.trays.filter((t) => t.id === 'inbox' ? v.settings.inboxAll : t.id === 'inbox-dm' ? v.settings.inboxDm : t.id === 'inbox-mix' ? v.settings.inboxMix : true);
// トレイの新着: 受信トレイはサーバー / DM の全件、それ以外は ★ したサーバー / チャンネル / DM のもの。
// 設定に従いサーバーごと（DM は DM ごと）かチャンネルごとに最新 1 件へまとめ、まとめた中に未読があれば未読にする
const byServer = () => v.settings.groupBy !== 'channel';
function trayMessages(t) {
  const groups = new Map();
  for (const m of v.messages) { // 新しい順
    if (t.id === 'inbox-mix' ? false : t.id === 'inbox' ? !m.guildId : t.id === 'inbox-dm' ? !!m.guildId : !t.favorites[m.channelId] && !(m.guildId && t.favorites[m.guildId])) continue;
    const k = byServer() ? m.guildId || m.channelId : m.channelId;
    groups.has(k) ? groups.get(k).push(m) : groups.set(k, [m]);
  }
  return [...groups.values()].map((g) => ({ ...g[0], read: g.every((x) => x.read) }));
}
const open = new Set();
try { cur = localStorage.tray || null; } catch {}

function el(tag, cls, text) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text != null) e.textContent = text; // メッセージ本文は必ず textContent で入れる
  return e;
}

function avatar(url, name) {
  if (!url) return el('span', 'av', [...name][0] ?? '?');
  const i = el('img', 'av');
  i.src = url;
  return i;
}

// icon: undefined ならアイコンなし、null なら頭文字
// onClick を省くと行のクリックで ☆ を切り替える（受信トレイでは何もしない）
function row(cls, id, name, onClick, icon) {
  const r = el('div', 'row ' + cls);
  const fav = !!favs[id];
  const toggle = () => { if (!isInbox()) api.setFav(cur, id, !fav); };
  const s = el('span', 'star' + (fav ? ' on' : ''), fav ? '★' : '☆');
  s.title = 'お気に入り';
  s.onclick = (e) => { e.stopPropagation(); toggle(); };
  if (!isInbox()) r.append(s);
  if (icon !== undefined) r.append(avatar(icon, name));
  r.append(el('span', 'name', name));
  const unread = msgs.filter((m) => !m.read && (m.channelId === id || m.guildId === id)).length;
  if (unread) r.append(el('span', null, unread));
  r.onclick = onClick ?? toggle;
  return r;
}

const qInput = document.getElementById('q');
const sortSel = document.getElementById('sort');
let favOnly = false; // お気に入りのみ表示（並べ替えのプルダウンで切り替える）

// 新着が届いた最新時刻（届いていなければ 0）
function lastTime(id) {
  const m = msgs.find((x) => x.channelId === id || x.guildId === id); // 新しい順
  return m ? Date.parse(m.time) : 0;
}

function sorted(items, id, name) {
  if (sortSel.value === 'name') return [...items].sort((a, b) => name(a).localeCompare(name(b), 'ja'));
  return [...items].sort((a, b) => lastTime(id(b)) - lastTime(id(a)));
}

// DM 追加欄（再描画で入力が消えないよう 1 つの要素を使い回す）
const dmBox = el('div', 'dmAdd');
const dmInput = el('input');
const dmErr = el('div', 'err');
dmBox.hidden = true;
dmInput.placeholder = 'DM のチャンネル ID を貼り付けて Enter';
dmBox.append(dmInput, dmErr);
const closeDmBox = () => { dmBox.hidden = true; renderNav(); };
dmInput.onkeydown = async (e) => {
  if (e.key === 'Escape') return closeDmBox();
  if (e.key !== 'Enter') return;
  dmErr.textContent = '確認中…';
  const r = await api.addDm(dmInput.value);
  if (r.error) dmErr.textContent = r.error;
  else closeDmBox();
};
addEventListener('mousedown', (e) => { if (!dmBox.hidden && !dmBox.contains(e.target)) closeDmBox(); });

function renderNav() {
  const nav = document.getElementById('nav');
  nav.replaceChildren();
  const q = qInput.value.trim().toLowerCase();
  const hit = (s) => !q || s.toLowerCase().includes(q);
  const dmSec = el('div', 'sec hov', 'ダイレクトメッセージ');
  const ab = el('button', 'refresh', '＋');
  ab.title = 'チャンネル ID で DM を追加';
  ab.onclick = () => { dmBox.hidden = false; dmErr.textContent = ''; dmInput.value = ''; renderNav(); };
  dmSec.append(ab);
  nav.append(dmSec);
  if (!dmBox.hidden) { nav.append(dmBox); dmInput.focus(); }
  // お気に入りのみ: このトレイで ★ したものだけ（受信トレイには ★ が無いので効かない）
  const fo = favOnly && !isInbox();
  const dms = sorted(Object.entries(v.dms).filter(([id, d]) => hit(d.name) && (!fo || favs[id])), ([id]) => id, ([, d]) => d.name);
  if (!dms.length && !q && !fo) nav.append(el('div', 'sec', '（DM を受信すると表示されます）'));
  for (const [id, d] of dms) {
    const r = row('', id, d.name, null, d.icon ?? null);
    r.oncontextmenu = (e) => navMenu(e, id, null, id, true);
    nav.append(r);
  }
  const sec = el('div', 'sec hov', 'サーバー');
  const rb = el('button', 'refresh', '⟳');
  rb.title = 'サーバー・チャンネル一覧を取り直す';
  rb.onclick = () => api.refresh();
  sec.append(rb);
  nav.append(sec);
  for (const g of sorted(v.guilds, (g) => g.id, (g) => g.name)) {
    const chs = v.channels[g.id] || [];
    const gHit = hit(g.name);
    const favChs = fo ? chs.filter((c) => favs[c.id]) : [];
    if (fo && !favs[g.id] && !favChs.length) continue;
    // 閉じていても、検索に一致したチャンネルとお気に入りのみ表示時の ★ チャンネルは出す
    const chHits = q ? (fo ? favChs : chs).filter((c) => hit(c.name)) : favChs;
    if (!gHit && !chHits.length) continue;
    // 検索中はサーバー名が一致しなければ一致したチャンネルだけ出す
    const shown = q && !gHit ? chHits : open.has(g.id) ? chs : chHits;
    // ▶ で開閉、行のほかの部分は ☆ の切り替え
    const gr = row('', g.id, g.name, null, g.icon_url);
    const arrow = el('span', 'arrow', open.has(g.id) ? '▼' : '▶');
    arrow.title = open.has(g.id) ? '閉じる' : 'チャンネルを表示';
    arrow.onclick = (e) => {
      e.stopPropagation();
      open.has(g.id) ? open.delete(g.id) : (open.add(g.id), api.loadChannels(g.id));
      render();
    };
    gr.prepend(arrow);
    gr.oncontextmenu = (e) => navMenu(e, g.id, g.id, null);
    nav.append(gr);
    for (const c of shown) {
      const r = row('ch', c.id, '# ' + c.name);
      r.oncontextmenu = (e) => navMenu(e, c.id, g.id, c.id);
      nav.append(r);
    }
  }
}

// 投稿者アイコン + テキスト
function who(m, tag, cls, text) {
  const e = el(tag, cls);
  e.append(avatar(m.avatar, m.author), el('span', 'txt', text));
  return e;
}

// where は「サーバー名 #チャンネル名」か「DM 相手の名前」。チャンネル名に空白は入らないので最後の ' #' で分ける
function place(m) {
  if (!m.guildId) return ['DM', '@' + m.where.replace(/^DM /, '')];
  const i = m.where.lastIndexOf(' #');
  return i < 0 ? [m.where, ''] : [m.where.slice(0, i), m.where.slice(i + 1)];
}

// w: 既定の幅(px)。本文は残りの幅を使う
const COLS = [
  // 受信トレイだけ: 他のトレイで ★ しているチャンネル / サーバー / DM に ★ を出す。行ごとに別の grid なので、無い行も透明な ★ で幅をそろえる
  { key: 'star', label: '★', fit: true, cell: (m) => el('span', v.trays.some((t) => t.favorites[m.channelId] || (m.guildId && t.favorites[m.guildId])) ? 'fav' : 'fav off', '★') },
  { key: 'time', label: '時間', w: 140, cell: (m) => el('span', 't', new Date(m.time).toLocaleString()) },
  { key: 'author', label: '投稿者', w: 160, cell: (m) => who(m, 'span', 'w', m.author) },
  { key: 'where', label: 'サーバー', w: 160, cell: (m) => el('span', 't', place(m)[0]) },
  { key: 'channel', label: 'チャンネル', w: 140, cell: (m) => el('span', 't', place(m)[1]) },
  { key: 'group', label: 'グループ名', w: 160, cell: (m) => el('span', 't', v.dms[m.channelId]?.group ? v.dms[m.channelId].name : '') },
  { key: 'content', label: '本文', cell: (m) => el('span', null, m.content) },
];
// DM トレイではサーバー / チャンネルの代わりにグループ名を出す
const trayCols = () => COLS.filter((c) => (isInbox() || c.key !== 'star') && (cur === 'inbox-dm' ? c.key !== 'where' && c.key !== 'channel' : c.key !== 'group'));
let hidden = new Set(), colW = {};
try { hidden = new Set(JSON.parse(localStorage.hiddenCols || '[]')); } catch {}
try { colW = JSON.parse(localStorage.colW || '{}'); } catch {}
const colsCss = (cols) => cols.map((c) => (c.fit ? 'max-content' : c.w ? (colW[c.key] ?? c.w) + 'px' : 'minmax(0, 1fr)')).join(' ');

function renderList() {
  const list = document.getElementById('list');
  list.replaceChildren();
  const cols = trayCols().filter((c) => !hidden.has(c.key));
  list.style.setProperty('--cols', colsCss(cols));
  const head = el('div', 'msg head');
  head.append(...cols.map((c) => {
    const h = el('span', null, c.label);
    if (!c.w) return h; // fit の列も幅は固定
    // 右端をドラッグして幅を変える
    const rz = el('span', 'rz');
    rz.onmousedown = (e) => {
      e.preventDefault();
      const x0 = e.clientX, w0 = h.getBoundingClientRect().width;
      const move = (ev) => { colW[c.key] = Math.max(40, Math.round(w0 + ev.clientX - x0)); list.style.setProperty('--cols', colsCss(cols)); };
      const up = () => {
        removeEventListener('mousemove', move);
        removeEventListener('mouseup', up);
        try { localStorage.colW = JSON.stringify(colW); } catch {}
      };
      addEventListener('mousemove', move);
      addEventListener('mouseup', up);
    };
    h.append(rz);
    return h;
  }));
  list.append(head);
  const shown = msgs;
  if (!shown.length) list.append(el('div', 'empty', isInbox() ? 'まだ新着はありません。' : 'サイドバーで ☆ を付けると、このトレイに新着が表示されます。'));
  for (const m of shown) {
    const r = el('div', 'msg' + (m.read ? '' : ' unread') + (selected === m.id ? ' sel' : ''));
    r.append(...cols.map((c) => c.cell(m)));
    // サーバーごとのときはまとめた分を全部既読にする
    r.onclick = () => { selected = m.id; if (!m.read) byServer() && m.guildId ? api.markReadIn(m.guildId) : api.markRead(m.id); render(); };
    r.ondblclick = () => api.open(m.id);
    r.oncontextmenu = (e) => { e.preventDefault(); e.stopPropagation(); showMenu(e, msgMenu(m)); };
    list.append(r);
  }
}

function renderPreview() {
  const p = document.getElementById('preview');
  p.replaceChildren();
  const m = v.messages.find((x) => x.id === selected);
  if (!m) return;
  const b = el('button', null, 'Discord で開く');
  b.id = 'openBtn';
  b.onclick = () => api.open(m.id);
  p.append(who(m, 'div', 'meta', `${m.author}  ${m.where}  ${new Date(m.time).toLocaleString()}`), b, el('p', null, m.content));
}

// 状態は変わったときだけ数秒表示する。対応が必要なもの（エラー・切断など）は解決するまで出し続ける
const ALERT = /エラー|切断|拒否|起動していません|許可して/;
let lastStatus = null, statusTimer;
function flash(text, ms) {
  const st = document.getElementById('status');
  clearTimeout(statusTimer);
  st.textContent = text;
  st.classList.toggle('alert', ALERT.test(text));
  statusTimer = setTimeout(() => {
    const keep = ALERT.test(v.status);
    st.textContent = keep ? v.status : '';
    st.classList.toggle('alert', keep);
  }, ms);
}
function showStatus(s) {
  if (s === lastStatus) return;
  lastStatus = s;
  flash(s, ALERT.test(s) ? 2 ** 31 - 1 : 5000);
}

function selectTray(id) {
  if (id === cur) return; // 描き直すとダブルクリックが効かなくなるので同じタブなら何もしない
  cur = id;
  selected = null;
  try { localStorage.tray = id; } catch {}
  render();
}

function renameTab(id) {
  const t = v.trays.find((x) => x.id === id);
  const tab = document.querySelector(`#tabs .tab[data-id="${id}"]`); // 描き直し後でも今の要素を取る
  if (!t || !tab) return;
  editing = true;
  const inp = el('input');
  inp.value = t.name;
  inp.onclick = inp.ondblclick = (e) => e.stopPropagation();
  const done = (ok) => {
    if (!editing) return;
    editing = false;
    if (ok && inp.value.trim() && inp.value.trim() !== t.name) api.renameTray(t.id, inp.value.trim());
    else renderTabs();
  };
  inp.onkeydown = (e) => { if (e.key === 'Enter') done(true); if (e.key === 'Escape') done(false); };
  inp.onblur = () => done(true);
  tab.replaceChildren(inp);
  inp.focus();
  inp.select();
}

function deleteTab(t) {
  if (!confirm(`「${t.name}」を削除しますか？`)) return;
  if (cur === t.id) cur = null;
  api.deleteTray(t.id);
}

// 右クリックメニュー用: タブの表示/非表示チェック（最後の1つは消せない）
function tabToggles() {
  return usable().map((t) => {
    const l = el('label');
    const cb = el('input');
    cb.type = 'checkbox';
    cb.checked = !hiddenTabs.has(t.id);
    cb.disabled = cb.checked && usable().filter((x) => !hiddenTabs.has(x.id)).length === 1;
    cb.onchange = () => {
      cb.checked ? hiddenTabs.delete(t.id) : hiddenTabs.add(t.id);
      try { localStorage.hiddenTabs = JSON.stringify([...hiddenTabs]); } catch {}
      menu.hidden = true;
      render();
    };
    l.append(cb, t.name);
    return l;
  });
}

function renderTabs() {
  if (editing) return;
  const tabs = document.getElementById('tabs');
  tabs.oncontextmenu = (e) => { e.preventDefault(); showMenu(e, tabToggles()); };
  tabs.replaceChildren(...usable().filter((t) => !hiddenTabs.has(t.id)).map((t) => {
    const tab = el('div', 'tab' + (t.id === cur ? ' on' : ''));
    tab.dataset.id = t.id;
    tab.append(el('span', null, t.name));
    const n = trayMessages(t).filter((m) => !m.read).length;
    if (n && t.notify.badge) tab.append(el('span', 'badge', n));
    tab.onclick = () => selectTray(t.id);
    tab.ondblclick = () => renameTab(t.id);
    // ドラッグで並べ替え（左右半分で前/後ろに入れる）。カーソルが変わらないよう HTML のドラッグ機能は使わない
    tab.onmousedown = (e) => {
      if (e.button !== 0 || editing) return;
      let target = null, after = false, moved = false;
      const clear = () => document.querySelectorAll('#tabs .tab').forEach((x) => x.classList.remove('drop-before', 'drop-after'));
      const move = (ev) => {
        if (!moved && Math.abs(ev.clientX - e.clientX) < 5) return; // クリックと区別する
        moved = true;
        clear();
        target = document.elementFromPoint(ev.clientX, ev.clientY)?.closest('#tabs .tab[data-id]');
        if (!target || target.dataset.id === t.id) return (target = null);
        const r = target.getBoundingClientRect();
        after = ev.clientX > r.left + r.width / 2;
        target.classList.add(after ? 'drop-after' : 'drop-before');
      };
      const up = () => {
        removeEventListener('mousemove', move);
        removeEventListener('mouseup', up);
        clear();
        if (!target) return;
        const ids = v.trays.map((x) => x.id).filter((x) => x !== t.id);
        ids.splice(ids.indexOf(target.dataset.id) + (after ? 1 : 0), 0, t.id);
        api.reorderTrays(ids);
      };
      addEventListener('mousemove', move);
      addEventListener('mouseup', up);
    };
    tab.oncontextmenu = (e) => {
      e.preventDefault();
      e.stopPropagation();
      const unread = trayMessages(t).some((m) => !m.read);
      showMenu(e, [
        menuItem('すべて既読にする', () => api.markAllRead(t.id), !unread),
        el('hr'),
        ...(v.portable ? [] : [menuCheck('トースト通知', t.notify.toast, (on) => api.setTrayNotify(t.id, { toast: on }))]),
        menuCheck('通知音', !!t.notify.sound, (on) => api.setTrayNotify(t.id, { sound: on ? 'chime' : false })),
        menuCheck('未読バッジ', t.notify.badge, (on) => api.setTrayNotify(t.id, { badge: on })),
        el('hr'),
        menuItem('名前を変更', () => renameTab(t.id)),
        menuItem('複製', async () => { const id = await api.duplicateTray(t.id); v = await api.get(); selectTray(id); }),
        menuItem('削除', () => deleteTab(t), inboxId(t.id) || usable().length === 1),
        el('hr'),
        ...tabToggles(),
      ]);
    };
    return tab;
  }));
  const add = el('div', 'tab add', '＋');
  add.title = 'トレイを追加';
  add.onclick = async () => {
    const id = await api.addTray('新しいトレイ');
    v = await api.get();
    selectTray(id);
    renameTab(id);
  };
  tabs.append(add);
}

function render() {
  const visible = usable().filter((t) => !hiddenTabs.has(t.id));
  if (!visible.some((t) => t.id === cur)) cur = (visible[0] ?? usable()[0] ?? v.trays[0]).id;
  const tray = v.trays.find((t) => t.id === cur);
  favs = tray.favorites;
  msgs = trayMessages(tray);
  showStatus(v.status);
  syncSort(); renderTabs(); renderNav(); renderList(); renderPreview();
  if (dlg.open) renderSettings();
}

// ---- 設定画面 ----
const dlg = document.getElementById('settings');
function check(checked, onchange) {
  const cb = el('input');
  cb.type = 'checkbox';
  cb.checked = checked;
  cb.onchange = () => onchange(cb.checked);
  return cb;
}
function renderSettings() {
  const { inboxAll: a, inboxDm: d, inboxMix: x } = v.settings;
  document.getElementById('inboxMode').value = x ? 'mix' : a && d ? 'split' : a ? 'server' : d ? 'dm' : 'off';
  document.getElementById('groupBy').value = v.settings.groupBy;
  document.getElementById('autostart').checked = v.autostart;
  document.getElementById('startMinimized').checked = v.settings.startMinimized;
  document.getElementById('startMinimized').disabled = !v.autostart;
  document.getElementById('soundCooldown').value = v.settings.soundCooldown;
  document.getElementById('alwaysOnTop').checked = v.settings.alwaysOnTop;
  document.getElementById('trayBadge').checked = v.settings.trayBadge;
  document.getElementById('clientId').value = v.settings.clientId;
  document.getElementById('toastHead').hidden = v.portable;
  document.getElementById('notifyRows').replaceChildren(...usable().map((t) => {
    const tr = el('tr');
    const cells = [
      el('td', null, t.name),
      el('td'),
      el('td'),
      el('td'),
    ];
    cells[1].append(check(t.notify.toast, (on) => api.setTrayNotify(t.id, { toast: on })));
    const sel = el('select');
    sel.append(new Option('オフ', ''), ...Object.entries(SOUNDS).map(([id, x]) => new Option(x.label, id)));
    sel.value = t.notify.sound || '';
    sel.onchange = () => { playSound(sel.value); api.setTrayNotify(t.id, { sound: sel.value || false }); };
    cells[2].append(sel);
    const badge = check(t.notify.badge, (on) => api.setTrayNotify(t.id, { badge: on }));
    cells[3].append(badge);
    if (v.portable) cells.splice(1, 1); // ポータブル版はトーストなし
    tr.append(...cells);
    return tr;
  }));
}
document.getElementById('gear').onclick = () => { renderSettings(); dlg.showModal(); };
document.getElementById('closeSettings').onclick = () => dlg.close();
document.getElementById('groupBy').onchange = (e) => api.setSettings({ groupBy: e.target.value });
// 受信トレイの使い方: [サーバー用, DM 用, まとめ] のどれを使うか
const INBOX_MODES = { off: [0, 0, 0], mix: [0, 0, 1], split: [1, 1, 0], server: [1, 0, 0], dm: [0, 1, 0] };
document.getElementById('inboxMode').onchange = (e) => {
  const [a, d, x] = INBOX_MODES[e.target.value];
  api.setSettings({ inboxAll: !!a, inboxDm: !!d, inboxMix: !!x });
};
document.getElementById('autostart').onchange = (e) => api.setAutostart(e.target.checked);
document.getElementById('startMinimized').onchange = (e) => api.setSettings({ startMinimized: e.target.checked });
document.getElementById('trayBadge').onchange = (e) => api.setSettings({ trayBadge: e.target.checked });
document.getElementById('alwaysOnTop').onchange = (e) => api.setSettings({ alwaysOnTop: e.target.checked });
document.getElementById('soundCooldown').onchange = (e) => api.setSettings({ soundCooldown: Math.max(0, Math.floor(+e.target.value) || 0) });
document.getElementById('applyClientId').onclick = () => api.setClientId(document.getElementById('clientId').value).catch(alert);
document.getElementById('testNotify').onclick = () => api.testNotify();

// 通知音（音声ファイルを使わず WebAudio で鳴らす）。notes: [周波数Hz, 開始秒, 長さ秒]
const C5 = 523.25, E5 = 659.25, G5 = 783.99, C6 = 1046.5, D6 = 1174.66, E6 = 1318.51;
const SOUNDS = {
  pon: { label: 'ポン（短）', type: 'sine', notes: [[C6, 0, 0.15]] },
  pico: { label: 'ピコッ', type: 'square', vol: 0.06, notes: [[E6, 0, 0.08], [C6 * 2, 0.09, 0.15]] },
  chime: { label: 'チャイム', type: 'sine', notes: [[880, 0, 0.3], [E6, 0.12, 0.35]] },
  bell: { label: 'ベル', type: 'triangle', notes: [[G5, 0, 1.0], [G5 * 2, 0, 0.8], [G5 * 3, 0, 0.5]] },
  melody: { label: 'メロディ', type: 'triangle', notes: [[C5, 0, 0.35], [E5, 0.3, 0.35], [G5, 0.6, 0.35], [C6, 0.9, 0.6]] },
  musicbox: { label: 'オルゴール（長）', type: 'sine', notes: [[E6, 0, 0.6], [C6, 0.3, 0.6], [D6, 0.6, 0.6], [G5, 0.9, 0.7], [G5, 1.4, 0.6], [D6, 1.7, 0.6], [E6, 2.0, 0.5], [C6, 2.2, 0.4]] },
};
let audio;
function playSound(id) {
  const snd = SOUNDS[id];
  if (!snd) return;
  audio ??= new AudioContext();
  const t0 = audio.currentTime + 0.02;
  for (const [freq, at, dur] of snd.notes) {
    const o = audio.createOscillator();
    const g = audio.createGain();
    o.type = snd.type;
    o.frequency.value = freq;
    g.gain.setValueAtTime(0.0001, t0 + at);
    g.gain.exponentialRampToValueAtTime(snd.vol ?? 0.2, t0 + at + 0.01);
    g.gain.exponentialRampToValueAtTime(0.0001, t0 + at + dur);
    o.connect(g).connect(audio.destination);
    o.start(t0 + at);
    o.stop(t0 + at + dur + 0.02);
  }
}
api.onSound(playSound);

try { document.body.classList.toggle('collapsed', localStorage.sidebar === 'closed'); } catch {}
// 本文プレビュー欄の表示/非表示（⚙ と、プレビュー欄の右クリックで切り替える）
const prevCb = document.getElementById('showPreview');
function setPreview(on) {
  document.body.classList.toggle('noPreview', !on);
  prevCb.checked = on;
  try { localStorage.preview = on ? '' : 'hidden'; } catch {}
}
try { setPreview(localStorage.preview !== 'hidden'); } catch { setPreview(true); }
prevCb.onchange = () => setPreview(prevCb.checked);
document.getElementById('preview').oncontextmenu = (e) => { e.preventDefault(); showMenu(e, [menuItem('プレビューを隠す（⚙ で再表示）', () => setPreview(false))]); };
document.getElementById('toggle').onclick = () => {
  const closed = document.body.classList.toggle('collapsed');
  try { localStorage.sidebar = closed ? 'closed' : 'open'; } catch {}
};
// 右クリックメニュー
const menu = el('div');
menu.id = 'menu';
menu.hidden = true;
document.body.append(menu);
function showMenu(e, items) {
  menu.replaceChildren(...items);
  menu.hidden = false;
  // 画面からはみ出さない位置に出す
  menu.style.left = Math.max(0, Math.min(e.clientX, innerWidth - menu.offsetWidth - 4)) + 'px';
  menu.style.top = Math.max(0, Math.min(e.clientY, innerHeight - menu.offsetHeight - 4)) + 'px';
}
function menuItem(label, fn, disabled) {
  const i = el('div', 'mi' + (disabled ? ' off' : ''), label);
  i.onclick = () => { menu.hidden = true; fn(); };
  return i;
}
addEventListener('mousedown', (e) => { if (!menu.contains(e.target)) menu.hidden = true; });

// 右クリックメニュー用: 列の表示/非表示
function colToggles() {
  const cols = trayCols();
  return cols.map((c) => {
    const l = el('label');
    const cb = el('input');
    cb.type = 'checkbox';
    cb.checked = !hidden.has(c.key);
    cb.disabled = cb.checked && cols.filter((x) => !hidden.has(x.key)).length === 1; // 最後の1列は消せない
    cb.onchange = () => {
      cb.checked ? hidden.delete(c.key) : hidden.add(c.key);
      try { localStorage.hiddenCols = JSON.stringify([...hidden]); } catch {}
      menu.hidden = true;
      renderList();
    };
    l.append(cb, c.label);
    return l;
  });
}
document.getElementById('list').oncontextmenu = (e) => { e.preventDefault(); showMenu(e, colToggles()); };

// ---- サイドバー・新着の右クリックメニュー ----
// Discord のリンク: https はどこに貼ってもリンクになり、discord:// は直接アプリで開く
const linkPath = (gid, cid, mid) => ['channels', gid || '@me', cid, mid].filter(Boolean).join('/');
const webUrl = (p) => 'https://discord.com/' + p;
const appUrl = (p) => 'discord://-/' + p;

async function copy(text, what) {
  try { await navigator.clipboard.writeText(text); flash(`${what}をコピーしました`, 1500); }
  catch { flash('コピーできませんでした', 1500); }
}

function menuCheck(label, checked, onchange) {
  const l = el('label');
  const cb = el('input');
  cb.type = 'checkbox';
  cb.checked = checked;
  cb.onchange = () => { menu.hidden = true; onchange(cb.checked); };
  l.append(cb, label);
  return l;
}

// id: 行の id、gid: サーバー id（DM は null）、cid: チャンネル / DM の id（サーバー行は null）
function navMenu(e, id, gid, cid, dm) {
  e.preventDefault();
  const p = linkPath(gid, cid);
  const unread = v.messages.some((m) => !m.read && (m.channelId === id || m.guildId === id));
  showMenu(e, [
    menuItem('既読にする', () => api.markReadIn(id), !unread),
    menuItem('Discord で開く', () => api.openUrl(appUrl(p))),
    el('hr'),
    menuItem('リンクをコピー', () => copy(webUrl(p), 'リンク')),
    menuItem('リンクをコピー（アプリ用 discord://）', () => copy(appUrl(p), 'リンク')),
    menuItem('ID をコピー', () => copy(id, 'ID')),
    el('hr'),
    el('div', 'mh', 'トレイに追加'),
    ...trayChecks(id),
    ...(dm ? [el('hr'), menuItem('一覧から削除', () => {
      if (confirm(`「${v.dms[id].name}」を DM 一覧から削除しますか？
★ と新着も消えます（また通知が届くと一覧に戻ります）。`)) api.removeDm(id);
    })] : []),
  ]);
}

// 受信トレイ以外の各トレイで id を ★ するチェック
function trayChecks(id) {
  return v.trays.filter((t) => !inboxId(t.id)).map((t) => menuCheck(t.name, !!t.favorites[id], (on) => api.setFav(t.id, id, on)));
}

function msgMenu(m) {
  const p = linkPath(m.guildId, m.channelId, m.id);
  return [
    m.read ? menuItem('未読に戻す', () => api.markRead(m.id, false)) : menuItem('既読にする', () => api.markRead(m.id)),
    menuItem('Discord で開く', () => api.open(m.id)),
    el('hr'),
    menuItem('メッセージのリンクをコピー', () => copy(webUrl(p), 'リンク')),
    menuItem('メッセージのリンクをコピー（アプリ用 discord://）', () => copy(appUrl(p), 'リンク')),
    menuItem('本文をコピー', () => copy(m.content, '本文')),
    el('hr'),
    el('div', 'mh', m.guildId ? 'チャンネルをトレイに追加' : 'DM をトレイに追加'),
    ...trayChecks(m.channelId),
    ...(m.guildId ? [el('div', 'mh', 'サーバーをトレイに追加'), ...trayChecks(m.guildId)] : []),
  ];
}

// 新着一覧と本文の境目をドラッグ
const main = document.querySelector('main');
try { if (localStorage.split) main.style.setProperty('--split', localStorage.split); } catch {}
document.getElementById('split').onmousedown = (e) => {
  e.preventDefault();
  const move = (ev) => {
    const r = main.getBoundingClientRect();
    const pct = Math.min(90, Math.max(10, ((ev.clientY - r.top) / r.height) * 100));
    main.style.setProperty('--split', pct.toFixed(1) + '%');
  };
  const up = () => {
    removeEventListener('mousemove', move);
    removeEventListener('mouseup', up);
    try { localStorage.split = main.style.getPropertyValue('--split'); } catch {}
  };
  addEventListener('mousemove', move);
  addEventListener('mouseup', up);
};

// 並べ替えのプルダウン。「お気に入りのみ表示」は選ぶたびにオン/オフし、並べ替えの選択は戻す
let sortMode = 'recent';
try { sortMode = localStorage.sort === 'name' ? 'name' : 'recent'; favOnly = localStorage.favOnly === '1'; } catch {}
// 受信トレイには ★ が無いので「お気に入りのみ表示」と仕切りは出さない
function syncSort() {
  const fo = favOnly && !isInbox();
  for (const o of sortSel.options) {
    if (o.disabled || o.value === 'fav') o.hidden = isInbox();
    if (o.disabled) continue; // 仕切り
    if (o.value === 'fav') o.text = (fo ? '✓ ' : '') + '★のみ表示';
    else o.text = (o.value === 'name' ? '名前順' : '更新順') + (fo ? '（★のみ）' : '');
  }
  sortSel.value = sortMode;
}
sortSel.onchange = () => {
  if (sortSel.value === 'fav') favOnly = !favOnly;
  else sortMode = sortSel.value;
  try { localStorage.sort = sortMode; localStorage.favOnly = favOnly ? '1' : ''; } catch {}
  syncSort();
  renderNav();
};
qInput.oninput = () => renderNav();

api.onUpdate((nv) => { v = nv; render(); });
api.get().then((nv) => { v = nv; render(); });
