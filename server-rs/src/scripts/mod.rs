// 脚本系统(阶段三 3b):后端 JS 运行时 + TavernHelper 兼容桥 + 脚本树装载
//
// 代际: L3(青层·活 / Frontier)——探索层,**失败必须被隔离**。
// 判据: 执行角色卡携带的不可信 JS(P3 隔离性成立),隔离手段齐备:
//       独立 rquickjs Runtime + 64MB 内存上限 + 1s 协作式中断 + 512KB 栈上限
//       (非显式设置会崩进程,见 runtime.rs)+ 5s 墙钟 + 单轮 32 脚本上限。
// 纪律: 只依赖 L1;不得依赖 L2(services/tools/agents)。**新模板能力的唯一合法落点**
//       (EJS 自研解释器已冻结,见 MAINTENANCE.md §0)。
// 详见 docs/契约-架构与数据.md §2.5、§2.6。
pub mod bridge;
pub mod loader;
pub mod runtime;
