# DashTray

Discord のメッセージをタスクトレイから確認できる Windows 用 Tauri アプリ（Discord RPC + OAuth2 PKCE）。

![デモ](docs/demo.gif)

## こんなときに

### サーバーが多すぎて、大事な発言を見落とす
参加しているサーバーが増えると、Discord の赤いバッジはいつも点きっぱなしで、本当に読むべき発言が埋もれてしまいます。
DashTray では、気になるサーバー・チャンネル・DM に ★ を付けるだけで、その新着が 1 つのトレイに集まります。「仕事」「趣味」のようにトレイを分けておけば、今は仕事の連絡だけ、と切り替えて見られます。何も選ばずに全部受け取る受信トレイもあります。

<img src="docs/scene-trays.png" alt="タブごとの未読バッジとサイドバーの ★" width="401">

### 作業に集中したいけど、呼ばれたらすぐ気づきたい
Discord を開きっぱなしにすると、つい雑談を追ってしまう。閉じると大事な連絡に気づかない。
DashTray はタスクトレイに常駐し、トレイごとに通知の仕方を決められます。仕事のトレイだけ通知音で知らせ、趣味のトレイは静かに未読を溜めておく、といった使い分けができます。会話が盛り上がっても、同じチャンネルの通知音は一定時間鳴らないので、通知音が鳴りっぱなしになりません。

<img src="docs/scene-notify.png" alt="タブの右クリックで通知音・未読バッジを切り替え" width="213">

### 朝 PC をつけたら、夜の間の連絡をまとめて確認
Windows の起動と一緒に裏で立ち上がり、止まっていた間に届いたメッセージを取り込みます。朝はメールの受信箱のように、未読を上から順に読んでいくだけ。発言はチャンネルごと（またはサーバーごと）の最新 1 件にまとまるので、一覧が長くなりすぎません。

<img src="docs/scene-list.png" alt="未読が太字で並ぶ一覧" width="820">

### 気になった 1 件だけ Discord で開く
一覧で選ぶと下に本文が出るので、ほとんどは DashTray の中で読み終わります。返事をしたいものだけ、ダブルクリックで Discord のそのメッセージへ飛べます。右クリックでメッセージのリンクをコピーできるので、タスク管理ツールやメモに貼って「あとでやる」にもできます。

<img src="docs/scene-open.png" alt="メッセージの右クリックメニュー" width="832">

### 画面の隅に小さく置いておく
サイドバーは左上の ☰ で、本文のプレビュー欄は右クリック（または ⚙ の設定）で隠せます。一覧の列も右クリックで必要なものだけに絞れ、幅はドラッグで変えられるので、新着の一覧だけを小さなウィンドウで表示できます。「常に前面に表示」と組み合わせれば、作業中のアプリの横に置いて、未読が増えたらひと目で分かります。

<img src="docs/scene-minimal.png" alt="サイドバーとプレビューを隠したミニマル表示" width="380">

## 導入

**[導入手順](docs/install.md)** を見てください。現在は Discord 側の制限で、作者がテスター登録した人だけが使えます。

## 動作環境

- Windows 10 / 11
- Discord デスクトップアプリが起動していること（Discord RPC で接続します）
- 初回起動時に Discord の画面で連携を許可します
- 設定と既読状態は `%APPDATA%\app.discordmailer` に保存されます


## セキュリティとプライバシー

| 気になること | DashTray では |
|---|---|
| メッセージがどこかに送られない？ | 送られません。通信するのは Discord だけで、作者のサーバーはありません。利用状況の収集もしていません。 |
| 勝手に発言されない？ | されません。メッセージを読む権限しか持たず、送信・削除・設定の変更はできません。 |
| ログイン情報は安全？ | 暗号化して保存しています。ファイルを別の PC や別のユーザーにコピーしても読めません。 |
| PC 内のほかのソフトにログイン情報を横取りされない？ | されません。Discord のふりをする別のソフトには接続せず、本物の Discord にだけログイン情報を渡します。 |
| 怪しいメッセージを開いても大丈夫？ | 本文は文字として表示するだけなので、仕込まれたスクリプトは動きません。 |
| 管理者権限は必要？ | 不要です。自動起動も今のユーザーの権限で登録します。 |

<details>
<summary>技術的な詳細</summary>

- 通信先: PC 内の Discord アプリ（名前付きパイプ）、`discord.com`（ログイン）、`cdn.discordapp.com`（アイコン画像）
- 権限（OAuth2 スコープ）: `rpc`、`messages.read`、`rpc.notifications.read`
- 使う RPC 命令: `AUTHORIZE`、`AUTHENTICATE`、`GET_GUILDS`、`GET_CHANNELS`、`GET_CHANNEL`、`SUBSCRIBE`、`UNSUBSCRIBE`
- ログインは PKCE 方式で、Client Secret を exe に含みません
- 接続前にパイプの相手のプロセスが Discord の exe かを確認します
- トークンは Windows の DPAPI（今のユーザー用）で暗号化して `%APPDATA%\app.discordmailer\state.json` に保存します。同じユーザーとして動くソフトからは守れない点は、ほかの Windows アプリと同じです
- 本文は `textContent` で表示し、HTML として解釈しません
- 自動起動はタスクスケジューラに LeastPrivilege（管理者権限なし）で登録します

</details>

## 配布物の検証

v1.0.1 以降の [Releases](https://github.com/Tomozow/DashTray/releases) のファイルは、このリポジトリのソースから GitHub Actions でビルドしています。ダウンロードしたファイルが改ざんされていないかは、次のどちらかで確かめられます。

GitHub CLI でビルド証明を確かめる:

```
gh attestation verify DashTray.zip --repo Tomozow/DashTray
```

PowerShell でハッシュを出し、Release ノートの SHA-256 と比べる:

```
Get-FileHash DashTray.zip -Algorithm SHA256
```

## ビルド

```
npm install
npm run build:portable   # dist/DashTray.exe と dist/DashTray.zip
```

## License

MIT
