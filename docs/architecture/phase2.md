# nibwarden 階段 2 架構設計

- 狀態：草稿
- 日期：2026-11-02
- 範圍：**只做 Linux，並且做完整**：GUI（解鎖視窗、使用前確認、快速存取）、上鎖時保留請求、螢幕鎖定與休眠時上鎖、即時同步、各發行版的打包與桌面整合
- 前提：階段 1（`phase1.md`）的 M1 到 M5 已完成，本文只描述新增與改變的部分

---

## 1. 目標與範圍

階段 1 做出了「一個 daemon、所有前端共用」的骨架，但只在有終端機時才好用：
vault 上鎖時，ssh 與 git 只能失敗並發一則通知；每次使用金鑰或秘密都自動放行；而且只支援 Linux。
階段 2 在 Linux 上補齊 1Password 式的桌面體驗，目標是 Linux 版可以當成日常主力使用。macOS 與 Windows 的實作延後到之後的階段，但平台抽象層在本階段先建好（E4、§8）。

### 1.1 階段 2 目標

1. **解鎖視窗**：vault 上鎖時，ssh、git、沒有終端機的 CLI 發出的請求**先保留**，daemon 跳出解鎖視窗並叫到前景，解鎖後請求自動繼續。
2. **使用前確認**：SSH 簽章、讀取秘密前跳出確認視窗，顯示「哪個程式、要用哪把金鑰或哪個項目、做什麼」，可記住同一個程式一段時間。項目設定了「需重新輸入主密碼」（reprompt）時，確認視窗改為要求主密碼。
3. **快速存取**：用快捷鍵叫出搜尋框，複製帳號、密碼或 TOTP；剪貼簿逾時自動清除。
4. **跟著系統上鎖**：螢幕鎖定、休眠時上鎖（可設定）。
5. **即時同步**：接 notifications hub（WebSocket），其他裝置修改 vault 後數秒內反映。
6. **Linux 發佈完整化**：deb 以外加上 rpm、AUR、Nix；在真實的 systemd 工作階段驗證自動啟動；`.desktop` 檔、圖示、man page。
7. **桌面環境**：GNOME 與 KDE Plasma 的 Wayland 為主要目標，X11 為次要目標。
8. **維持輕量**：沒有視窗時，常駐的只有 daemon；GUI 只在需要時啟動。

### 1.2 階段 2 不做

| 項目 | 預定階段 |
|---|---|
| 瀏覽器擴充套件整合：自己的擴充套件、自動填入 | 3（用官方擴充套件的「以生物辨識解鎖」改由 nibwarden 解鎖已在本階段做，§5.8） |
| 生物辨識解鎖（Touch ID、Windows Hello、fprintd） | 3（解鎖視窗的介面在本階段先預留，§5.3） |
| 遠端解鎖的範圍隔離、手機核准 | 3 |
| macOS 與 Windows 的實作 | 之後的階段（介面在本階段的平台抽象層先建好，§8） |
| vault 編輯介面（新增、修改項目） | 未排定；快速存取只讀 |
| refresh token 改存 Secret Service | 視時間放在 P2-M9（選），否則延到 3 |

---

## 2. 關鍵決策

### E1. GUI 工具組：Slint

評估過 GTK4、Qt、Tauri、iced、egui、Makepad、Dioxus（Desktop 與 Native）之後選 Slint（1.18）：

- **跨平台且輕量**：純 Rust，不需要另外安裝執行環境、不用網頁引擎；本階段只做 Linux，但之後移植 macOS 與 Windows 時不必換工具組。
- **無障礙與輸入法**：以 AccessKit 支援螢幕報讀器；輸入法可用（第三方實測：選字中的暫存字可能缺字，送出結果正確）。這兩點是 iced 與 Makepad 被排除的主因。
- **密碼欄位**：`TextInput` / `LineEdit` 有 `input-type: password`。
- **授權**：可選 GPLv3，與本專案的 GPL-3.0-only 相容。
- **排除的理由**：GTK 在 macOS 與 Windows 要附帶整套執行環境、Qt 太重；Tauri 與 Dioxus Desktop 依賴系統網頁引擎，記憶體與攻擊面都大；Dioxus Native 還在早期。

Linux 上建置需要 fontconfig 與 xkbcommon 的開發檔（本機已有）；用 winit 後端，Wayland 與 X11 都支援。
P2-M1 先做原型，實測執行檔大小、記憶體、冷啟動時間與前景化（§5.4），數字記錄在 §12.2。
只用軟體繪圖（`renderer-software`），不用 OpenGL：對一個小對話框，它啟動較快、記憶體少三分之二（§12.2）。

### E2. GUI 是獨立程序，由 daemon 按需啟動

```
nibwardend ──spawn──▶ nibwarden-gui prompt   （解鎖、確認；回答後結束）
nibwarden quick / 快捷鍵 ──▶ nibwarden-gui quick ──control socket──▶ nibwardend
```

- daemon 本身**不連結任何 GUI 函式庫**，維持無介面：在 SSH 遠端登入、沒有圖形環境的機器上照常運作，記憶體也不會因為 GUI 增加。
- 每次需要視窗才啟動 `nibwarden-gui`，回答後立刻結束。輸入過的主密碼留在 GUI 程序記憶體的時間只有幾秒（Slint 的字串型別不會 zeroize，見 §10）。
- 代價是每次開視窗都有冷啟動時間。P2-M1 會量；如果太慢（目標 < 300 ms），再考慮讓 GUI 常駐並在背景預熱。

### E3. 確認與解鎖走「繼承的管道」，不走 control socket

daemon 啟動 `nibwarden-gui prompt` 時，把一對管道接在子程序的 stdin/stdout 上，提示內容與使用者的回答都只走這對管道。

- **為什麼**：如果確認視窗是透過 control socket 回答，任何同 uid 的程式都能連上去冒充 GUI，自己按下「允許」。管道只有 daemon 親手啟動的子程序拿得到。
- GUI 執行檔的路徑由 daemon 決定（與 daemon 同目錄，或設定檔指定），不從請求者那裡讀。
- 這不能防禦能 ptrace 或注入其他程序的同 uid 惡意程式（階段 1 §9.2 的限制仍然存在），但擋掉了「連上 socket 就能自己核准自己」這條最便宜的路。

### E4. 先建平台抽象層 `nibwarden-platform`，只實作 Linux

跨平台移植需要的抽象**現在就建好**，而不是等移植時再改：

- 本階段第一步（P2-M0）建立 `nibwarden-platform` crate，把階段 1 散在 daemon、CLI、IPC 裡的 Linux 專屬程式碼搬進去（§8 列出清單），行為不變。
- 之後新寫的平台相關功能（螢幕鎖定與休眠、通知、剪貼簿、全域快捷鍵、前景化）一律直接寫進這個 crate。
- 其他 crate 不寫 `#[cfg(target_os = …)]`、不直接用 `libc`、`/proc`、Unix socket 型別；只透過 `nibwarden-platform` 的介面。
- macOS 與 Windows 的實作先是空殼：編譯得過，執行時回傳 `Unsupported`。這樣可以用 `cargo check` 檢查整個 workspace 在其他平台能否編譯，及早發現介面裡混進了 Linux 型別。
- 本階段只有 Linux 的實作需要可用、可測；其他平台的實作留到移植階段。

### E5. GUI 的執行緒模型

- Slint 的事件迴圈在主執行緒，與 daemon 溝通的 I/O 放在背景執行緒，透過 `slint::invoke_from_event_loop` 回到 UI。GUI 不使用 tokio 以外的 async runtime。
- 其他沿用階段 1 的決策（D1 到 D5），包括 release 版只接受 HTTPS 伺服器（`phase1.md` §12.5）。

---

## 3. 元件總覽

```
            ┌──────────────── 使用者工作階段 ────────────────┐
 ssh / git ─┤ ssh-agent socket ─┐                              │
 nibwarden ─┤ control socket ───┼─▶ nibwardend                 │
 (CLI)      │                   │    ├─ LockController ◀─ SessionEvents（螢幕鎖定、休眠）
            │                   │    ├─ UnlockGate：上鎖時把請求排隊，等待解鎖
            │                   │    ├─ PolicyEngine ─▶ GuiApprover ─┐
            │                   │    └─ PromptBroker ◀──────────────┘
            │                   │          │ spawn + 管道（stdin/stdout）
            │                   │          ▼
            │                   │    nibwarden-gui prompt（解鎖／確認視窗，回答後結束）
 快捷鍵 ────┤ nibwarden-gui quick ──control socket──▶ nibwardend（一般的 client）
            └────────────────────────────────────────────────┘
```

---

## 4. Daemon 的改動

### 4.1 UnlockGate：上鎖時保留請求

目前需要解鎖 vault 的請求在上鎖時立刻回 `locked`。改為：

1. 請求進入 `PromptBroker::unlock(daemon, peer)`（實作上 UnlockGate 併入了 PromptBroker）。
2. 第一個等待者開一個解鎖視窗；之後的等待者加入同一個視窗，daemon 以 `waiting` 訊息更新視窗上的清單（例如「還有 1 個程式在等待」）。
3. 解鎖成功：所有等待者繼續處理。使用者取消、關閉視窗或逾時（120 秒）：所有等待者都失敗，行為和階段 1 相同。所有等待者都放棄時（例如 ssh 被中斷），視窗也會關閉；在別處（例如終端機）解鎖時，視窗自動關閉並放行。
4. 只有「可以開視窗」時才排隊（§6）；否則回 `unavailable`，呼叫端照階段 1 的方式處理。

| 請求來源 | 階段 1 上鎖時 | 階段 2 上鎖時 |
|---|---|---|
| SSH agent `REQUEST_IDENTITIES` | 回空清單＋通知 | 等待解鎖，解鎖後回真正的清單；取消時回空清單（不通知，使用者剛看過視窗）；無法開視窗時回空清單＋通知 |
| SSH agent `SIGN_REQUEST` | `FAILURE`＋通知 | 等待解鎖後簽章；同上 |
| CLI（有終端機） | 在終端機問主密碼 | **不變**：終端機優先，SSH 遠端登入也能用 |
| CLI、`nibwarden-ssh-sign`（沒有終端機） | 失敗，提示 `nibwarden unlock` | 等待解鎖視窗 |

- ssh 等 agent 回覆時沒有逾時，所以讓 `REQUEST_IDENTITIES` 等待是安全的；這也是 1Password 的做法。
- CLI 不需要新的協定欄位：收到 `locked` 且沒有終端機時，自己呼叫 `lock.unlock_prompt` 等待視窗，成功後重試；收到 `unavailable` 就照舊回報 `locked`（結束碼 3）。`nibwarden-ssh-sign` 用同一段程式。

### 4.2 使用前確認（`approvals.rs`）

依使用者要求改為 1Password SSH agent 的模型（2026-11-02，參考 1Password 開發者文件的 SSH agent 安全性說明）：

- **SSH 簽章（登入、git）一律需要核准**，沒有關閉的選項。讀取秘密（`get`、`read`、`run`、`totp`）預設也要核准，但可在設定關閉；快速存取本身不需要（§7）。
- **核准的範圍**（設定 `[approval] remember`，一律在 vault 上鎖時失效）：
  - `application`（預設）：同一個應用程式（及它啟動的所有程式）在它結束前不再詢問。
  - `terminal`：同一個應用程式的同一個終端機分頁。
  - `request`：每次都問。
  - 視窗上另有「核准所有應用程式」：勾選後這把金鑰（或這個欄位）對所有程式有效，直到上鎖。
- **核准的有效時間**（`[approval] remember_hours`，跟 1Password 的「記住核准」一樣）：0（預設）＝直到上鎖；1、4、12 小時＝時間到或上鎖，以先到者為準。
- **「應用程式」**：請求者往上找父程序，停在使用者的服務管理器（`systemd --user`）、登入服務（`sshd`）、init 或讀不到執行檔的程序（別的使用者）之下的那一個，例如終端機模擬器或 IDE。以 pid 加上程序啟動時間辨識，所以應用程式結束後、同一個 pid 的新程序不會沿用核准（平台層的 `Peer.app`）。
  - 原本「只記住系統路徑下的程式」的限制拿掉了：核准綁在一個執行中的程序上，替換磁碟上的執行檔不會沿用它。
- **沒有視窗可問時**（遠端 SSH 工作階段、沒有桌面）：
  - SSH 簽章被拒。要在這種環境簽章，用主密碼授權這個終端機工作階段：`nibwarden ssh authorize`，或 git 簽章時由 `nibwarden-ssh-sign` 在終端機詢問。輸入主密碼本身就是核准，效力到上鎖為止。
  - 讀取秘密依 `[approval] without_window`（預設允許，維持 CLI 在 SSH 裡可用）。
- 項目設定了「需重新輸入主密碼」（reprompt）時：一律開 reprompt 視窗驗證主密碼（`Account::verify_password`，不改變上鎖狀態）；無法開視窗時拒絕。剛為這個請求輸入過主密碼時視為已驗證。
- **為某個請求解鎖，就等於允許它**（§5.3）：請求訊框可帶 `unlock`（`window` 或 `password`），vault 上鎖時 daemon 先照這個方式解鎖，並依上面的範圍記住。
- 確認視窗顯示請求者（名稱、路徑、pid）、它所在的應用程式、要做的事（讀哪個項目的哪個欄位，或用哪把 SSH 金鑰做什麼），以及這次核准的效力範圍。

### 4.3 SessionEvents：跟著系統上鎖

- 螢幕鎖定、休眠前上鎖，各自可在設定檔關閉（`[lock] on_screen_lock`、`on_sleep`，預設都開）。
- 透過 system bus 上的 logind（平台層的 `session_events`，用 zbus）：
  - session 的 `Lock` 訊號（`loginctl lock-session`）與 `LockedHint` 屬性變成 true（GNOME 與 KDE 的鎖定畫面出現時會設定）；只理會屬於目前使用者的 session（向 logind 查 `User.Sessions` 比對）。
  - manager 的 `PrepareForSleep(true)`：休眠前就上鎖，不等醒來後的閒置計時。
- 連不上 system bus（例如容器）時只記錄一行，其他功能照常。
- 使用者切換沒有獨立處理：切換時原使用者的 session 會鎖定，`LockedHint` 已涵蓋。

---

## 5. GUI 程序（`nibwarden-gui`）

### 5.1 子命令

| 子命令 | 誰啟動 | 與 daemon 的通道 | 結束時機 |
|---|---|---|---|
| `prompt` | 只由 daemon 啟動 | 繼承的 stdin/stdout 管道（§5.2） | 回答或取消後 |
| `quick` | 使用者（快捷鍵、`nibwarden quick`） | control socket，和 CLI 一樣的 client | 複製後或按 Esc |

