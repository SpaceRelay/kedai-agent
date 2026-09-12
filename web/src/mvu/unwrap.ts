// mvu 双树模型「裸值解包 / 显示串构造」共享实现(重构 D-6 收敛点)。
//
// 双树模型(约定源自 MagVarUpdate,原作者:MagicalAstrogy,
// github.com/MagicalAstrogy/MagVarUpdate,MIT;Kedai 为独立兼容实现):
//   - stat_data:逻辑值树,叶子为 [新值, 更新原因] 成对数组(如 ['2027', '时间流逝']);
//   - display_data:展示树,叶子为 "老值->新值(原因)" 字符串(如 '2026->2027(时间流逝)')。
// 消费方(状态栏脚本、_.get、回放镜像)需要的是「裸值」——即叶子对的 [0]。
// 本模块是解包与显示串构造的唯一宿主实现;此前同一逻辑散落在
// variables.ts / initvar.ts / host.ts / sandbox/boot-script.ts 五处副本中。
//
// 注意:sandbox/boot-script.ts 是字符串注入的沙箱 boot 代码,无法 import 本模块,
// 其内联副本(__kdUnwrap 与 _.get 的解包段)与本文件保持语义镜像,两侧注释互指,
// 改动必须双侧同改(见各函数头的「镜像」注释)。

/** 显示串构造:display_data 叶子统一为 "老值->新值(原因)"。
 *  两侧值一律经 String() 字符串化;调用方负责老值侧的 null/undefined 兜底
 *  (variables.applyUpdate 的 set 分支传 oldVal ?? '',remove 分支传 '' 配 'null')。 */
export function displayChange(oldValue: unknown, newValue: unknown, reason: string): string {
  return `${String(oldValue)}->${String(newValue)}(${reason})`;
}

/**
 * 宽松叶子对判定:任意长度 ≥1 的数组即视为 [值, 原因] 叶子对。
 * 与 MagVarUpdate 生态的 _.get 约定一致(历史实现 variables.statValue / host _.get
 * 均用此判定);比 unwrapStatTree 的严格判定(长度=2 且 [1] 为字符串)更宽容——
 * 点路径已定位到叶子时,非空数组按约定就是成对存储,直接取 [0]。
 */
export function isStatLeafArray(v: unknown): v is unknown[] {
  return Array.isArray(v) && v.length >= 1;
}

/**
 * 宽松叶子解包(单点,不递归):[值, 原因] 叶子对 → [0];其余原样返回。
 * 用于已知「此处应是叶子」的点解包场景:
 *   - variables.statValue(命令应用时读旧值);
 *   - host 与沙箱两侧 _.get 的自动解包(MagVarUpdate 生态约定)。
 * 镜像:sandbox/boot-script.ts 内联 _.get 的解包段与本函数语义同步,改动必须双侧同改。
 */
export function unwrapStatLeaf(v: unknown): unknown {
  return isStatLeafArray(v) ? v[0] : v;
}

/**
 * 严格递归整树解包:把整棵 stat_data 树转为裸值视图(状态栏脚本直接属性读)。
 * 判定比 unwrapStatLeaf 严格:仅「长度=2 且第二元素为字符串」的数组视为 [值, 原因]
 * 叶子对并取 [0];其余数组视为真实数组值递归逐元素解包,对象递归逐键解包,标量原样。
 * 严格判定的原因:整树遍历时无法先验区分「叶子对」与「真实数组值」(如 [1,2,3]),
 * 宽松判定会把真实数组错拆成首元素;点路径场景(unwrapStatLeaf)无此歧义。
 *
 * 镜像:本函数与 sandbox/boot-script.ts 的 __kdUnwrap 语义同步,改动必须双侧同改。
 * 源函数内容 SHA-256 短哈希:6ccd4bda787e(口径:unwrapStatTree 函数全文,
 * 自 "export function" 起至收尾大括号做 SHA-256 取前 12 位,不含本 JSDoc;
 * 改动本函数后重算并同步 boot-script.ts 侧注释,两侧哈希不一致即漂移)。
 */
export function unwrapStatTree(v: unknown): unknown {
  if (Array.isArray(v)) {
    if (v.length === 2 && typeof v[1] === 'string') return v[0];
    return v.map(unwrapStatTree);
  }
  if (v !== null && typeof v === 'object') {
    const o: Record<string, unknown> = {};
    for (const k of Object.keys(v)) o[k] = unwrapStatTree((v as Record<string, unknown>)[k]);
    return o;
  }
  return v;
}
