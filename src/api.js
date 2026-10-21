// main.js 時代の window.api を Tauri の invoke / listen で提供する
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
window.api = {
  get: () => invoke('get'),
  loadChannels: (guildId) => invoke('load_channels', { guildId }),
  setFav: (trayId, id, fav) => invoke('set_fav', { trayId, id, fav }),
  refresh: () => invoke('refresh'),
  markRead: (id, read = true) => invoke('mark_read', { id, read }),
  markReadIn: (id) => invoke('mark_read_in', { id }),
  removeDm: (id) => invoke('remove_dm', { id }),
  openUrl: (url) => invoke('open_url', { url }),
  markAllRead: (trayId) => invoke('mark_all_read', { trayId }),
  addTray: (name) => invoke('add_tray', { name }),
  duplicateTray: (id) => invoke('duplicate_tray', { id }),
  reorderTrays: (ids) => invoke('reorder_trays', { ids }),
  renameTray: (id, name) => invoke('rename_tray', { id, name }),
  deleteTray: (id) => invoke('delete_tray', { id }),
  addDm: (id) => invoke('add_dm', { id }),
  open: (id) => invoke('open', { id }),
  setSettings: (patch) => invoke('set_settings', { patch }),
  setClientId: (id) => invoke('set_client_id', { id }),
  setTrayNotify: (id, patch) => invoke('set_tray_notify', { id, patch }),
  setAutostart: (on) => invoke('set_autostart', { on }),
  testNotify: () => invoke('test_notify'),
  onSound: (fn) => listen('sound', (e) => fn(e.payload)),
  onUpdate: (fn) => listen('update', (e) => fn(e.payload)),
};