`prompt` 檢查 stdin 不是終端機、且收到的第一個訊息格式正確，否則直接結束，避免被手動執行後當成一般程式使用。

### 5.2 Prompt 協定

沿用 IPC 的訊框（4 bytes 長度＋JSON），定義在 `nibwarden-ipc::prompt`。顯示給使用者的內容一律以資料傳送（名稱、代碼），句子由 GUI 依語言組成：

```jsonc
// daemon → gui：第一個訊息決定視窗種類
{ "kind": "unlock", "email": "a@example.com", "waiting": [{ "name": "ssh", "path": "/usr/bin/ssh", "pid": 1234 }],
  "remember_minutes": 0 }                                    // 只解鎖
{ "kind": "unlock", "email": "…", "waiting": [ … ], "remember_minutes": 10,
  "access": { "requester": { … }, "action": { "type": "ssh_sign", "key": "Work", "purpose": { "kind": "git" } } } }
                                                             // 解鎖並允許
{ "kind": "approve", "access": { … "action": { "type": "read_secret", "item": "GitHub", "field": "password" } },
  "remember_minutes": 10 }                                   // 已解鎖，確認
{ "kind": "reprompt", "email": "…", "access": { … } }        // 再次輸入主密碼
// daemon → gui：之後的訊息
{ "kind": "waiting", "waiting": [ … ] }                      // 等待清單更新
{ "kind": "retry", "reason": "wrong_password" }              // 讓視窗留著重試
// gui → daemon
{ "answer": "password", "password": "…", "remember": false } // unlock、reprompt
{ "answer": "allow", "remember": true }                      // approve
{ "answer": "cancel" }                                       // 拒絕、取消、關閉視窗
```

- 簽章用途：`login`（帶 `user`）、`git`、`file`（帶 `namespace`）、`unknown`。
- daemon 在結束時（成功、逾時、所有等待者放棄）關閉管道，GUI 看到 EOF 就結束；確認視窗在回答後立即結束。
- 每個視窗 120 秒逾時；一次只開一個視窗。

### 5.3 視窗

因為請求而跳出的視窗有三種情況（依使用者的要求確定）：

| 情況 | 視窗 | 送出代表 | 取消代表 |
|---|---|---|---|
| vault 上鎖，請求**需要**確認（§4.2 的政策判定為 `AskUser`） | **解鎖並允許**：請求內容（程式、操作、金鑰或項目、用途）加上主密碼欄位，一個視窗完成 | 解鎖，並允許畫面上列出的請求 | 拒絕所有列出的請求，vault 保持上鎖 |
| vault 上鎖，請求**不需要**確認 | **解鎖**：列出在等待的程式（P2-M1 已實作） | 解鎖，請求繼續 | 請求收到 `locked` |
| vault 已解鎖，請求需要確認 | **確認**：請求內容；「拒絕」為預設按鈕（按 Enter 不會誤核准），「允許」需要點擊或 Tab 移過去；可選「記住此程式 N 分鐘」 | 允許 | 拒絕 |

- 上鎖時不再另外跳確認：輸入主密碼本身就是比按「允許」更強的確認，而且畫面上已經寫明要允許的是什麼。
- 「解鎖並允許」視窗也提供「記住此程式 N 分鐘」，和確認視窗一致。
- 等待中的請求若有多個，畫面列出全部；送出時一併允許的只有畫面上列出的那些。視窗開著時才進來的新請求，視窗更新清單後才算數。
- **reprompt**：與解鎖相同的主密碼欄位，標題寫明項目名稱；不能記住。
- **快速存取**（使用者自己叫出，不是請求）：搜尋框、結果清單（名稱、帳號、資料夾）、快捷鍵：Enter 複製密碼、Ctrl+C 帳號、Ctrl+T TOTP。
- 預留「用生物辨識解鎖」按鈕的位置（階段 3）。
- 外觀與語言見 §5.5。

### 5.4 前景化（叫到前景）

daemon 是背景程序，作業系統通常不讓背景程序啟動的視窗搶走焦點。各平台的作法與**待驗證**的部分：

| 平台 | 作法 | 待驗證（P2-M1 原型） |
|---|---|---|
| Linux Wayland（GNOME、KDE） | xdg-activation：沒有 token 時視窗可能不會取得焦點 | GNOME 50 對沒有 token 的新視窗的處理；KDE Plasma 6 |
| Linux 後備 | 先送通知，帶「解鎖」動作；使用者點擊時通知伺服器提供 activation token，交給 GUI | GNOME 與 KDE 是否支援 freedesktop 通知規格的 activation token |
| Linux X11 | `_NET_ACTIVE_WINDOW` 加上時間戳 | 視窗管理員的防搶焦點設定 |

驗證結果決定每個桌面環境的預設行為，記在 §12。最差情況下的保底是：通知一定送得出去，點擊通知就能打開解鎖視窗。

GNOME 50（Wayland）已實測：由 systemd user service 啟動、沒有 activation token 的視窗，會直接出現在最前面並取得鍵盤焦點（Q2）。

### 5.5 外觀與語言

使用者設定檔 `config.toml`（Linux：`$XDG_CONFIG_HOME/nibwarden/config.toml`），由新的 `nibwarden-config` crate 讀取，daemon、CLI、GUI 共用：

```toml
[appearance]
theme = "system"      # "system"（跟隨系統）、"light"、"dark"
language = "system"   # "system"（跟隨系統），或語言標籤，例如 "zh-TW"、"zh-CN"、"en"
```

- 檔案不存在就全部用預設值；格式錯誤時記錄警告並用預設值，不讓 GUI 因此打不開。不認得的鍵直接忽略，讓舊版能讀新版的設定檔。
- **主題**：`system` 用 Slint 的 `Palette.color-scheme`，它在 GNOME Wayland 上會跟著系統的深淺色設定（已實測）；`light`／`dark` 由程式覆寫設計 token 的 `Theme.dark`。
- **語言**：
  - 介面字串在 `.slint` 裡用 `@tr()` 標記，翻譯是 gettext 的 `.po` 檔（`crates/nibwarden-gui/lang/<語言>/LC_MESSAGES/nibwarden-gui.po`），**編譯進執行檔**：不需要安裝 `.mo`，三個平台都一樣。
  - 目前有英文（原文）、正體中文（`zh_TW`）、簡體中文（`zh_CN`）。新增語言只要加一個 `.po` 檔並登記在 `appearance::TRANSLATIONS`；單元測試會檢查每個 `@tr()` 字串在每個語言都有翻譯。
  - `system` 依 gettext 的順序讀 `LANGUAGE`、`LC_ALL`、`LC_MESSAGES`、`LANG`（平台層的 `locale` 模組）。中文的 `zh_HK`、`zh_MO` 對應正體，其他（含只有 `zh`）對應簡體；沒有符合的語言就用英文。
  - daemon 送給 GUI 的不是句子而是代碼（例如 `retry` 的 `reason: "wrong_password"`），由 GUI 依語言顯示。
  - CLI 的訊息維持英文：和 git、ssh 一樣，腳本會比對這些輸出。
- 之後的設定（自動上鎖時間、確認政策、剪貼簿逾時）也放在同一個檔案。

### 5.6 設定視窗

`nibwarden-gui settings`（`nibwarden settings`，或快速存取的齒輪）：

| 區塊 | 項目 | `config.toml` |
|---|---|---|
| 外觀 | 主題（跟隨系統、淺色、深色）、語言（跟隨系統、English、正體中文、简体中文） | `[appearance]` |
| 鎖定 | 閒置後自動上鎖（1／5／15／30／60 分鐘、永不）、螢幕鎖定時上鎖、休眠前上鎖 | `[lock] idle_minutes`、`on_screen_lock`、`on_sleep` |
| 剪貼簿 | 自動清除複製的密碼與驗證碼（開關）、清除時間（30 秒到 5 分鐘） | `[clipboard] clear`、`clear_after_seconds` |
| 使用前確認 | 核准的範圍（單次請求、每個應用程式、每個終端機分頁）、核准的有效時間（直到鎖定、1／4／12 小時；單次請求時不顯示）、程式讀取密碼前詢問、無法顯示視窗時允許讀取秘密 | `[approval] remember`、`remember_hours`、`read_secret`、`without_window` |
| 快速存取 | 快捷鍵：一鍵寫入 GNOME 自訂快捷鍵 | （桌面設定） |
| SSH 與 Git | 使用 SSH agent（在 `~/.ssh/config` 結尾加上 nibwarden 的 `Host *`／`IdentityAgent` 區塊，自行指定 agent 的主機維持原設定）、用 SSH 金鑰簽署 git commit（全域 git config，與 `nibwarden ssh git-setup --global` 相同）、簽署用的金鑰（有多把時） | （`~/.ssh/config`、git config） |
| 瀏覽器 | 用 nibwarden 解鎖 Bitwarden 瀏覽器擴充套件（§5.8）；有瀏覽器登記的是 Bitwarden 桌面版時列出 | （瀏覽器的 native messaging manifest） |

- 開著的其他 nibwarden 視窗每秒檢查一次 `config.toml` 的修改時間，變了就重新套用主題與語言。

- 沒有「儲存」按鈕：每次改動立刻用 `toml_edit` 寫入那一個鍵（保留檔案裡其他內容與註解），再送 `config.reload` 給 daemon；daemon 沒在執行時，下次啟動會讀到。主題與語言在設定視窗裡立即生效。
- daemon 的 `--lock-timeout` 改為選用：有給就以命令列為準，否則用設定檔的 `idle_minutes`（預設 15 分鐘）。

### 5.7 登入與註冊

沒有登入時，主視窗顯示登入頁，不再只叫使用者去終端機執行 `nibwarden login`。流程照官方客戶端：

- **登入**：伺服器（bitwarden.com、bitwarden.eu、自架網址）、電子郵件、主密碼。帳號有兩步驟驗證時接著問驗證碼：驗證器 App 優先；只有電子郵件時自動寄出驗證碼；兩種都有時可以切換。沒有兩步驟驗證的帳號在 Bitwarden 不認得的裝置上登入時，伺服器會寄驗證碼確認新裝置（`device_error`）；這走同一個驗證碼頁面（標題「驗證新裝置」，可重新寄送），CLI 的 `nibwarden login` 也一樣會問。
- **註冊**：伺服器、電子郵件、名稱（選填）→ 伺服器寄出驗證信，使用者把信裡的連結貼回來（也可以直接開連結在瀏覽器完成，再回來登入）；伺服器不驗證地址時（沒設郵件的自架伺服器）直接進下一步 → 主密碼兩次與提示（選填），至少 12 個字元 → 建立後自動登入，顯示空的密碼庫。
- 快速存取沒有登入頁，只說明尚未登入，並提供「開啟 nibwarden」按鈕。

daemon 新增兩個請求，跟登入一樣只要求是同一個使用者：

| 請求 | 參數 | 結果 |
|---|---|---|
| `auth.register_start` | `server`、`email`、`name` | `{token}`：伺服器直接給的 token；`null` 表示已寄出驗證信 |
| `auth.register_finish` | `server`、`email`、`token`、`password`、`hint` | null；之後用 `auth.login` 登入 |

- 金鑰在 daemon 裡產生（PBKDF2 600,000 次，與官方客戶端新帳號相同），主密碼不離開本機。
- 主密碼以電子郵件為 salt，所以 daemon 在註冊與登入時都把地址去掉頭尾空白、轉成小寫，與官方客戶端相同；否則大小寫不同就會變成另一把金鑰。
- 第一步不會說地址是否已有帳號（伺服器刻意不透露），已存在要到完成時才會被拒絕，並顯示伺服器的說明。

### 5.8 官方瀏覽器擴充套件的桌面整合

使用者繼續用官方 Bitwarden 擴充套件（自動填入等），但擴充套件的「以生物辨識解鎖」改由 nibwarden 處理：nibwarden 取代 Bitwarden 桌面版在 native messaging 中的角色，不必在瀏覽器裡再打一次主密碼。

- **註冊**：`nibwarden browser enable`（或設定視窗「瀏覽器」）在已安裝的瀏覽器寫入 native messaging host 的 manifest：名稱 `com.8bit.bitwarden`（擴充套件寫死連這個名稱），程式是 `nibwarden-browser-host`，只允許官方擴充套件（Chrome 商店、Chrome beta、Edge、Opera 的 extension origin，Firefox 的 extension id；與桌面版相同）。位置與桌面版相同：`~/.mozilla/native-messaging-hosts/`、`~/.config/{google-chrome,chromium,microsoft-edge,vivaldi,BraveSoftware/Brave-Browser,net.imput.helium}/NativeMessagingHosts/`。某個瀏覽器已登記 Bitwarden 桌面版時預設不覆蓋（`--replace` 才覆蓋），`disable` 只刪 nibwarden 自己的。Nix 每次更新路徑都會變，所以 daemon 啟動時把 nibwarden 自己的 manifest 改指向目前的程式。
- **`nibwarden-browser-host`**：瀏覽器啟動它，stdin／stdout 是 native messaging（4 位元組本機位元組序長度＋JSON）。連得到 daemon 先送 `{"command":"connected"}`，連不到送 `disconnected` 後結束（擴充套件顯示桌面程式沒有執行），與桌面版的 `desktop_proxy` 一樣。之後每則訊息原封不動以 `browser.message` 轉給 daemon（每則一條連線：解鎖要等使用者，擴充套件同時可能查狀態），daemon 的回覆再寫回瀏覽器。協定全部在 daemon。
- **協定**（桌面版 `biometric-message-handler.service.ts` 的那一套，bitwarden.com 目前的 `biometrics-sdk-ipc` 旗標是關的，擴充套件走這一套）：
  1. 擴充套件送 `setupEncryption`（RSA-2048 公鑰、`userId`）。`userId` 不是 nibwarden 登入的帳號時回 `wrongUserId`。否則 daemon 產生一把 64 位元組的 AES-256-CBC-HMAC 金鑰，以 RSA-OAEP（SHA-1）加密給擴充套件，存在記憶體（以擴充套件的 `appId` 為鍵，登出時清除）。
  2. 之後的訊息都是以這把金鑰加密的 type 2 EncString，內含 `timestamp`，超過 10 秒的不理。解不開或沒有這個 `appId` 時回 `invalidateEncryption`，擴充套件會重新設定。
  3. 指令：`getBiometricsStatus` → 0（可用）；`getBiometricsStatusForUser` → 同一帳號 0，否則 8（桌面程式沒有啟用）；`canEnableBiometricUnlock` → true；`authenticateWithBiometrics`（擴充套件開啟此設定時）→ 開確認視窗，回 true／false；`unlockWithBiometricsForUser` → 開確認視窗，允許後回 `userKeyB64`（使用者金鑰）。
