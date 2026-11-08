# nibwarden 階段 1 架構設計

- 狀態：草稿
- 日期：2026-10-28
- 範圍：常駐 daemon、登入與同步、CLI、SSH agent、Git SSH 簽章

---

## 1. 背景與目標

nibwarden 是一個與 Bitwarden 伺服器相容的桌面客戶端，目標是做到 1Password 等級的便利性：
所有前端（CLI、SSH agent、瀏覽器、GUI）共用同一個已解鎖的 daemon，而不是各自維護登入狀態。

### 1.1 階段 1 目標

1. 常駐 daemon（`nibwardend`）負責登入、同步、持有解鎖後的金鑰、上鎖。
2. CLI（`nibwarden`）只透過 IPC 向 daemon 取資料，自己**不**持有 vault、token 或 `BW_SESSION`。
3. SSH agent：使用 vault 中的 SSH Key 項目做 SSH 登入。
4. Git commit/tag 簽章：使用 SSH 簽章（`gpg.format=ssh`），透過 agent 完成。
5. 透過 SSH 登入的終端機中也能解鎖（階段 1 的解鎖本來就走 TTY，因此自然支援）。

### 1.2 階段 1 不做（之後的階段）

| 項目 | 預定階段 |
|---|---|
| GUI（解鎖視窗、授權確認、快速存取） | 2 |
| 每次使用金鑰前要求確認（per-key confirm） | 2，但介面在階段 1 先定好（§6.4） |
| 瀏覽器擴充套件整合（native messaging / 生物辨識解鎖） | 3（官方擴充套件的解鎖已在階段 2 做，phase2.md §5.8） |
| 遠端解鎖的範圍隔離、手機核准 | 3 |
| OpenPGP 簽章、autotype、passkey | 4 |
| macOS / Windows | Linux 穩定後再移植；抽象層在階段 1 先預留（§10） |
| SSO 登入、Key Connector、TDE | 未排定 |

---

## 2. 關鍵決策

### D1. 完全重寫，不 fork Goldwarden

