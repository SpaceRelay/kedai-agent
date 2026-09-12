// renderHints.ts — 聊天顶栏两条引导条的可见性判定(纯函数,便于无 DOM 环境测试)
//
// 两条提示互斥且各记各的关闭状态,原因不同:
//   1) HTML 渲染未开:界面完全不显示,用户只看到纯文本 → 引导开启 HTML;
//   2) HTML 已开但 JS 未授权:界面能显示却完全不响应点击(下拉不更新描述、按钮无反应),
//      用户无从判断原因 → 引导授权 JS。
// 拆出为纯函数是为了在项目现有的 node 纯测试模式下覆盖真值表(无 jsdom)。

/** 卡内是否含可渲染界面的启用脚本(有非空替换体) */
export function hasRenderableScripts(
  scripts: ReadonlyArray<{ enabled?: boolean; replace_string?: string | null }>,
): boolean {
  return scripts.some((s) => s.enabled !== false && !!s.replace_string?.trim());
}

export interface RenderHintState {
  characterId: string | null;
  renderHtml: boolean;
  scriptAuthorized: boolean;
  hasRenderable: boolean;
  /** 该角色已被用户关闭 HTML 提示 */
  renderHintDismissed: boolean;
  /** 该角色已被用户关闭 JS 授权提示 */
  jsHintDismissed: boolean;
}

/** 「HTML 未开」提示是否显示(提示开启渲染) */
export function shouldShowRenderHint(s: RenderHintState): boolean {
  return !!s.characterId && s.hasRenderable && !s.renderHtml && !s.renderHintDismissed;
}

/**
 * 「JS 未授权」提示是否显示。只在 HTML 已开时出现:HTML 未开时界面还没显示,
 * 先解决渲染可见性(上一条提示),避免两条同时压在顶栏下。
 */
export function shouldShowJsAuthHint(s: RenderHintState): boolean {
  return (
    !!s.characterId &&
    s.hasRenderable &&
    s.renderHtml &&
    !s.scriptAuthorized &&
    !s.jsHintDismissed
  );
}