- **確認**：每次都問，不記住（交出的是整個密碼庫的金鑰）；不受「程式讀取密碼前詢問」關閉的影響。請求者顯示為啟動 host 的瀏覽器（peer 的 application）。nibwarden 上鎖時開「解鎖並允許」視窗：在 nibwarden 輸入主密碼，同時解鎖 nibwarden 與擴充套件。
- **限制**：Flatpak／Snap 版瀏覽器的 manifest 位置與沙箱不同，未支援；Bitwarden 若把 `biometrics-sdk-ipc` 打開，擴充套件會改用 SDK 的 IPC 協定，需要另外實作；與 Bitwarden 桌面版同時使用時，同一個瀏覽器只能登記其中一個。

---

## 6. 何時開視窗、何時用終端機

vault 上鎖時：

1. 請求來自有終端機的 CLI → 由 CLI 在終端機詢問主密碼，連同請求一起送出（`unlock: password`），解鎖並允許。透過 SSH 解鎖也是走這條。
2. 沒有終端機 → 請 daemon 開「解鎖並允許」視窗（`unlock: window`），條件是：
   - daemon 的環境有 `WAYLAND_DISPLAY` 或 `DISPLAY`（systemd user unit 從圖形工作階段繼承；GNOME 已確認，Q3）。
   - 請求者**不是**遠端 SSH 工作階段（請求者所屬的 logind session 不是 remote）。否則有人從遠端執行 `ssh` 或 `git`，視窗卻跳在本機螢幕上，坐在電腦前的人一按就核准了。
3. 都不行 → 回 `locked`，行為和階段 1 相同。

- SSH agent 沒有終端機可用：上鎖時簽章一律走第 2 條；公鑰清單用最近一次解鎖時記下的（§12.4）。
- `nibwarden-ssh-sign`：先問 daemon 能否為它開視窗（`lock.can_prompt`）。能 → 直接交給 `ssh-keygen`，由 agent 在簽章時開「解鎖並允許」視窗；不能 → 在終端機解鎖（第 1 條）。
- vault 已解鎖、需要確認時：同樣的條件下開確認視窗；不能開時依 `without_window` 設定。

---

## 7. 快速存取

`nibwarden-gui quick`（或 `nibwarden quick`）：搜尋 vault、複製一個欄位，像 1Password 的 Quick Access。

- **操作**（參考 1Password Quick Access 的快捷鍵文件與 Bitwarden 的項目檢視）：
  - 清單：打字即搜尋，上下鍵選擇；選取的那一列直接有「複製使用者名稱／密碼／驗證碼」按鈕。
  - Enter、右鍵頭或點擊打開項目：每個欄位一列（使用者名稱、密碼、一次性驗證碼、網站、自訂欄位、備註），點整列或複製鈕即複製；秘密欄位可顯示／隱藏；驗證碼即時顯示並倒數，到期自動換新（需要再次輸入主密碼的項目不自動顯示）。
  - 快捷鍵與 1Password 相同：Ctrl+C 使用者名稱、Ctrl+Shift+C 密碼、Ctrl+Alt+C 一次性驗證碼，清單與項目中都可用。Esc 依序是清除搜尋、返回清單、關閉；項目中左鍵頭返回。
  - 複製後只顯示短暫的「已複製密碼」提示，視窗保持可用。
  - 搜尋在 GUI 內進行（`search::rank`）：每個字都要出現在名稱、使用者名稱、資料夾或網址之一；名稱以查詢開頭者最前，其次是名稱中某個字以它開頭、名稱包含它，最後是只在其他欄位符合；同分時最愛優先。
  - vault 上鎖時，視窗本身顯示主密碼欄位（直接送 `lock.unlock`），解鎖期間顯示旋轉動畫（金鑰衍生約需數秒）。
  - 一次只開一個：`$XDG_RUNTIME_DIR/nibwarden/quick.lock`（`flock`）；已開著時再按快捷鍵不做任何事。
  - 項目內容來自新的 `item.view`：只有不敏感的值（名稱、資料夾、使用者名稱、網站、文字型自訂欄位）與「有哪些秘密」，秘密在顯示或複製時才以 `item.get`／`item.totp` 讀取。
  - 標題列右上角的齒輪開啟設定（§5.6）。
- **叫出方式**：Wayland 不允許一般程式自行登記全域快捷鍵。
  - 目前：`nibwarden quick --install-shortcut [--binding '<Control><Shift>space']` 在 GNOME 寫入自訂快捷鍵（gsettings 的 `custom-keybindings`），重複執行會取代原本的設定。其他桌面先顯示指令，請使用者在桌面設定裡自行加入。
  - 之後：GNOME 50 已提供 GlobalShortcuts portal（Q4），但非沙盒程式要先處理 portal 的 app id 識別，且需要常駐程序（daemon）登記；KDE 也走這條。
- **剪貼簿**（平台層的 `clipboard`，以 `clipboard` feature 隔開，daemon 不連結）：
  - 用 `arboard` 寫入，並標記為敏感內容（KDE 的 `x-kde-passwordManagerHint`），剪貼簿歷史工具會略過。
  - Linux 的剪貼簿由設定它的程式持續提供，所以關閉視窗後程式留在背景：到時間（`[clipboard] clear_after_seconds`，預設 90 秒，最少 5 秒）在內容仍是我們放的時清除；關閉自動清除時，等到剪貼簿被別的內容取代才結束。開關與時間在設定視窗裡調整。
  - 已在 GNOME 50（Wayland）實測：其他程式讀得到，清除後為空（`cargo run -p nibwarden-platform --features clipboard --example clipboard-check`）。
- **確認**：快速存取是使用者直接操作，daemon 認得自己的 GUI（請求者的執行檔就是設定的 `nibwarden-gui`），讀取不另跳確認視窗；需要重新輸入主密碼的項目照樣要求。

---

## 8. 平台抽象層（`nibwarden-platform`）

| 模組 | 介面 | Linux（本階段實作） | 階段 1 的現況 | macOS（之後） | Windows（之後） |
|---|---|---|---|---|---|
| `paths` | 資料、設定、執行時目錄 | XDG | `nibwarden-ipc::paths`、daemon | `~/Library/Application Support`；socket 放 `$TMPDIR`（路徑上限 104 bytes） | `%LOCALAPPDATA%` |
| `transport` | 本機 IPC 的 listener、stream、client 連線；只允許目前使用者 | Unix socket，`0600`，目錄 `0700` | daemon 的 `bind()`、IPC client | Unix socket | Named pipe，DACL 只允許目前使用者 |
| `peer` | 連線對方的身分：pid、使用者、執行檔路徑、是否為遠端工作階段；pid 重用防護 | `SO_PEERCRED` + pidfd + `/proc/<pid>/exe`、logind session | `nibwarden-daemon::peer` | `LOCAL_PEERCRED` + `LOCAL_PEERTOKEN` + `proc_pidpath` | `GetNamedPipeClientProcessId` + token SID + `QueryFullProcessImageNameW` |
| `clock` | 包含休眠時間的單調時鐘 | `CLOCK_BOOTTIME` | daemon 的閒置計時 | `mach_continuous_time` | `QueryInterruptTime` |
| `hardening` | 禁止 core dump、禁止 debugger 附加 | `PR_SET_DUMPABLE=0`、`RLIMIT_CORE=0` | daemon 的 `main` | `PT_DENY_ATTACH`、`RLIMIT_CORE=0` | `SetProcessMitigationPolicy`、關閉 WER dump |
| `service` | 按需啟動 daemon、安裝自動啟動 | `systemctl --user start`、systemd user unit | CLI 的 `connect` | LaunchAgent | 登入啟動項（不是服務：服務在 session 0，不能開視窗） |
| `process` | 以另一個程式取代自己（`run`、`nibwarden-ssh-sign`） | `exec` | CLI | `exec` | 啟動子程序、等待、回傳結束碼 |
| `terminal` | 是否有終端機、在終端機上讀取輸入 | `/dev/tty` | CLI | `/dev/tty` | `CONIN$` |
| `notify` | 桌面通知，可帶「解鎖」動作與 activation token | freedesktop Notifications（D-Bus，取代 `notify-send`） | daemon 的 `ssh_agent` | `UNUserNotificationCenter` | Toast |
| `session_events` | 螢幕鎖定、使用者切換、休眠前的事件 | logind D-Bus | 無（新功能） | `NSWorkspace` 通知 | `WTSRegisterSessionNotification`、`WM_POWERBROADCAST` |
| `clipboard` | 寫入敏感內容、逾時清除 | Wayland／X11（`x-kde-passwordManagerHint`） | 無（新功能） | `org.nspasteboard.ConcealedType` | `ExcludeClipboardContentFromMonitorProcessing` |
| `shortcut` | 全域快捷鍵 | GlobalShortcuts portal，後備為寫入桌面設定 | 無（新功能） | `global-hotkey` | `global-hotkey` |
| `foreground` | 把 GUI 視窗叫到前景 | xdg-activation、`_NET_ACTIVE_WINDOW` | 無（新功能） | `LSUIElement` app 加上 activate | `SetForegroundWindow` 與後備 |

- SSH agent 用的也是 `transport`。Windows 上的 agent pipe 名稱與系統 `openssh-ssh-agent` 的衝突，到移植時再處理。
- `foreground` 只有 GUI 會用；`nibwarden-platform` 以 feature 區分，daemon 不會因此連結到 GUI 相關的依賴。

---

## 9. Workspace 結構（新增與搬動）

```
crates/
├── nibwarden-platform/        # 新：平台抽象層（§8），本階段只實作 Linux
├── nibwarden-ipc/
│   └── prompt.rs              #   新：daemon ↔ GUI 的提示協定（§5.2）
├── nibwarden-daemon/
│   ├── unlock_gate.rs         #   新：上鎖時排隊等待（§4.1）
│   ├── prompt_broker.rs       #   新：啟動 GUI、管道、逾時、合併多個等待者
│   ├── approver.rs            #   新：GuiApprover 與記住的核准（§4.2）
│   ├── push_sync.rs           #   新：notifications hub（P2-M5）
│   └── browser.rs             #   新：官方瀏覽器擴充套件的協定（§5.8）
├── nibwarden-cli/
│   └── bin/browser_host.rs    #   新：nibwarden-browser-host（§5.8）
├── nibwarden-gui/             # 新：binary nibwarden-gui（Slint）
│   └── ui/*.slint             #   unlock、approve、quick 三個視窗
└── …（其餘沿用階段 1）
```

依賴方向：`gui → ipc, platform`；`cli → ipc, platform`；`ipc → platform`；`daemon → core, ipc, platform, ssh-agent`；`platform` 不依賴其他 nibwarden crate；`gui` 不依賴 `core`，所以 GUI 程序裡沒有 SDK 與 vault 金鑰。

---

## 10. 安全模型的變化

| 項目 | 說明 |
|---|---|
| 主密碼經過 GUI 程序 | Slint 的字串不會 zeroize，主密碼會留在 GUI 程序記憶體直到程序結束；所以 GUI 回答後立刻結束（E2）。送進管道前轉成 `Zeroizing<String>`。 |
| 冒充確認視窗 | 確認只走 daemon 啟動的子程序的管道（E3）。 |
| 冒充請求者 | 確認視窗顯示的程式身分來自作業系統（peer 身分），不是請求者自報；用途字串標示為「程式宣稱」。 |
| 遠端觸發本機視窗 | 來自遠端 SSH 工作階段的請求不在本機開視窗（§6）。 |
| 同 uid 惡意程式 | 使用前確認讓「在背景默默取資料」變得困難，但能 ptrace 或注入的程式仍然可以繞過，限制與階段 1 相同，要寫進使用者文件。 |
| 鍵盤側錄 | X11 上任何程式都能監聽鍵盤，無法防禦；Wayland 不允許。文件建議使用 Wayland 工作階段。 |
| 剪貼簿 | 敏感標記與逾時清除（§7）。 |
| 瀏覽器擴充套件拿到使用者金鑰 | 與 Bitwarden 桌面版相同：允許後擴充套件拿到使用者金鑰，以它解鎖。只有使用者在視窗允許（或解鎖）後才給，每次都問；能連 control socket 的同 uid 程式也可以假裝成擴充套件，但一樣得經過這個視窗，視窗顯示的是作業系統認定的程式。通道的 RSA 交換沒有驗證對方，這也與桌面版相同。 |

---

## 11. 測試策略

- **GUI 與 daemon 分開測**：`PromptBroker` 啟動的 GUI 路徑可用環境變數 `NIBWARDEN_GUI` 換掉。e2e 用一個腳本扮演 GUI，依劇本回答（輸入主密碼、允許、拒絕、逾時），這樣 `scripts/e2e.sh` 不需要圖形環境就能涵蓋 §4 的全部流程。
- **GUI 本身**：視窗邏輯放在 Rust（狀態、驗證、協定），用單元測試；Slint 畫面用 Slint 的測試後端做冒煙測試（點按鈕、輸入文字）。
- **前景化與真實桌面**：手動測試清單（GNOME Wayland、KDE Plasma Wayland、X11），每個里程碑結束時跑一次。KDE 用 VM 或巢狀 KWin 測。
- **打包**：每種套件都在對應發行版的乾淨容器裡安裝並跑 git 簽章（延續 `scripts/deb-test.sh`）：Ubuntu、Fedora、Arch；Nix 用 `nix build` 加 `nix run`。

---

## 12. 待確認問題與風險