Goldwarden 自 2025-08 起停止開發。作者表示官方即將在 auth、crypto、vault item 上做不相容的改動，
而 Goldwarden 已無力跟進（[goldwarden#331](https://github.com/quexten/goldwarden/issues/331#issuecomment-3221660132)）。
自己實作加密層，正是它最後停下來的原因。

### D2. 加密、API、vault 邏輯一律使用 `bitwarden/sdk-internal`

- 官方所有客戶端共用這套 Rust 核心，官方的加密或格式改動會直接反映在這裡。
- 本文件撰寫時參照的 commit 是 `4d983d78ecc4e87ffb2a86c5c2b114b94836eaef`（2026-10-29）。
- **風險**：README 明言這套 SDK「不供外部使用，介面隨時可能改變」。對策如下：
  - 所有 SDK 呼叫都集中在 `nibwarden-core::sdk` 這一層（§4），其他 crate 不直接 import `bitwarden_*`。
  - 依賴以 **git + 固定 commit** 鎖定，再由 Renovate 或手動定期升級，升級時跑完整整合測試（§11）。
- crates.io 的發佈狀況（2026-10-28 確認）：
  - **已發佈**：`bitwarden-core`、`bitwarden-crypto`、`bitwarden-auth`、`bitwarden-state`、`bitwarden-api-api`（皆為 4.0.0），以及 `bitwarden-vault`（1.0.0）。
  - **未發佈**：`bitwarden-pm`、`bitwarden-unlock`、`bitwarden-sync`、`bitwarden-ssh`、`bitwarden-ipc`。
  - 因此統一走 git 依賴，避免 crates.io 與 git 版本混用而造成型別不一致。

```toml
# Cargo.toml（workspace）
[workspace.dependencies]
bitwarden-pm     = { git = "https://github.com/bitwarden/sdk-internal", rev = "4d983d78", features = ["cli"] }
bitwarden-core   = { git = "https://github.com/bitwarden/sdk-internal", rev = "4d983d78" }
bitwarden-state  = { git = "https://github.com/bitwarden/sdk-internal", rev = "4d983d78" }
bitwarden-auth   = { git = "https://github.com/bitwarden/sdk-internal", rev = "4d983d78" }
bitwarden-vault  = { git = "https://github.com/bitwarden/sdk-internal", rev = "4d983d78" }
bitwarden-unlock = { git = "https://github.com/bitwarden/sdk-internal", rev = "4d983d78", features = ["cli"] }
bitwarden-sync   = { git = "https://github.com/bitwarden/sdk-internal", rev = "4d983d78" }
```

> 實際的 feature 名稱要在 M1 時對照該 commit 的 `Cargo.toml` 確認。目前已知 `UnlockMethod::SessionKey` 需要開啟 `cli` feature。

### D3. 語言與授權

- 語言：Rust。edition 2024，toolchain 與 sdk-internal 保持一致（目前是 1.98.0，最低 rust-version 1.92，Slint 1.18 的要求）。
- 授權：**GPL-3.0-only**。sdk-internal 採 GPL-3.0 與 Bitwarden SDK License 雙授權，選 GPL 才能開源發佈。
  這也讓我們能合法參考或沿用 `bitwarden/clients` 中 `desktop_native` 的 GPL 程式碼，例如 `ssh_agent`。

### D4. 參考 SDK 自己的 `bw` 測試 CLI

sdk-internal 裡的 `crates/bw` 是官方正在開發的 Rust CLI，示範了以下流程：

- `StateRegistry::new_with_db(Sqlite{..})`
- `PasswordManagerClient::load_from_state` 與 `save_to_state`
- `auth().login_password(PasswordLoginRequest{..})`
- `unlock().unlock(UnlockMethod::..)`、`generate_session_key()`、`lock()`

daemon 的登入與狀態持久化流程以它為藍本。

### D5. 為什麼不等官方的 shared-unlock

sdk-internal 已經有 `bitwarden-shared-unlock`，設計是讓 CLI 和瀏覽器把**鎖定狀態**同步給桌面 App。
不過它目前標示 `publish = false`，同步的只是鎖定與解鎖狀態，而不是「CLI 透過桌面 App 取資料」。
nibwarden 的模型更進一步：前端完全不持有金鑰。這個模組之後可作為階段 3 跟官方瀏覽器擴充套件互通時的參考。

---

## 3. 元件總覽

```
 ┌─────────────────────────────── nibwardend（每個使用者一個，systemd --user） ───────────────────────────────┐
 │                                                                                                          │
 │   AccountManager ── PasswordManagerClient（sdk-internal）── StateRegistry(SQLite) ── Bitwarden / Vaultwarden │
 │        │                                                                                                 │
 │   LockController（狀態機、閒置逾時、螢幕鎖定、休眠）                                                      │
 │        │                                                                                                 │
 │   PolicyEngine（誰可以拿什麼：peer 身分 × 操作 × 鎖定狀態）── Approver trait（階段 1：自動放行；階段 2：GUI）│
 │        │                                                                                                 │
 │   ┌────┴──────────────┬───────────────────────────┐                                                      │
 │   │ Control IPC        │ SSH agent                 │                                                      │
 │   │ daemon.sock        │ ssh-agent.sock            │                                                      │
 └───┴────────────────────┴───────────────────────────┴──────────────────────────────────────────────────────┘
         ▲                         ▲
   nibwarden（CLI）            ssh / git（ssh-keygen -Y sign）
```

- 所有 socket 都放在 `$XDG_RUNTIME_DIR/nibwarden/`，目錄權限 `0700`。
- daemon 是唯一能與 Bitwarden 伺服器通訊、也是唯一持有解鎖金鑰的程序。

---

## 4. Workspace 結構

```
nibwarden/
├── Cargo.toml                 # workspace，集中管理 sdk-internal 的 rev
├── crates/
│   ├── nibwarden-core/        # 唯一允許 import bitwarden_* 的 crate
│   │   ├── sdk/               #   包裝 PasswordManagerClient：login / sync / unlock / lock / 查詢
│   │   ├── vault.rs           #   清單、查項目、讀單一欄位、TOTP（不外漏 SDK 型別）
│   │   └── generator.rs       #   密碼與 passphrase 產生器
│   ├── nibwarden-ipc/         # control 協定：訊息定義、編解碼、版本協商、client
│   │   └── reference.rs       #   秘密參照語法 nw://[folder/]item/field（純語法，CLI 也要用）
│   ├── nibwarden-daemon/      # binary：nibwardend
│   │   ├── daemon.rs          #   帳號狀態機、閒置上鎖、定期同步
│   │   ├── policy.rs          #   PolicyEngine + Approver trait
│   │   ├── control.rs         #   control socket server
│   │   ├── ssh_agent.rs       #   agent socket：Backend 實作（上鎖判斷、政策、通知）
│   │   └── error.rs           #   core 錯誤 → IPC 錯誤碼
│   ├── nibwarden-ssh-agent/   # SSH agent 協定伺服器：訊框、簽章、SSHSIG 用途解析；金鑰經 Backend trait 取得
│   └── nibwarden-cli/         # binary：nibwarden、nibwarden-ssh-sign（git 的 gpg.ssh.program，§8.3）
├── packaging/
│   ├── systemd/nibwardend.service
│   ├── deb/build.sh           # 只用 dpkg-deb 打 .deb（§12.5）
│   ├── environment.d/         # 選用：讓整個使用者工作階段改用 nibwarden 的 agent（不預設安裝）
│   └── （socket activation 的 agent unit 暫不做：agent 由 daemon 自己 bind，見 §12.4）
└── docs/architecture/
```

依賴方向只能單向：`cli → ipc`；`daemon → core, ipc, ssh-agent`；`ssh-agent` 不依賴 `core`，而是透過 trait 向 daemon 取金鑰。

---

## 5. Daemon（`nibwardend`）

### 5.1 帳號與鎖定狀態機

```
            login(ok)                    unlock(ok)
 LoggedOut ───────────▶ Locked ◀──────────────────────▶ Unlocked
     ▲                    │         lock / 逾時 / 螢幕鎖定 / 休眠
     └──── logout ────────┴──────────────────────────────┘
```

| 狀態 | 記憶體中的資料 | 磁碟上的資料 |
|---|---|---|
| LoggedOut | 無 | 無（`logout` 會呼叫 `StateRegistry::wipe()`） |
| Locked | 無 user key | SDK 狀態：token、加密過的帳號金鑰狀態、加密過的 vault 快取 |
| Unlocked | SDK KeyStore 中的 user key，以及解密所需的金鑰 | 同 Locked |

- 階段 1 只支援**單一帳號**。資料結構以 `HashMap<UserId, Account>` 設計，保留多帳號的空間。
- 解鎖方式：
  - 輸入主密碼：`UnlockMethod::Legacy(InitUserCryptoRequest{ MasterPasswordUnlock })`。
  - daemon 重啟時**不**自動解鎖，不在磁碟上保存 session key。
- 上鎖的觸發條件：
  - 閒置逾時，預設 15 分鐘，以「最後一次成功的 IPC 或 agent 請求」為準。
  - 螢幕鎖定，監聽 logind `Session.Lock` 與 `org.freedesktop.ScreenSaver.ActiveChanged`。
  - 系統休眠，監聽 logind `PrepareForSleep`。
  - 手動執行 `nibwarden lock`。
- 上鎖時呼叫 `unlock().lock()`，並清空 daemon 自己的所有快取（例如 ssh 公鑰清單以外的任何明文）。

### 5.2 登入

- 階段 1 支援三種登入方式：
  1. email、主密碼，加上兩步驟驗證（TOTP、Email）。
  2. Personal API key（`client_id`、`client_secret`），之後仍需輸入主密碼才能解鎖。
  3. 新裝置驗證（Email OTP）。Bitwarden 近年對未知裝置會要求這一步，需確認 SDK 的支援程度（§12 Q2）。
- 主密碼與 OTP 由 CLI 從 TTY 讀取（不回顯），經 control socket 傳給 daemon，**絕不**出現在 argv 或環境變數中。
- 伺服器選擇：`--server`，可填 `bitwarden.com`、`bitwarden.eu` 或自訂網址（Vaultwarden）。由此推導 `BaseUrls { api_url, identity_url }`。
- 裝置識別：
  - `device_identifier` 產生 UUID 並持久化。
  - `device_type`、`user_agent`、`bitwarden_client_version` 的取值見 §12 Q1。

### 5.3 同步

- 登入後、解鎖後各同步一次，之後每 5 分鐘同步一次（可設定）。`nibwarden sync` 可強制同步。
- 在 Locked 狀態下也可以同步，因為 vault 是以密文快取。
- 階段 1 不接 notifications hub（WebSocket 推播），留到階段 2。

### 5.4 程序強化

- 啟動時執行：
  - `prctl(PR_SET_DUMPABLE, 0)`：禁止 core dump，也阻止同 uid 的 ptrace（Yama 未啟用時的補強）。
  - `setrlimit(RLIMIT_CORE, 0)`。
- 金鑰記憶體由 SDK 的 KeyStore 管理，它會在 drop 時 zeroize。daemon 自己暫存的明文（例如回傳給 CLI 的欄位）一律使用 `Zeroizing<String>`，用完就丟。
- 是否 `mlockall` 待評估，會影響記憶體用量；`desktop_native/secure_memory` 可作參考。
- 日誌一律不記錄秘密內容。`tracing` 欄位只記錄 item id，不記錄名稱以外的內容。

---

## 6. Control IPC（`nibwarden-ipc`）

### 6.1 傳輸層

- Linux：`$XDG_RUNTIME_DIR/nibwarden/daemon.sock`（`SOCK_STREAM`，權限 `0600`）。
- 抽象成 `trait Transport`，之後可接 macOS 的 Unix socket 與 Windows 的 named pipe。
- 訊框格式：4 bytes 大端長度加上 JSON（`serde_json`），單一訊息上限 16 MiB。
  選 JSON 是為了好除錯，日後要換成 CBOR 也只需改編解碼層。

### 6.2 訊息格式

```jsonc
// 請求
{ "v": 1, "id": 42, "method": "item.get", "params": { "folder": "Work", "item": "GitHub", "field": "password" } }
// 成功
{ "v": 1, "id": 42, "result": { "value": "…" } }
// 失敗
{ "v": 1, "id": 42, "error": { "code": "locked", "message": "vault is locked" } }
```

- 連線建立後，第一個訊息必須是 `hello { client: "nibwarden-cli", version }`，daemon 會回傳 `{ protocol: 1, daemon_version }`。
- 錯誤碼是固定的列舉：`locked`、`logged_out`、`already_logged_in`、`not_found`、`ambiguous`、`denied`、`two_factor_required`、`bad_request`、`internal`。
  主密碼錯誤、伺服器拒絕登入都回 `denied`；`two_factor_required` 會附帶 `providers`（`authenticator`、`email`）。

### 6.3 方法清單（階段 1）

| method | 需要的狀態 | 說明 |
|---|---|---|
| `status` | 任何 | 帳號、伺服器、鎖定狀態、最後同步時間 |
| `auth.login` / `auth.login_2fa` / `auth.logout` | LoggedOut / 任何 | 兩步驟驗證採多輪交握：先回 `two_factor_required{providers}`，再送 `auth.login_2fa`。待完成的登入只存在**該條連線**的記憶體中，斷線即丟棄 |
| `auth.send_email_code` | 有待完成的登入 | 請伺服器寄出 Email 驗證碼（bitwarden.com 需要；Vaultwarden 有時會自己寄，新寄的會取代舊的） |
| `lock.unlock` / `lock.lock` | Locked / Unlocked | |
| `sync` | 已登入 | |
| `item.list {search?, folder?}` | Unlocked | 只回傳 id、名稱、類型、資料夾、URI、是否有 TOTP 等**不敏感的欄位**（TOTP 金鑰本身不回傳） |
| `item.get {folder?, item, field}` | Unlocked | 只回傳**指定欄位**；不提供「整個項目明文」的 API |
| `item.totp {folder?, item}` | Unlocked | 回傳目前的 TOTP code、剩餘秒數與週期 |
| `generate.password` / `generate.passphrase` | 任何 | |
| `ssh.list` | Unlocked | 列出 SSH 金鑰項目的 id、名稱、公鑰與指紋（不含私鑰） |

### 6.4 Peer 身分與授權

1. 接受連線時以 `SO_PEERCRED` 取得 `pid`、`uid`、`gid`。**`uid` 不等於 daemon 的 uid 就直接拒絕。**
2. 立即用 `pidfd_open(pid)` 鎖定該程序，避免 PID 被重用；再讀 `/proc/<pid>/exe` 取得執行檔路徑，作為顯示與政策判斷的依據。
3. 收集 session 環境資訊，用於判斷是否為 SSH 遠端呼叫：
   - 讀取 `/proc/<pid>/environ` 中的 `SSH_CONNECTION`。這是盡力而為，因為環境變數可以偽造，**只能用於顯示，不能當作安全依據**。
   - 讀取 logind session，以 `sd_pid_get_session` 取得 session 類型（`tty`、`x11`、`wayland`），以及是否為 remote。
4. 每個請求都要經過 `PolicyEngine::check(peer, operation) -> Decision`：

```rust
pub enum Decision { Allow, Deny, AskUser(ApprovalRequest) }

#[async_trait]
pub trait Approver: Send + Sync {
    /// 階段 1：AutoApprover（Unlocked 時一律回 Approved）
    /// 階段 2：GuiApprover（把確認視窗叫到前景）
    /// 階段 3：TtyApprover / RemoteApprover
    async fn approve(&self, req: ApprovalRequest) -> ApprovalOutcome;
}
```

階段 1 的政策是「已解鎖就允許」，與官方 `bw` 搭配 `BW_SESSION` 的安全強度相同，但秘密不會進入環境變數。
`Approver` 介面在階段 1 就要定好，否則階段 2 加入確認機制時會需要大改。

---

## 7. CLI（`nibwarden`）

```
nibwarden login [--server <url>] [--apikey]
nibwarden logout
nibwarden unlock            # 從 TTY 讀主密碼（不回顯）；SSH 遠端登入的終端機同樣適用
nibwarden lock
nibwarden status [--json]
nibwarden sync
nibwarden list [--search <q>] [--folder <f>] [--json]
nibwarden get <ref|name|id> [--field password|username|totp|notes|uri|<custom>] [--folder <f>] [-n]
nibwarden read nw://[<folder>/]<item>/<field> [-n]           # 給腳本用，只輸出值
nibwarden totp <name|id> [--folder <f>]
nibwarden run [--env-file .env.tpl] -- <cmd> [args…]           # 把 nw:// 參照解析後注入子程序的環境變數
nibwarden generate [--length N] [--special] [--passphrase --words N --separator S] …
nibwarden ssh list
nibwarden ssh git-setup [--global] [--key <name|id>]           # 寫入 git 的 SSH 簽章設定（§8.3）
```

- 輸出原則：
  - 預設給人看，加上 `--json` 給機器解析。
  - 秘密值只輸出到 stdout，其他訊息一律輸出到 stderr。
- 錯誤 `locked`：如果 stdin 是 TTY，就直接提示輸入主密碼解鎖，之後重試；不是 TTY 的話，回傳非零退出碼並提示執行 `nibwarden unlock`。
- daemon 不在時，CLI 會嘗試透過 `systemctl --user start nibwardend` 啟動，失敗才報錯。
- 結束碼：`3` 表示 vault 上鎖、`4` 表示未登入，其他錯誤為 `1`；`run` 會以子程序取代自己（`exec`），結束碼就是子程序的。

### 7.1 秘密參照語法

`nw://[<folder>/]<item>/<field>`

- `<folder>`：資料夾名稱或 id，`*` 或省略代表任何資料夾。階段 1 只有個人 vault，所以第一段是資料夾而不是 vault。
- `<item>`：項目名稱或 id。先比對 id，再比對完全相同的名稱，最後不分大小寫比對；符合多個時回 `ambiguous` 並列出候選的 id。
- `<field>`：內建欄位（`password`、`username`、`totp`、`notes`、`uri`、卡片的 `number`／`code`／`expiry`、SSH 的 `private_key`／`public_key`／`fingerprint` 等）或自訂欄位名稱。內建名稱只在項目有對應區段時生效，所以登入項目上名為 `code` 的自訂欄位仍然讀得到。
- 名稱裡的 `/` 寫成 `%2F`、`%` 寫成 `%25`，其他 `%XX` UTF-8 跳脫也接受。

---

## 8. SSH agent 與 Git 簽章

### 8.1 Agent 伺服器

- Socket 路徑：`$XDG_RUNTIME_DIR/nibwarden/ssh-agent.sock`。使用者自行設定 `SSH_AUTH_SOCK`，或在 `~/.ssh/config` 中設定 `IdentityAgent`。
- 協定依據 draft-ietf-sshm-ssh-agent。階段 1 支援以下訊息：

  | 訊息 | 行為 |
  |---|---|
  | `REQUEST_IDENTITIES` | 列出 vault 中所有 SSH Key 項目（`CipherType::SshKey = 5`）的公鑰 |
  | `SIGN_REQUEST` | 支援 ed25519、ecdsa p256/p384/p521、rsa（含 `rsa-sha2-256` / `rsa-sha2-512` 旗標） |
  | `ADD_IDENTITY` / `REMOVE_*` / `LOCK` / `UNLOCK` | 回傳 `FAILURE`（vault 才是唯一的金鑰來源） |
  | `EXTENSION` | 回傳 `FAILURE` |

- 實作來源（Q4 的結論）：自己實作協定層，不沿用 `desktop_native/ssh_agent`。
  - 協定本身很小（訊框加三種訊息），`desktop_native` 的分層大多是為了 napi／Electron。
  - 金鑰解析與簽章用 `ssh-key 0.7.0-rc.11`，與 SDK 的 `bitwarden-ssh` 同版本，同一把金鑰在兩邊的解析結果一致，依賴樹裡也只有一份。
  - Socket 預設放在 control socket 旁邊（`--ssh-agent-socket` 可改，`--no-ssh-agent` 可關）。
- Agent 在 **Locked** 狀態下的行為：
  - `REQUEST_IDENTITIES` 回傳空清單，這樣 ssh 會改用其他認證方式，而不是直接失敗。
  - `SIGN_REQUEST` 回傳 `FAILURE`。
  - 兩者都會送出桌面通知：「nibwarden is locked」，內容寫明是哪個程式要金鑰、請執行 `nibwarden unlock`。
    - 通知掛在 `REQUEST_IDENTITIES` 上是必要的：上鎖時 ssh 與 `ssh-keygen -Y sign` 拿到空清單後就不會再送簽章請求，只掛在簽章上幾乎不會觸發。1Password 也是在列金鑰時跳出解鎖。
    - 每 10 秒最多一則，因為一次 `ssh` 或 `git commit` 可能連續開好幾條 agent 連線。
    - 透過 `notify-send` 送出；沒有 session bus（例如純 SSH 登入）時安靜失敗。
  - 階段 2 改為跳出解鎖視窗，請求先保留，解鎖後再繼續處理。

### 8.2 判斷請求用途

`SIGN_REQUEST` 的資料如果以 `SSHSIG` 開頭，就是 `ssh-keygen -Y sign` 發出的檔案簽章請求，可以從中解析出 namespace（Git 用的是 `git`）。
官方的 `desktop_native` 也用同樣方式區分「SSH 登入」和「Git 簽章」。
階段 1 只把用途記錄到日誌，並放進 `ApprovalRequest`；階段 2 的確認視窗再拿來顯示，例如「git 要求簽署 commit」。

### 8.3 Git SSH 簽章

Git 預設的 `gpg.ssh.program` 就是 `ssh-keygen`。只要 `user.signingkey` 填的是公鑰字面值，
`ssh-keygen` 就會透過 `SSH_AUTH_SOCK` 向 agent 要簽章，**不需要額外的輔助程式**。

`nibwarden ssh git-setup` 會寫入以下設定（預設寫進目前 repo，`--global` 寫進使用者設定）：

```ini
[gpg]
    format = ssh
[user]
    signingkey = key::ssh-ed25519 AAAAC3Nza… nibwarden:<item-name>
[commit]
    gpgsign = true
[gpg "ssh"]
    program = /usr/bin/nibwarden-ssh-sign
    allowedSignersFile = ~/.config/git/allowed_signers   # 已有設定就沿用那個檔案，不覆寫
```

- allowed signers 檔會加上一行 `<user.email> namespaces="git" <pubkey>`（同一組已存在就不重複），讓 `git log --show-signature` 與 `verify-commit` 能驗證自己的 commit。
- vault 裡有多把 SSH 金鑰時必須用 `--key` 指定（名稱不分大小寫，或 id）；只有一把就直接用。

`gpg.ssh.program` **一律**設成 `nibwarden-ssh-sign`，而不是依賴 shell 的 `SSH_AUTH_SOCK`：
- 桌面環境通常已經有自己的 agent（gnome-keyring、gpg-agent），IDE 與 GUI git 客戶端也不一定讀得到 shell 設定；1Password 的 `op-ssh-sign` 是同樣做法。
- 它是 CLI crate 裡的第二個執行檔，參數原封不動交給 `ssh-keygen`，只是把 `SSH_AUTH_SOCK` 換成 daemon 回報的 agent socket。
- 只有 `-Y sign` 才會連 daemon：vault 上鎖時，有終端機就詢問主密碼、解鎖後繼續簽；沒有終端機就失敗並提示 `nibwarden unlock`。驗證（`-Y verify`、`find-principals` 等）不需要 vault，直接執行 `ssh-keygen`。

---

## 9. 磁碟資料與安全模型

### 9.1 檔案位置

| 路徑 | 內容 | 權限 |
|---|---|---|
| `$XDG_DATA_HOME/nibwarden/user.sqlite` | SDK 的 StateRegistry：token、加密過的帳號狀態、vault 密文快取 | `0600` |
| `$XDG_CONFIG_HOME/nibwarden/config.toml` | 伺服器網址、逾時設定、同步間隔 | `0644` |
| `$XDG_RUNTIME_DIR/nibwarden/*.sock` | IPC | 目錄 `0700` |

refresh token 以明文存放在 SQLite 中，與官方 CLI 相同。拿到它的人可以下載**密文** vault，但無法解密。
階段 2 再評估改存到 Secret Service（libsecret）中。

### 9.2 威脅模型

| 威脅 | 階段 1 的防護 |
|---|---|
| 其他 uid 的使用者 | socket 與資料目錄的權限，加上 `SO_PEERCRED` 的 uid 檢查 |
| 磁碟或備份外洩 | 只有密文；解鎖需要主密碼 |
| 記憶體轉儲、core dump | `PR_SET_DUMPABLE=0`、`RLIMIT_CORE=0`、zeroize |
| 秘密透過 argv、環境變數、日誌外洩 | 主密碼只走 TTY 到 socket；不使用 `BW_SESSION`；日誌過濾秘密 |
| **同一個 uid 下的惡意程式** | **階段 1 無法防範**：解鎖期間任何同 uid 的程序都能透過 socket 取資料，與官方 CLI 的模型相同。階段 2 的 per-app 確認與 exe 路徑白名單可以降低風險，但在桌面 Linux 上無法完全防禦（同 uid 可以 ptrace、注入其他程序）。這點要在文件中明確寫給使用者知道 |

---

## 10. 跨平台抽象（階段 1 只實作 Linux）

| 抽象 | Linux（階段 1） | macOS（之後） | Windows（之後） |
|---|---|---|---|
| `Transport` | Unix socket | Unix socket | Named pipe |
| `PeerIdentity` | `SO_PEERCRED` + pidfd + `/proc` | `LOCAL_PEERCRED` / audit token | `GetNamedPipeClientProcessId` |
| `SessionEvents`（螢幕鎖定、休眠） | logind D-Bus | `NSWorkspace` 通知 | `WTSRegisterSessionNotification` |
| `Notifier` | freedesktop Notifications | `UNUserNotificationCenter` | Toast |
| 服務管理 | systemd user unit | launchd agent | Startup task |

`desktop_native` 中已經有各平台對應的實作，例如 `biometric`、`powermonitor`、`ipc`，可以作為參考。

---

## 11. 測試策略

- **單元測試**：IPC 編解碼、參照語法解析、PolicyEngine、LockController（用模擬時鐘）、agent 協定的訊框處理。
- **整合測試**：CI 中用 docker 起一個 Vaultwarden，建立測試帳號並涵蓋以下流程：
  - login、sync、unlock、`item.get`、lock、確認上鎖後查詢會被拒絕
  - 用 `ssh-add -L` 列出公鑰
  - 用 `ssh-keygen -Y sign -n git` 簽章，再用 `-Y verify` 驗證
  - 在暫存 repo 中執行 `git commit -S`，再用 `git verify-commit` 驗證
  - 對 sshd 容器執行 `ssh -o IdentityAgent=…` 登入
- **官方伺服器的煙霧測試**：用 bitwarden.com 的測試帳號，手動或排程執行，**不**放進每個 PR 的 CI。
- **SDK 升級檢查**：每次調整 sdk-internal 的 rev 都要跑完整整合測試。`nibwarden-core::sdk` 這層的測試是升級時的主要防線。

---

## 12. 待確認問題與風險

| # | 問題 | 狀態 | 結論或下一步 |
|---|---|---|---|
| Q1 | 用什麼身分向伺服器自我識別？官方伺服器是否會拒絕未知客戶端？ | 部分解決 | `Bitwarden-Client-Name` 由 SDK 依 `DeviceType` 推導，**無法自訂**。目前用 `LinuxCLI`，User-Agent 是 `nibwarden/<ver>`。`Bitwarden-Client-Version` 原本刻意不送，**M4 改為送 `2024.12.0`**：伺服器把它當功能門檻，沒送就被當成舊客戶端，`/sync` 會把 SSH 金鑰項目濾掉（見 §12.4）。另外，SDK 舊版密碼登入把 `client_id=web`、`deviceName=firefox` 寫死在程式裡。**bitwarden.com 尚未實測**。（階段 2 改用桌面版的 `DeviceType`，並自己送密碼登入請求，見 phase2.md §12.16） |
| Q2 | 是否支援新裝置驗證與各種 2FA provider？ | 部分解決 | SDK 支援 TOTP 與 Email 2FA，probe 已接上，但還沒實測。新裝置驗證需要對 bitwarden.com 實測。我們已經把 `device-id` 持久化，避免每次登入都被當成新裝置 |
| Q3 | daemon 重啟後能否只用主密碼解鎖？ | **已解決** | 從同步回應的 `userDecryption.masterPasswordUnlock` 取得 KDF 參數與被包起來的 user key，存到 `unlock.json`（內容不是秘密），再搭配 SDK state 中的 `ACCOUNT_CRYPTO_STATE` 組出 `InitUserCryptoRequest`。已用 Vaultwarden 1.37.4 驗證，跨程序可行 |
| Q4 | `desktop_native/ssh_agent` 是直接引用還是複製改寫？ | **已解決** | 都不是：自己實作協定層，簽章用與 SDK 相同版本的 `ssh-key`（§8.1） |
| Q5 | sdk-internal 的 API 變動頻率 | 觀察中 | 持續追蹤 |
| Q6 | 伺服器端金鑰輪替後，本機的帳號金鑰狀態如何更新？ | **新增** | 官方的 `CryptoSyncHandler` 在這個 rev 下，沒有 TS 端的 state bridge 時會 panic，註解也寫明「在 sync 移進 SDK 前不使用」，所以目前沒有註冊。影響是：金鑰在別處輪替後，要重新登入才會生效。之後 SDK 補齊時改用官方 handler；或者在偵測到解鎖失敗時，提示使用者重新登入 |
| R1 | Bitwarden 對第三方客戶端的條款與政策 | 未開始 | 正式發佈前閱讀 Bitwarden ToS；以 Vaultwarden 作為主要開發目標 |

### 12.1 M1 發現的 SDK 使用陷阱

以下都已經在 `nibwarden-core` 中處理，升級 SDK 時要重新檢查：

1. **`client.sync()` 每次都會回傳新的 `SyncClient`**，handler 註冊表是空的。必須保存並重用同一個實例（見 `Account::sync`）。SDK 自己的 `PasswordManagerClient::new_with_sync` 也有這個問題，它註冊的 handler 實際上不會被呼叫到。
2. **SDK 沒有內建 cipher 的 sync handler**，因為官方 TS 客戶端自己存 cipher。我們實作了 `CipherSyncHandler`，否則 `vault().ciphers().list()` 永遠是空的。
3. **`LocalUserDataKeyState` 是「由宿主程式管理」的 repository**，不在 SDK 的 SQLite 遷移清單中。沒有註冊的話，解鎖會失敗並出現 `Unable to initialize local user data key`。我們用 `FileRepository` 存成 JSON，內容是被 user key 包起來的密文。
4. **SDK 用 umask 建立 `user.sqlite`**，預設權限是 0644。我們開啟資料庫後會改成 0600。
5. **Vaultwarden 1.37 的註冊流程是兩段式**（`register/send-verification-email` 加上 `register/finish`），這只影響測試帳號的建立。

### 12.2 M2 的實作決定與尚未完成的部分

- **閒置計時**：只有「用到已解鎖 vault 的請求」會重設計時（M2 只有 `lock.unlock`，M3 起加上 `item.*`）。`status`、`sync` 不算，否則 shell prompt 輪詢 `status` 會讓 vault 永遠不上鎖。
- **時鐘**：閒置計時使用 `CLOCK_BOOTTIME`，休眠期間也會累計。`Instant`（`CLOCK_MONOTONIC`）在休眠時會停，筆電闔上超過逾時時間後打開仍是解鎖狀態。
- **啟動順序**：先 bind socket 再開資料庫，第二個 daemon 在碰到 SQLite 之前就會失敗退出。
- **Peer 身分**：優先使用 `SO_PEERPIDFD`（Linux 6.5+，由核心在 `connect()` 時取得），沒有才退回 `pidfd_open(pid)`。讀完 `/proc/<pid>` 後再用 pidfd 確認程序仍存活，否則丟棄讀到的 exe 與環境資訊。
- **政策**：`auth.logout` 走 `AskUser`（階段 1 由 `AutoApprover` 自動放行），其餘操作直接允許。
- **登入錯誤**：identity server 回 4xx 時，只取 JSON 中的人類可讀訊息（`server refused: …`），不把整段回應丟給使用者。
- **systemd**：`packaging/systemd/nibwardend.service` 已通過 `systemd-analyze --user verify`，並用 `systemd-run --user` 套同樣的強化設定實測（`NoNewPrivileges`、`MemoryDenyWriteExecute`、`LimitCORE=0`、`UMask=0077`）。CLI 在預設 socket 連不上時會執行 `systemctl --user start nibwardend.service`；這段**尚未在實際安裝的 unit 上測過**。
- **尚未實作，留待後續**：
  - logind 的螢幕鎖定與休眠事件（§5.1）。目前靠閒置逾時與 `CLOCK_BOOTTIME` 涵蓋休眠的情況。
  - peer 的 logind session 類型（§6.4 第 3 點）。目前只讀 `SSH_CONNECTION` 作為顯示用的提示。
  - Personal API key 登入、新裝置驗證（§5.2），等 bitwarden.com 實測後再做。
  - 2FA 流程已接上 IPC 與 CLI，但還沒有開了 2FA 的帳號可以實測。

### 12.3 M3 的實作決定與限制

- **整合測試**：`scripts/e2e.sh` 會用 docker 起一個用完即丟的 Vaultwarden，用 probe 建立資料夾與項目（模擬另一台裝置），再透過 daemon 與 CLI 跑 36 項檢查。daemon 以 `RUST_LOG=trace` 執行，最後確認主密碼、項目密碼、自訂欄位、TOTP 金鑰、產生的密碼都**沒有**出現在 log、資料目錄（只有密文）或 daemon 的 argv 中。CLI 的每次呼叫都用 `setsid` 拿掉控制終端，模擬腳本環境；另外用 `script` 模擬終端機，驗證上鎖時會詢問主密碼並重試。
- **政策**：`item.get`、`item.totp` 歸類為 `ReadSecret`，走 `AskUser`（階段 1 由 `AutoApprover` 放行），`ApprovalRequest` 會帶上請求的項目與欄位，階段 2 的確認視窗可以直接顯示。`item.list`、`generate.*` 直接允許。
- **閒置計時**：`item.list`、`item.get`、`item.totp` 成功時才重設。
- **組織權限**：項目的 `view_password` 為 false 時，`password`、`totp` 與隱藏型自訂欄位一律回 `denied`。
- **產生器**：不需要登入或解鎖，上鎖時也能用。預設密碼長度 20、不含特殊字元；passphrase 預設 5 個字、以 `-` 分隔（SDK 預設 3 個字太弱）。
- **`run`**：只替換「整個值」就是 `nw://` 參照的環境變數；同一個參照只向 daemon 要一次。秘密會出現在子程序的環境變數中，這是 `run` 的目的；nibwarden 自己不保留。
- **限制，之後處理**：
  - 記憶體中的明文只能盡力清除：SDK 的 `CipherView` 欄位、`serde_json::Value` 都是一般 `String`，不會 zeroize。我們自己的緩衝區與回傳值有用 `Zeroizing`。
  - 項目設定了「需重新輸入主密碼」（reprompt）時，目前不會額外要求；等階段 2 的確認視窗一起做。
  - 連結型（linked）自訂欄位不支援，會提示改讀它指向的欄位。
  - `run` 不會遮蔽子程序輸出中的秘密（1Password 的 `op run` 會）。

### 12.4 M4 的實作決定與限制

- **`Bitwarden-Client-Version`**：Vaultwarden 在 `/sync` 中，對沒送或低於 `2024.12.0` 的客戶端濾掉 SSH 金鑰項目（`atype == 5`），官方伺服器也有同類的最低版本門檻。這個值在伺服器端是功能門檻而不是身分，所以我們送「能解鎖 SDK 已支援功能的最低版本」`2024.12.0`，碰到下一個門檻再調高，而不是冒充最新的官方版本（`nibwarden-core/src/platform.rs` 的 `FEATURE_LEVEL`）。
  - 副作用：在這個改動之前登入的工作階段，要等帳號有變動或執行 `nibwarden sync --force`，SSH 金鑰才會出現，因為一般同步看到修訂時間沒變就會略過。
- **分層**：`nibwarden-ssh-agent` 只處理協定與簽章，透過 `Backend` trait 向 daemon 要公鑰清單與單把私鑰；上鎖判斷、政策、通知都在 daemon 的 `ssh_agent.rs`。私鑰只在簽章時取出，用 `Zeroizing` 包著，簽完即丟。
- **簽章**：支援 ed25519、ecdsa P-256／P-384／P-521、RSA。RSA 依旗標使用 `rsa-sha2-256`／`rsa-sha2-512`；沒帶旗標時用舊的 SHA-1 `ssh-rsa`，與 OpenSSH 的 agent 一致。簽章前會確認私鑰確實對應請求的公鑰。
- **政策**：每次簽章都是 `Operation::SshSign { key, purpose }`，走 `AskUser`（階段 1 自動放行）。用途從資料解析：SSHSIG 取 namespace（`git` 顯示為「git signing」），SSH 登入取使用者名稱。這些位元組由客戶端控制，只用於顯示與日誌。
- **閒置計時**：簽章成功才算 vault 使用；列公鑰不算，否則 ssh 每次連線都會延長解鎖時間。
- **Peer**：agent 連線一樣做 uid 檢查與 pidfd 鎖定，日誌與核准請求都帶上請求者，例如 `/usr/bin/ssh-keygen (pid 1234)`。
- **整合測試**（`scripts/e2e.sh`，共 53 項）：
  - 用 `ssh-keygen` 產生 ed25519、RSA 3072、ECDSA P-521 三把金鑰，經 probe 匯入後**刪掉磁碟上的私鑰檔**。否則 ssh 與 `ssh-keygen` 會在 agent 沒給金鑰時，自動改用 `.pub` 旁邊的私鑰檔，測試就證明不了簽章來自 agent。
  - `ssh-add -L`／`-l`、三種金鑰的 `ssh-keygen -Y sign` 加 `-Y verify`、對使用者模式 `sshd` 的實際 ssh 登入（RSA 走 `rsa-sha2-512`）、暫存 repo 中的 `git commit -S` 加 `git verify-commit`。
  - 上鎖後：`ssh-add -L` 沒有金鑰、簽章與登入都失敗、三次請求只發出一則通知（以假的 `notify-send` 記錄），解鎖後 agent 恢復提供金鑰。
  - 私鑰內容也加入外洩檢查（trace 日誌、資料目錄、daemon argv）。
- **限制，之後處理**：
  - 每次列公鑰都會解密所有 SSH 金鑰項目；項目很多時可以改成快取公鑰清單（公鑰不是秘密）。
  - 不支援 SSH 憑證（certificate）與 FIDO（`sk-`）金鑰。
  - 階段 2 的確認視窗與「上鎖時先保留請求、解鎖後繼續」（§8.1）尚未實作。

### 12.5 M5 的實作決定與限制

- **`git-setup` 與 `nibwarden-ssh-sign`**：見 §8.3。CLI crate 改成 lib + 兩個 bin，`connect`（含 systemd 自動啟動）與「上鎖時在終端機解鎖」由兩者共用。
- **release 版只接受 HTTPS 伺服器**：SDK 的 HTTP client 在非 debug 編譯時設定 `https_only(true)`，對 `http://` 伺服器只會回一個看不懂的 reqwest「builder error」。daemon 現在在 release 版登入時先檢查，直接回「the server URL must start with https://」。
  - 這也代表在區網用 `http://` 架 Vaultwarden 的使用者必須加上 TLS；我們不打算繞過 SDK 這個限制。
  - `scripts/e2e.sh` 用 debug 版，所以仍可對 `http://` 的 Vaultwarden 測試。
- **打包選 deb**（`packaging/deb/build.sh`；公開前改為 `cargo xtask dist`，見 phase2.md §12.18）：開發機與主要目標是 Ubuntu；只需要 `dpkg-deb`，不需要 debhelper 或 Debian 原始碼套件。
  - 內容：`/usr/bin/{nibwarden,nibwardend,nibwarden-ssh-sign}`（已 strip）、`/usr/lib/systemd/user/nibwardend.service`、`/usr/share/doc/nibwarden/`。
  - 執行檔只連結 libc、libm、libgcc_s（TLS 是 rustls），所以 `Depends` 是 `libc6 (>= <用到的最新 GLIBC 符號版本>)` 加 `libgcc-s1`，由腳本從 `objdump -T` 算出。`Recommends: openssh-client, git`，`Suggests: libnotify-bin`（上鎖通知用的 `notify-send`）。
  - **不**自動設定 `SSH_AUTH_SOCK`：那會蓋掉桌面原有的 agent。範例檔放在 `/usr/share/doc/nibwarden/examples/60-nibwarden-ssh-agent.conf`，使用者自行複製到 `~/.config/environment.d/`。git 簽章不受影響，因為走 `nibwarden-ssh-sign`。
- **安裝測試**（`scripts/deb-test.sh`，對應 M5 驗收條件；P2-M6 起改名為 `scripts/package-test.sh deb`）：
  - 在乾淨的 `ubuntu:26.04` 容器裡 `apt-get install` 這個 .deb，以一般使用者手動啟動 `nibwardend`（容器裡沒有 systemd）、登入、`ssh git-setup --global`，然後 `git commit` 與 `git verify-commit`，並確認簽章的指紋是 vault 裡那把金鑰。
  - 因為 release 版只走 HTTPS，測試用一次性的 CA 簽 Vaultwarden 的 TLS 憑證：容器用 `update-ca-certificates` 信任它，主機上的 probe 用 `SSL_CERT_FILE`。
  - 同時確認對 `http://` 伺服器登入會得到上述的明確錯誤。
- **整合測試**（`scripts/e2e.sh`，共 63 項）新增：
  - `git-setup` 要求選金鑰、不在 repo 時提示 `--global`、名稱不分大小寫、重複執行不會重複寫入 allowed signers。
  - 把 `SSH_AUTH_SOCK` 指到不存在的路徑後 `git commit` 仍能簽章並通過 `verify-commit`、`%G?` 為 `G`；`--global` 對沒有自己設定的 repo 生效，並用 RSA 金鑰簽（以 `%GF` 比對指紋）。
  - vault 上鎖時：沒有終端機的 `git commit` 失敗並提示 `nibwarden unlock`；有終端機（`script`）時詢問主密碼、解鎖後完成簽章。
  - 測試期間用 `GIT_CONFIG_GLOBAL`、`GIT_CONFIG_NOSYSTEM`、`XDG_CONFIG_HOME` 隔離，不讀也不寫使用者自己的 git 設定與 allowed signers。
- **尚未驗證／之後處理**：
  - 裝好的 systemd user unit 搭配 CLI 自動啟動，仍未在真的 systemd 工作階段上測過（容器裡沒有 systemd）。
  - rpm、AUR、Nix 未做；deb 也還沒有 changelog、man page 與 lintian 檢查。
  - 沒有終端機時（例如 GUI git 客戶端）vault 上鎖只能失敗，要等階段 2 的解鎖視窗。

---

## 13. 里程碑

| 里程碑 | 內容 | 驗收標準 |
|---|---|---|
| **M1 SDK 探路**（Vaultwarden 部分已完成） | workspace 骨架、`nibwarden-core::sdk`；用一個小程式完成 login、sync、unlock、列出項目 | 對 Vaultwarden 與 bitwarden.com 都成功；Q1 到 Q3 有結論 |
| **M2 Daemon + IPC**（已完成，見 §12.2） | `nibwardend` 的狀態機、control socket、peer 驗證、systemd unit | `nibwarden status/login/unlock/lock/sync` 可用；daemon 重啟後是 Locked 而不是 LoggedOut |
| **M3 CLI 查詢**（已完成，見 §12.3） | `list`、`get`、`read`、`run`、`generate`、`totp` | 整合測試通過；秘密不會出現在 argv、環境變數或日誌中（用測試檢查） |
| **M4 SSH agent**（已完成，見 §12.4） | `ssh-agent.sock`、上鎖時的行為、通知 | `ssh-add -L`、ssh 登入、`ssh-keygen -Y sign/verify` 測試通過 |
| **M5 Git 簽章與打包**（已完成，見 §12.5） | `ssh git-setup`；deb、rpm、AUR 或 Nix 擇一 | 在乾淨環境中安裝後，`git commit -S` 與 `git verify-commit` 可通過 |
