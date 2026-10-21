#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_opener::OpenerExt as _;
use tauri_winrt_notification::Toast;
use tokio::io::{AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::sync::oneshot;

const SCOPES: [&str; 3] = ["rpc", "messages.read", "rpc.notifications.read"];
const TEXT_TYPES: [i64; 2] = [0, 5]; // テキスト / アナウンス
const CATCH_UP_GAP_MS: u64 = 200; // 起動時の取り込みで GET_CHANNEL を送る間隔
const MAX_FRAME: usize = 16 << 20; // RPC で受け取る 1 フレームの上限（壊れた長さで巨大なメモリを確保しない）

// Public Client + PKCE なので Client Secret は持たない（Developer Portal で Public Client を ON にしておく）
// 共有の Client ID。RPC はテスター登録した人しか認可できないので、各自のアプリの ID を設定でも指定できる
const CLIENT_ID: &str = "1559561513273135277";
const REDIRECT_URI: &str = "http://localhost";

fn pkce_challenge(verifier: &str) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier))
}

#[test]
fn pkce_challenge_rfc7636() {
    assert_eq!(pkce_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
}

// トークンは Windows の DPAPI（今のユーザー用）で暗号化して "dpapi:<base64>" で保存する。
// 別のユーザーや別の PC では復号できず、トークンなしとして読む（再認可になる）
fn dpapi(data: &[u8], protect: bool) -> Option<Vec<u8>> {
    use windows_sys::Win32::{Foundation::LocalFree, Security::Cryptography::*};
    let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    let ok = unsafe {
        if protect {
            CryptProtectData(&input, std::ptr::null(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        } else {
            CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), CRYPTPROTECT_UI_FORBIDDEN, &mut out)
        }
    };
    if ok == 0 {
        return None;
    }
    let v = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    unsafe { LocalFree(out.pbData as _) };
    Some(v)
}

mod dpapi_token {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};
    use serde_json::Value;
    const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::STANDARD;

    pub fn serialize<S: Serializer>(t: &Option<Value>, s: S) -> Result<S::Ok, S::Error> {
        match t.as_ref().and_then(|t| super::dpapi(t.to_string().as_bytes(), true)) {
            Some(enc) => s.serialize_str(&format!("dpapi:{}", B64.encode(enc))),
            None => s.serialize_none(), // 暗号化できなければ平文では残さない
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
        Ok(match Option::<Value>::deserialize(d)? {
            // 旧版の平文トークンはそのまま読む（次の保存で暗号化される）
            Some(Value::String(s)) => s
                .strip_prefix("dpapi:")
                .and_then(|b| B64.decode(b).ok())
                .and_then(|enc| super::dpapi(&enc, false))
                .and_then(|plain| serde_json::from_slice(&plain).ok()),
            t => t,
        })
    }
}

// ---- 保存データ（state.json。Electron 版と同じ形式） ----
#[derive(Serialize, Deserialize, Clone)]
struct Fav {
    #[serde(rename = "type")]
    kind: String, // 'guild' | 'channel' | 'dm'
    name: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Notify {
    toast: bool,
    sound: Value, // false か通知音の id
    #[serde(default = "yes")]
    badge: bool, // 未読をタスクトレイのアイコンに赤い点で出す
}
fn yes() -> bool {
    true
}
fn default_notify() -> Notify {
    Notify { toast: true, sound: "chime".into(), badge: true }
}

// id 'inbox' / 'inbox-dm' / 'inbox-mix' は受信トレイ: ★ を持たず、受け取ったサーバー / DM / 両方の全メッセージを表示する
#[derive(Serialize, Deserialize, Clone)]
struct TrayDef {
    id: String,
    name: String,
    #[serde(default)]
    favorites: IndexMap<String, Fav>,
    #[serde(default = "default_notify")]
    notify: Notify,
}

#[derive(Serialize, Deserialize, Clone)]
struct Dm {
    name: String,
    icon: Option<String>,
    #[serde(default)]
    group: Option<bool>, // グループ DM か。None は未確認（接続時に確かめる）
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct Msg {
    id: String,
    channel_id: String,
    guild_id: Option<String>,
    r#where: String,
    author: String,
    avatar: Option<String>,
    content: String,
    time: String,
    read: bool,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
struct Settings {
    inbox_all: bool, // 受信トレイ（全サーバー）を使う。オフなら ★ したものだけ受信する
    inbox_dm: bool,  // 受信トレイ（全 DM）を使う
    inbox_mix: bool, // サーバーと DM をまとめた受信トレイを使う
    #[serde(skip_serializing)]
    inbox_badge: bool, // 旧「未読数をタブに表示」。読み込み時に受信トレイの Notify.badge へ移す
    group_by: String, // 新着のまとめ方 'server' | 'channel'
    start_minimized: bool, // 自動起動ではトレイに常駐するだけにする
    sound_cooldown: u64,   // 同じチャンネルの通知音を鳴らさない秒数（0 で無効）
    always_on_top: bool,
    tray_badge: bool, // タスクトレイのアイコンにも未読の赤い点を出す（どのトレイを数えるかは Notify.badge）
    client_id: String, // 自分で作った Developer Portal アプリの ID。空なら共有の CLIENT_ID
}
impl Default for Settings {
    fn default() -> Self {
        Settings { inbox_all: false, inbox_dm: false, inbox_mix: false, inbox_badge: true, group_by: "server".into(), start_minimized: true, sound_cooldown: 0, always_on_top: false, tray_badge: true, client_id: String::new() }
    }
}

#[derive(Serialize, Deserialize, Default)]
struct Saved {
    #[serde(default, with = "dpapi_token")]
    token: Option<Value>,
    #[serde(default)]
    trays: Vec<TrayDef>,
    #[serde(default)]
    dms: IndexMap<String, Dm>,
    #[serde(default)]
    messages: Vec<Msg>, // チャンネル・DM ごとに最新 1 件、新しい順
    #[serde(default)]
    settings: Settings,
}

struct ChanInfo {
    name: String,
    guild_id: String,
    guild_name: String,
}

#[derive(Default)]
struct Data {
    saved: Saved,
    status: String,
    me: Option<Value>,
    guilds: Vec<Value>,
    channels: HashMap<String, Vec<Value>>, // guildId -> テキストチャンネル
    chan_info: HashMap<String, ChanInfo>,
    subs: HashSet<String>,
    last_sound: HashMap<String, Instant>, // チャンネルごとに最後に通知音を鳴らした時刻
    autostart: bool,
}

type Pending = HashMap<String, oneshot::Sender<Result<Value, String>>>;

struct Core {
    app: AppHandle,
    path: PathBuf,
    data: Mutex<Data>,
    writer: tokio::sync::Mutex<Option<WriteHalf<NamedPipeClient>>>,
    pending: Mutex<Pending>,
    reconnect: tokio::sync::Notify, // Client ID を変えたとき、今の接続を切ってやり直す
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or("").to_string()
}

fn is_inbox(id: &str) -> bool {
    id == "inbox" || id == "inbox-dm" || id == "inbox-mix"
}

// タスクトレイ用アイコン（Discord と見分けやすい封筒だけの形。未読ありは赤い点付き）
fn tray_icon(unread: bool) -> tauri::image::Image<'static> {
    let png: &[u8] = if unread { include_bytes!("../icons/tray-unread.png") } else { include_bytes!("../icons/tray.png") };
    tauri::image::Image::from_bytes(png).unwrap()
}

fn in_tray(t: &TrayDef, m: &Msg) -> bool {
    match t.id.as_str() {
        "inbox-mix" => true,
        "inbox" => m.guild_id.is_some(),
        "inbox-dm" => m.guild_id.is_none(),
        _ => t.favorites.contains_key(&m.channel_id) || m.guild_id.as_ref().is_some_and(|g| t.favorites.contains_key(g)),
    }
}

// 受信トレイは設定でオンのときだけ使う
fn tray_on(s: &Settings, t: &TrayDef) -> bool {
    match t.id.as_str() {
        "inbox" => s.inbox_all,
        "inbox-dm" => s.inbox_dm,
        "inbox-mix" => s.inbox_mix,
        _ => true,
    }
}

// 受け取ったメッセージを残すか（使っているトレイのどれかに表示されるものだけ）
fn receives(saved: &Saved, m: &Msg) -> bool {
    saved.trays.iter().any(|t| tray_on(&saved.settings, t) && in_tray(t, m))
}

fn avatar_url(u: &Value) -> Option<String> {
    let id = u["id"].as_str()?;
    Some(match u["avatar"].as_str() {
        Some(a) => format!("https://cdn.discordapp.com/avatars/{id}/{a}.png?size=64"),
        None => format!("https://cdn.discordapp.com/embed/avatars/{}.png", (id.parse::<u64>().unwrap_or(0) >> 22) % 6),
    })
}

// DM の相手（履歴の自分以外の発言者）
fn other_author<'a>(ch: &'a Value, me_id: Option<&str>) -> Option<&'a Value> {
    ch["messages"].as_array()?.iter().map(|m| &m["author"]).find(|a| a.is_object() && a["id"].as_str() != me_id)
}

// 本文 + 取得できない内容の説明。RPC のメッセージにはスタンプの情報が無いので、空なら「取得できません」
fn describe(m: &Value) -> String {
    let mut parts: Vec<String> = m["content"].as_str().filter(|c| !c.is_empty()).map(String::from).into_iter().collect();
    for a in m["attachments"].as_array().into_iter().flatten() {
        let ct = a["content_type"].as_str().unwrap_or("");
        let kind = if ct.starts_with("image/") { "画像" } else if ct.starts_with("video/") { "動画" } else { "ファイル" };
        parts.push(format!("[{kind}: {}]", s(&a["filename"])));
    }
    if parts.is_empty() && m["embeds"].as_array().is_some_and(|e| !e.is_empty()) {
        parts.push("[埋め込み]".into());
    }
    if parts.is_empty() { "[スタンプなど（内容を取得できません）]".into() } else { parts.join(" ") }
}

// JS の Date で読める ISO 8601 (UTC)。日付計算は Howard Hinnant の civil_from_days
fn iso(secs: i64) -> String {
    let (days, rem) = (secs.div_euclid(86400), secs.rem_euclid(86400));
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", rem / 3600, rem % 3600 / 60, rem % 60)
}
fn iso_now() -> String {
    iso(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64)
}

// pid のプロセスが Discord 本体か
fn is_discord(pid: u32) -> bool {
    use windows_sys::Win32::{Foundation::CloseHandle, System::Threading::*};
    let mut buf = [0u16; 512];
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(h, 0, buf.as_mut_ptr(), &mut len);
        CloseHandle(h);
        let exe = String::from_utf16_lossy(&buf[..len as usize]).to_lowercase();
        ok != 0 && ["\\discord.exe", "\\discordptb.exe", "\\discordcanary.exe"].iter().any(|x| exe.ends_with(x))
    }
}

