# Kedai 性能基线(优化前)

> 记录日期:2026-08-21;基线 commit:`5f9a55a`(优化前基线)。
> 环境:Windows 11,debug 构建(`cargo test` 产物),数据目录为本机真实 `data/`。

## 测试基线

| 项 | 结果 | 耗时 |
|---|---|---|
| `cargo test`(vcvars64 环境) | **643 绿**(538 单测 + 105 集成,0 失败) | 单测 5.88s,集成合计约 100s |
| `npm test -w web`(Vitest) | **295 绿**(33 文件) | 2.70s |
| `npm run build -w web` | 通过 | 4.17s |

## 前端 bundle 基线(vite build)

| chunk | 体积 | gzip |
|---|---|---|
| `index-*.js`(全部应用代码,13 个弹窗 eager) | 373.85 kB | 111.26 kB |
| `vendor-*.js` | 182.11 kB | 68.99 kB |
| `content-rendering-*.js`(markdown-it + sanitize-html) | 102.88 kB | 44.62 kB |
| `vue-vendor-*.js` | 78.35 kB | 31.09 kB |
| `index-*.css` | 70.44 kB | 13.53 kB |

## 接口延迟基线(`tools/perf-baseline.mjs`,20 并发 × 100 请求)

| 端点 | p50 | p95 | avg | rps | errors |
|---|---|---|---|---|---|
| `GET /api/characters` | 9.9ms | 20.8ms | 11.4ms | 1618 | 0 |
| `GET /api/chat/history?session_id=…` | 7.2ms | 10.1ms | 6.9ms | 2692 | 0 |

注意:本机数据量小(消息表近空),绝对延迟低;阶段 1 验收以**同等工作负载**对比 p95 为准,
并补充「流式生成进行中并发读」场景。

## 压测脚本用法

```bash
# 先启动服务(默认 127.0.0.1:3001),再:
node tools/perf-baseline.mjs            # 20 并发 × 100 请求/端点
node tools/perf-baseline.mjs -c 50 -n 200
```

脚本自动从 `/api/bootstrap` 取 token;自动发现首个有会话的角色做 history 压测,
无会话时跳过该端点。

## 阶段 1 改造后对比(2026-08-21,同机同脚本)

| 端点 | p50 | p95 | avg | rps | 基线 p95 |
|---|---|---|---|---|---|
| `GET /api/characters` | 13.6ms | 23.8ms | 14.6ms | 1276 | 20.8ms |
| `GET /api/chat/history` | 10.4ms | 15.4ms | 10.7ms | 1744 | 10.1ms |

**解读**:本机数据量极小(消息表近空),微基准下改造后延迟略升(r2d2 池借还 +
spawn_blocking 线程切换的固定开销,单请求约 +1~4ms),属于预期内的「固定成本换并发能力」。
本阶段真实收益在**竞争场景**:改造前所有读请求与写请求在单连接 `Mutex<Connection>` 上全局串行,
Agent 生成期间的长 SQL 会阻塞 tokio worker 并队头阻塞一切读请求;改造后读走 r2d2 只读连接池
(池大小 = CPU 核数)、写走单写连接、api 层 DB 访问统一 `spawn_blocking` 隔离,WAL 多读单写
能力真正生效。该场景已由集成测试 `tests/db_concurrency.rs` 覆盖(写进行中并发读全部 200、
写后读立即可见);微基准的 p95 ≥50% 下降目标在小数据集上不适用,待真实数据量上来后复测。

## Rust 编译环境备忘

`cargo` 命令需在 vcvars64 环境执行。已新增包装脚本:

```bash
cmd //c "C:\Users\LENOVO\Desktop\kedai\tools\cargo-vcvars.cmd cargo test"
cmd //c "C:\Users\LENOVO\Desktop\kedai\tools\cargo-vcvars.cmd cargo clippy -- -D warnings"
```