| # | 問題 | 狀態 | 處理方式 |
|---|---|---|---|
| Q1 | Slint GUI 的冷啟動時間、記憶體、執行檔大小是否符合「輕量」（目標：啟動 < 300 ms、記憶體 < 50 MB） | **已解決** | 軟體繪圖：約 40 ms、21–25 MiB；不需要常駐（§12.2） |
| Q2 | GNOME 與 KDE 的 Wayland 上，背景程序啟動的視窗能否取得焦點；通知動作的 activation token 是否可用（§5.4） | **GNOME 已解決** | GNOME 50：直接出現在最前面、可直接輸入（使用者以示範腳本實測（腳本已移除））；KDE 在 P2-M2 驗證 |
| Q3 | systemd user unit 是否取得 `WAYLAND_DISPLAY`／`DISPLAY`（§6） | **GNOME 已確認** | Ubuntu 26.04 GNOME 50：`systemctl --user show-environment` 有 `WAYLAND_DISPLAY=wayland-0`、`DISPLAY=:0`；KDE 在 P2-M2 確認 |
| Q4 | GNOME 的 GlobalShortcuts portal 支援程度（§7） | **部分解決** | GNOME 50 有 portal（version 1）；本階段先用 gsettings 自訂快捷鍵，portal 留到之後（§7） |
| Q5 | Wayland 上剪貼簿逾時清除需要 GUI 延後結束（§7） | **已解決** | GUI 複製後留在背景直到清除；GNOME 50 實測可讀、可清除 |
| Q6 | sdk-internal 是否提供 notifications hub 的 client，或需要自己實作 SignalR 協定 | **已解決** | SDK 只有推播類型的 API 模型，沒有 hub client；自己實作（§12.8） |
| Q7 | bitwarden.com 的實測（階段 1 遺留） | 待使用者提供測試帳號 | 任何時候都可以做 |
| Q8 | 在 Linux 開發機上能否對 macOS 與 Windows 目標跑 `cargo check`（`ring` 等依賴的建置腳本可能需要對應的 C 工具鏈） | **已解決** | Windows 可以檢查整個 workspace；macOS 只能檢查不依賴 SDK 的 crate（§12.1） |

### 12.1 P2-M0 的實作決定與結果

- **`nibwarden-platform` 的模組**：`paths`、`fs`、`transport`、`peer`、`clock`、`hardening`、`service`、`process`、`terminal`、`notify`、`signal`。§8 表中的 `session_events`、`clipboard`、`shortcut`、`foreground` 隨各自的里程碑加入。
  - 每個模組的公開介面是平台無關的函式與型別，內部 `#[cfg(target_os = "linux")] mod imp` 與 `#[cfg(not(target_os = "linux"))] mod imp` 各一份。
  - 其他平台的空殼：`transport` 的 `Listener`／`Stream` 與 `peer` 的程序把手是**無值型別**（空的 enum），所以「拿到一條連線卻不知道對方是誰」在型別上就不可能發生；其他函式回傳 `ErrorKind::Unsupported`。
  - 例外：`clock` 在其他平台暫時用 `Instant`（休眠時可能停止計時），`signal` 用 `ctrl_c`。這兩個在其他平台本來就能正確運作，只是不完整。
- **搬過來的程式碼**（行為不變）：daemon 的 `peer.rs`、`bind()`、`harden()`、`boot_clock()`、資料目錄、訊號處理、`notify-send`；IPC 的 `paths` 與 client 的 Unix socket；core 的檔案權限；CLI 的 `systemctl` 自動啟動、`/dev/tty`、`exec`。
  - 搬完之後，daemon、CLI、IPC、core 都不再直接依賴 `libc` 或 `rpassword`，也沒有 `std::os::unix`、`/proc`、`/dev/tty`、`cfg(target_os)`。
- **順手的小變動**：
  - 資料目錄也比照 socket 目錄檢查擁有者：不屬於目前使用者就拒絕啟動（之前只建立、不檢查）。
  - `Listener` 被 drop 時自己刪除 socket 檔，daemon 不再手動清理。
  - CLI 自動啟動失敗的訊息不再寫死 systemd。
  - 被拒絕的外部 uid 連線，日誌裡也記下對方的執行檔。
- **Q8 的結果**（`scripts/cross-check.sh`，以 `-D warnings` 執行；公開前改由 CI 與 `cargo xtask check --target` 取代，§12.18）：
  - Windows（`x86_64-pc-windows-gnu`）：整個 workspace，包括 SDK，都能 `cargo check`；需要 mingw-w64 編 `ring` 與 SQLite 的 C 程式。
  - macOS（`aarch64-apple-darwin`）：`ring` 與 `libsqlite3-sys` 需要 Apple SDK 的標頭檔，Linux 上沒有；所以只檢查不依賴 SDK 的 `platform`、`ipc`、`ssh-agent`、`cli`。完整檢查要等 macOS 的 CI。
- **驗證**：e2e 63 項、deb 安裝測試、單元測試（`platform` 新增 transport 的 bind／stale／權限／清理測試，peer 測試從 daemon 搬來）、clippy、fmt。

### 12.2 P2-M1 的實作決定與結果

- **流程**：`nibwarden unlock --gui` → control 請求 `lock.unlock_prompt` → daemon 的 `PromptBroker` 啟動 `nibwarden-gui prompt`，用管道交談（`nibwarden-ipc::prompt`）→ 主密碼錯誤時回 `retry`，視窗留著讓使用者重試 → 解鎖成功後 daemon 關閉管道，視窗隨之結束。
  - 一次只開一個解鎖視窗；120 秒逾時；GUI 沒在 2 秒內結束就強制終止。
  - 沒有圖形環境、找不到 GUI 時回新的錯誤碼 `unavailable`，訊息寫明原因。
  - 提示裡的請求者是結構化的（名稱、路徑、pid，來自 peer 身分），不是字串。
  - GUI 路徑：`--gui`／`NIBWARDEN_GUI`，預設是 daemon 旁邊的 `nibwarden-gui`。
- **GUI 程式結構**：主執行緒跑 Slint 事件迴圈；I/O 執行緒用一個單執行緒 tokio runtime，讀與寫是兩個獨立的迴圈（避免在 `select!` 中途取消而截斷訊框）。視窗關閉時，最後的「取消」會等它寫出去（最多 1 秒）再結束。
- **外觀**：使用者要求做到 1Password 的水準。不用 std-widgets 的預設外觀，改用自己的元件（`ui/theme.slint` 的設計 token、`ui/components.slint` 的按鈕、密碼欄位、請求者卡片、圖示），淺色與深色主題跟著系統。
  - 版面：品牌圖示、標題、帳號、「哪個程式在等」卡片、大的密碼欄位（顯示密碼切換、欄位內的送出箭頭）、錯誤訊息、取消。Esc 取消、Enter 送出。
  - 軟體繪圖不支援漸層的圓角與陰影，所以圖示只用純色。
  - `cargo run -p nibwarden-gui --example snapshot -- <dir>` 不需要顯示器就能把視窗畫成 PNG（淺色、深色、錯誤狀態），用來檢查設計。
- **Q1 實測**（本機 Ubuntu 26.04、GNOME 50 Wayland；冷啟動到視窗出現；記憶體是視窗開著時的 RSS）：

  | 組態 | 執行檔（strip 後） | 啟動 | 記憶體 |
  |---|---|---|---|
  | release，femtovg（OpenGL） | — | 約 100 ms（第一次 340 ms） | 65–69 MiB |
  | release，軟體繪圖 | 19.9 MiB | 約 40–55 ms | 25 MiB |
  | 軟體繪圖，加上 `lto = "fat"`、`codegen-units = 1`、`opt-level = "s"`、`panic = "abort"` | 10.9 MiB | 約 40 ms | 21 MiB |

  - 結論：GUI 按需啟動就夠快，不需要常駐（E2 維持）；OpenGL 只多花記憶體，拿掉。
  - 最後一列的編譯設定在 P2-M6 打包時套用，而且只用在 GUI：`opt-level = "s"` 會拖慢 daemon 的 KDF 與加密，`panic = "abort"` 會讓 daemon 一個 task 出錯就整個結束。
  - 我先前估計「執行檔 3–5 MB」是錯的；實際最小約 11 MiB，主要來自 winit、字型處理與無障礙（AccessKit 與 D-Bus）。無障礙不能拿掉。
- **e2e**（69 項）：`scripts/e2e/fake-gui` 依劇本回答（錯誤密碼、正確密碼、取消），daemon 以固定的 `WAYLAND_DISPLAY` 啟動，所以不需要真的顯示器。涵蓋：解鎖成功、提示內容（帳號與請求者）、錯誤密碼重試、取消後仍上鎖。
- **跨平台**：`nibwarden-gui` 已加入 `scripts/cross-check.sh`，Windows 與 macOS 目標都能編譯。
- **Q2（GNOME）**：使用者以示範腳本實測（腳本已移除），從 systemd user service 開出的視窗直接在最前面、可直接輸入。這個腳本不碰 vault，第一次輸入的密碼會被故意拒絕，用來看錯誤狀態。
- **外觀與語言**（§5.5）：使用者要求支援多語系與主題（淺色、深色、跟隨系統），在 P2-M1 一起完成。新增 `nibwarden-config` crate、平台層的 `paths::config_dir` 與 `locale`，`retry` 改傳代碼；快照工具會輸出每種語言的淺色、深色、錯誤狀態。所有元件加上無障礙標籤（按鈕、顯示密碼、送出）。

### 12.3 P2-M2 的實作決定與結果

- **等待解鎖**（§4.1）：`PromptBroker` 同時是 UnlockGate。一個視窗一輪（`UnlockRound`），等待者以 `watch` 通道取得同一個結果；視窗的回答在獨立的 task 讀取，主迴圈的 `select!` 不會把訊框讀一半就取消。
- **Peer 的遠端判斷**：平台層的 `Peer.remote_session` 讀 `/proc/<pid>/sessionid`（audit session，一般程式不能改），再看 `/run/systemd/sessions/<id>` 是否為 `REMOTE=1`。和 `SSH_CONNECTION` 不同，請求者偽造不了。來自遠端 session 的請求一律不在本機開視窗（`unavailable`）。
  - 沒有經過 PAM 的登入（例如 e2e 裡以使用者身分跑的 `sshd`）不會有 logind session，會被當成本機；系統的 `sshd` 走 PAM，不受影響。這條路徑目前沒有自動測試。
- **SSH agent**：取消時不再送通知；只有無法開視窗（沒有桌面、沒有 GUI、遠端 session）時才通知，和階段 1 相同。
- **跟著系統上鎖**（§4.3）：`nibwarden-config` 新增 `[lock]`；daemon 啟動時讀取。
- **e2e**（85 項），分兩階段：
  - 前半段的 daemon 沒有顯示器，維持階段 1 的全部檢查（上鎖時空的金鑰清單、通知、終端機提示）。
  - 新增私有的 D-Bus 與 `scripts/e2e/mock-logind`（`DBUS_SYSTEM_BUS_ADDRESS` 指向它），檢查：其他使用者的 session 鎖定不影響、`Lock`、`LockedHint`、`PrepareForSleep` 都會上鎖。測試不會碰到真的 logind，也不會鎖住本機螢幕。
  - 後半段重新啟動 daemon，給它假的顯示器與假 GUI：上鎖時 `ssh-add -L`、沒有終端機的 `git commit` 與 `nibwarden get` 都在視窗解鎖後成功；取消時沒有金鑰也沒有通知、`get` 失敗、vault 保持上鎖；兩個程式同時要求時共用一個視窗，清單更新為兩者。
- **未驗證**：KDE Plasma 上的前景化（Q2）與 `LockedHint`，需要 KDE 環境；本機只有 GNOME。真實 GNOME 50 的螢幕鎖定已由使用者實測（`cargo run -p nibwarden-platform --example session-events`，按 Super+L）：收到 `ScreenLocked`。

### 12.4 P2-M3 的實作決定與結果

- **流程**：
  - 讀秘密（control）：必要時依請求的 `unlock` 解鎖 → 解析並解密項目（這時才知道真正的項目名稱與 reprompt 設定）→ 若解鎖不是為了這個請求，交給 `Approvals`。
  - SSH 簽章（agent）：上鎖時開「解鎖並允許」視窗（顯示金鑰名稱與用途）→ 找到金鑰 → 若解鎖不是為了這個請求，交給 `Approvals`。
- **公鑰清單在上鎖時照樣提供**：ssh 先要公鑰清單、之後才簽章；若在要清單時就跳解鎖視窗，簽章時又得再確認一次，變成兩個視窗。所以 daemon 在記憶體中保存最近一次解鎖時看到的公鑰（每次解鎖後在背景更新，登出時清除，不寫入磁碟），上鎖時照樣提供，等真正簽章時才開一個「解鎖並允許」視窗（1Password 也是這樣）。daemon 重新啟動後還沒解鎖過時，要清單仍會先開「只解鎖」視窗。
  - 代價：上鎖時，同一使用者的任何程式都能列出公鑰。公鑰不是秘密，但會透露使用者有哪些金鑰。
- **視窗**：解鎖視窗增加「解鎖並允許」與 reprompt 兩種模式；新增確認視窗（Enter 與 Esc 都是拒絕，允許要點擊；「允許」仍是藍色主按鈕，因為它是使用者通常要做的事，鍵盤預設則取安全的一邊）。請求內容由結構化資料在 Slint 中組句，三種語言都有翻譯（例如「想用 SSH 金鑰「Work」簽署 git commit」）。英文用 ASCII 引號：彎引號在中文語系下會落到 CJK 字型，變成全形。
- **broker**：所有視窗共用一個 `Window` 型別（啟動、讀答案的獨立 task、逾時、關閉）與一把「一次一個視窗」的鎖；「只解鎖」的請求仍可共用同一個視窗。
- **core**：`read_field`／`totp` 一併回傳項目 id、名稱與 reprompt 設定；`Account::verify_password` 用 SDK 的 `MasterKey::derive` 加上解開 user key 驗證主密碼，不改變狀態。
- **e2e**（98 項）：
  - 前半段（沒有顯示器）：上鎖時 agent 仍列出公鑰、簽章失敗並通知（指名 ssh-keygen）；reprompt 項目在沒有視窗時被拒，但在終端機輸入主密碼解鎖時放行。
  - 後半段（假 GUI）：
    - 重新啟動後第一次列公鑰需先解鎖。
    - 確認視窗：允許與拒絕；顯示的請求內容；自己可寫的 CLI 不提供「不再詢問」，`/usr/bin/ssh-keygen` 提供 10 分鐘。
    - reprompt：密碼錯誤時重試。
    - 記住：記住後第二次簽章不跳視窗；上鎖會清除；上鎖時的 git 簽章與 `get` 都只有一個「解鎖並允許」視窗，且可以從那裡記住。
    - 取消：簽章失敗且不送通知。
    - 兩個只需解鎖的請求共用一個視窗。
- **未自動測試**：`without_window = "deny"` 與 `[approval]` 的 `allow`／`deny` 政策（只有設定檔解析的單元測試）。

### 12.5 P2-M4 的實作決定與結果