// パイプの相手が Discord 本体か（同じ名前のパイプを先に作った別のプロセスにトークンを渡さない）
fn pipe_is_discord(p: &NamedPipeClient) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Pipes::GetNamedPipeServerProcessId;
    let mut pid = 0;
    unsafe { GetNamedPipeServerProcessId(p.as_raw_handle(), &mut pid) != 0 && is_discord(pid) }
}

// 最前面のウィンドウが Discord ならそのタイトル（"#チャンネル | サーバー - Discord" / "@相手 - Discord"）
fn discord_title() -> Option<String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let mut buf = [0u16; 512];
    unsafe {
        let w = GetForegroundWindow();
        let mut pid = 0;
        GetWindowThreadProcessId(w, &mut pid);
        if !is_discord(pid) {
            return None;
        }
        let n = GetWindowTextW(w, buf.as_mut_ptr(), buf.len() as i32);
        Some(String::from_utf16_lossy(&buf[..n.max(0) as usize]))
    }
}

// Discord のタイトルがこのチャンネル / DM を開いているものか
fn title_shows(t: &str, ch: Option<&ChanInfo>, dm: Option<&Dm>) -> bool {
    match (ch, dm) {
        (Some(c), _) => t.contains(&format!("#{} ", c.name)) && t.contains(&c.guild_name),
        (None, Some(d)) => t.contains(&format!("@{} ", d.name)),
        _ => false,
    }
}

// 最後に鳴らしてから secs 秒経っていれば鳴らしてよい（時刻を記録する）
fn sound_ok(last: &mut HashMap<String, Instant>, cid: &str, secs: u64, now: Instant) -> bool {
    if last.get(cid).is_some_and(|t| now.duration_since(*t) < Duration::from_secs(secs)) {
        return false;
    }
    last.insert(cid.into(), now);
    true
}

// 自動起動の調査用: startup.log に日時 (UTC) 付きで追記
fn log_to(dir: &std::path::Path, msg: &str) {
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("startup.log")) {
        let _ = writeln!(f, "{} {msg}", iso_now());
    }
}

fn show(app: &AppHandle) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.unminimize();
        let _ = w.show();
        let _ = w.set_focus();
    }
}

