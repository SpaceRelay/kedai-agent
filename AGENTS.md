# AGENTS.md — Kedai 开发说明

> 本文件仅供开发 agent 阅读，用于代码维护、测试与构建；它不是产品运行时提示词，禁止注入聊天模型。
> 产品运行时主 Agent 提示词统一保存在 `DATA_DIR/AGENTS_RUNTIME.md`，由后端文件服务、Engine 与设置页共用同一路径。

## 项目范围

- Rust 后端：`server-rs/`
- Vue 前端：`web/`
- 桌面壳：`src-tauri/`
- 开发数据目录：`data/`

## 实现约定

- 面向用户的提示、错误和代码注释使用简体中文。
- 修改核心行为先写失败测试，再做最小实现。
- 不泄露聊天正文、API Key 或本地真实数据。
- 不顺手实现未要求的安全/API 批次，不提交 Git。

## 验证命令

```powershell
cargo test --manifest-path server-rs/Cargo.toml
npm test -w web
npm run build -w web
cargo build --manifest-path server-rs/Cargo.toml
```
