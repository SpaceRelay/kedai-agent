# tools/dev/.fe-scan.py —— 前端目录依赖图**一次性分析脚本**（非门禁）
#
# 地位：与 .arch-scan.py 同批产出的人工排查工具，**不接入 CI、不被门禁或文档调用**。
#       架构护栏的权威实现是 tools/check-arch.mjs 规则 J（前端方向规则见该文件
#       对 backendAllowed / frontendAllowed 的区分注释）；本脚本**不区分前后端
#       依赖模型**，其「双向对(潜在环)」输出只是线索，不是违规判定。
# 历史：2026-09-14 架构审计期间临时编写，用于找出此前被静默跳过的前端跨目录边。
# 用法：python tools/dev/.fe-scan.py（工作目录须为仓库根）
import os, re, collections

ROOT = 'web/src'
TOPS = sorted([d for d in os.listdir(ROOT) if os.path.isdir(os.path.join(ROOT, d))])
print("TOPS:", TOPS)

files = {}
for cur, dirs, fs in os.walk(ROOT):
    for f in fs:
        if f.endswith('.ts') or f.endswith('.vue'):
            p = os.path.join(cur, f)
            files[p] = os.path.relpath(p, ROOT).replace(os.sep, '/')


def top_of(rel):
    p = rel.split('/')
    return p[0] if p[0] in TOPS else '<root>'


pat = re.compile(r"""from\s+['"]([^'"]+)['"]""")
edges = collections.Counter()
detail = collections.defaultdict(list)
for p, rel in files.items():
    src = open(p, encoding='utf-8', errors='replace').read()
    frm = top_of(rel)
    for mm in pat.finditer(src):
        spec = mm.group(1)
        if spec.startswith('@/'):
            to = spec[2:].split('/')[0]
        elif spec.startswith('.'):
            base = os.path.dirname(rel)
            to = os.path.normpath(os.path.join(base, spec)).replace(os.sep, '/').split('/')[0]
        else:
            continue
        if to in TOPS and to != frm:
            edges[(frm, to)] += 1
            if len(detail[(frm, to)]) < 2:
                detail[(frm, to)].append(rel)

g = collections.defaultdict(set)
for (a, b) in edges:
    g[a].add(b)
print("=== 前端目录间依赖 ===")
for a in sorted(g):
    print("%-14s -> %s" % (a, ', '.join(sorted(g[a]))))
print()
print("=== 双向对(潜在环) ===")
for (a, b) in sorted(edges):
    if (b, a) in edges:
        print("  %s <-> %s  (%d / %d)  例:%s | %s" % (a, b, edges[(a, b)], edges[(b, a)], detail[(a, b)][0], detail[(b, a)][0]))
print()
print("=== 边计数 ===")
for (a, b), c in sorted(edges.items()):
    print("  %-14s -> %-14s x%-3d %s" % (a, b, c, detail[(a, b)][0]))