- **元件**：`nibwarden-gui quick`（`ui/quick.slint`、`src/quick.rs`、`src/search.rs`）、`nibwarden quick [--install-shortcut]`、平台層的 `clipboard`、`shortcut`、`fs::try_lock`，設定檔的 `[quick_access]`，daemon 的 `PromptBroker::is_own_gui`。
- **執行緒**：主執行緒跑視窗；工作執行緒以一般 client 身分連 control socket，並持有剪貼簿與清除計時。清單與搜尋結果放在 UI 執行緒的 thread-local，工作執行緒用 `upgrade_in_event_loop` 交過去。
- **狀態**：載入中、上鎖（視窗內解鎖）、清單、已複製（確認畫面 0.9 秒後消失）、訊息（未登入、daemon 沒在執行，或 daemon 回的錯誤）。常見的狀態以代碼送給 Slint 翻譯。
- **試用**：示範腳本（公開前已移除）在 docker 起一次性的 Vaultwarden 與獨立的 daemon（測試帳號與幾個項目，含一個 reprompt 項目），vault 上鎖、印出主密碼，打開真的快速存取視窗；結束時全部清除，不碰使用者自己的資料。
  - 第一次試跑時抓到工作執行緒的 tokio runtime 只開了計時器、沒開 I/O，連 socket 時 panic；已修正。
- **測試**：搜尋排序、GNOME 設定值格式、鎖定檔、`is_own_gui` 的單元測試；三種狀態、三種語言、淺深色的快照；e2e 98 項照樣通過。鍵盤操作與實際複製需要在桌面上手動試用。

### 12.6 P2-M4 使用者試用後的修正

使用者用示範腳本試用後回報五點，處理如下：

1. **標題列是方框**：GNOME 的 Wayland 不提供伺服器端標題列，winit 自己畫的標題列字型沒有中文。所有視窗改成無外框（`no-frame`），標題列由 nibwarden 自己畫（`TitleBar`，用 Slint 的 `WindowMoveArea` 拖曳，含關閉鈕），並加一圈細框線，因為無外框視窗沒有桌面畫的邊。
2. **解鎖像卡住**：主密碼的金鑰衍生約 2.6 秒，期間沒有回饋。加上旋轉動畫（`Spinner`）：密碼欄位的送出鈕在處理中變成旋轉圈，快速存取另外顯示「正在解鎖…」。
3. **操作邏輯**：依 1Password Quick Access 與 Bitwarden 的設計重做（§7）：清單上的快速複製鈕、可點開的項目檢視、逐欄位複製、即時驗證碼、1Password 的快捷鍵。
4. **複製後的說明與清除設定**：拿掉「N 秒後清除」的說明，改為短暫的「已複製」提示；清除的開關與時間改到新的設定視窗（§5.6）。
5. **複製後卡住**：原本複製後只停止事件迴圈、沒有隱藏視窗，視窗留在畫面上卻不再回應，直到 90 秒後程式結束。現在複製不會關閉視窗；關閉時先隱藏再停止事件迴圈。

另外：
- 遮住的秘密改用自己畫的圓點：`•` 字元在中文語系下會落到 CJK 字型，間距過寬。
- 多個狀態原本放在同一個版面配置裡，隱藏的清單仍佔空間；改為重疊在同一個區域，一次只顯示一個。
- e2e 98 項照樣通過；快照涵蓋清單、項目、解鎖中、設定，三種語言與淺深色。
- **未自動測試**：`item.view`、`config.reload` 與設定視窗的寫入只有單元測試（設定檔的保留式寫入、選項對應）與試跑；需要使用者再次實際操作。

### 12.7 第二輪試用後的修正

使用者回報六點：

1. **解鎖的旋轉圖示不動**：軟體繪圖只支援 90 度的畫面旋轉，不支援任意角度的 `transform-rotation`。旋轉圖示改為每個畫面重新計算弧線端點（`MoveTo`／`ArcTo`），不依賴旋轉；快照工具也改成 0.3 秒後再畫一次，若兩張相同就失敗。
2. **複製按鈕看不出是複製**：改為帶圖示與文字的按鈕（「複製」；清單上是「使用者名稱」「密碼」「驗證碼」）。
3. **快速存取不該有設定鈕**：拿掉；設定從 `nibwarden settings` 開。
4. **改設定時其他視窗沒有跟著變**：每個視窗監看 `config.toml`，主題與語言即時套用。
5. **SSH／git 一律要詢問，像 1Password 那樣授權**：見 §4.2。新的 `ssh.authorize`／`nibwarden ssh authorize`、`lock.can_prompt` 多回報是否已授權。GPG 簽章 nibwarden 目前不支援（git 走 SSH 簽章）。
6. **方角視窗不符合現在的審美**：視窗改為透明，可見的部分是圓角卡片（`WindowCard`，14 px 圓角），四周留 12 px 畫柔和陰影（無外框視窗沒有桌面畫的陰影）。
   - 軟體繪圖在 Wayland 上的緩衝區沒有 alpha（softbuffer 用 `Xrgb8888`），做不出透明，所以改用 femtovg（OpenGL）；沒有 OpenGL 時退回軟體繪圖（方角）。
   - **代價**：記憶體由約 25 MiB 增為約 70 MiB，第一個畫面約 100 ms（冷啟動 210 ms），超過 §12.2 的 50 MiB 目標。視窗只在需要時存在，所以仍可接受；如果要更省，可以加一個設定改回軟體繪圖（方角）。
- e2e 103 項通過：前半段新增「沒有核准時簽章被拒」「`ssh authorize`」「密碼錯誤」，後半段改為依應用程式記住的流程。

### 12.8 P2-M5 即時同步的實作決定與結果

- **協定**（依 Vaultwarden 原始碼與官方客戶端的用法）：WebSocket 連到 hub（自架：`<base>/notifications/hub`；官方雲端：`notifications.<domain>/hub`），`access_token` 放在查詢字串，不做 SignalR 的 negotiate；送 `{"protocol":"messagepack","version":1}` 加 0x1e 握手；之後每個二進位訊息是「varint 長度＋MessagePack 陣列」。
  - `[1, {}, nil, "ReceiveMessage", [{ContextId, Type, Payload}]]`：`Type` 0–10 是 vault 變動，11 是伺服器要求登出；`ContextId` 是做出變動的裝置，等於本機 device id 的就略過（自己的變動）。
  - 每 15 秒送 `[6]`（ping）保持連線。
- **daemon 的 `push_sync`**：登入時立刻連線（登入會喚醒它），連上後先同步一次補上斷線期間的變動；收到變動後等 1 秒（合併一連串的變動）再同步；收到登出就登出。斷線後以 5 秒起、最多 5 分鐘的間隔重連。原本的定期同步保留為後備。
- **權杖**：core 的 `Account::push_endpoint` 從 SDK 的狀態讀 access token，快過期時先做一次 `sync(false)`（只查修訂時間，便宜）讓 SDK 刷新。
- **依賴**：`tokio-tungstenite`（rustls，讀系統憑證，和 SDK 的憑證處理一致）、`rmpv`。
- **測試**：
  - 單元測試：訊息解析（自己的變動略過、登出、ping）、varint 長度。
  - e2e（105 項）：daemon 的定期同步關閉，probe 以另一台裝置新增項目，15 秒內出現在 `nibwarden list`，日誌顯示是推播觸發的同步。
  - deb 安裝測試：release 版在 TLS（一次性 CA）下以 `wss://` 收到推播並同步。第一次跑時發現登出狀態下每 10 秒才檢查一次連線，登入後的變動會漏掉；改為登入時立刻喚醒、連上後先同步。
- **未驗證**：bitwarden.com 的 hub（Q7，需要測試帳號）。

### 12.9 P2-M7 主視窗的實作決定與結果

- **版面**（照 1Password）：左欄是分類（全部、我的最愛、各類型，只列出有項目的）和資料夾，各附數量；中欄是搜尋和項目清單；右欄是項目詳細資料，沒選時顯示提示。左下角是上鎖、立即同步、設定。鎖定、載入中、未登入時整個視窗只顯示對應的面板。
- **和快速存取共用**：vault 的狀態移到 Slint 的 `Vault` global（vault.slint），清單、項目詳細資料、解鎖、訊息面板都是共用元件；Rust 端的控制器（`vault_ui.rs`，原本的 quick.rs）對任何有 `Vault` 的視窗都能用，兩個視窗只各自處理關閉與外框。
- **跟上 daemon**：主視窗會開很久，所以每 3 秒問一次 daemon（本機 socket），vault 上鎖了就換成解鎖畫面，別處解鎖了就顯示清單，項目有變（例如推播觸發的同步）就就地換掉，搜尋、分類和開著的項目不受影響。
- **無外框視窗**：和其他視窗一樣自己畫最小化、最大化、關閉按鈕；邊緣和四角可拖曳調整大小（Slint 的 `resize-border-width`，見下面第一輪修正）；最大化時拿掉陰影邊距和圓角。
- **開啟方式**：`nibwarden open`，或直接執行 `nibwarden-gui`（預設就是主視窗）。同時只會有一個主視窗。
- **修掉的問題**：沒有可用的 OpenGL 時，femtovg 的嘗試會用掉 winit 每個程序唯一一次的事件迴圈，退回軟體繪圖也因此失敗，視窗完全開不起來。改為重新執行自己並設定 `SLINT_BACKEND=winit-software`（exec，保留 prompt 的 stdin/stdout）。在 Xvfb（沒有 GLX）上測試時發現。
- **測試**：
  - 快照（三種語言 × 淺色、深色）：主視窗（開著項目）、主視窗鎖定。
  - Xvfb 上的端到端：真的 daemon 加測試 vault，主視窗顯示項目、分類、資料夾；CLI `nibwarden lock` 後幾秒內換成解鎖畫面。快速存取在重構後同樣測過。
- **使用者試用後的修正（第一輪）**：
  - 調整大小放開左鍵後仍卡在調整狀態：原本自己在邊緣放 TouchArea，按下時先交給 Slint 再交給視窗管理員，放開的事件被視窗管理員拿走，Slint 一直以為還按著。改用 Slint 內建的 `resize-border-width`（按下直接交給視窗管理員，不經過 UI），範圍就是陰影邊距；拿掉 `unstable-winit-030`。
  - 右鍵選單：項目列（開啟、複製使用者名稱／密碼／驗證碼、開啟網站）與欄位（開啟網站、複製、顯示／隱藏）。
  - 驗證碼旁的倒數圓環（剩餘時間比例，最後 5 秒變紅），週期取自伺服器（不一定是 30 秒）。
  - 網址改成「開啟」，用系統瀏覽器開（`nibwarden_platform::browser`）；只開 http(s)，沒有 scheme 的補 https://，`file:`、`javascript:`、app id 一律不開（項目內容是 vault 的資料，不能讓它啟動任意程式）。每個網址各一列。
  - 圖示按鈕滑過顯示說明（Slint 的 `Tooltip`）。
  - 左欄一律列出所有類型（像 Bitwarden），沒有項目的顯示 0。
- **使用者試用後的修正（第二輪）**：
  - hover 高亮與右鍵選單過幾秒消失：每 3 秒的背景刷新用 `set_vec` 整批換掉清單，Slint 重建每一列。改成只通知有變的列（`update_model`）。
  - 按鈕說明不出現：Slint 的 `Tooltip` 只看得到它父元素底下子元素上的滑鼠，放在 TouchArea 旁邊會被 TouchArea 蓋住；改放進 TouchArea 裡。
  - 驗證碼圓環沒有垂直置中：排版裡的 `y` 由排版決定，改用 `VerticalLayout { alignment: center }`。
  - 右鍵選單是 Slint 自己畫的：Slint 在 Linux 沒有接原生選單（只有 macOS 用系統選單），這是框架的限制。
- **未驗證**：在你的 GNOME 上拖曳調整大小、最大化、最小化、右鍵選單的外觀。

### 12.10 P2-M10 項目管理的實作決定與結果

- **協定**（新請求）：`item.content`（整個項目，含秘密，給編輯表單）、`item.save`（沒有 id 是新增）、`item.favorite`、`item.trash`、`item.restore`、`item.delete`、`folder.list`；`item.list` 多了 `trash`，`item.view` 多了卡片、身分、SSH 金鑰的值（秘密的只給名稱，用 `item.get` 以同一個鍵讀）。
- **項目內容的形狀**：類型專屬的值以鍵存放（`username`、`number`、`first_name`、`private_key`…），各類型的鍵與順序是一張表（`nibwarden-ipc::ITEM_KINDS`，core 有同一張，daemon 的測試確認兩者一致）。表單照表產生，不用為每種類型寫一個畫面。
- **編輯不丟資料**：存檔時從 vault 裡的原項目出發，只改表單有的部分；passkey、網址比對規則、連結型自訂欄位、附件都保留，密碼變更由 SDK 移進密碼歷史。項目類型不能改。
- **誰可以改 vault**：
  - nibwarden 自己的視窗（daemon 認得的 GUI 執行檔）直接執行：使用者就在裡面操作。
  - 其他程式（CLI、腳本）：每次都開確認視窗（「想要將『X』移到垃圾桶」等），不記住、沒有「核准所有應用程式」，沒有視窗可問時拒絕。改 vault 的風險比讀一個欄位大，記住核准沒有意義。
  - `item.content` 等同讀取整個項目的秘密：照讀取秘密的規則（自家視窗不另問；需要重新輸入主密碼的項目照樣要求）。
  - 永久刪除只接受已在垃圾桶的項目，一個請求毀不掉使用中的項目。
- **SSH 金鑰**：新項目的私鑰留空就產生 Ed25519；貼上的私鑰（OpenSSH 或 PKCS#8，不加密）會驗證並算出公鑰與指紋；私鑰沒變就沿用。
- **GUI**：
  - 中欄搜尋旁的「+」或清單空白處右鍵：新增登入資訊、付款卡、身分、安全筆記、SSH 金鑰（在某個資料夾或「我的最愛」時，新項目預設放在那裡）。Ctrl+N 新增登入資訊。
  - 項目標題列：我的最愛（星號）、移到垃圾桶、編輯（Ctrl+E）；在垃圾桶中則是還原、永久刪除（先問）。項目列與標題的右鍵選單有同樣的動作。
  - 編輯表單取代右欄的項目內容：名稱、類型專屬欄位、網站（可多個）、資料夾、我的最愛、使用前再次輸入主密碼、自訂欄位（文字、隱藏、核取方塊）、備註；密碼欄旁有「產生密碼」（20 字元，P2-M11 再加選項）。Esc 取消、Ctrl+S 儲存。
  - 左欄最後是「垃圾桶」。
  - 表單內容（含秘密）存在 GUI 程序的記憶體裡直到關閉表單；Slint 字串不會 zeroize，與顯示秘密時相同（§10）。
