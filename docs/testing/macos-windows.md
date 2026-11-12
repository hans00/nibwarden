# macOS 與 Windows 實機測試清單

macOS 與 Windows 的平台層（phase2.md §8）只經過交叉編譯、CI 上的單元測試，以及 Windows 在 Wine 下的測試。下面每一項都要在實機上確認過，README 才能拿掉「尚未支援」。

建置：`cargo build --release`，執行檔放在同一個目錄（`nibwardend`、`nibwarden`、`nibwarden-gui`、`nibwarden-ssh-sign`、`nibwarden-browser-host`）。

## 共同

- [ ] `cargo test --workspace` 通過。
- [ ] 第一次執行 `nibwarden status` 時 daemon 會被啟動，登入、同步、解鎖都正常。
- [ ] 第二個 `nibwardend` 啟動時回報位址已被使用；強制結束 daemon 之後可以再啟動。
- [ ] 以另一個使用者的身分連不上 daemon 與 SSH agent。
- [ ] 確認視窗顯示的程式、它所在的應用程式都正確：分別從終端機、IDE、以及 SSH 登入連進來的工作階段發出請求。從 SSH 工作階段發出時，本機不能跳出視窗。
- [ ] 系統驗證解鎖（macOS：Touch ID 或登入密碼；Windows：Windows Hello）。對話框要出現在 nibwarden 視窗上；取消時顯示「未確認」；沒有設定 Hello 或 Touch ID 時，設定裡的開關不能打開。
- [ ] 螢幕鎖定之後 vault 會上鎖（macOS 最多延遲約 1 秒）。休眠喚醒之後 vault 是鎖著的。閒置計時有算進休眠的時間。
- [ ] 設定裡的「登入時啟動」可以開關，登出再登入之後確實生效。
- [ ] 系統匣：
  - 圖示會跟著鎖定狀態變換。
  - macOS 選單列的圖示在深色與淺色模式下都看得清楚，Dock 裡沒有圖示。
  - Windows 的工作列在深色與淺色下圖示都看得清楚；左鍵點擊開主視窗，右鍵開選單。
  - 選單每一項都有作用；結束之後圖示會消失，Windows 上不會留下滑過才消失的殘影。
  - 登入之後會自動出現（設定有開的話）。
- [ ] 快速存取的快捷鍵：
  - 在設定裡設定後立刻就能用，登入之後也還能用。
  - 清除後背景的 `nibwarden-gui shortcut` 會在幾秒內結束；更換按鍵後新的按鍵會生效。
  - 按鍵被別的程式佔用時，設定裡要顯示錯誤。
  - 快速存取視窗會跳到最前面並取得焦點。
- [ ] 剪貼簿：複製的密碼會依設定時間清除。macOS 的剪貼簿管理程式（例如 Maccy）不會記錄它；Windows 的剪貼簿歷史（Win+V）裡也不會出現。
- [ ] 掃描畫面上的 QR code（新增驗證碼）：macOS 第一次使用時會要求「螢幕錄製」權限，之後可以掃到。
- [ ] 介面語言會跟著系統設定，試試正體中文。
- [ ] 開啟項目的網址；桌面通知會出現。
- [ ] 瀏覽器整合：在設定裡註冊之後，Bitwarden 擴充套件能透過 nibwarden 解鎖。分別試 Chrome、Firefox、Edge。
- [ ] 以 `nibwarden run` 執行的程式能拿到輸入與輸出，結束碼也會傳回來。
- [ ] 在終端機輸入主密碼時可以輸入非 ASCII 字元。

## macOS

- [ ] socket 在 `~/Library/Caches/nibwarden/`，權限是 `0600`，目錄是 `0700`。
- [ ] `launchctl print gui/$(id -u)/me.hans00.nibwarden.daemon` 看得到 daemon；日誌在 `~/Library/Logs/nibwarden/`。
- [ ] 透過 SSH 登入時，daemon 是直接啟動的，不經 launchd，也不會開視窗。
- [ ] 從 launchd 啟動的 daemon 也能叫出 Touch ID 或密碼的對話框（這一項最可能出問題）。
- [ ] 確認視窗中的應用程式：Terminal.app、iTerm2 要顯示成它們本身，而不是 `login`、shell 或 `iTermServer`。
- [ ] SSH 工作階段的 `SSH_CONNECTION` 會顯示出來。
- [ ] release 版的 `nibwardend` 用 `lldb -p` 附加不上。
- [ ] SSH：`ssh-add -L` 列得出金鑰，`git commit -S` 能用 `nibwarden-ssh-sign` 簽章。

## Windows

- [ ] `nibwarden status` 顯示的 SSH agent 是 `\\.\pipe\nibwarden-<SID>-ssh-agent`。設定 `SSH_AUTH_SOCK` 為這個名稱之後，`ssh-add -L` 列得出金鑰。
- [ ] 在設定裡啟用 ssh 之後，`ssh -G <host>` 讀回的 `IdentityAgent` 和 pipe 名稱一致。`git push` 能透過它設定的 `core.sshCommand`（系統的 OpenSSH）運作。
- [ ] `git commit -S` 能透過 `nibwarden-ssh-sign`（呼叫系統的 `ssh-keygen`）簽章。
- [ ] 在檔案總管裡，資料目錄與檔案的安全性只列出目前使用者。
- [ ] 同一個使用者的其他程式用 `OpenProcess(PROCESS_VM_READ)` 打不開 daemon；工作管理員仍能結束它。
- [ ] 登入時 daemon 啟動不會閃出主控台視窗，或閃一下但可以接受。
- [ ] daemon 和系統匣都出現在「設定 > 應用程式 > 啟動」裡；在那裡關掉之後，nibwarden 的設定也顯示為關閉。
- [ ] RDP 工作階段：視為遠端；`opengl_available` 在 RDP 與沒有 3D 加速的 VM 裡為 false，GUI 改用軟體繪製。
- [ ] Modern Standby 的筆電：至少螢幕鎖定時 vault 會上鎖。
- [ ] Windows Hello 的提示出現在 GUI 視窗的上方。
