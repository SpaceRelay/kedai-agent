// API 层门面:后端接口封装与线格式类型的统一出口。
//
// 代际: L1(老层·稳 / Anchored Core)——**线格式的单一来源**。
// 判据: 前端全部 HTTP 契约与类型定义集中于此;`types.ts` 与后端
//       `server-rs/src/models/types.rs`、`task_core` 的线格式一一对应,
//       由 `tools/check-contract.mjs` 做字段/枚举差集守卫(缺字段即 FAIL)。
// 纪律: 改线格式须**双端同步**且过 check-contract;本层不得依赖上层(stores/components)。
// 详见 docs/契约-架构与数据.md §2.5。
//
export * from './types';
export * from './health';
export * from './characters';
export * from './sessions';
export * from './worldbooks';
export * from './plugins';
export * from './settings';
export * from './skills';
export * from './scripts';
export * from './quickReplies';
export * from './slash';
export * from './macros';
export * from './agent';
export * from './chat';
export * from './audio';
export * from './tasks';
export * from './diagnostics';
export * from './memory';
export * from './repoIndex';
export * from './undo';
export * from './exec';