- **修掉的問題**：清單的類型名稱是 `ssh-key`，GUI 用 `ssh_key`，左欄的 SSH 金鑰數量一直是 0；統一成 `ssh_key`。
- **測試**：
  - e2e（119 項）新增 14 項：從非 GUI 程式新增（拒絕、允許，確認視窗的內容）、讀整個項目、編輯（保留網址、自訂欄位）、我的最愛、使用中的項目不能永久刪除、垃圾桶、還原、永久刪除、產生 SSH 金鑰、付款卡的檢視值。
  - 單元測試：core 的鍵與 SDK 欄位對應、編輯保留網址比對規則、SSH 金鑰的產生／匯入／沿用；GUI 的表單內容。
  - 快照：編輯登入資訊、新增付款卡（含錯誤訊息）、永久刪除的確認。
  - Xvfb：真的 daemon 加測試 vault，主視窗顯示含垃圾桶的分類。
- **未驗證**：實際點按表單（這台機器沒有可在 Xvfb 點擊的工具）。

### 12.11 第三輪試用後的修正與 P2-M11 產生器

- **版面**：
  - 鎖定畫面在視窗中垂直置中（扣掉上方標題列）。
  - 項目標題列的星號、垃圾桶、編輯靠右並垂直置中：Slint 1.18 版面的交叉軸預設是 stretch，固定高度的元件會貼齊上緣，改用 `cross-axis-alignment: center`。
- **項目內容照 Bitwarden 分組**：登入憑證、網站、付款卡資料、個人資料／身分證明／聯絡資訊、SSH 金鑰、備註、自訂欄位、項目歷史紀錄、密碼歷史紀錄，各自一張卡片。編輯表單同樣分組（項目資料、各類型欄位、網站、自訂欄位、其他選項）。身分的欄位順序因此改成「個人、證明、聯絡」，core 與 IPC 兩張表一起改。
  - 實作：`Vault.fields` 仍是一個平面清單，`Vault.groups` 記每組的起點與筆數，畫面用 `Vault.fields[start + i]` 取值，倒數與顯示秘密的更新方式不變。
- **網站**：只顯示網址，旁邊有「開啟」與「複製」兩個按鈕。
- **備註**：直接顯示，不再當成秘密遮住。開啟項目時 GUI 一併讀取備註（nibwarden 自己的視窗讀非 reprompt 項目不必確認）；要求再次輸入主密碼的項目仍先遮住。
- **項目歷史紀錄**：最後編輯、建立時間、密碼更新時間（`item.view` 新增 `created`、`revised`、`password_revised`），以及密碼歷史紀錄：列出每個舊密碼被取代的時間，值用 `item.get` 的 `password_history_<n>` 讀取，跟讀密碼一樣檢查權限。SDK 的歷史是新的在前。
- **P2-M11 密碼產生器**：從密碼欄的「產生密碼」或左欄下方的按鈕打開。密碼（長度 5–128、A-Z、a-z、0-9、特殊字元、避免易混淆字元）或密語（3–20 個單字、分隔符號、首字母大寫、包含數字），選項一改就重新產生。從表單打開時可「使用這組密碼」帶回表單；單獨打開時只能複製。選項只在視窗開著時記住。
- **OTP 的 QR code**：驗證器金鑰欄旁的「掃描 QR code」可以掃描螢幕畫面（Linux 走 xdg-desktop-portal 的 Screenshot，桌面可能先問使用者；截圖讀完就刪掉）或選擇圖片檔，用 rqrr 解碼，只接受 `otpauth://`、`steam://`，整串網址放進欄位。
- **SSH 金鑰**：私鑰欄旁的「從檔案匯入」（rfd，Linux 走 portal）；檔案上限 64 KiB、須含 `PRIVATE KEY-----`，格式由 daemon 存檔時檢查。也可以把檔案拖進視窗（編輯 SSH 金鑰時當私鑰、編輯登入資訊時當 QR code 圖片），用 Slint 的 `unstable-winit-030` 取得 winit 的 `DroppedFile`；**Wayland 上 winit 沒有拖放**，只有 X11、Windows、macOS 收得到。
- **測試**：
  - e2e 121 項（新增 2 項）：`item.view` 有日期與一筆密碼歷史，`password_history_0` 讀回舊密碼。
  - 單元測試：產生 QR code 再解碼、非驗證器的 QR code 不接受、金鑰檔判斷、身分欄位分組。
  - 快照：分組的項目內容、編輯表單、產生器（密碼、密語）、鎖定畫面。
- **未驗證**：實際的螢幕截圖 portal 流程、檔案選擇視窗、X11 上的拖放。

### 12.12 P2-M8 系統匣的實作決定與結果

- **形式**：`nibwarden-gui tray` 是獨立的常駐程序（單一實例，`tray.lock`），跟其他視窗一樣是 daemon 的一般 client，不常駐任何秘密。Linux 用 ksni 實作 StatusNotifierItem：KDE 原生顯示，GNOME 需要 AppIndicator 擴充功能（Ubuntu 預設已啟用）。平台層新增 `tray`（GUI 專用 feature）與 `autostart`，macOS、Windows 先回傳 Unsupported。
- **圖示**：用主題內建的 `changes-prevent-symbolic`／`changes-allow-symbolic`（上鎖／解鎖的鎖頭），面板會依深淺色重新上色；nibwarden 自己的圖示等 P2-M6 打包時再換。滑鼠移上去的提示顯示目前狀態。
- **選單**：帳號、狀態（已上鎖、已解鎖、尚未登入、nibwarden 沒有在執行）、開啟 nibwarden、快速存取、上鎖（上鎖時變成「解鎖…」，打開主視窗的解鎖畫面）、設定…、結束。點圖示本身開主視窗。「結束」只關掉圖示，daemon 照常執行。
  - 選單文字放在一個不顯示的 Slint 元件 `TrayText`，跟視窗共用翻譯；設定改語言時跟著更新。
  - 開視窗時把桌面給的 `XDG_ACTIVATION_TOKEN` 傳給新程序，Wayland 上視窗才能到最前面。
- **跟著 daemon**：跟主視窗一樣每 3 秒查一次 `status`（不算 vault 活動，不影響閒置上鎖），狀態變了才更新圖示與選單；daemon 重啟後自動重連。第一次連線跟主視窗一樣會按需啟動 daemon。
- **開機自動啟動**：設定視窗新增「系統匣」區塊的開關（`[tray] enabled`，預設關閉，因為 GNOME 沒裝擴充功能就看不到）。打開時寫入 `~/.config/autostart/nibwarden-tray.desktop`（`Exec=… tray --autostart`）並立刻啟動圖示；關閉時刪掉這個檔案，執行中的圖示看到設定改變就自己結束。帶 `--autostart` 啟動時若設定已關閉就直接結束，避免殘留的 autostart 檔把它叫起來。
- **測試**：
  - 單元測試：選單隨 vault 狀態改變（上鎖、解鎖、daemon 沒在執行）、autostart 檔的 `Exec` 跳脫。
  - 實機（Ubuntu 26.04 GNOME 50、示範腳本）：透過 D-Bus 確認項目已向 `StatusNotifierWatcher` 註冊、圖示名稱與提示、選單內容（正體中文）；CLI 解鎖後一秒內圖示與選單改成已解鎖；從選單按「上鎖」後 vault 上鎖；「開啟 nibwarden」與點圖示開出主視窗（重複點不會開第二個）；「結束」後項目從 watcher 移除。
  - e2e 121 項照樣通過，跨平台編譯檢查通過。
- **未驗證**：KDE；登入後由 autostart 實際啟動；設定視窗開關的實際點擊。

### 12.13 P2-M12 Send 的實作決定與結果

- **core**：SDK 的 `bitwarden-send` 已經有 Send 的 repository、sync handler 與加解密；nibwarden 註冊 `SendSyncHandler`，再加上 `list_sends`、`send_info`、`send_secret`（連結與文字）、`create_send`（文字或檔案）、`delete_send`。
  - 分享連結是 `<web vault>/#/send/<access id>/<key>`；web vault 由 API 網址推得（bitwarden.com 的 `api.` 換成 `vault.`，自架伺服器去掉 `/api`）。
  - 刪除期限 1 到 31 天。檔案上限 10 MiB：檔案由 client 讀取後放進一則控制訊息（base64），訊息上限是 16 MiB。上傳失敗時把剛建立的 Send 刪掉，不留沒有檔案的 Send。
- **IPC 與確認**：新增 `send.list`、`send.get`、`send.create`、`send.delete`。清單只有名稱、類型、日期、次數，不含內容與連結。
  - 連結帶有解密金鑰，拿到連結就能讀內容，所以讀取連結跟建立、刪除一樣都要確認。nibwarden 自己的視窗不必確認，其他程式每次都問、不記住。
  - 確認視窗新增 `send` 動作，句子例如「要建立 Send『X』」。
- **CLI**：`nibwarden send list|create|get|delete`。
  - `create` 的文字從 stdin 讀（不經 argv），`--file` 傳檔案，`--password` 在終端機詢問收件者密碼；建立後印出連結。
  - `get` 印出連結，加 `--text` 印出文字。
- **主視窗**：左欄在分類下方多了「Send」。
  - 中欄：「+」選單可選文字或檔案，搜尋列換成標題，下面是 Send 清單。
  - 右欄：Send 的內容卡片，包括連結（開啟、複製）、文字或檔案、刪除時間、開啟次數、是否有密碼、是否隱藏電子郵件、備註，標題列有複製連結與刪除。
  - 新增表單：名稱、文字（可勾選先隱藏）或選擇檔案、刪除天數（1/2/3/7/14/30）、最多開啟次數、密碼、隱藏電子郵件、備註。建立後跟 Bitwarden 一樣自動複製連結。
  - 實作在 `sends.rs`，有自己的 worker 與 daemon 連線；複製走主視窗的剪貼簿，照樣逾時清除。vault 上鎖時清掉讀到的連結與文字。
- **收件端（測試用）**：Vaultwarden 1.37 已移除不帶 token 的 `POST /api/sends/access/<id>`，收件者要先向 identity 取得 `send_access` token。core 的 `receive_send` 用 SDK 的 `SendReceiveClient` 完成整個流程，probe 的 `open-send` 用它開連結、下載並解密檔案。
- **測試**：
  - e2e 134 項（新增 13 項）：建立時拒絕與允許、確認視窗的動作、連結格式、清單不含秘密、收件者用連結讀到文字、檔案 Send 下載後內容相同、超過 10 MiB 被拒絕、密碼與開啟次數上限、刪除時拒絕與允許、刪除後連結失效；Send 的文字也列入「不落地、不進 log」的檢查。
  - 單元測試：web vault 網址、連結解析、新 Send 的檢查。
  - 快照：Send 清單與內容、新增檔案 Send 的表單。
- **未驗證**：主視窗中實際點擊建立、刪除、選檔案的流程（快照只驗證畫面）；bitwarden.com 上的檔案 Send（需要付費方案）。

### 12.14 P2-M13 匯入／匯出的實作決定與結果

- **匯出**：用 SDK 的 exporter，格式有 Bitwarden JSON、CSV（只含登入資訊與安全筆記）、有密碼保護的 JSON（用帳號的 KDF 和檔案密碼加密）。只匯出個人項目，不含垃圾桶與組織項目。
  - 每次匯出都要再輸入主密碼，nibwarden 自己的視窗也一樣，因為檔案裡有全部的秘密。其他程式還會再跳確認視窗（每次問、不記住）。
  - 檔案由 client 寫入，一建立就只有使用者自己能讀；CLI 不覆寫已存在的檔案（要加 `--force`），覆寫時也會把權限收緊。
- **匯入**：SDK 不會讀 Bitwarden 自己的 JSON／CSV，所以 core 自己解析成 SDK 的 `ImportingCipher`，再用 SDK 的 `encrypt_import` 加密，一次送到伺服器的匯入端點，之後同步。KeePass（.kdbx）交給 SDK 的 importer。
  - 依內容判斷格式：KDBX 開頭的特徵碼、`{` 開頭是 JSON（有密碼保護的先用檔案密碼解開）、其他當 CSV。需不需要密碼的判斷放在 `nibwarden-ipc`，CLI 和視窗共用。
  - 只用帳號金鑰加密的 JSON（不是密碼保護）無法匯入，會請使用者改用密碼保護或一般 JSON 重新匯出。
  - 同名的資料夾沿用既有的（送出既有資料夾的 id），不會重複建立；項目一律新增，不會覆蓋或合併。
  - 密碼歷史不會匯入（SDK 的匯入模型沒有這個欄位）。
  - 檔案上限 10 MiB，理由同 Send。確認視窗會寫出檔名與項目數（KeePass 在送出前不知道數量，只寫檔名）。
- **CLI**：`nibwarden export --format encrypted-json|json|csv -o FILE`（預設有密碼保護的 JSON；`--password-stdin` 依序讀主密碼與檔案密碼），`nibwarden import FILE`（需要時詢問檔案密碼）。
- **主視窗**：左欄底部新增「匯入與匯出」按鈕，選單有「匯入…」「匯出…」。
  - 匯出對話框：選格式（未加密時顯示警告）、檔案密碼兩次、主密碼，按「匯出…」後才開存檔對話框；完成後顯示檔案位置與項目數。
  - 匯入對話框：選檔案（json、csv、kdbx），需要時出現檔案密碼欄；完成後顯示新增的項目與資料夾數，清單隨即更新。vault 上鎖時對話框關閉並清掉輸入的密碼。
  - Slint 的對話框高度用 `preferred-height` 計算時，巢狀兩層 `if` 在對話框開啟後才出現的元素不會算進高度，所以匯出、匯入、完成畫面分成三個元件，每個只有一層 `if`。
- **測試**：
  - e2e 151 項（新增 17 項）：主密碼錯誤時拒絕且不寫檔、匯出的確認視窗（拒絕與允許）、JSON 項目數與檔案權限 600、不覆寫既有檔案、CSV 欄位、有密碼保護的 JSON 不含明文、匯入的確認視窗（檔名與項目數）、JSON 匯回後項目加倍且內容相同、沿用既有資料夾、錯誤的檔案密碼、有密碼保護的 JSON 與 CSV 匯入、KeePass 資料庫（probe 的 `make-kdbx` 產生）錯誤密碼與匯入後群組成為資料夾；檔案密碼與 KeePass 密碼也列入「不落地、不進 log」的檢查。
  - 單元測試：Bitwarden JSON 與 CSV 的解析、拒絕其他檔案、判斷是否需要密碼。
  - 快照：匯出（未加密警告與主密碼錯誤）、匯入（需要檔案密碼）、匯入完成、匯入的確認視窗。
