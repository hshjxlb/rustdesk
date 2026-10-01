# RustDesk 定制版功能总结

> 仓库：`github.com/hshjxlb/rustdesk`（fork 自 rustdesk/rustdesk，基线 **1.5.0**）
> 最终版本：**1.5.0-13**（tag `1.5.0-13`，源码 commit `aec1350b3` + 本文档提交）
> 读者：接手维护的人 / WorkBuddy 新会话。升级操作见 [UPGRADE.md](./UPGRADE.md)。

## 1. 功能版本时间线

| 版本 | 内容 |
|---|---|
| v7 | 本地审计日志落地：`allow-audit-log` 开关 + `audit-log-path` 路径（机器级 Config，UI 与系统服务双进程一致）；开关打开即预创建文件 |
| v9 | 审计行 `user=` 记录**控制端名称**（LoginRequest.my_name），CONNECT_IN / DISCONNECT_IN 成对出现 |
| v12 | Win11 IoT LTSC / Windows Server 通知兼容（Toast 不可靠时自动改气泡）；审计诊断日志 |
| v13（最终） | 移除登录闸门（所有入站会话均记录）；路径 `~` 展开；服务端兜底建文件；诊断日志升为 info；Linux CM 窗口对已授权连接隐藏（消除 GNOME「已就绪」通知）；设置界面显示默认日志路径 |

## 2. 功能详解

### 2.1 本地审计日志（核心定制）
- 开关：设置 → 安全性 → **Logging → Enable logging**（选项 `allow-audit-log`，值恰为 `Y` 才开，默认关）
- 路径：选项 `audit-log-path`，留空用默认；填目录则自动补 `audit.log`；支持 `~` 开头
  - Windows：`C:\ProgramData\RustDesk\audit.log`
  - Linux：`~/RustDesk/audit.log`（活跃桌面用户的 home；root 服务与 UI 进程解析到同一文件）
- 记录内容（明文，Notepad 可读；Windows 为 CRLF）：
  - `LOGIN` / `LOGOUT`：本机 API 账户登录/登出
  - `CONNECT_IN` / `DISCONNECT_IN`：入站会话开始/结束（含 `duration`、`ip`、`type`）
- 超 10MB 自动轮转为 `audit.log.1`
- 服务端每次收到选项同步时兜底预创建文件（`ensure_file_if_enabled`）
- 诊断：服务日志 info 级有 `audit_log:` 前缀行（开关状态、路径解析、写入结果）

### 2.2 入站连接通知
- 设置 → 安全性 → **Remote access notifications → Notify me when someone connects**（`allow-incoming-notify`）
- Linux：`notify-send`；Windows：托盘提示（检测到 Win11 IoT LTSC / Windows Server 时走 balloon 气泡）

### 2.3 Linux CM 窗口行为（v13）
- **已授权**（密码自动接入）连接：CM 窗口保持隐藏、不请求焦点 → 不再出现 GNOME `“ID - RustDesk”已就绪` 通知
- **待审批**连接：正常弹出并请求焦点
- 实现：`main.dart` 启动时隐藏 + `server_model.dart` 的 `_cmShouldAutoShow()` 门控

### 2.4 设置界面
- `Logging` 卡片：开关 + 路径输入框（hint 显示平台默认完整路径）+ Change 选目录
- Linux hint：`$HOME/RustDesk/audit.log`；Windows hint：`C:\ProgramData\RustDesk\audit.log`

## 3. 关键文件地图（改哪里 / 合并冲突时先看哪）

| 文件 | 内容 |
|---|---|
| `src/audit_log.rs` | 审计日志全部逻辑：选项常量、`resolve_path`/`expand_tilde`/`home_for_expansion`/`default_path`、写盘与轮转、事件入口（`log_login`/`log_logout`/`log_incoming_connect`/`log_incoming_disconnect`/`note_switch`/`ensure_file_if_enabled`） |
| `src/server/connection.rs` | 挂点：authorize 成功后调 `log_incoming_connect`；连接循环退出调 `log_incoming_disconnect`；字段 `audit_connected_at`（用符号名搜索定位） |
| `src/flutter_ffi.rs` | `main_set_option` 内审计开关钩子（`note_switch`）；登录/登出路径 `log_login`/`log_logout`/`sync_account` |
| `src/ipc.rs` | `Data::Options` 处理器中的 `ensure_file_if_enabled()` |
| `src/tray.rs` | `should_use_toast_notification()`（IoT/Server 判定）、`show_desktop_notify()`（Linux notify-send）及调用点 |
| `flutter/lib/consts.dart` | `kOptionAllowAuditLog` / `kOptionAuditLogPath` |
| `flutter/lib/desktop/pages/desktop_setting_page.dart` | `logging()` 卡片与通知卡片 |
| `flutter/lib/main.dart` | `runConnectionManagerScreen()`：Linux 启动即隐藏 CM 窗口 |
| `flutter/lib/models/server_model.dart` | `_cmShouldAutoShow()` 与 timerCallback / updateClientState / addConnection / _addTab 的门控 |

## 4. 配置项速查

| 选项键 | 含义 | 默认 |
|---|---|---|
| `allow-audit-log` | 审计日志开关（`Y`=开） | 关 |
| `audit-log-path` | 日志文件或目录（支持 `~` 开头；填目录自动补文件名） | 空=平台默认 |
| `audit-account` | 登录账户镜像（服务端内部使用） | 空 |
| `allow-incoming-notify` | 入站连接通知开关 | 关 |

## 5. 安装后验证清单

- [ ] 勾选 Enable logging → 默认路径文件立即出现（Linux：`~/RustDesk/audit.log`）
- [ ] 手动路径填 `~/xxx/audit.log` → Apply → 文件出现在展开后的真实路径
- [ ] 远程连入 → 出现 `CONNECT_IN` 行；断开 → `DISCONNECT_IN` 行（`duration` 正确）
- [ ] Linux：密码自动接入的连接无 GNOME「已就绪」通知、无窗口弹出
- [ ] Windows Server / Win11 IoT：连接时托盘气泡正常
- [ ] 服务日志（info 级）可见 `audit_log:` 诊断行

## 6. 已知平台限制（不是 bug）

- **ESXi 7 的 macOS 客户机无 3D/Metal**：Flutter 界面无法渲染（进程在跑但无窗口）。用 ESXi Web 控制台 / VMRC / VNC 操作该虚拟机。
- **macOS 包未公证**：首次运行前 `xattr -cr /Applications/RustDesk.app`；报「已损坏」加 `codesign --force --deep --sign -`；VM 中若 dyld 报 `no Team ID`，用带 `disable-library-validation` entitlements 的重签（命令见 UPGRADE.md 附录）。
- Windows 老记事本不识别 LF：审计日志在 Windows 已用 CRLF（已处理，勿改回）。

## 7. 发布规则

- tag 形如 `1.5.0-13`（`v` 前缀亦可）推送即触发 **Flutter Tag Build**（`.github/workflows/flutter-tag.yml`），约 80 分钟出 28 个资产并挂到同名 Release
- 仓库现有 tag：`1.5.0-13`（最终版）、`fdroid-version`（CI 附属，**勿删**）
- 已删除的历史 tag/Release：1.5.0-8、1.5.0-10、1.5.0-11、1.5.0-12
