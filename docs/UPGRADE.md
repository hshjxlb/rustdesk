# 官方新版升级指南

> 目标：官方 rustdesk/rustdesk 发布新版本（如 1.5.1 / 1.6.0）后，把定制功能带到新版并出包。
> 全程约 2–3 小时（其中 CI 编译约 80 分钟）。当前基线：**1.5.0-13**。
> 定制内容与关键文件先读 [CUSTOMIZATION.md](./CUSTOMIZATION.md)。

## 0. 环境与前置

- Windows 开发机，仓库 `D:\rustdesk`，远端 `origin = github.com/hshjxlb/rustdesk`
- 本机 **没有** Rust / Flutter 工具链 —— 一切编译验证靠 GitHub Actions，本地只做静态检查
- `gh` CLI 不可用，统一用 `curl` + GitHub REST API
- GitHub 凭据（推送、读 job 日志）：
  ```bash
  printf "protocol=https\nhost=github.com\n\n" | git-credential-manager.exe get
  ```
- 访问 GitHub 需要代理时：`export https_proxy=http://127.0.0.1:5132 http_proxy=http://127.0.0.1:5132`
  （502 多为瞬时故障，重试即可）

## 1. 准备

```bash
cd /d/rustdesk
git status                    # 必须干净
git log --oneline -3          # 记录当前基线
git remote add upstream https://github.com/rustdesk/rustdesk.git    # 仅首次
git fetch upstream --tags
```

## 2. 合并官方新版本

推荐 **merge**（保留双方历史，冲突一次解完）：

```bash
git checkout -b upgrade-1.5.1
git merge 1.5.1               # 官方 tag；如本地没有：git merge upstream/tags/1.5.1
```

> 也可以用 rebase（历史更干净）：`git rebase 1.5.1`，定制提交不足 10 个，冲突逐个重放。
> 拿不准就选 merge。

## 3. 冲突解决（重点）

按 CUSTOMIZATION.md 第 3 节的文件地图逐个处理（用符号名搜索定位，不要依赖行号）：

- `src/audit_log.rs`：我们新增的整文件，通常无冲突；若上游出现同名文件，以我们的为准
- `src/server/connection.rs`：恢复两处挂点（authorize 后 `log_incoming_connect`、循环退出 `log_incoming_disconnect`）和 `audit_connected_at` 字段
- `src/ipc.rs`：恢复 `Data::Options` 处理器里的 `ensure_file_if_enabled()`
- `src/tray.rs`：恢复 `should_use_toast_notification()` / `show_desktop_notify()` 及调用点
- `flutter/lib/main.dart`、`models/server_model.dart`：恢复 Linux CM 门控（`_cmShouldAutoShow` 等）
- `flutter/lib/desktop/pages/desktop_setting_page.dart`：恢复 `logging()` 卡片的路径 hint
- 上游若重构了这些区域（函数改名/移动），把定制逻辑按新结构「翻译」过去；上游新功能照常保留

## 4. 本地静态自检（没有编译器，靠眼睛 + CI）

- Rust 侧最易错的两类（v11 全平台失败教训）：
  - **`if let` 模式里不能写类型标注**（`if let Ok(x): Result<...> = ...` 非法）
  - 新符号记得导入（如 `use winreg::enums::HKEY_LOCAL_MACHINE;`）；泛型推断不足时用 turbofish `get_value::<String, _>()`
- `flutter_rust_bridge_codegen` 以**文本方式**解析 `.rs`——任何 Rust 语法错误都会让 `generate-bridge` job 直接 panic，与 rustc 同时挂
- Dart 侧检查括号/引号配对、import 完整性（如 `import 'dart:io' show Platform;`）

## 5. 提交、打 tag、触发 CI

```bash
git add -A && git commit -m "merge upstream 1.5.1 + keep customizations"
git checkout master && git merge upgrade-1.5.1
git push origin master
git tag 1.5.1-1 && git push origin 1.5.1-1     # 触发 Flutter Tag Build
```

- tag 规则（flutter-tag.yml）：`1.5.1-1`、`v1.5.1-1` 均可触发；产品版本号来自上游 Cargo.toml，`-N` 只是我们的发布序号
- **不要**动 `fdroid-version` tag/Release（CI 附属）

## 6. 盯 CI（约 80 分钟）

```bash
TOK=<token>
RUN=$(curl -s -H "Authorization: token $TOK" \
  "https://api.github.com/repos/hshjxlb/rustdesk/actions/runs?branch=1.5.1-1&per_page=1" \
  | python -c "import json,sys;print(json.load(sys.stdin)['workflow_runs'][0]['id'])")
# 总状态
curl -s -H "Authorization: token $TOK" \
  "https://api.github.com/repos/hshjxlb/rustdesk/actions/runs/$RUN" \
  | python -c "import json,sys;d=json.load(sys.stdin);print(d['status'],d['conclusion'])"
# 各 job 明细（定位失败步骤）
curl -s -H "Authorization: token $TOK" \
  "https://api.github.com/repos/hshjxlb/rustdesk/actions/runs/$RUN/jobs?per_page=30" \
  | python -c "import json,sys
for j in json.load(sys.stdin)['jobs']:
    bad=[s['name'] for s in j['steps'] if s['conclusion']=='failure']
    print(j['conclusion'] or j['status'], '|', j['name'], '|', bad)"
```

判定顺序：先看 **generate-bridge** 两个 job（语法错误最先死在这）→ linux-sciter / i686-windows → 全部 success（约 80 分钟）。
job 日志需认证且要跟 302：`curl -u hshjxlb:$TOK -L <logs_url>`

## 7. 出包后

- 确认 Release 资产齐全（约 28 个：deb / rpm / AppImage / flatpak / dmg / msi / apk / exe / sciter 变体）
- PATCH Release：标题、正文、`prerelease: false`（正文格式参考 1.5.0-13）
- 清理失败/中间 tag：`DELETE /releases/{id}` + `DELETE /git/refs/tags/<tag>` + 本地 `git tag -d`
- 更新 docs/CUSTOMIZATION.md 的版本时间线与基线 commit

## 8. 常见坑速查

| 症状 | 原因 / 处理 |
|---|---|
| generate-bridge 与 rustc 同时报语法错 | `if let` 类型标注 / 缺 `use` —— 见第 4 节 |
| macOS 双击无反应 | `xattr -cr /Applications/RustDesk.app`；「已损坏」加 codesign 重签 |
| macOS 终端运行报 dyld `no Team ID and is not a platform binary` | 未签名包被库校验拦截：`sudo codesign --force --deep --sign - /Applications/RustDesk.app`；仍不行用 entitlements（`com.apple.security.cs.disable-library-validation=true`）重签 |
| 进程在跑但 macOS 无窗口（`Could not acquire Metal device`） | 虚拟机无 3D/Metal（ESXi macOS 客户机必现），非 bug —— 用 ESXi Web 控制台 / VMRC / VNC |
| 审计日志空 | 确认开关为 `Y`；看服务日志 info 级 `audit_log:` 行定位路径/权限 |
| Ubuntu 连接弹「已就绪」通知 | v13 已修；若再现检查是否有人绕过 `_cmShouldAutoShow` 直接 show+focus |
| Windows 日志粘成一行 | 写入端必须 CRLF（audit_log.rs 已处理） |
| 连不上 GitHub / 502 | 代理 `http://127.0.0.1:5132`；瞬时 502 重试 |
| job 日志 403 | 带 `-u user:token` 且 `curl -L` 跟随 302 |