- **未驗證**：主視窗中實際點擊與系統存檔／開檔對話框；bitwarden.com 上沿用既有資料夾的行為（只在 Vaultwarden 驗證）；官方客戶端產生的匯出檔（e2e 用的是 SDK 產生的檔案，格式相同）。

### 12.15 P2-M6 Linux 發佈完整化的實作決定與結果

- **共用的安裝配置**（公開前改為 `cargo xtask install`，§12.18）：`packaging/install.sh DESTDIR [PREFIX]` 決定每個檔案放哪裡，deb、rpm、PKGBUILD、Nix 都呼叫它，所以四種套件的內容相同：
  - `bin/`：`nibwarden`、`nibwardend`、`nibwarden-ssh-sign`、`nibwarden-gui`（daemon 在自己旁邊找 GUI，所以一律放在同一個目錄）。
  - `lib/systemd/user/nibwardend.service`：`ExecStart` 依 PREFIX 改寫（Nix 是 store 路徑）。
  - `share/applications/nibwarden.desktop`、`share/metainfo/nibwarden.metainfo.xml`、`share/icons/hicolor/{scalable,symbolic}/apps/`、`share/man/man1/*.1.gz`、`share/doc/nibwarden/examples/60-nibwarden-ssh-agent.conf`。
- **GUI 的編譯設定**：新增 `[profile.release-gui]`（`opt-level = "s"`、`lto = "fat"`、`codegen-units = 1`、`panic = "abort"`），只用在 GUI，理由見 §12.2 Q1。strip 後 GUI 17.6 MiB、daemon 23.8 MiB（Q1 的 11 MiB 是只有軟體繪圖、功能也少很多的時候）。
- **桌面整合**：
  - `nibwarden.desktop`：開主視窗，動作有「快速存取」「設定」，名稱與說明有正體、簡體中文。
  - 所有視窗的 Wayland app id／X11 class 設成 `nibwarden`（`slint::set_xdg_app_id`），桌面才會用這個項目的圖示並把視窗歸在一起。app id 要等 Slint 的平台建立之後才能設，所以 `SLINT_BACKEND` 的路徑也先選好後端；選失敗時直接印出原因，因為 winit 一個程序只能建一次事件迴圈，之後開視窗只會說「EventLoop can't be recreated」。
  - 圖示：主圖示就是視窗裡的品牌標誌（藍色圓角方塊與鑰匙孔）的 SVG；系統匣改用自己的單色圖示（上鎖是實心方塊挖出鑰匙孔，解鎖是外框加實心鑰匙孔），只有在 hicolor 主題裡找得到時才用，直接從編譯目錄執行時仍用主題的鎖頭。
  - AppStream metainfo 通過 `appstreamcli validate`，只剩兩個警告：ID 不是反向網域名稱、沒有首頁網址；專案還沒有公開網址，所以先保留 `nibwarden`。
- **man page**：`nibwarden(1)`、`nibwardend(1)`、`nibwarden-gui(1)`、`nibwarden-ssh-sign(1)`，手寫（範例、設定檔、結束代碼這些 clap 產生不了）。為了不和程式脫節，CLI 與 daemon 各有一個單元測試，檢查每個子命令與長選項都出現在 man page 裡；第一次跑就抓到 5 個只寫了短選項的地方。
- **套件**：
  - deb：`packaging/deb/build.sh` 改用 `install.sh`，加上 GUI。Depends 加 `libfontconfig1`、`libfreetype6`（GUI 直接連結）與 `libxkbcommon0`；Wayland、EGL、X11 的函式庫是執行時載入的，放在 Recommends。Installed-Size 改用檔案大小計算（`du --apparent-size`），原本算的是磁碟區塊，會偏小。
  - rpm：`packaging/rpm/nibwarden.spec`，`packaging/rpm/build.sh` 在 Fedora 44 容器裡用 `rpmbuild` 從原始碼 tarball 建置。
  - Arch：`packaging/arch/PKGBUILD`，`packaging/arch/build.sh` 在 Arch 容器裡用 `makepkg` 建置。上 AUR 時要把 `source` 改成正式的 tarball 網址並填 checksum。
  - Nix：`flake.nix` 與 `packaging/nix/package.nix`（`buildRustPackage`，git 相依用 `allowBuiltinFetchGit`）。NixOS 沒有共用的函式庫路徑，所以把 winit 與 OpenGL 會載入的函式庫加進 GUI 的 rpath。NixOS 使用者以 `systemd.packages` 取得 user unit。
  - 原始碼 tarball 由 `packaging/source.sh` 產生：工作目錄裡沒被 `.gitignore` 排除的檔案，已 commit 與否都算。
  - 新增 `LICENSE`（GPL-3.0 全文；授權早已在 `Cargo.toml` 宣告，但沒有附全文）。
- **X11 需要的函式庫**：winit 在 X11 上一定要 libXcursor、libXrandr、libXi（加上 libX11、libxkbcommon-x11）。Arch 不會自動裝選用相依，第一次測試就因此開不出視窗，所以三種套件都把它們列進去（deb、rpm 是 Recommends，Arch 是 optdepends）。
- **沒有 OpenGL 驅動時沒有視窗**：Nix 的測試容器有 libglvnd 的 libEGL，卻沒有任何驅動（NixOS 沒開 `hardware.graphics` 時也是這樣）。Slint 的 femtovg renderer 是先建立、等視窗開啟時才建 GL context，所以選後端照樣成功，到開視窗時才失敗，結果是程序還在、卻沒有任何視窗，原本「選後端失敗就改用軟體繪圖」的退路接不住。現在選 renderer 之前先問 EGL（`nibwarden_platform::display::opengl_available`：載入 `libEGL.so.1`，對預設 display 呼叫 `eglInitialize`），不行就直接用軟體繪圖。不能等開視窗後再補救：那時解鎖視窗已經從 stdin 讀走 daemon 的請求，重新啟動會把它弄丟。
- **最低 Rust 版本**：Slint 1.18 要求 rustc 1.92，`rust-version` 原本寫 1.88 是錯的，改成 1.92；nixos-25.11 的 rustc 是 1.91.1 編不過，flake 改用 nixos-26.05。
- **daemon 的 log**：只有在終端機上才加顏色；在 systemd 底下 stderr 會進 journal，原本會夾著 ANSI 色碼。
- **測試**：
  - `scripts/package-test.sh deb|rpm|arch|nix`（原本的 `deb-test.sh`）：在各發行版的乾淨容器裡安裝套件（Nix 是在 nixos/nix 容器裡 `nix build`），然後檢查：只接受 https、登入、git 簽章與驗證、wss 即時同步；接著在 Xvfb 上重新啟動 daemon（已上鎖），`nibwarden unlock --gui` 開出解鎖視窗（用 X11 class `nibwarden` 找視窗，也就驗證了 app id），以 xdotool 輸入主密碼後確認已解鎖，並把視窗截圖存成 `target/<kind>-unlock.png`。
  - `scripts/systemd-test.sh`：Ubuntu 容器以 systemd 為 PID 1（需要 privileged），讓使用者的 user manager 跑起來（linger，相當於登入），第一次執行 `nibwarden status` 前 unit 是 inactive，之後是 active，SSH agent socket 在，daemon 能回應；unit 的沙箱設定（`NoNewPrivileges`、`MemoryDenyWriteExecute`、`UMask=0077`）都生效。
  - 結果（全部以最終的程式碼重建後重跑）：deb（Ubuntu 26.04）、rpm（Fedora 44）、Arch、Nix（nixos/nix，nixpkgs 26.05）四種都通過；systemd user unit 測試通過，journal 裡的 log 沒有色碼。套件大小：deb 13.4 MiB、rpm 15.4 MiB、Arch 17.8 MiB。Nix 的截圖沒有粗體，因為容器裡沒有 fontconfig 設定檔（NixOS 有 `/etc/fonts`）。
  - 另外 e2e 151 項、單元測試 69 項（新增 2 項 man page 檢查）、clippy、fmt、跨平台編譯檢查都通過。
- **未驗證**：
  - 在真正的桌面登入後（GNOME、KDE）由 `.desktop` 檔啟動、工作列圖示與視窗分組；系統匣的新圖示在面板上的樣子。
  - Wayland 上的套件安裝測試（容器裡只有 X11）。
  - AUR 實際上架、Fedora 官方的離線建置（rpm 的 `%build` 需要網路抓 crate）。

---

### 12.16 登入與註冊的實作決定與結果

- **為什麼要自己發註冊請求**：SDK 的 `post_register_send_verification_email` 丟掉了回應內容，而伺服器不驗證地址時 token 就在回應裡；所以兩個請求都用 SDK 自己的 HTTP 用戶端（同樣的 TLS 設定與用戶端標頭）直接 POST。完成註冊只送舊式欄位（`masterPasswordHash`、`userSymmetricKey`、`kdf`），Vaultwarden 1.37 與 Bitwarden 伺服器都接受。
- **錯誤訊息**：伺服器拒絕時，取 `validationErrors` 的第一則（Bitwarden 把具體原因放這裡），沒有才用 `message`。
- **GUI**：`account.slint` 的 `Account` global 與 `account.rs`。照 `transfer` 的做法用自己的工作執行緒與連線；兩步驟驗證必須在同一條連線上完成，因為 daemon 把待驗證的登入記在連線上。登入成功後請主視窗重新整理，主視窗就切到密碼庫。
- **登入請求自己送**：SDK 的密碼登入把 `deviceType` 寫死成 9（ChromeBrowser）、`deviceName` 寫死成 `firefox`、`deviceIdentifier` 寫死成所有 SDK 客戶端共用的同一個值，伺服器的新登入通知信與裝置清單因此顯示成不明的 Chrome；它也帶不了 `newDeviceOtp`。所以 `login.rs` 自己發 `/connect/token`：`client_id` 為 `desktop`、`deviceType` 為該平台的桌面版（Linux 為 8，與每個請求的 `Device-Type` 標頭同一個值，`platform::DEVICE_TYPE`）、`deviceName` 為 `nibwarden`、`deviceIdentifier` 為資料目錄的 `device-id`。成功後照 SDK 自己登入時的做法，把 token 與登入方式（`client_id` 一併存下，換新 token 時沿用）寫進 SDK 的 state，再用回應中的 `userDecryptionOptions.masterPasswordUnlock` 與 `privateKey` 呼叫 `initialize_user_crypto`。用的都是 SDK 的公開 API。e2e 以 Vaultwarden 的 `/api/devices` 確認伺服器記下的是 `nibwarden`、type 8、我們的裝置識別碼。改版前登入的工作階段照舊用 `web` 換新 token，不受影響；改版後第一次登入，bitwarden.com 會把它當新裝置，寄一次驗證碼。
- **新裝置驗證**（參考 rbw PR #319）：伺服器回 `error: device_error`、`error_description: New device verification required` 時，core 回報 `TwoFactorRequired([NewDevice])`，daemon、IPC、CLI、GUI 都把它當成一種兩步驟方式處理（`auth.send_email_code` 對它改為呼叫 `POST /accounts/resend-new-device-otp`，驗證碼綁在 `Device-Identifier` 標頭的裝置上）。輸入的驗證碼以 `newDeviceOtp` 隨同一個登入請求送出。Vaultwarden 沒有這個機制，只有單元測試（錯誤分類）。
- **登出**：主視窗側邊欄上方的帳號（頭像與電子郵件）點下去是選單：鎖定、登出。登出送 `auth.logout`，之後主視窗回到登入頁。在 Xvfb 容器裡實際點過：選單出現、登出後顯示登入頁、`nibwarden status` 為未登入。
- **捲軸**：Slint 的 `Flickable` 沒有捲軸，所以加了 `ScrollBar`（`components.slint`），疊在 Flickable 右緣（是兄弟元件，因為 Flickable 的子元件會跟著捲動）；內容放得下時不顯示，可拖曳，點軌道翻頁。項目清單、Send 清單、項目內容、編輯表單、側邊欄、設定視窗都有。
- **精簡文字**：搜尋框只寫「搜尋」；設定裡沒有控制項的說明列（SSH 一律詢問）與多餘的說明都拿掉，只留「無法顯示視窗時允許讀取秘密」的「例如透過 SSH 連線時。」；翻譯檔裡沒用到的字串一併清掉。
- **驗證碼欄位與忙碌狀態**：兩步驟與新裝置驗證碼固定 6 位數，做成 6 個方格（`CodeField`）：輸入其實進到一個不畫出來的 TextInput，方格只顯示內容與下一位的游標；貼上 `123 456`、`123-456` 只留數字、最多 6 位（Slint 沒有取子字串，所以取第幾位與清理由 Rust 的 pure callback 提供）。登入、註冊、解鎖進行中只把按鈕換成轉圈圈（`Button.busy`），不再另外顯示「正在登入…」「正在解鎖…」之類的文字；daemon 的登入目前不回報進度（例如首次同步），所以沒有更細的狀態可以顯示。
- **驗證信的連結**：Vaultwarden 是 `…/#/finish-signup/?email=…&token=…`，Bitwarden 是 `…/redirect-connector.html#finish-signup?token=…&email=…`；兩種都從 `?`、`&`、`#` 切開找 `token` 與 `email` 並做 percent-decode，也接受只貼 token。
- **順手修掉的問題**：
  - 空的密碼庫在清單裡的「密碼庫是空的」有一半被搜尋框蓋住：項目為 0 時清單內容高度是 0，120px 高的提示被置中在 0px 裡，y 變成 −60。新帳號一定會看到，所以改為貼齊頂端。
  - 套件沒有依賴 `ca-certificates`：沒有 CA 憑證時 SDK 建立 HTTP 用戶端會 panic，連線直接斷掉（GUI 只看到「peer closed the connection」）。deb、rpm、Arch 都加上。
- **測試**：
  - e2e 新增 5 項（共 156 項）：註冊拿到 token、用混合大小寫地址註冊後以小寫登入、新帳號是空的、同一地址再註冊在完成時被拒、新主密碼不在 trace log。
  - 單元測試：伺服器錯誤訊息的解析、地址正規化、驗證信連結的解析。
  - 真的視窗：在 Ubuntu 26.04 容器的 Xvfb 裡用 xdotool 操作主視窗，從登入頁點「建立帳號」→ 選自架並輸入網址、地址、名稱 → 設定主密碼與提示 → 自動登入並顯示空的密碼庫，`nibwarden status` 顯示已解鎖。
  - 截圖（`examples/snapshot.rs`）：登入、登入錯誤、兩步驟驗證、新裝置驗證、註冊、查看信箱、設定主密碼、視窗太矮時可捲動、快速存取的未登入畫面、空的密碼庫。