impl Core {
    fn lock(&self) -> MutexGuard<'_, Data> {
        self.data.lock().unwrap()
    }

    fn log(&self, msg: &str) {
        log_to(self.path.parent().unwrap(), msg);
    }

    fn save(&self, d: &Data) {
        let _ = std::fs::write(&self.path, serde_json::to_string(&d.saved).unwrap());
    }

    fn view(&self) -> Value {
        let d = self.lock();
        let autostart = d.autostart;
        json!({
            "status": d.status, "guilds": d.guilds, "channels": d.channels, "dms": d.saved.dms,
            "trays": d.saved.trays, "messages": d.saved.messages, "settings": d.saved.settings, "autostart": autostart,
            "portable": cfg!(feature = "portable"),
        })
    }

    fn update(&self) {
        let unread = {
            let d = self.lock();
            d.saved.messages.iter().filter(|m| !m.read && receives(&d.saved, m)).count()
        };
        let badge = {
            let d = self.lock();
            let s = &d.saved;
            s.settings.tray_badge
                && s.messages.iter().any(|m| !m.read && s.trays.iter().any(|t| t.notify.badge && tray_on(&s.settings, t) && in_tray(t, m)))
        };
        let v = self.view();
        if let Some(t) = self.app.tray_by_id("main") {
            let _ = t.set_tooltip(Some(format!("DashTray — 未読 {unread}")));
            let _ = t.set_icon(Some(tray_icon(badge)));
        }
        let _ = self.app.emit("update", v);
    }

    fn set_status(&self, st: impl Into<String>) {
        self.lock().status = st.into();
        self.update();
    }

    // Discord 本体で開いているチャンネル / DM の未読を既読にする（未読が無ければタイトルも見ない）
    fn mark_viewed(&self) {
        {
            let mut d = self.lock();
            if d.saved.messages.iter().all(|m| m.read) {
                return;
            }
            let Some(t) = discord_title() else { return };
            let Data { saved, chan_info, .. } = &mut *d;
            let mut hit = false;
            for m in saved.messages.iter_mut().filter(|m| !m.read) {
                if title_shows(&t, chan_info.get(&m.channel_id), saved.dms.get(&m.channel_id)) {
                    m.read = true;
                    hit = true;
                }
            }
            if !hit {
                return;
            }
            self.save(&d);
        }
        self.update();
    }

    fn client_id(&self) -> String {
        let id = self.lock().saved.settings.client_id.clone();
        if id.is_empty() { CLIENT_ID.into() } else { id }
    }

    fn me_id(&self) -> Option<String> {
        self.lock().me.as_ref().map(|m| s(&m["id"]))
    }

    fn dm_icon(&self, ch: Option<&Value>) -> Option<String> {
        avatar_url(other_author(ch?, self.me_id().as_deref())?)
    }

    // ---- Discord RPC (local IPC) ----
    // frame: int32LE op, int32LE length, JSON. op 0=HANDSHAKE 1=FRAME 2=CLOSE 3=PING 4=PONG
    async fn send(&self, op: i32, v: &Value) -> Result<(), String> {
        let data = serde_json::to_vec(v).unwrap();
        let mut frame = Vec::with_capacity(8 + data.len());
        frame.extend(op.to_le_bytes());
        frame.extend((data.len() as i32).to_le_bytes());
        frame.extend(data);
        let mut w = self.writer.lock().await;
        let w = w.as_mut().ok_or("Discord に未接続")?;
        w.write_all(&frame).await.map_err(|e| e.to_string())
    }

    async fn cmd(&self, name: &str, args: Value, evt: Option<&str>) -> Result<Value, String> {
        let nonce = uuid::Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(nonce.clone(), tx);
        let mut msg = json!({ "cmd": name, "args": args, "nonce": nonce });
        if let Some(e) = evt {
            msg["evt"] = e.into();
        }
        if let Err(e) = self.send(1, &msg).await {
            self.pending.lock().unwrap().remove(&nonce);
            return Err(e);
        }
        rx.await.map_err(|_| "切断されました".to_string())?
    }

    // 切断まで読み続ける。Discord に拒否されたときはその理由を返す
    async fn read_loop(self: &Arc<Self>, mut r: ReadHalf<NamedPipeClient>) -> Option<String> {
        loop {
            let mut head = [0u8; 8];
            r.read_exact(&mut head).await.ok()?;
            let op = i32::from_le_bytes(head[..4].try_into().unwrap());
            let len = u32::from_le_bytes(head[4..].try_into().unwrap()) as usize;
            if len > MAX_FRAME {
                return Some(format!("Discord から大きすぎるデータ ({len} バイト) — 切断"));
            }
            let mut body = vec![0; len];
            r.read_exact(&mut body).await.ok()?;
            let msg: Value = serde_json::from_slice(&body).unwrap_or_default();
            match op {
                3 => {
                    let _ = self.send(4, &msg).await;
                }
                2 => return Some(format!("Discord に拒否されました: {}", s(&msg["message"]))),
                _ => {
                    let tx = self.pending.lock().unwrap().remove(msg["nonce"].as_str().unwrap_or(""));
                    if let Some(tx) = tx {
                        let r = if msg["evt"] == "ERROR" { Err(s(&msg["data"]["message"])) } else { Ok(msg["data"].clone()) };
                        let _ = tx.send(r);
                    } else if msg["cmd"] == "DISPATCH" {
                        let (me, evt, data) = (self.clone(), s(&msg["evt"]), msg["data"].clone());
                        tauri::async_runtime::spawn(async move { me.on_event(&evt, data).await });
                    }
                }
            }
        }
    }

    async fn run(self: Arc<Self>) {
        loop {
            let pipe = (0..10).find_map(|i| ClientOptions::new().open(format!(r"\\?\pipe\discord-ipc-{i}")).ok().filter(pipe_is_discord));
            let Some(pipe) = pipe else {
                self.set_status("Discord が起動していません — 再接続待ち");
                tokio::time::sleep(Duration::from_secs(5)).await;
                continue;
            };
            self.log("Discord に接続");
            let (r, w) = tokio::io::split(pipe);
            *self.writer.lock().await = Some(w);
            let reason = match self.send(0, &json!({ "v": 1, "client_id": self.client_id() })).await {
                Ok(()) => tokio::select! {
                    r = self.read_loop(r) => r,
                    _ = self.reconnect.notified() => Some("Client ID を変更 — 再接続".into()),
                },
                Err(e) => Some(e),
            };
            *self.writer.lock().await = None;
            self.lock().subs.clear();
            for (_, tx) in self.pending.lock().unwrap().drain() {
                let _ = tx.send(Err("切断されました".into()));
            }
            let reason = reason.unwrap_or_else(|| "切断 — 再接続待ち".into());
            self.log(&reason);
            self.set_status(reason);
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
    }

    async fn fetch_token(&self, params: &[(&str, &str)]) -> Result<(), String> {
        let id = self.client_id();
        let mut form = vec![("client_id", id.as_str())];
        form.extend_from_slice(params);
        let r = reqwest::Client::new()
            .post("https://discord.com/api/oauth2/token")
            .form(&form)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        let st = r.status();
        if !st.is_success() {
            return Err(format!("トークン取得失敗 {} {}", st.as_u16(), r.text().await.unwrap_or_default()));
        }
        let tok: Value = r.json().await.map_err(|e| e.to_string())?;
        let mut d = self.lock();
        d.saved.token = Some(tok);
        self.save(&d);
        Ok(())
    }

    async fn authenticate(&self) -> Result<Value, String> {
        let at = self.lock().saved.token.as_ref().map(|t| s(&t["access_token"])).unwrap_or_default();
        self.cmd("AUTHENTICATE", json!({ "access_token": at }), None).await
    }

    async fn auth(&self) -> Result<Value, String> {
        let refresh = self.lock().saved.token.as_ref().map(|t| s(&t["refresh_token"]));
        if let Some(rt) = refresh {
            if let Ok(v) = self.authenticate().await {
                return Ok(v);
            }
            if self.fetch_token(&[("grant_type", "refresh_token"), ("refresh_token", &rt)]).await.is_ok() {
                if let Ok(v) = self.authenticate().await {
                    return Ok(v);
                }
            }
        }
        self.set_status("Discord の画面で許可してください");
        // PKCE: verifier は uuid v4 ×2（約 244 ビットの乱数）、challenge はその SHA-256 を base64url
        let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let args = json!({ "client_id": self.client_id(), "scopes": SCOPES, "code_challenge": pkce_challenge(&verifier), "code_challenge_method": "S256" });
        let r = self.cmd("AUTHORIZE", args, None).await?;
        let code = s(&r["code"]);
        self.fetch_token(&[("grant_type", "authorization_code"), ("code", &code), ("redirect_uri", REDIRECT_URI), ("code_verifier", &verifier)])
            .await?;
        self.authenticate().await
    }

    async fn start(self: &Arc<Self>) {
        self.log("READY");
        if let Err(e) = self.try_start().await {
            self.log(&format!("起動エラー: {e}"));
            self.set_status(format!("エラー: {e}"));
        }
    }

    async fn try_start(self: &Arc<Self>) -> Result<(), String> {
        let me = self.auth().await?["user"].clone();
        // ログオン直後は Discord 本体が読み込み中でサーバー一覧が空で返るので、届くまで待つ（最大 1 分）
        let mut guilds = vec![];
        for i in 0..12 {
            guilds = self.cmd("GET_GUILDS", json!({}), None).await?["guilds"].as_array().cloned().unwrap_or_default();
            if !guilds.is_empty() {
                break;
            }
            self.log(&format!("サーバー一覧が空 ({}回目) — 待機", i + 1));
            self.set_status("Discord の準備待ち");
            tokio::time::sleep(Duration::from_secs(5)).await;
        }
        self.log(&format!("サーバー {} 件", guilds.len()));
        {
            let mut d = self.lock();
            d.me = Some(me.clone());
            d.guilds = guilds;
        }
        self.cmd("SUBSCRIBE", json!({}), Some("NOTIFICATION_CREATE")).await?;
        self.sync_subs().await;
        self.log(&format!("購読 {} チャンネル", self.lock().subs.len()));
        let me2 = self.clone();
        tauri::async_runtime::spawn(async move { me2.catch_up().await });
        let missing: Vec<String> = self.lock().saved.dms.iter().filter(|(_, d)| d.icon.is_none() || d.group.is_none()).map(|(id, _)| id.clone()).collect();
        for id in missing {
            let ch = self.cmd("GET_CHANNEL", json!({ "channel_id": id }), None).await.ok();
            let icon = self.dm_icon(ch.as_ref());
            if let Some(dm) = self.lock().saved.dms.get_mut(&id) {
                dm.icon = dm.icon.take().or(icon);
                dm.group = dm.group.or(ch.as_ref().map(is_group));
            }
        }
        {
            let d = self.lock();
            self.save(&d);
        }
        self.set_status(format!("接続中 ({})", s(&me["username"])));
        Ok(())
    }

    async fn load_channels(&self, gid: &str) -> Result<Vec<Value>, String> {
        if let Some(c) = self.lock().channels.get(gid) {
            return Ok(c.clone());
        }
        let res = self.cmd("GET_CHANNELS", json!({ "guild_id": gid }), None).await?;
        let list: Vec<Value> = res["channels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| c["type"].as_i64().is_some_and(|t| TEXT_TYPES.contains(&t)))
            .cloned()
            .collect();
        let mut d = self.lock();
        let guild_name = d.guilds.iter().find(|g| g["id"] == gid).map(|g| s(&g["name"])).unwrap_or_default();
        for c in &list {
            d.chan_info.insert(s(&c["id"]), ChanInfo { name: s(&c["name"]), guild_id: gid.into(), guild_name: guild_name.clone() });
        }
        d.channels.insert(gid.into(), list.clone());
        Ok(list)
    }

    // MESSAGE_CREATE を購読する。受信（サーバー）がオンなら全サーバーの全テキストチャンネル、
    // 受信（DM）がオンなら既知の DM 全部。オフの側は ★ したサーバーのチャンネル・チャンネル・DM だけ
    async fn sync_subs(&self) {
        let (all, favs, mut want, guild_ids) = {
            let d = self.lock();
            let favs: HashSet<String> = d.saved.trays.iter().flat_map(|t| t.favorites.keys().cloned()).collect();
            let all = d.saved.settings.inbox_all || d.saved.settings.inbox_mix;
            let all_dm = d.saved.settings.inbox_dm || d.saved.settings.inbox_mix;
            let dms: HashSet<String> = d.saved.dms.keys().filter(|id| all_dm || favs.contains(*id)).cloned().collect();
            (all, favs, dms, d.guilds.iter().map(|g| s(&g["id"])).collect::<Vec<_>>())
        };
        for g in guild_ids {
            for c in self.load_channels(&g).await.unwrap_or_default() {
                let id = s(&c["id"]);
                if all || favs.contains(&g) || favs.contains(&id) {
                    want.insert(id);
                }
            }
        }
        for id in &want {
            if self.lock().subs.contains(id) {
                continue;
            }
            match self.cmd("SUBSCRIBE", json!({ "channel_id": id }), Some("MESSAGE_CREATE")).await {
                Ok(_) => {
                    self.lock().subs.insert(id.clone());
                }
                Err(e) => eprintln!("SUBSCRIBE {id} {e}"),
            }
        }
        let extra: Vec<String> = self.lock().subs.iter().filter(|x| !want.contains(*x)).cloned().collect();
        for id in extra {
            let _ = self.cmd("UNSUBSCRIBE", json!({ "channel_id": id }), Some("MESSAGE_CREATE")).await;
            self.lock().subs.remove(&id);
        }
    }

    async fn on_event(self: Arc<Self>, evt: &str, data: Value) {
        let cid = s(&data["channel_id"]);
        match evt {
            "READY" => self.start().await,
            "MESSAGE_CREATE" => {
                let (place, gid) = self.place(&cid);
                self.add_message(&cid, place, gid, &data["message"], true);
            }
            "NOTIFICATION_CREATE" => {
                if self.lock().subs.contains(&cid) {
                    return;
                }
                // DM 一覧は RPC で取れないので、通知で届いた DM から一覧を作る
                let ch = self.cmd("GET_CHANNEL", json!({ "channel_id": cid }), None).await.ok();
                if ch.as_ref().is_some_and(|c| c["guild_id"].is_string()) {
                    return; // サーバーのメッセージは MESSAGE_CREATE で受け取る
                }
                let icon = self.dm_icon(ch.as_ref()).or_else(|| data["icon_url"].as_str().map(String::from));
                let (name, added) = {
                    let mut d = self.lock();
                    let fav = d.saved.trays.iter().find_map(|t| t.favorites.get(&cid)).map(|f| f.name.clone());
                    let name = [fav, ch.as_ref().map(|c| s(&c["name"])), data["title"].as_str().map(String::from)]
                        .into_iter()
                        .flatten()
                        .find(|x| !x.is_empty())
                        .unwrap_or_else(|| cid.clone());
                    let added = !d.saved.dms.contains_key(&cid);
                    if added {
                        d.saved.dms.insert(cid.clone(), Dm { name: name.clone(), icon, group: ch.as_ref().map(is_group) });
                        self.save(&d);
                    }
                    (name, added)
                };
                if added {
                    self.update();
                    let me = self.clone();
                    tauri::async_runtime::spawn(async move { me.sync_subs().await });
                }
                self.add_message(&cid, format!("DM {name}"), None, &data["message"], true);
            }
            _ => {}
        }
    }

    fn place(&self, cid: &str) -> (String, Option<String>) {
        let d = self.lock();
        match (d.chan_info.get(cid), d.saved.dms.get(cid)) {
            (Some(c), _) => (format!("{} #{}", c.guild_name, c.name), Some(c.guild_id.clone())),
            (None, Some(dm)) => (format!("DM {}", dm.name), None),
            _ => (cid.into(), None),
        }
    }

    // 起動していなかった間のメッセージを取り込む。保存済みで一番新しいメッセージより後のものだけ（ID は時刻順）。
    // トーストは出さず、音はタブの並びで先にあるトレイのものを 1 回だけ鳴らす
    async fn catch_up(&self) {
        let (since, ids) = {
            let d = self.lock();
            let since = d.saved.messages.iter().filter_map(|m| m.id.parse::<u64>().ok()).max();
            // ★ した DM・チャンネル・サーバー → その他の DM → 受信したことのあるチャンネル → その他 の順
            let favs: HashSet<&String> = d.saved.trays.iter().flat_map(|t| t.favorites.keys()).collect();
            let rank = |cid: &String| {
                let gid = d.chan_info.get(cid).map(|c| &c.guild_id);
                if favs.contains(cid) || gid.is_some_and(|g| favs.contains(g)) {
                    0
                } else if d.saved.dms.contains_key(cid) {
                    1
                } else if d.saved.messages.iter().any(|m| &m.channel_id == cid) {
                    2
                } else if gid.is_some_and(|g| d.saved.messages.iter().any(|m| m.guild_id.as_ref() == Some(g))) {
                    3 // 受信したことのあるサーバーの他のチャンネル
                } else {
                    4
                }
            };
            let mut ids: Vec<String> = d.subs.iter().cloned().collect();
            ids.sort_by_key(|c| rank(c));
            (since, ids)
        };
        let Some(since) = since else { return }; // 初回は過去の発言を拾わない
        let mut best: Option<(usize, String)> = None;
        let (total, mut added, mut skipped) = (ids.len(), 0, 0);
        for cid in ids {
            // 起動後にリアルタイムで受信済みのチャンネルは取り込まなくてよい
            if self.lock().saved.messages.iter().any(|m| m.channel_id == cid && m.id.parse::<u64>().unwrap_or(0) > since) {
                skipped += 1;
                continue;
            }
            // GET_CHANNEL のたびに Discord 本体がメッセージを読み込み直す。立て続けに送ると読み込みに失敗して
            // 「メッセージを読み込めませんでした」が出るので間を空ける。1 秒だと 900 チャンネルで約 15 分かかり
            // 起動直後に新着が出ないので 0.2 秒（約 3 分）。0.1 秒間隔でも取得結果は 1 秒と同じだった
            // ponytail: 間隔は固定。Discord 側でエラーが出るようなら 500ms などに戻す
            tokio::time::sleep(Duration::from_millis(CATCH_UP_GAP_MS)).await;
            let ch = match self.cmd("GET_CHANNEL", json!({ "channel_id": cid }), None).await {
                Ok(ch) => ch,
                Err(_) if self.writer.lock().await.is_none() => return, // 切断。再接続後の READY でやり直す
                Err(_) => continue,
            };
            let Some(m) = ch["messages"].as_array().and_then(|a| a.iter().max_by_key(|m| s(&m["id"]).parse::<u64>().unwrap_or(0))) else { continue };
            if s(&m["id"]).parse::<u64>().unwrap_or(0) <= since {
                continue;
            }
            let (place, gid) = self.place(&cid);
            added += 1;
            if let Some(hit) = self.add_message(&cid, place, gid, m, false) {
                if best.as_ref().is_none_or(|b| hit.0 < b.0) {
                    best = Some(hit);
                }
            }
        }
        self.log(&format!("取り込み: {total} チャンネル中 新着 {added} 件、受信済みで省略 {skipped} 件"));
        self.notify(String::new(), String::new(), false, best.map(|b| b.1));
    }

    // live = false（取り込み）なら通知せず、鳴らすはずだった音を（トレイの位置, 音）で返す
    fn add_message(&self, cid: &str, place: String, guild_id: Option<String>, m: &Value, live: bool) -> Option<(usize, String)> {
        let (title, body, toast, sound) = {
            let mut d = self.lock();
            let me_id = d.me.as_ref().map(|x| s(&x["id"]));
            let id = s(&m["id"]);
            let a = &m["author"];
            if !m.is_object() || (me_id.is_some() && a["id"].as_str() == me_id.as_deref()) || d.saved.messages.iter().any(|x| x.id == id) {
                return None;
            }
            let author = [&m["nick"], &a["global_name"], &a["username"]]
                .iter()
                .find_map(|v| v.as_str().filter(|x| !x.is_empty()))
                .unwrap_or("?")
                .to_string();
            let mut item = Msg {
                id,
                channel_id: cid.into(),
                guild_id,
                r#where: place.clone(),
                author: author.clone(),
                avatar: avatar_url(a),
                content: describe(m),
                time: m["timestamp"].as_str().filter(|x| !x.is_empty()).map_or_else(iso_now, String::from),
                read: false,
            };
            if !receives(&d.saved, &item) {
                return None;
            }
            // Discord でこのチャンネルを開いて見ているなら既読にして通知しない
            let viewing = discord_title().is_some_and(|t| title_shows(&t, d.chan_info.get(cid), d.saved.dms.get(cid)));
            item.read = viewing;
            d.saved.messages.retain(|x| x.channel_id != cid);
            // 新しい順を保つ（取り込みでは受信済みより古いものも来る）
            let n: u64 = item.id.parse().unwrap_or(0);
            let at = d.saved.messages.iter().position(|x| x.id.parse::<u64>().unwrap_or(0) < n).unwrap_or(d.saved.messages.len());
            d.saved.messages.insert(at, item.clone());
            self.save(&d);
            // このメッセージが表示されるトレイのどれかで有効なら 1 回だけ通知。音はタブの並びで先にあるトレイのもの
            let hits: Vec<(usize, &TrayDef)> =
                d.saved.trays.iter().enumerate().filter(|(_, t)| !viewing && tray_on(&d.saved.settings, t) && in_tray(t, &item)).collect();
            let toast = hits.iter().any(|(_, t)| t.notify.toast);
            let sound = hits.iter().find_map(|(i, t)| t.notify.sound.as_str().filter(|x| !x.is_empty()).map(|x| (*i, x.to_string())));
            if !live {
                drop(d);
                self.update();
                return sound;
            }
            let cd = d.saved.settings.sound_cooldown;
            let sound = sound.map(|x| x.1).filter(|_| sound_ok(&mut d.last_sound, cid, cd, Instant::now()));
            (format!("{author} — {place}"), item.content, toast, sound)
        };
        self.update();
        self.notify(title, body, toast, sound);
        None
    }

    fn notify(&self, title: String, body: String, toast: bool, sound: Option<String>) {
        // ポータブル版はインストールで登録されるアプリ ID が無くトーストを出せないので音だけ
        if toast && !cfg!(feature = "portable") {
            let app = self.app.clone();
            let body: String = body.chars().take(200).collect();
            std::thread::spawn(move || {
                // 開発中はインストールされていないので PowerShell の名前で出す
                let aumid = if cfg!(debug_assertions) { Toast::POWERSHELL_APP_ID.to_string() } else { app.config().identifier.clone() };
                let a = app.clone();
                let _ = Toast::new(&aumid)
                    .title(&title)
                    .text1(&body)
                    .sound(None) // 音はアプリ側で鳴らす
                    .on_activated(move |_| {
                        show(&a);
                        Ok(())
                    })
                    .show();
            });
        }
        if let Some(snd) = sound {
            let _ = self.app.emit("sound", snd);
        }
    }
}

// ---- 画面からの呼び出し ----
type S<'a> = State<'a, Arc<Core>>;

#[tauri::command]
fn get(c: S) -> Value {
    c.view()
}

#[tauri::command]
async fn load_channels(c: S<'_>, guild_id: String) -> Result<(), String> {
    c.load_channels(&guild_id).await?;
    c.update();
    Ok(())
}

#[tauri::command]
fn set_fav(c: S, tray_id: String, id: String, fav: bool) {
    {
        let mut g = c.lock();
        let d = &mut *g;
        let entry = if let Some(x) = d.guilds.iter().find(|x| x["id"] == id.as_str()) {
            Some(Fav { kind: "guild".into(), name: s(&x["name"]) })
        } else if let Some(ci) = d.chan_info.get(&id) {
            Some(Fav { kind: "channel".into(), name: format!("{} #{}", ci.guild_name, ci.name) })
        } else {
            d.saved.dms.get(&id).map(|dm| Fav { kind: "dm".into(), name: dm.name.clone() })
        };
        let Some(t) = d.saved.trays.iter_mut().find(|t| t.id == tray_id) else { return };
        if !fav {
            t.favorites.shift_remove(&id);
        } else if let Some(f) = entry {
            t.favorites.insert(id, f);
        }
        c.save(d);
    }
    c.update();
    resync(&c);
}

fn resync(c: &Arc<Core>) {
    let c = c.clone();
    tauri::async_runtime::spawn(async move { c.sync_subs().await });
}

#[tauri::command]
async fn refresh(c: S<'_>) -> Result<(), String> {
    let prev = c.lock().status.clone();
    c.set_status("更新中…");
    let r: Result<(), String> = async {
        let guilds = c.cmd("GET_GUILDS", json!({}), None).await?["guilds"].as_array().cloned().unwrap_or_default();
        let loaded: Vec<String> = {
            let mut d = c.lock();
            d.guilds = guilds;
            d.channels.drain().map(|(k, _)| k).collect()
        };
        for id in loaded {
            let _ = c.load_channels(&id).await;
        }
        c.sync_subs().await;
        Ok(())
    }
    .await;
    c.set_status(r.map_or_else(|e| format!("エラー: {e}"), |()| prev));
    Ok(())
}

// Discord のチャンネル種別 3 = GROUP_DM
fn is_group(ch: &Value) -> bool {
    ch["type"].as_i64() == Some(3)
}

// チャンネル ID を指定して DM を一覧に足す。失敗時は { error } を返す
#[tauri::command]
async fn add_dm(c: S<'_>, id: String) -> Result<Value, String> {
    let err = |m: String| -> Result<Value, String> { Ok(json!({ "error": m })) };
    let id = id.trim().to_string();
    if !(15..=21).contains(&id.len()) || !id.bytes().all(|b| b.is_ascii_digit()) {
        return err("チャンネル ID は数字で入力してください".into());
    }
    if c.lock().saved.dms.contains_key(&id) {
        return err("すでに一覧にあります".into());
    }
    let ch = match c.cmd("GET_CHANNEL", json!({ "channel_id": id }), None).await {
        Ok(ch) => ch,
        Err(e) => return err(format!("取得できませんでした: {e}")),
    };
    if ch["guild_id"].is_string() {
        return err("サーバーのチャンネルです。DM のチャンネル ID を入力してください".into());
    }
    let me_id = c.me_id();
    let other = other_author(&ch, me_id.as_deref()).cloned().unwrap_or_default();
    let name = [&ch["name"], &other["global_name"], &other["username"]]
        .iter()
        .find_map(|v| v.as_str().filter(|x| !x.is_empty()))
        .map_or_else(|| id.clone(), String::from);
    let icon = c.dm_icon(Some(&ch));
    {
        let mut d = c.lock();
        d.saved.dms.insert(id, Dm { name, icon, group: Some(is_group(&ch)) });
        c.save(&d);
    }
    c.update();
    c.sync_subs().await;
    Ok(json!({}))
}

// read: false で未読に戻す
#[tauri::command]
fn mark_read(c: S, id: String, read: bool) {
    {
        let mut d = c.lock();
        let Some(m) = d.saved.messages.iter_mut().find(|x| x.id == id) else { return };
        m.read = read;
        c.save(&d);
    }
    c.update();
}

// サーバー / チャンネル / DM の新着をまとめて既読にする
#[tauri::command]
fn mark_read_in(c: S, id: String) {
    {
        let mut d = c.lock();
        for m in d.saved.messages.iter_mut().filter(|m| m.channel_id == id || m.guild_id.as_deref() == Some(&id)) {
            m.read = true;
        }
        c.save(&d);
    }
    c.update();
}

// DM を一覧・全トレイの ★・新着から消す（また通知が届けば一覧に戻る）
#[tauri::command]
fn remove_dm(c: S, id: String) {
    {
        let mut d = c.lock();
        d.saved.dms.shift_remove(&id);
        for t in &mut d.saved.trays {
            t.favorites.shift_remove(&id);
        }
        d.saved.messages.retain(|m| m.channel_id != id);
        c.save(&d);
    }
    c.update();
    resync(&c);
}

// Discord アプリで開く。開けるのは discord:// のチャンネルのリンクだけ
#[tauri::command]
fn open_url(c: S, url: String) {
    if url.starts_with("discord://-/channels/") {
        let _ = c.app.opener().open_url(url, None::<&str>);
    }
}

#[tauri::command]
fn mark_all_read(c: S, tray_id: String) {
    {
        let mut g = c.lock();
        let d = &mut *g;
        if let Some(t) = d.saved.trays.iter().find(|x| x.id == tray_id) {
            for m in d.saved.messages.iter_mut().filter(|m| in_tray(t, m)) {
                m.read = true;
            }
        }
        c.save(d);
    }
    c.update();
}

#[tauri::command]
fn set_settings(c: S, patch: Value) {
    {
        let mut d = c.lock();
        if let Some(g) = patch["groupBy"].as_str().filter(|g| ["server", "channel"].contains(g)) {
            d.saved.settings.group_by = g.into();
        }
        if let Some(b) = patch["inboxAll"].as_bool() {
            d.saved.settings.inbox_all = b;
        }
        if let Some(b) = patch["inboxDm"].as_bool() {
            d.saved.settings.inbox_dm = b;
        }
        if let Some(b) = patch["inboxMix"].as_bool() {
            d.saved.settings.inbox_mix = b;
        }
        if let Some(b) = patch["startMinimized"].as_bool() {
            d.saved.settings.start_minimized = b;
        }
        if let Some(n) = patch["soundCooldown"].as_u64() {
            d.saved.settings.sound_cooldown = n;
        }
        if let Some(b) = patch["trayBadge"].as_bool() {
            d.saved.settings.tray_badge = b;
        }
        if let Some(b) = patch["alwaysOnTop"].as_bool() {
            d.saved.settings.always_on_top = b;
            if let Some(w) = c.app.get_webview_window("main") {
                let _ = w.set_always_on_top(b);
            }
        }
        c.save(&d);
    }
    c.update();
    resync(&c);
}

// 空なら共有の ID に戻す。トークンは元の ID のアプリのものなので捨て、Discord で許可し直す
#[tauri::command]
fn set_client_id(c: S, id: String) -> Result<(), String> {
    let id = id.trim().to_string();
    if !id.chars().all(|ch| ch.is_ascii_digit()) {
        return Err("Client ID は数字だけです".into());
    }
    {
        let mut d = c.lock();
        d.saved.settings.client_id = id; // 同じ ID でも、Portal の設定を直した後のやり直しに使えるよう接続し直す
        d.saved.token = None;
        c.save(&d);
    }
    c.reconnect.notify_waiters();
    c.update();
    Ok(())
}

#[tauri::command]
fn set_tray_notify(c: S, id: String, patch: Value) {
    {
        let mut d = c.lock();
        let Some(t) = d.saved.trays.iter_mut().find(|x| x.id == id) else { return };
        if let Some(b) = patch["toast"].as_bool() {
            t.notify.toast = b;
        }
        if let Some(b) = patch["badge"].as_bool() {
            t.notify.badge = b;
        }
        if !patch["sound"].is_null() {
            t.notify.sound = patch["sound"].clone();
        }
        c.save(&d);
    }
    c.update();
}

#[tauri::command]
fn set_autostart(c: S, on: bool) -> Result<(), String> {
    if on { autostart_enable() } else { autostart_disable() }?;
    c.lock().autostart = on;
    c.update();
    Ok(())
}

#[tauri::command]
fn test_notify(c: S) {
    c.notify("DashTray".into(), "通知のテストです".into(), true, Some("chime".into()));
}

#[tauri::command]
fn add_tray(c: S, name: String) -> String {
    let id = uuid::Uuid::new_v4().to_string();
    {
        let mut d = c.lock();
        d.saved.trays.push(TrayDef { id: id.clone(), name, favorites: IndexMap::new(), notify: default_notify() });
        c.save(&d);
    }
    c.update();
    id
}

#[tauri::command]
fn duplicate_tray(c: S, id: String) -> Option<String> {
    let new_id = {
        let mut d = c.lock();
        let i = d.saved.trays.iter().position(|t| t.id == id)?;
        let mut copy = d.saved.trays[i].clone();
        copy.id = uuid::Uuid::new_v4().to_string();
        copy.name = format!("{} のコピー", copy.name);
        let new_id = copy.id.clone();
        d.saved.trays.insert(i + 1, copy);
        c.save(&d);
        new_id
    };
    c.update();
    Some(new_id)
}

#[tauri::command]
fn reorder_trays(c: S, ids: Vec<String>) {
    {
        let mut d = c.lock();
        d.saved.trays.sort_by_key(|t| ids.iter().position(|x| *x == t.id));
        c.save(&d);
    }
    c.update();
}

#[tauri::command]
fn rename_tray(c: S, id: String, name: String) {
    {
        let mut d = c.lock();
        let Some(t) = d.saved.trays.iter_mut().find(|x| x.id == id) else { return };
        if name.is_empty() {
            return;
        }
        t.name = name;
        c.save(&d);
    }
    c.update();
}

#[tauri::command]
fn delete_tray(c: S, id: String) {
    if is_inbox(&id) {
        return;
    }
    {
        let mut d = c.lock();
        // 受信トレイ以外が無くなると ★ を付ける場所が無くなるので最後の 1 つは消さない
        if d.saved.trays.iter().filter(|t| !is_inbox(&t.id)).count() <= 1 {
            return;
        }
        d.saved.trays.retain(|t| t.id != id);
        c.save(&d);
    }
    c.update();
}

#[tauri::command]
fn open(c: S, id: String) {
    let url = c.lock().saved.messages.iter().find(|m| m.id == id).map(|m| {
        format!("discord://-/channels/{}/{}/{}", m.guild_id.as_deref().unwrap_or("@me"), m.channel_id, m.id)
    });
    if let Some(u) = url {
        let _ = c.app.opener().open_url(u, None::<&str>);
    }
}

// ---- 自動起動 ----
// Run キーは A: ドライブの exe だとログオン時に実行されなかったので、タスクスケジューラ（自分のログオン時）に登録する
const TASK: &str = "DashTray";
const OLD_TASK: &str = "Discord Mailer"; // 改名前のタスク名・Run キー名
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

fn quiet_cmd(exe: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    std::process::Command::new(exe).args(args).creation_flags(0x0800_0000).output() // CREATE_NO_WINDOW
}

fn ok(r: std::io::Result<std::process::Output>) -> Result<(), String> {
    let o = r.map_err(|e| e.to_string())?;
    if o.status.success() { Ok(()) } else { Err(String::from_utf8_lossy(&o.stderr).trim().to_string()) }
}

fn xml_escape(t: &str) -> String {
    t.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn autostart_enabled() -> bool {
    quiet_cmd("schtasks", &["/Query", "/TN", TASK]).is_ok_and(|o| o.status.success())
}

fn autostart_enable() -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let user = std::env::var("USERDOMAIN").unwrap_or_default() + "\\" + &std::env::var("USERNAME").unwrap_or_default();
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{u}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{u}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>7</Priority>
  </Settings>
  <Actions Context="Author"><Exec><Command>{e}</Command><Arguments>--hidden</Arguments></Exec></Actions>
</Task>"#,
        u = xml_escape(&user),
        e = xml_escape(&exe.to_string_lossy())
    );
    // schtasks の XML は UTF-16 (BOM 付き)
    let bytes: Vec<u8> = std::iter::once(0xFEFF).chain(xml.encode_utf16()).flat_map(|c: u16| c.to_le_bytes()).collect();
    let path = std::env::temp_dir().join("discordmailer-task.xml");
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    let r = ok(quiet_cmd("schtasks", &["/Create", "/F", "/TN", TASK, "/XML", &path.to_string_lossy()]));
    let _ = std::fs::remove_file(&path);
    r
}

