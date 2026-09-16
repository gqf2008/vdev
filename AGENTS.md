# Repository Guidelines

vdev（虚拟设备集合）是一个 Rust workspace：在 macOS / Windows 上造虚拟 HID、虚拟屏幕、虚拟摄像头、
虚拟声卡与 mic-agent 等设备，外加 `vdev` 命令行工具箱与宿主 App。

## 协同拓扑（主仓 / 镜像 / 发布）

- **主仓 + 协同 = walgit**（remote `origin`：`http://127.0.0.1:8081/gqf2008/vdev.git`）。
  - Web UI（含 Collab 页）：`http://127.0.0.1:8081`；
  - collab CLI：`walgit --config ~/.walgit/walgit.toml collab <ls|thread|pr|report|board> ...`
    （`walgit` 在 PATH：`~/.local/bin/walgit`；底层二进制在 `/Applications/walgit-tray.app/Contents/Resources/walgit`）。
- **GitHub = 只读镜像 + 发布通道**（remote `github`：`https://github.com/gqf2008/vdev.git`）。
  日常开发/评审不直接走 GitHub。
  - 镜像：本机 `~/.walgit/sync-to-github.sh` 常驻循环（screen `walgit-sync-github`，60s）自动发现
    walgit 上的仓库，把 `refs/heads/*` + `refs/tags/*` 镜像到 GitHub（`refs/collab/*` 属 walgit 特有，不推）；
  - 发布：在 main 打 tag → push `origin`（walgit）→ 镜像自动同步到 GitHub →
    `gh release create <tag> --generate-notes`（GitHub Releases）。
- push / pull / fetch 默认走 `origin`（walgit）；worktree 一律从本 checkout 的 `origin/main` 开，
  且必须落在仓库内 `.worktrees/<name>`（见全局规则 `RULE_worktree必须建在当前工程子目录下.md`）。
- issue / PR / review / CI 状态以 walgit collab 线程为准（`collab board` / `collab pr <id>`）。

### 协同记账铁律

开发流程每一步都要在 walgit collab 写**签名条目**，只开 git 分支不记账 = 没有任何协作记录：

| 步骤 | 条目 |
|---|---|
| 建 issue（线程根） | `--kind issue --body '{"title":"…","body":"…"}'`（parent 空） |
| 开工 | `--kind comment`（注明 worktree/分支）+ `--kind status`（`{"status":"in-progress","owner":"…","worktree":"wt-x","branch":"…"}`） |
| 实现完 | `--kind patch`（`--base refs/heads/main --head refs/heads/<分支>`）→ 线程变 PR + `status: needs-review` |
| 独立审查 | `--kind review`（`{"decision":"approve","agent":"…","note":"…"}`） |
| 合并 | 本地合并 push `origin` → `--kind merge_result` ×2 → `--kind status`（`closed`） |

命令/key/schema 细节见 walgit skill（`~/.agents/skills/walgit/SKILL.md`），签名 key `~/.walgit/keys/sqb.ed25519`。

## 本仓门禁（合并前必须全绿）

```bash
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --release          # 最终链接：FFI 框架声明错只有 build 会暴露
bash scripts/acceptance/macos-smoke.sh     # 验收脚本语法 + Swift 探针 typecheck + 语义守卫
python3 scripts/check-docs.py              # 文档围栏/断链
```

- walgit 去中心化 CI 跑其中的 `fmt` 与 `acceptance-smoke`（`.walgit/ci.toml`）；
  Windows 各 workspace 与 WDK 驱动矩阵仍在 GitHub Actions（镜像 push 触发）。
- 真机验收脚本（摄像头/声卡/HID 前台注入等）需要外设与前台焦点，按 `docs/` 与
  `scripts/acceptance/README.md` 的说明在真机上跑，结果写回对应 issue。

## 目录约定

- `crates/vdev-hid`、`crates/vdev-screen`、`crates/vdev-camera`、`crates/vdev-camera-ext`、`crates/vdev-audio`、
  `crates/vdev-filter`、`crates/vdev-mic-agent`、`crates/vdev-host`（`vdev` CLI）、`crates/vdev-app`（宿主 App）；
- Windows 侧 `crates/*-win` 各自是独立 workspace（自带 Cargo.lock），不参与根 workspace 构建；
- `docs/community/` 与 `docs/publish/` 是面向用户与发布稿的文档，改动代码时同步更新其中的
  命令/行号引用（`scripts/check-docs.py` 只查围栏与断链，行号要人工核）。