- **沒驗證的**：bitwarden.com、bitwarden.eu 上真的註冊（需要真的信箱，且官方伺服器可能另有限制）；寄驗證信的流程只用單元測試驗證連結解析，沒有在設定了郵件的伺服器上走過一次。

### 12.17 官方瀏覽器擴充套件整合的實作決定與結果

- **為什麼接官方擴充套件而不是自己寫**：自動填入（表單偵測、各網站的特例）是官方擴充套件多年累積的部分，自己寫是另一個大專案；官方擴充套件本來就能交給桌面程式解鎖，只要扮演桌面程式那一端即可。
- **加密都在 core**（`browser.rs`）：用 SDK 公開的 `PublicKey::from_der`、`UnsignedSharedKey::encapsulate_key_unsigned`（RSA-OAEP SHA-1，SDK 標為 deprecated 是因為沒有簽章，但擴充套件只認這個）、`EncString`；使用者金鑰用 `CryptoClient::get_user_encryption_key`（SDK 為生物辨識留的介面）。送給擴充套件的 EncString 照桌面版序列化（`encryptedString`、`encryptionType`、`iv`、`data`、`mac`），收的兩種寫法都接受。
- **測試**：
  - e2e 新增 13 項（共 169 項）：在另一個 HOME 裡 `browser enable` 寫出 Firefox 與 Chromium 的 manifest、Chrome 已登記桌面版時不動它、`disable` 只刪自己的；`nibwarden-probe browser-extension` 扮演擴充套件，經由真的 `nibwarden-browser-host` 完成 RSA 交換、查狀態、要求解鎖：允許後拿到的金鑰與 probe 自己解鎖得到的使用者金鑰相同，拒絕時 `false`，上鎖時一個解鎖視窗同時解鎖，另一個帳號回 `wrongUserId` 且不開視窗。
  - 單元測試：通道兩端的設定與加解密、manifest 的內容與辨認。
  - 截圖：確認視窗（瀏覽器解鎖）、設定視窗的「瀏覽器」。
- **沒驗證的**：真的瀏覽器與官方擴充套件（Firefox、Chrome）實際操作；Flatpak／Snap 瀏覽器。

### 12.17.1 設定視窗的 SSH 與 Git

- 試用時發現設定視窗沒有 1Password／Bitwarden 那樣「設為 SSH agent」「用來簽 git」的開關，只有 CLI。寫入 `~/.ssh/config` 與 git config 的程式搬到 `nibwarden-config::integration`，CLI 與設定視窗共用。
- **SSH agent**：仿 1Password，在 `~/.ssh/config` 加上 `Host *` 與 `IdentityAgent "<daemon 回報的 socket 路徑>"`，以註解行標出開頭結尾，關閉時整塊移除。放在檔案結尾：ssh 以第一個取得的值為準，所以使用者替個別主機指定的 agent 仍然有效（視窗只在有這種設定時加一句說明）。寫實際路徑而不是 `${XDG_RUNTIME_DIR}`，因為沒有這個環境變數的工作階段裡 ssh 會直接失敗。檔案不存在時以 0600 建立（ssh 拒絕他人可寫的設定）。CLI：`nibwarden ssh agent-setup [--remove]`。
- **git 簽章**：開啟時列出 vault 的 SSH 金鑰（上鎖時開解鎖視窗），用目前選的（或第一把）寫入全域 git config；有多把時多一列「簽署用的金鑰」可切換。關閉只移除 nibwarden 自己寫的設定（`user.signingkey` 帶 `nibwarden:` 標記時），allowed signers 保留，舊 commit 仍可驗證。CLI：`nibwarden ssh git-setup --remove`。
- **不需要 git 的 user.email**：原本 `git-setup` 沒有 `user.email` 就拒絕，因為要把金鑰寫進 allowed signers（以電子郵件對應金鑰，供本機 `git log --show-signature` 驗證）。但簽章本身不需要它，1Password 與 Bitwarden 的設定也只寫 `gpg.format`、`user.signingkey`、`gpg.ssh.program`、`commit.gpgsign`。所以改為：有 `user.email` 才加 allowed signers，沒有就只設定簽章並提示。
- **CLI 不再負責開視窗**：`nibwarden quick`（含 `--install-shortcut`）、`nibwarden open`、`nibwarden settings` 只是啟動 `nibwarden-gui`，拿掉；視窗從應用程式選單、系統匣、快捷鍵（設定視窗裡設定）或直接 `nibwarden-gui <模式>` 開。`nibwarden unlock --gui` 保留，因為那是請 daemon 開它自己的解鎖視窗。下文提到這三個指令的地方是當時的記錄。
- **解鎖先用視窗**：`nibwarden unlock` 與遇到上鎖的指令（`get`、`list`…）原本有終端機就在終端機問。改為先問 daemon 能不能開視窗（`CanPrompt`：有圖形工作階段、不是從遠端 SSH 來、有安裝 GUI），能就開解鎖視窗，不能才在終端機問。由 daemon 判斷而不是 CLI 自己看 `DISPLAY`，因為 SSH 的 X forwarding 也會有 `DISPLAY`。`--gui`、`--terminal` 可指定。
- **GPG**：Bitwarden 的項目沒有 OpenPGP 金鑰，所以沒有 GPG agent；git 以 SSH 簽章取代。
- **測試**：e2e 新增 8 項（共 177 項）：桌面上從終端機 `nibwarden unlock` 會開視窗；沒有 user.email 也能設定簽章；以 `ssh -G` 確認一般主機用 nibwarden 的 agent、自訂 agent 的主機維持原值、重複執行只有一個區塊、移除後檔案還原；`git-setup --global` 與 `--remove`。

### 12.18 公開前改用 cargo xtask

- **為什麼**：原本的打包與檢查都是 shell 腳本（`packaging/install.sh`、`source.sh`、`deb|rpm|arch/build.sh`、rpm spec、`scripts/cross-check.sh`、`package-test.sh`、`systemd-test.sh`），要 docker、dpkg-deb、rpmbuild、mingw 等工具，別人不方便用；打包腳本又是為了測試而寫，產物不是能直接發佈的版本。公開前全部換成 Rust 生態常用的 `cargo xtask`（`xtask/`，不發佈的 crate，`.cargo/config.toml` 的 alias）。
- **`cargo xtask check`**：`cargo fmt --check`、`clippy -D warnings`、`cargo test`；`--target <triple>` 另外對其他平台跑 clippy（缺的 target 用 rustup 補）。CI 則直接在 macOS 與 Windows 的 runner 上編譯整個 workspace，不再需要 mingw，也不會像以前那樣因為 Apple SDK 的標頭而只能檢查部分 crate。
- **`cargo xtask install --destdir D --prefix P`**：取代 `install.sh`，檔案清單（`layout()`）只有一份；PKGBUILD 與 Nix 都改呼叫它。
- **`cargo xtask dist`**：在 `target/dist` 產生 .deb、.rpm、通用的執行檔 tarball、原始碼 tarball（`git archive HEAD`）、填好版本與 checksum 的 PKGBUILD，以及 `SHA256SUMS`。deb（ar＋tar）與 rpm（`rpm` crate，gzip、不簽章）直接用 Rust 寫出，不需要任何發行版的工具。檔案時間用 `SOURCE_DATE_EPOCH` 或最後一個 commit 的時間，同一個 commit 產出相同的套件內容；release 建置以 `--remap-path-prefix` 去掉建置機器的路徑，`[profile.release]` 加上 `strip`。glibc 的最低版本從執行檔裡的 `GLIBC_2.x` 符號版本取最大值，寫進 deb 的 `libc6 (>= …)` 與 rpm 的 `libc.so.6(GLIBC_…)(64bit)`。
- **發佈**：推送 `v<版本>` 標籤時，`.github/workflows/release.yml` 在 Ubuntu 22.04（x86_64 與 aarch64，glibc 2.35，所以 Debian 12、Ubuntu 22.04 以後都能裝）執行 `cargo xtask dist`，合併後建立 release 草稿。
- **驗證**：本機 `cargo xtask dist` 的 deb 在 Ubuntu 26.04 容器以 apt 安裝（相依套件解析、程式可執行、unit 的路徑、移除）、rpm 在 Fedora 44 容器以 dnf 安裝（`rpm -V`、相依、授權檔、移除）都通過；PKGBUILD 在 Arch 容器以 makepkg 建置、以 pacman 安裝；`nix build` 在 nixos/nix 容器裡建置成功（unit 指向 store 路徑、man page、授權檔都在）；執行檔 tarball 解到 `/usr/local` 可直接執行。原本 `package-test.sh` 那種在容器裡實際簽 git commit、開解鎖視窗的安裝測試不再保留；這些功能由 e2e 涵蓋。
- **沒驗證的**：GitHub Actions 上實際執行（CI、release，包含 macOS 與 Windows 上編譯整個 workspace，以前只在 Linux 交叉檢查過 Windows 與 macOS 的一部分 crate）。

## 13. 里程碑

| 里程碑 | 內容 | 驗收條件 |
|---|---|---|
| **P2-M0 平台抽象層**（已完成，見 §12.1） | 建立 `nibwarden-platform`，把 §8 中「階段 1 的現況」那一欄的程式碼搬進去；macOS 與 Windows 的空殼實作 | Linux e2e 63 項與 deb 安裝測試照樣通過；daemon、CLI、IPC 不再有 `cfg(target_os)`、`libc`、`/proc`；`cargo check` 在 macOS 與 Windows 目標可以通過（Q8） |
| **P2-M1 GUI 原型與提示協定**（已完成，見 §12.2） | `nibwarden-gui`（Slint）的解鎖視窗、`nibwarden-ipc::prompt`、`PromptBroker`；實測 Q1，驗證 GNOME Wayland 前景化（Q2）與 Q3 | 由 `nibwarden unlock --gui` 開視窗解鎖；Q1、Q2（GNOME）、Q3 有數字與結論 |
| **P2-M2 上鎖時保留請求＋跟著系統上鎖**（除 KDE 驗證外已完成，見 §12.3） | `UnlockGate`、agent 與 `nibwarden-ssh-sign` 與無終端機 CLI 等待解鎖、多個等待者共用視窗、逾時、遠端 SSH 不開視窗；logind 的螢幕鎖定與休眠；KDE 前景化 | e2e（假 GUI）涵蓋：上鎖時 `ssh` 登入與 `git commit` 在輸入主密碼後成功；取消與逾時時行為和階段 1 相同；螢幕鎖定後為 Locked |
| **P2-M3 使用前確認**（已完成，見 §12.4） | `GuiApprover`、確認視窗、上鎖時的「解鎖並允許」視窗（§5.3）、記住的核准、CLI 的終端機工作階段範圍、reprompt、設定檔的政策 | e2e：允許、拒絕、記住、上鎖後清除、reprompt 都有測試；確認視窗預設按鈕是拒絕 |
| **P2-M4 快速存取**（已完成，見 §12.5 到 §12.7；設計經使用者兩輪試用後確認） | `nibwarden-gui quick`、快捷鍵（portal 或系統設定）、剪貼簿逾時清除與敏感標記 | GNOME 與 KDE 上都能用快捷鍵叫出、搜尋、複製；逾時後剪貼簿被清除 |
| **P2-M5 即時同步**（已完成，見 §12.8） | notifications hub 的 WebSocket、斷線重連、退回定期同步 | 另一台裝置修改項目後數秒內反映（Vaultwarden；bitwarden.com 視 Q7） |
| **P2-M6 Linux 發佈完整化**（已完成，見 §12.15） | rpm、AUR、Nix；`.desktop` 檔與圖示；man page；systemd user unit 的實機自動啟動 | 每種套件在乾淨環境安裝後，git 簽章與解鎖視窗可用；登入桌面後第一次使用時 daemon 自動啟動 |
| **P2-M7 主視窗**（已完成，見 §12.9） | 瀏覽整個 vault（資料夾、搜尋、項目詳細資料、複製）、上鎖與解鎖、手動同步、開設定；沿用快速存取的元件 | 不用 CLI 就能完成日常查看與複製 |
| **P2-M8 系統匣常駐**（除 KDE 驗證外已完成，見 §12.12） | 系統匣圖示（上鎖狀態、開主視窗、快速存取、上鎖、設定、結束），開機自動啟動 | GNOME（AppIndicator）與 KDE 上都看得到並可操作 |
| **P2-M10 項目管理**（已完成，見 §12.10） | 新增（登入、安全筆記、付款卡、身分、SSH 金鑰）、編輯、加入／移除我的最愛、移到垃圾桶、垃圾桶（還原、永久刪除）；daemon 新增對應請求，寫入一律需要 vault 已解鎖並經同一套確認 | 不用官方客戶端就能管理項目 |
| **P2-M11 密碼產生器** | 視窗中的產生器（長度、字元種類、passphrase），也能在新增／編輯時直接帶入 | 已完成，見 §12.11 |
| **P2-M12 Send** | 建立、列出、刪除文字與檔案 Send | 已完成，見 §12.13 |
| **P2-M13 匯入／匯出** | 匯出（加密 JSON、JSON、CSV，需再輸入主密碼）、匯入（Bitwarden JSON／CSV 等 SDK 支援的格式） | 已完成，見 §12.14 |
| **P2-M14 登入與註冊**（已完成，見 §5.7、§12.16） | 主視窗的登入頁（伺服器選擇、兩步驟驗證）與註冊流程（驗證信連結、設定主密碼） | 不用 CLI 就能登入或建立帳號 |
| **P2-M15 官方瀏覽器擴充套件整合**（已完成，見 §5.8、§12.17） | `nibwarden-browser-host`、`browser.message`、manifest 的登記；擴充套件的「以生物辨識解鎖」改由 nibwarden 的視窗確認 | 不用 Bitwarden 桌面版，擴充套件就能交給 nibwarden 解鎖 |
| **P2-M9（選）Secret Service** | refresh token 改存 Secret Service（沒有時退回目前的 SQLite） | 資料目錄中不再有明文 refresh token |

順序的理由：平台抽象層（M0）最先，後面所有新功能都直接寫在抽象層上；GUI 與 daemon 的互動（M1 到 M3）是其他功能的基礎，接著做；快速存取與即時同步彼此獨立；打包放最後，因為前面的里程碑會新增執行檔與桌面檔案。
