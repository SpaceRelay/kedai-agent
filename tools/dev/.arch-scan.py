# tools/dev/.arch-scan.py —— 后端顶层模块依赖图**一次性分析脚本**（非门禁）
#
# 地位：仅供人工排查时快速打印依赖矩阵，**不接入 CI、不被任何门禁或文档调用**。
#       架构护栏的权威实现是 tools/check-arch.mjs 规则 J（读 tools/arch-layers.json
#       的代际归属做越代判定，且带登记白名单与「只减不增」ratchet）。
#       本脚本无白名单、无基线、不做代际判定——它的输出**不能用作合规结论**。
# 历史：2026-09-14 全仓架构审计期间临时编写，用于交叉验证规则 J 的依赖图。
# 用法：python tools/dev/.arch-scan.py（工作目录须为仓库根）
import os, re, collections

ROOT = 'server-rs/src'
TOPS = sorted([d for d in os.listdir(ROOT) if os.path.isdir(os.path.join(ROOT, d))])

files = {}
for cur, dirs, fs in os.walk(ROOT):
    for f in fs:
        if f.endswith('.rs'):
            p = os.path.join(cur, f)
            rel = os.path.relpath(p, ROOT).replace(os.sep, '/')
            files[p] = rel


def top_of(rel):
    parts = rel.split('/')
    return parts[0] if parts[0] in TOPS else '<root>/' + parts[0]


edges = collections.Counter()
detail = collections.defaultdict(list)
for p, rel in files.items():
    src = open(p, encoding='utf-8', errors='replace').read()
    m = re.search(r'^#\[cfg\(test\)\]', src, re.M)
    prod = src[:m.start()] if m else src
    frm = top_of(rel)
    for mm in re.finditer(r'use\s+crate::([a-z_]+)', prod):
        to = mm.group(1)
        if to in TOPS and to != frm:
            edges[(frm, to)] += 1
            detail[(frm, to)].append(rel)

g = collections.defaultdict(set)
for (a, b) in edges:
    g[a].add(b)

print("=== 顶层模块间依赖 from -> [to...] (生产代码, 已剔除 #[cfg(test)] 之后) ===")
for a in sorted(g):
    print("%-12s -> %s" % (a, ', '.join(sorted(g[a]))))
print()
print("=== 边明细(计数 / 示例文件) ===")
for (a, b), c in sorted(edges.items()):
    print("  %-12s -> %-12s x%-3d %s" % (a, b, c, detail[(a, b)][0]))
print()
print("=== 未作为 crate:: 目标出现的模块(可能是叶子或被间接引用) ===")
targets = set(b for (a, b) in edges)
sources = set(a for (a, b) in edges)
for t in TOPS:
    if t not in targets:
        print("  无入边:", t)
for t in TOPS:
    if t not in sources:
        print("  无出边:", t)
