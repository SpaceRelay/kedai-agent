// characterScriptSandbox.ts — 角色卡脚本沙箱门面:实现按职责拆分至 ./sandbox/(protocol/boot-script/dom-rpc/sanitize/iframe-lifecycle),此处 re-export 保持对外导入面不变
export {
  MAX_BOOT_MESSAGE_BYTES,
  type SandboxCleanup,
  type SandboxEnvironment,
  type SandboxExecutionContext,
} from './sandbox/protocol';
export { sandboxScript } from './sandbox/boot-script';
export { readCardGlobals, readScriptLocalStorage } from './sandbox/dom-rpc';
export {
  broadcastCardEvent,
  broadcastMvuUpdate,
  executeSandboxedCharacterScript,
  sandboxAttributes,
} from './sandbox/iframe-lifecycle';