fn autostart_disable() -> Result<(), String> {
    if autostart_enabled() { ok(quiet_cmd("schtasks", &["/Delete", "/F", "/TN", TASK])) } else { Ok(()) }
}

// 旧版が Run キーや改名前のタスク名で登録した自動起動を、今の exe を指す新しいタスクに移す
fn migrate_run_key() {
    if quiet_cmd("reg", &["query", RUN_KEY, "/v", OLD_TASK]).is_ok_and(|o| o.status.success()) && autostart_enable().is_ok() {
        let _ = quiet_cmd("reg", &["delete", RUN_KEY, "/v", OLD_TASK, "/f"]);
    }
    if quiet_cmd("schtasks", &["/Query", "/TN", OLD_TASK]).is_ok_and(|o| o.status.success()) && autostart_enable().is_ok() {
        let _ = quiet_cmd("schtasks", &["/Delete", "/F", "/TN", OLD_TASK]);
    }
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, _, _| show(app)))
        .plugin(tauri_plugin_opener::init())
        // ウィンドウの大きさ・位置・最大化を記憶。表示/非表示はトレイ常駐と自動起動で決めるので記憶しない
        .plugin(
            tauri_plugin_window_state::Builder::default()
                .with_state_flags(tauri_plugin_window_state::StateFlags::all() & !tauri_plugin_window_state::StateFlags::VISIBLE)
                .build(),
        )
        .setup(|app| {
            // トークンを含むので、ポータブル版も exe の隣ではなく %APPDATA% に置く（dist を丸ごと渡すと漏れるため）
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            // 旧ポータブル版が exe の隣に置いたファイルを移す
            let exe_dir = std::env::current_exe()?.parent().unwrap().to_path_buf();
            if exe_dir != dir {
                let (old_state, new_state) = (exe_dir.join("state.json"), dir.join("state.json"));
                // 両方あれば更新日時が新しい方を残す
                let mtime = |p: &std::path::Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
                let keep_new = mtime(&new_state).is_some_and(|n| mtime(&old_state).is_some_and(|o| n >= o));
                if old_state.exists() && (keep_new || std::fs::copy(&old_state, &new_state).is_ok()) {
                    let _ = std::fs::remove_file(old_state);
                }
                let _ = std::fs::remove_file(exe_dir.join("startup.log"));
            }
            log_to(&dir, &format!("{:?}", std::env::args().skip(1).collect::<Vec<_>>()));
            let path = dir.join("state.json");
            // 初回は Electron 版 (%APPDATA%\discordmailer) のデータを引き継ぐ
            let old = app.path().config_dir()?.join("discordmailer").join("state.json");
            let text = std::fs::read_to_string(&path).or_else(|_| std::fs::read_to_string(old)).unwrap_or_default();
            let mut saved: Saved = serde_json::from_str(&text).unwrap_or_default();
            if saved.trays.is_empty() {
                saved.trays.push(TrayDef {
                    id: "inbox".into(),
                    name: "サーバー".into(),
                    favorites: IndexMap::new(),
                    notify: Notify { toast: false, sound: false.into(), badge: true },
                });
            }
            // 受信トレイが 1 つだった頃のデータは、サーバー用と DM 用に分ける（DM 用は設定を引き継いですぐ右に置く）
            if !saved.trays.iter().any(|t| t.id == "inbox-dm") {
                let i = saved.trays.iter().position(|t| t.id == "inbox");
                let mut dm = i.map_or_else(
                    || TrayDef { id: String::new(), name: String::new(), favorites: IndexMap::new(), notify: Notify { toast: false, sound: false.into(), badge: true } },
                    |i| saved.trays[i].clone(),
                );
                if let Some(i) = i.filter(|&i| saved.trays[i].name == "受信トレイ") {
                    saved.trays[i].name = "サーバー".into();
                }
                dm.id = "inbox-dm".into();
                dm.name = "DM".into();
                saved.trays.insert(i.map_or(0, |i| i + 1), dm);
                saved.settings.inbox_dm = saved.settings.inbox_all;
            }
            if !saved.settings.inbox_badge {
                saved.trays.iter_mut().filter(|t| is_inbox(&t.id)).for_each(|t| t.notify.badge = false);
            }
            // サーバーと DM をまとめた受信トレイ（サーバー用の設定を引き継いですぐ左に置く）
            if !saved.trays.iter().any(|t| t.id == "inbox-mix") {
                let i = saved.trays.iter().position(|t| t.id == "inbox").unwrap_or(0);
                let mut mix = saved.trays.get(i).cloned().unwrap_or_else(|| TrayDef { id: String::new(), name: String::new(), favorites: IndexMap::new(), notify: Notify { toast: false, sound: false.into(), badge: true } });
                mix.id = "inbox-mix".into();
                mix.name = "受信トレイ".into();
                mix.favorites.clear();
                saved.trays.insert(i, mix);
            }
            // 受信トレイは既定でオフなので、★ を付けるトレイを 1 つは用意する
            if saved.trays.iter().all(|t| is_inbox(&t.id)) {
                saved.trays.push(TrayDef { id: uuid::Uuid::new_v4().to_string(), name: "お気に入り".into(), favorites: IndexMap::new(), notify: default_notify() });
            }
            let start_minimized = saved.settings.start_minimized;
            if saved.settings.always_on_top {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.set_always_on_top(true);
                }
            }
            let core = Arc::new(Core {
                app: app.handle().clone(),
                path,
                data: Mutex::new(Data { saved, status: "接続待ち".into(), autostart: { migrate_run_key(); autostart_enabled() }, ..Default::default() }),
                writer: Default::default(),
                pending: Default::default(),
                reconnect: Default::default(),
            });
            app.manage(core.clone());

            let menu = Menu::with_items(
                app,
                &[&MenuItem::with_id(app, "show", "表示", true, None::<&str>)?, &MenuItem::with_id(app, "quit", "終了", true, None::<&str>)?],
            )?;
            TrayIconBuilder::with_id("main")
                .icon(tray_icon(false))
                .tooltip("DashTray")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, e| match e.id().as_ref() {
                    "show" => show(app),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|t, e| {
                    if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                        show(t.app_handle());
                    }
                })
                .build(app)?;

            // 自動起動（--hidden）で「最小化して起動」ならトレイに常駐するだけにする
            if !(start_minimized && std::env::args().any(|a| a == "--hidden")) {
                show(app.handle());
            }
            let c = core.clone();
            std::thread::spawn(move || loop {
                std::thread::sleep(Duration::from_secs(1));
                c.mark_viewed();
            });
            tauri::async_runtime::spawn(core.run());
            Ok(())
        })
        .on_window_event(|w, e| {
            if let WindowEvent::CloseRequested { api, .. } = e {
                api.prevent_close();
                let _ = w.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            get, load_channels, set_fav, refresh, add_dm, mark_read, mark_read_in, remove_dm, open_url, mark_all_read, set_settings, set_client_id, set_tray_notify,
            set_autostart, test_notify, add_tray, duplicate_tray, reorder_trays, rename_tray, delete_tray, open
        ])
        .run(tauri::generate_context!())
        .expect("起動に失敗しました");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_and_describe() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(iso(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(describe(&json!({ "content": "hi", "attachments": [{ "content_type": "image/png", "filename": "a.png" }] })), "hi [画像: a.png]");
        assert_eq!(describe(&json!({ "content": "", "embeds": [{}] })), "[埋め込み]");
        assert_eq!(describe(&json!({})), "[スタンプなど（内容を取得できません）]");
    }

    #[test]
    fn title_match() {
        let c = ChanInfo { name: "general".into(), guild_id: "1".into(), guild_name: "鯖".into() };
        let dm = Dm { name: "太郎".into(), icon: None, group: None };
        assert!(title_shows("#general | 鯖 - Discord", Some(&c), None));
        assert!(!title_shows("#general-2 | 鯖 - Discord", Some(&c), None));
        assert!(!title_shows("#general | 別 - Discord", Some(&c), None));
        assert!(title_shows("@太郎 - Discord", None, Some(&dm)));
        assert!(!title_shows("@太郎2 - Discord", None, Some(&dm)));
        assert!(!title_shows("Discord", None, None));
    }

    // Discord 以外が作ったパイプにはつながない
    #[test]
    fn fake_pipe_rejected() {
        tauri::async_runtime::block_on(async {
            let name = format!(r"\\.\pipe\discordmailer-test-{}", std::process::id());
            let _server = tokio::net::windows::named_pipe::ServerOptions::new().create(&name).unwrap();
            let client = ClientOptions::new().open(&name).unwrap();
            assert!(!pipe_is_discord(&client));
        });
        assert!(!is_discord(std::process::id()));
    }

    #[test]
    fn token_encrypted_on_disk() {
        let tok = json!({ "access_token": "secret-at", "refresh_token": "secret-rt" });
        let text = serde_json::to_string(&Saved { token: Some(tok.clone()), ..Default::default() }).unwrap();
        assert!(!text.contains("secret") && text.contains("\"token\":\"dpapi:"));
        assert_eq!(serde_json::from_str::<Saved>(&text).unwrap().token, Some(tok.clone()));
        // 旧版の平文、壊れた値、トークンなし
        let plain = format!(r#"{{"token":{tok}}}"#);
        assert_eq!(serde_json::from_str::<Saved>(&plain).unwrap().token, Some(tok));
        assert_eq!(serde_json::from_str::<Saved>(r#"{"token":"dpapi:AAAA"}"#).unwrap().token, None);
        assert_eq!(serde_json::from_str::<Saved>(r#"{"token":null}"#).unwrap().token, None);
        assert_eq!(serde_json::from_str::<Saved>("{}").unwrap().token, None);
    }

    #[test]
    fn sound_cooldown() {
        let (mut last, t) = (HashMap::new(), Instant::now());
        assert!(sound_ok(&mut last, "a", 10, t));
        assert!(!sound_ok(&mut last, "a", 10, t + Duration::from_secs(9)));
        assert!(sound_ok(&mut last, "b", 10, t + Duration::from_secs(9)));
        assert!(sound_ok(&mut last, "a", 10, t + Duration::from_secs(10)));
        assert!(sound_ok(&mut last, "a", 0, t + Duration::from_secs(10)));
    }
}
