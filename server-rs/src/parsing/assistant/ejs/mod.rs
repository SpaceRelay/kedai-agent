// 酒馆助手(SillyTavern-Assistant)兼容:EJS 模板引擎(迷你 JS 解释器)。
//
// 支持语法(酒馆助手角色卡常用子集,按「更完整」档实现):
//   <% code %>           执行语句,不输出
//   <%= expr %> / <%- expr %>  输出表达式求值结果(不转义,prompt 场景)
//   <%_ code _%>         空白修剪变体:<%_ 移除标签前空白,_%> 移除标签后空白(含换行)
//   <%# comment %>       注释,不输出
//   语句:if/else if/else、for(;;)、for...of、while、var/let/const、块、
//         function 声明/表达式、箭头函数、return/break/continue、表达式语句(含赋值/++/--)
//   表达式:数字/字符串(单双引号、模板字符串 `${}`)/布尔/null/undefined、
//          对象/数组字面量、成员访问、下标、函数调用、一元 ! - +、二元
//          || && == != === !== < <= > >= + - * / %、三元 ?:
//   内建函数:getvar/setvar/addvar、parseInt/parseFloat/Number/String/Boolean/isNaN、
//            Math.*、数组 forEach/map/filter/push/pop/join/includes/indexOf、
//            字符串方法(length/toUpperCase/toLowerCase/includes/startsWith/endsWith/
//            split/join/trim/replace/charAt/indexOf/slice/substring)、
//            Object.keys/values/entries、JSON.stringify/parse、
//            matchChatMessages、print、parseJSON、jsonPatch、evalTemplate、
//            变量写入别名(setGlobalVar/setLocalVar/setMessageVar)、incvar/decvar、
//            variables 常量(变量树根,即 getvar(null) 语义)
//   ST-Prompt-Template 兼容读取类(无上下文容错,渲染时返回默认值绝不报错):
//            getwi/getWorldInfo、getchar/getChara、getpreset/getPresetPrompt、
//            getqr/getQuickReply、getChatMessage、getChatMessages、
//            injectPrompt、getPromptsInjected、hasPromptsInjected
//
// 语义:JS 宽松语义简化版(undefined 参与数值运算得 NaN、== null 匹配 undefined/null、
//      数字字符串自动转换、falsy = undefined/null/false/0/NaN/"")。
// 容错:单块语法/求值错误 → 跳过该块输出空并收集错误信息,绝不 panic。
//
// 目录模块分层(lexer → ast → parser → value → env → eval → exec,渲染入口在本文件):
//   mod.rs    模板编译 + 整体执行入口(render_template/quote_js_string/find_tag_end)
//   lexer.rs  词法分析(Tok/Kw/P + lex 系列)
//   ast.rs    语法树定义(Stmt/Expr/运算符枚举)
//   parser.rs 递归下降解析(Parser + expr_to_target)
//   value.rs  运行时值(JsValue/JsFn/Builtin + 类型转换/比较)
//   env.rs    执行环境(Env/Flow + 内建全局与属性读写)
//   eval.rs   表达式求值(eval_expr/binary_eval/...)
//   exec.rs   语句执行与函数调用(exec/call/call_builtin)
use super::{AssistantVars, RenderCtx};

mod ast;
mod env;
mod eval;
mod exec;
mod lexer;
mod parser;
mod value;

// 供 assistant 模块(AssistantVars::get_js/set_js/add_js)引用的公共项
use env::Env;
use exec::exec;
use lexer::lex;
use parser::Parser;
pub(crate) use value::{js_to_json, value_to_js, JsValue};

// ===================== 模板渲染 =====================

/// 渲染 EJS 模板(兼容入口:空渲染上下文,保持既有「无上下文容错」语义)。
/// 公开兼容入口仍由 assistant::render_assistant_content 提供;本函数保留供模块内测试复用。
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_template(
    template: &str,
    vars: &mut AssistantVars,
    locals: &[(&str, JsValue)],
) -> (String, Vec<String>) {
    let mut ctx = RenderCtx::new(vars);
    render_template_with_ctx(template, &mut ctx, locals)
}

/// 渲染 EJS 模板(带渲染上下文:内建 getwi/getchar/getqr/getChatMessage/injectPrompt
/// 等读取类函数经 ctx 读写;渲染副作用(变量写入/injectPrompt 登记)落回 ctx)。
///
/// 实现方式:把模板**编译为单一 JS 程序**再整体执行——
///   - 文本段 → `__kedai_out__("...");`(输出语句)
///   - `<% code %>` → 原样语句(if/for 的 `{` 与 `}` 可跨标签配对)
///   - `<%= expr %>` / `<%- expr %>` → `__kedai_out__(String(expr));`
///
/// 所有语句共享同一个 Env,局部变量跨标签保留(与真实 EJS 语义一致)。
/// 返回 (输出文本, 错误列表)——语句出错跳过该语句并记录错误,不影响其余部分。
pub(crate) fn render_template_with_ctx(
    template: &str,
    ctx: &mut RenderCtx<'_>,
    locals: &[(&str, JsValue)],
) -> (String, Vec<String>) {
    // 渲染资源预算(迭代步数 + 墙钟):角色卡不可信,模板可含死循环。
    // 守卫在本函数退出(含 panic 展开)时恢复预算,嵌套渲染共享同一预算。
    let _render_budget = exec::RenderGuard::new();
    // <#escape-ejs> 块预处理:块内 `<%`/`%>` 替换为唯一占位符,避免被编译阶段
    // 当作 EJS 标签执行(原样输出、不执行);渲染完成后按序还原。占位符含控制字符,
    // lexer 字符串解析会原样保留,不会被当作标签。
    let (escaped, escape_replaces) = escape_ejs_blocks(template);
    let mut errors = Vec::new();

    // ===== 1. 编译:模板 → JS 程序源码 =====
    let mut src = String::with_capacity(escaped.len() + 64);
    let bytes = escaped.as_bytes();
    let mut i = 0usize;
    let mut text = String::new();
    // <%_ 之后的文本段需要 trim 前导空白
    let mut trim_next_text = false;

    while i < bytes.len() {
        if bytes[i] == b'<' && i + 1 < bytes.len() && bytes[i + 1] == b'%' {
            let mut j = i + 2;
            let mut trim_l = false;
            if j < bytes.len() && bytes[j] == b'_' {
                trim_l = true;
                j += 1;
            }
            let mut kind = 'c'; // c=code, o=output, h=comment
            if j < bytes.len() && (bytes[j] == b'=' || bytes[j] == b'-') {
                kind = 'o';
                j += 1;
            } else if j < bytes.len() && bytes[j] == b'#' {
                kind = 'h';
                j += 1;
            }
            let rest = &escaped[j..];
            let mut trim_r = false;
            let Some(end_idx) = find_tag_end(rest, &mut trim_r) else {
                errors.push(format!("EJS 标签未闭合(位置 {i})"));
                // 无法继续解析,剩余全部当文本
                text.push_str(&escaped[i..]);
                break;
            };
            let inner = &rest[..end_idx];
            // %> 结束符 2 字节(_%> 的 end_idx 指向 `_`,共 3 字节)
            let consumed = (j - i) + end_idx + if trim_r { 3 } else { 2 };

            // 前导 trim:移除已累积文本的尾部空白
            if trim_l {
                while text.ends_with(char::is_whitespace) {
                    text.pop();
                }
            }
            if trim_next_text {
                text = text.trim_start().to_string();
                trim_next_text = false;
            }
            // 文本段 → 输出语句
            if !text.is_empty() {
                src.push_str("__kedai_out__(");
                src.push_str(&quote_js_string(&text));
                src.push_str(");\n");
            }
            text.clear();

            match kind {
                'o' => {
                    let expr = inner.trim();
                    if !expr.is_empty() {
                        src.push_str("__kedai_out__(String(");
                        src.push_str(expr);
                        src.push_str("));\n");
                    }
                }
                'c' => {
                    src.push_str(inner);
                    // 追加分号保证语句边界(双分号无害;错误恢复靠它定位语句结束)
                    src.push_str(";\n");
                }
                _ => {} // 注释
            }
            // 尾部 trim:后续文本段前导空白清除
            if trim_r {
                trim_next_text = true;
            }
            // %> 后紧跟换行:吃掉(标准 EJS 行为)
            let mut k = i + consumed;
            if k < bytes.len() && bytes[k] == b'\r' && k + 1 < bytes.len() && bytes[k + 1] == b'\n'
            {
                k += 2;
            } else if k < bytes.len() && bytes[k] == b'\n' {
                k += 1;
            }
            i = k;
            continue;
        }
        // 外层循环条件 i < bytes.len() 保证必有下一字符
        let ch = escaped[i..].chars().next().expect("i 在界内,必有下一字符");
        text.push(ch);
        i += ch.len_utf8();
    }
    if trim_next_text {
        text = text.trim_start().to_string();
    }
    if !text.is_empty() {
        src.push_str("__kedai_out__(");
        src.push_str(&quote_js_string(&text));
        src.push_str(");\n");
    }

    // ===== 2. 解析并整体执行 =====
    let mut out = String::new();
    match lex(&src) {
        Ok(toks) => {
            let mut p = Parser::new(&toks);
            let stmts = p.parse_program();
            errors.extend(p.errors.iter().map(|e| format!("EJS 语法错误: {e}")));
            let mut env = Env::new(&mut *ctx.vars, locals, &mut out, &mut ctx.data);
            for s in &stmts {
                if let Err(e) = exec(&mut env, s) {
                    errors.push(format!("EJS 执行失败: {e}"));
                }
            }
        }
        Err(e) => errors.push(format!("EJS 词法错误: {e}")),
    }
    // ===== 3. 还原 escape 块占位符 =====
    let out = restore_escape_placeholders(&out, &escape_replaces);
    (out, errors)
}

/// <#escape-ejs> 块预处理:块内 `<%` 与 `%>` 替换为唯一占位符(渲染后还原),
/// 起止标签是模板控制标记,不进入输出;块内 EJS 标签原样输出、不执行。
/// 返回 (替换后的模板, 占位符列表——按类型编码 OPEN/CLOSE,还原时区分)。
fn escape_ejs_blocks(template: &str) -> (String, Vec<String>) {
    const OPEN_TAG: &str = "<#escape-ejs";
    const CLOSE_TAG: &str = "</#escape-ejs";
    let mut out = String::with_capacity(template.len());
    let mut replaces: Vec<String> = Vec::new();
    let mut i = 0usize;
    let mut block_open = false;
    let bytes = template.as_bytes();
    while i < bytes.len() {
        // 起止标签(ASCII 大小写不敏感;标签前缀后允许空白再跟 '>'),匹配则跳过(不输出)
        if !block_open && starts_with_ci_ascii(template, i, OPEN_TAG) {
            if let Some(gt) = find_tag_gt(template, i, OPEN_TAG.len()) {
                i = gt + 1;
                block_open = true;
                continue;
            }
        }
        if block_open && starts_with_ci_ascii(template, i, CLOSE_TAG) {
            if let Some(gt) = find_tag_gt(template, i, CLOSE_TAG.len()) {
                i = gt + 1;
                block_open = false;
                continue;
            }
        }
        if block_open {
            // 块内:转义 EJS 标签(占位符编号独立递增,类型由编码区分)
            if i + 1 < bytes.len() && bytes[i] == b'<' && bytes[i + 1] == b'%' {
                let ph = format!("\x01KDAI_ESC_OPEN{}\x01", replaces.len());
                out.push_str(&ph);
                replaces.push(ph);
                i += 2;
                continue;
            }
            if i + 1 < bytes.len() && bytes[i] == b'%' && bytes[i + 1] == b'>' {
                let ph = format!("\x01KDAI_ESC_CLOSE{}\x01", replaces.len());
                out.push_str(&ph);
                replaces.push(ph);
                i += 2;
                continue;
            }
        }
        // 外层循环条件 i < bytes.len() 保证必有下一字符
        let ch = template[i..].chars().next().expect("i 在界内,必有下一字符");
        out.push(ch);
        i += ch.len_utf8();
    }
    (out, replaces)
}

/// 标签前缀后允许空白再找 `>`(如 `<#escape-ejs >`);找不到返回 None
fn find_tag_gt(template: &str, start: usize, prefix_len: usize) -> Option<usize> {
    let bytes = template.as_bytes();
    let mut j = start + prefix_len;
    while j < bytes.len() && (bytes[j] == b' ' || bytes[j] == b'\t') {
        j += 1;
    }
    if j < bytes.len() && bytes[j] == b'>' {
        Some(j)
    } else {
        None
    }
}

/// s[at..] 是否以 needle 开头(仅 ASCII 大小写不敏感)
fn starts_with_ci_ascii(s: &str, at: usize, needle: &str) -> bool {
    let rest = &s[at..];
    if rest.len() < needle.len() {
        return false;
    }
    rest.as_bytes()[..needle.len()]
        .iter()
        .zip(needle.bytes())
        .all(|(a, b)| a.eq_ignore_ascii_case(&b))
}

/// 渲染后还原 escape 块占位符:OPEN 占位 → `<%`,CLOSE 占位 → `%>`(按编码类型还原)
fn restore_escape_placeholders(out: &str, replaces: &[String]) -> String {
    let mut s = out.to_string();
    for ph in replaces {
        let repl = if ph.contains("ESC_OPEN") { "<%" } else { "%>" };
        s = s.replace(ph, repl);
    }
    s
}

/// 文本 → JS 字符串字面量(转义引号/反斜杠/换行)
fn quote_js_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(c),
        }
    }
    out.push('"');
    out
}

/// 从 rest 找标签结束 `%>` 或 `_%>`;返回结束符前索引,并设置 trim_r
fn find_tag_end(rest: &str, trim_r: &mut bool) -> Option<usize> {
    let bytes = rest.as_bytes();
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 1 < bytes.len() && bytes[i + 1] == b'>' {
            return Some(i);
        }
        if bytes[i] == b'_'
            && i + 1 < bytes.len()
            && bytes[i + 1] == b'%'
            && i + 2 < bytes.len()
            && bytes[i + 2] == b'>'
        {
            *trim_r = true;
            return Some(i);
        }
        i += 1;
    }
    None
}

// ===================== 测试 =====================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsing::assistant::AssistantVars;
    use serde_json::json;

    fn render(template: &str, vars: &mut AssistantVars) -> String {
        let (out, errors) = render_template(template, vars, &[]);
        assert!(errors.is_empty(), "渲染错误: {errors:?}\n模板: {template}");
        out
    }

    #[test]
    fn renders_plain_text() {
        let mut v = AssistantVars::new();
        assert_eq!(render("你好,世界", &mut v), "你好,世界");
    }

    #[test]
    fn renders_output_tag() {
        let mut v = AssistantVars::new();
        assert_eq!(render("1+2=<%= 1 + 2 %>", &mut v), "1+2=3");
        assert_eq!(render("str=<%= 'a' + 'b' %>", &mut v), "str=ab");
        assert_eq!(render("s=<%- 7 * 6 %>", &mut v), "s=42");
    }

    #[test]
    fn renders_if_else_chain() {
        let mut v = AssistantVars::new();
        v.set("心之所向.好感度", json!(0));
        let tpl = "<%_ if (getvar('stat_data.心之所向.好感度') <= 0) { _%>惊弓之鸟<%_ } else if (getvar('stat_data.心之所向.好感度') <= 100) { _%>羞怯壁花<%_ } else { _%>其他<%_ } _%>";
        assert_eq!(render(tpl, &mut v), "惊弓之鸟");
        v.set("心之所向.好感度", json!(50));
        assert_eq!(render(tpl, &mut v), "羞怯壁花");
        v.set("心之所向.好感度", json!(250));
        assert_eq!(render(tpl, &mut v), "其他");
    }

    #[test]
    fn undefined_comparison_is_false() {
        let mut v = AssistantVars::new();
        let tpl = "<% if (getvar('不存在') <= 0) { %>A<% } else { %>B<% } %>";
        assert_eq!(render(tpl, &mut v), "B");
        // undefined >= 0 也 false
        let tpl2 = "<% if (getvar('不存在') >= 0) { %>A<% } else { %>B<% } %>";
        assert_eq!(render(tpl2, &mut v), "B");
    }

    #[test]
    fn renders_for_and_arrays() {
        let mut v = AssistantVars::new();
        let tpl = "<% var list = ['a', 'b', 'c']; for (var i = 0; i < list.length; i++) { %><%= list[i] %><% } %>";
        assert_eq!(render(tpl, &mut v), "abc");
        let tpl2 =
            "<% var list = ['x', 'y']; list.forEach(function (item) { %><%= item %><% }); %>";
        assert_eq!(render(tpl2, &mut v), "xy");
        let tpl3 = "<% var list = [1, 2, 3]; var out = list.map(function (n) { return n * 2; }); %><%= out.join(',') %>";
        assert_eq!(render(tpl3, &mut v), "2,4,6");
    }

    #[test]
    fn renders_template_literals() {
        let mut v = AssistantVars::new();
        v.set("世界.年分", json!(2024));
        let tpl = "<%= `现在是 ${getvar('世界.年分')} 年` %>";
        assert_eq!(render(tpl, &mut v), "现在是 2024 年");
    }

    #[test]
    fn setvar_addvar_write_tree() {
        let mut v = AssistantVars::new();
        v.set("计数", json!(10));
        let tpl = "<% setvar('计数', getvar('计数') + 5); %><%= getvar('计数') %>";
        assert_eq!(render(tpl, &mut v), "15");
        let tpl2 = "<% addvar('计数', 3); %><%= getvar('计数') %>";
        assert_eq!(render(tpl2, &mut v), "18");
    }

    #[test]
    fn math_and_string_methods() {
        let mut v = AssistantVars::new();
        assert_eq!(render("<%= Math.floor(3.7) %>", &mut v), "3");
        assert_eq!(render("<%= Math.max(1, 5, 3) %>", &mut v), "5");
        assert_eq!(render("<%= 'Hello'.toUpperCase() %>", &mut v), "HELLO");
        assert_eq!(render("<%= 'a,b,c'.split(',').length %>", &mut v), "3");
        assert_eq!(render("<%= parseInt('42px') %>", &mut v), "42");
    }

    #[test]
    fn comments_are_ignored() {
        let mut v = AssistantVars::new();
        assert_eq!(render("A<%# 注释 %>B", &mut v), "AB");
    }

    #[test]
    fn trim_variants() {
        let mut v = AssistantVars::new();
        let tpl = "  <%_ var x = 1; _%>  内容  ";
        assert_eq!(render(tpl, &mut v), "内容  ");
    }

    #[test]
    fn error_skips_block_only() {
        let mut v = AssistantVars::new();
        let (out, errors) = render_template("前<% var = 3 %>后", &mut v, &[]);
        assert_eq!(out, "前后");
        assert!(!errors.is_empty());
    }

    #[test]
    fn object_literal_and_ternary() {
        let mut v = AssistantVars::new();
        let tpl = "<% var o = { name: '芽衣', age: 16 }; %><%= o.name %>/<%= o.age >= 18 ? '成年' : '未成年' %>";
        assert_eq!(render(tpl, &mut v), "芽衣/未成年");
    }

    #[test]
    fn closure_capture_in_callback() {
        let mut v = AssistantVars::new();
        let tpl = "<% var total = 0; var nums = [1, 2, 3]; nums.forEach(function (n) { total = total + n; }); %><%= total %>";
        assert_eq!(render(tpl, &mut v), "6");
    }

    /// 箭头函数默认参数:`(key, indent = 0) => ...`(酒馆助手卡常见)
    #[test]
    fn arrow_default_params() {
        let mut v = AssistantVars::new();
        let tpl = "<% const f = (a, b = 3) => a + b; %><%= f(2) %>/<%= f(2, 4) %>";
        assert_eq!(render(tpl, &mut v), "5/6");
        let tpl2 = "<% const g = (x, y = x * 2) => y; %><%= g(5) %>";
        assert_eq!(render(tpl2, &mut v), "10");
    }

    /// getMessageVar / getGlobalVar / getLocalVar 别名映射到变量树
    #[test]
    fn message_and_global_var_aliases() {
        let mut v = AssistantVars::new();
        v.set("用户.名称", json!("漂泊者"));
        assert_eq!(
            render("<%= getMessageVar('用户.名称') %>", &mut v),
            "漂泊者"
        );
        assert_eq!(render("<%= getGlobalVar('用户.名称') %>", &mut v), "漂泊者");
        assert_eq!(render("<%= getLocalVar('用户.名称') %>", &mut v), "漂泊者");
    }

    /// trimEnd / repeat / find 等补充方法
    #[test]
    fn added_string_and_array_methods() {
        let mut v = AssistantVars::new();
        assert_eq!(render("<%= '  abc  '.trimEnd() %>", &mut v), "  abc");
        assert_eq!(render("<%= 'ab'.repeat(3) %>", &mut v), "ababab");
        assert_eq!(
            render(
                "<%= [1, 2, 3].find(function (n) { return n > 1; }) %>",
                &mut v
            ),
            "2"
        );
    }

    /// matchChatMessages(渲染上下文无历史)→ false,不报错
    #[test]
    fn match_chat_messages_is_false() {
        let mut v = AssistantVars::new();
        let tpl = "<% if (matchChatMessages(['家', '黑海岸'])) { %>A<% } else { %>B<% } %>";
        assert_eq!(render(tpl, &mut v), "B");
    }

    /// 中文标识符(属性访问等)可用
    #[test]
    fn unicode_identifiers() {
        let mut v = AssistantVars::new();
        v.set("数据.是否在场", json!(true));
        // 中文成员访问读取变量树
        let tpl = "<% var data = getvar('数据'); %> <%= data.是否在场 ? '在场' : '缺席' %>";
        assert_eq!(render(tpl, &mut v), " 在场");
        // 中文属性链继续访问
        let tpl2 =
            "<% var cg = { 角色: { 漂泊者: { 形态: ['A'] } } }; %><%= cg.角色.漂泊者.形态[0] %>";
        assert_eq!(render(tpl2, &mut v), "A");
    }

    // ===== ST-Prompt-Template 兼容读取类函数 =====

    /// 无上下文容错读取:getwi/getWorldInfo/getchar/getpreset/getqr/getChatMessage 均返回 ""
    #[test]
    fn st_read_functions_return_empty_string() {
        let mut v = AssistantVars::new();
        assert_eq!(render("<%= getwi('地名') %>", &mut v), "");
        // getwi 带可选 title 参数
        assert_eq!(render("<%= getwi('地名', '标题') %>", &mut v), "");
        // getWorldInfo 别名
        assert_eq!(render("<%= getWorldInfo('地名') %>", &mut v), "");
        assert_eq!(render("<%= getchar('芽衣') %>", &mut v), "");
        assert_eq!(render("<%= getChara('芽衣') %>", &mut v), "");
        assert_eq!(render("<%= getpreset('main') %>", &mut v), "");
        assert_eq!(render("<%= getPresetPrompt('main') %>", &mut v), "");
        assert_eq!(render("<%= getqr('确认') %>", &mut v), "");
        assert_eq!(render("<%= getQuickReply('确认') %>", &mut v), "");
        assert_eq!(render("<%= getChatMessage(0, 'user') %>", &mut v), "");
        assert_eq!(render("<%= getChatMessage(-1) %>", &mut v), "");
    }

    /// getChatMessages(任意参数数量)→ 空数组;配合 join/length 使用不报错
    #[test]
    fn get_chat_messages_returns_empty_array() {
        let mut v = AssistantVars::new();
        // 无参数
        assert_eq!(render("<%= getChatMessages().join('|') %>", &mut v), "");
        // 带索引/数量参数
        assert_eq!(render("<%= getChatMessages(0, 5).length %>", &mut v), "0");
        // 数组上调用 includes/filter 等也不报错
        assert_eq!(
            render("<% var msgs = getChatMessages(); %> <%= msgs.filter(function (m) { return m; }).length %>", &mut v),
            " 0"
        );
    }

    /// 无上下文读取函数带错误参数也不 panic(返回默认值)
    #[test]
    fn st_read_functions_never_panic() {
        let mut v = AssistantVars::new();
        // 参数缺失/多余/类型错误均不报错
        let tpl = "<% getwi(); getWorldInfo(); getchar(); getpreset(); getqr(); getChatMessage(); getChatMessages(1, 2, 'x'); %>OK";
        assert_eq!(render(tpl, &mut v), "OK");
        // 非字符串参数
        assert_eq!(render("<%= getwi(123) %>", &mut v), "");
        assert_eq!(render("<%= getChara(null) %>", &mut v), "");
        assert_eq!(render("<%= getChatMessage('abc', {}) %>", &mut v), "");
    }

    /// injectPrompt 静默成功返回 "";getPromptsInjected 返回 "";hasPromptsInjected 返回 false
    #[test]
    fn inject_prompt_is_silent_placeholder() {
        let mut v = AssistantVars::new();
        // injectPrompt 各种参数形态都不报错
        let tpl =
            "<% injectPrompt('key', '提示内容'); injectPrompt('k2', 'p2', 10, true, 'uid'); %>OK";
        assert_eq!(render(tpl, &mut v), "OK");
        // 返回值空字符串
        assert_eq!(render("<%= injectPrompt('key', '内容') %>", &mut v), "");
        // 读取注入清单:空字符串 / false
        assert_eq!(render("<%= getPromptsInjected('key') %>", &mut v), "");
        assert_eq!(
            render(
                "<% if (hasPromptsInjected('key')) { %>有<% } else { %>无<% } %>",
                &mut v
            ),
            "无"
        );
        // 缺参也不报错
        assert_eq!(
            render(
                "<%= getPromptsInjected() %>|<%= hasPromptsInjected() %>",
                &mut v
            ),
            "|false"
        );
    }

    /// parseJSON:合法 JSON 返回解析值;非法 JSON 原样返回字符串(不报错)
    #[test]
    fn parse_json_lenient() {
        let mut v = AssistantVars::new();
        // 合法 JSON 对象 → 可访问属性
        assert_eq!(
            render("<%= parseJSON('{\"name\":\"芽衣\"}').name %>", &mut v),
            "芽衣"
        );
        // 合法 JSON 数组 → length
        assert_eq!(render("<%= parseJSON('[1,2,3]').length %>", &mut v), "3");
        // 非法 JSON → 原样返回字符串
        assert_eq!(render("<%= parseJSON('不是 JSON') %>", &mut v), "不是 JSON");
        // 数字/空串等边界也不报错
        assert_eq!(render("<%= parseJSON('') %>", &mut v), "");
        assert_eq!(render("<%= parseJSON('42') %>", &mut v), "42");
    }

    /// jsonPatch:返回原对象(容错),不修改原对象
    #[test]
    fn json_patch_returns_dest() {
        let mut v = AssistantVars::new();
        let tpl = "<% var obj = { a: 1 }; var r = jsonPatch(obj, { op: 'replace', path: '/a', value: 2 }); %><%= r.a %>/<%= obj.a %>";
        assert_eq!(render(tpl, &mut v), "1/1");
        // 非对象参数不报错
        assert_eq!(render("<%= jsonPatch(null, {}) %>", &mut v), "null");
        assert_eq!(render("<%= jsonPatch() %>", &mut v), "undefined");
    }

    /// evalTemplate:嵌套渲染返回结果;失败/递归返回原文,不报错
    #[test]
    fn eval_template_nested_render() {
        let mut v = AssistantVars::new();
        v.set("世界.年分", json!(2024));
        // 嵌套模板内容经变量树传入(避免外层编译扫描内层 <%),运行时由 evalTemplate 重新渲染
        v.set("子模板", json!("现在是 <%= getvar('世界.年分') %> 年"));
        assert_eq!(
            render("<%= evalTemplate(getvar('子模板')) %>", &mut v),
            "现在是 2024 年"
        );
    }

    /// evalTemplate:超深度嵌套渲染返回原文(不栈溢出/不 panic)
    #[test]
    fn eval_template_recursion_safe() {
        let mut v = AssistantVars::new();
        // 递归模板内容存于变量树(自引用):每次嵌套渲染都再次调用 evalTemplate,
        // 形成真实无限递归,深度超限后返回原文,整体渲染不 panic
        v.set("rec_tpl", json!("<%= evalTemplate(getvar('rec_tpl')) %>"));
        let tpl = "<%= evalTemplate(getvar('rec_tpl')) %>";
        let (out, _errs) = render_template(tpl, &mut v, &[]);
        // 深度受限后返回原文(内容含 evalTemplate),被外层 String 化输出
        assert!(out.contains("evalTemplate"), "out: {out:?}");
    }

    /// print(...args):追加到输出,与 __kedai_out__ 语义一致
    #[test]
    fn print_appends_to_output() {
        let mut v = AssistantVars::new();
        assert_eq!(render("<% print('A', 1, true); %>B", &mut v), "A1trueB");
        // 无参数不报错
        assert_eq!(render("<% print(); %>X", &mut v), "X");
    }

    /// variables 常量 = 变量树根(即 getvar(null) 语义)
    #[test]
    fn variables_constant_is_tree_root() {
        let mut v = AssistantVars::new();
        v.set("世界.年分", json!(2024));
        v.set("芽衣.名称", json!("芽衣"));
        // variables 成员访问 = 树根下的点路径
        assert_eq!(render("<%= variables.世界.年分 %>", &mut v), "2024");
        assert_eq!(render("<%= variables['芽衣'].名称 %>", &mut v), "芽衣");
        // 空变量树时访问不报错,undefined 按 JS 语义 String 化输出 "undefined"
        let mut empty = AssistantVars::new();
        assert_eq!(render("<%= variables.不存在 %>", &mut empty), "undefined");
        // 模板自定义 variables 局部变量时以用户定义为准
        assert_eq!(
            render(
                "<% var variables = { 自定: 1 }; %><%= variables.自定 %>",
                &mut v
            ),
            "1"
        );
    }

    /// 变量写入/自增别名:setGlobalVar/setLocalVar/setMessageVar/incvar/decvar
    #[test]
    fn variable_write_and_inc_dec_aliases() {
        let mut v = AssistantVars::new();
        v.set("计数", json!(5));
        // incvar/decvar 自增自减
        let tpl = "<% incvar('计数'); decvar('计数'); decvar('计数'); %><%= getvar('计数') %>";
        assert_eq!(render(tpl, &mut v), "4");
        // 路径不存在按 0 起步(不报错)
        let tpl2 = "<% incvar('新计数'); %><%= getvar('新计数') %>";
        assert_eq!(render(tpl2, &mut v), "1");
        // 写入别名映射 setvar(读回一致)
        let tpl3 = "<% setGlobalVar('全局.值', 'A'); setLocalVar('局部.值', 'B'); setMessageVar('消息.值', 'C'); %>";
        assert_eq!(render(tpl3, &mut v), "");
        assert_eq!(render("<%= getGlobalVar('全局.值') %>", &mut v), "A");
        assert_eq!(render("<%= getLocalVar('局部.值') %>", &mut v), "B");
        assert_eq!(render("<%= getMessageVar('消息.值') %>", &mut v), "C");
    }

    /// ST-Prompt-Template 兼容读取类函数缺参/错误参数均不 panic 的批量校验
    #[test]
    fn st_compat_functions_accept_any_args() {
        let mut v = AssistantVars::new();
        // 全部缺参调用:各自返回默认值(空串/空数组/false/undefined/原文),不 panic
        let tpl = "<% print(injectPrompt(), getwi(), getchar(), getpreset(), getqr(), getChatMessage(), getChatMessages(), getPromptsInjected(), hasPromptsInjected(), parseJSON(), jsonPatch(), evalTemplate()); %>DONE";
        let out = render(tpl, &mut v);
        assert!(out.starts_with("[]"), "空数组 String 化为 []: {out:?}");
        assert!(
            out.contains("false"),
            "hasPromptsInjected() 返回 false: {out:?}"
        );
        assert!(out.ends_with("DONE"), "print 不 panic: {out:?}");
    }

    // ============ <#escape-ejs> 块内 EJS 标签转义 ============

    /// escape 块内 EJS 标签原样输出(不执行),起止标签是控制标记、不输出
    #[test]
    fn escape_ejs_block_outputs_literal() {
        let mut v = AssistantVars::new();
        let tpl = "<#escape-ejs>这是原样: <%= 1 + 1 %> 结束</#escape-ejs>";
        assert_eq!(render(tpl, &mut v), "这是原样: <%= 1 + 1 %> 结束");
        // 纯语句块同样原样
        let tpl2 = "<#escape-ejs><% var x = 1; %></#escape-ejs>";
        assert_eq!(render(tpl2, &mut v), "<% var x = 1; %>");
    }

    /// 块外正常渲染,块内原样(混合场景)
    #[test]
    fn escape_ejs_block_keeps_outside_rendering() {
        let mut v = AssistantVars::new();
        v.set("好感度", json!(7));
        let tpl = "外渲染=<%= getvar('好感度') %> 内原样=<#escape-ejs><%= 2 * 2 %> 与 <%%></#escape-ejs> 再渲染=<%= 3 + 4 %>";
        assert_eq!(
            render(tpl, &mut v),
            "外渲染=7 内原样=<%= 2 * 2 %> 与 <%%> 再渲染=7"
        );
    }

    /// 嵌套文本(块内多行 + 嵌套 EJS 片段)原样保留
    #[test]
    fn escape_ejs_block_handles_nested_text() {
        let mut v = AssistantVars::new();
        // 块内多行 + 多层嵌套 EJS 片段全部原样,起止标签不输出
        let tpl = "<#escape-ejs>\n<% if (x) { %>\n  <%- inner %>\n<% } %>\n</#escape-ejs>";
        assert_eq!(
            render(tpl, &mut v),
            "\n<% if (x) { %>\n  <%- inner %>\n<% } %>\n"
        );
        // 孤立 %> 不触发标签解析,原样保留
        let tpl2 = "<#escape-ejs>a%>b</#escape-ejs>";
        assert_eq!(render(tpl2, &mut v), "a%>b");
    }

    /// 失败路径:未闭合 escape 块不报错;无 escape 块时行为与之前完全一致
    #[test]
    fn escape_ejs_block_unclosed_or_absent() {
        let mut v = AssistantVars::new();
        // 未闭合:块内内容按原文保留(标签不输出),不报错
        let tpl = "<#escape-ejs><%= 1 + 1 %>";
        assert_eq!(render(tpl, &mut v), "<%= 1 + 1 %>");
        // 无 escape 块:行为与之前完全一致
        assert_eq!(render("1+2=<%= 1 + 2 %>", &mut v), "1+2=3");
        assert_eq!(render("纯文本", &mut v), "纯文本");
    }

    // ============ 渲染资源预算(防不可信卡死循环/爆栈) ============

    /// while(true){} 必须在预算耗尽后中止(不挂死):
    /// 返回 Err 且**在 2 秒墙钟预算内返回**(测试整体给 10 秒余量)。
    #[test]
    fn while_true_is_aborted_by_budget() {
        let mut v = AssistantVars::new();
        let t0 = std::time::Instant::now();
        let (out, errs) = render_template("<% while (true) { } %>END", &mut v, &[]);
        let elapsed = t0.elapsed();
        assert!(
            elapsed.as_secs() < 10,
            "while(true) 未在预算内中止,耗时 {elapsed:?}"
        );
        assert!(
            errs.iter().any(|e| e.contains("预算")),
            "应报预算超限错误,实际: {errs:?}"
        );
        // 死循环中的输出不产出,但后续文本仍渲染(单语句失败不影响整体)
        assert!(out.ends_with("END"), "out: {out:?}");
    }

    /// for(;;){} 同样被预算中止
    #[test]
    fn for_forever_is_aborted_by_budget() {
        let mut v = AssistantVars::new();
        let (out, errs) = render_template("<% for (;;) { } %>END", &mut v, &[]);
        assert!(
            errs.iter().any(|e| e.contains("预算")),
            "应报预算超限错误,实际: {errs:?}"
        );
        assert!(out.ends_with("END"), "out: {out:?}");
    }

    /// 深一元链 `!!!!…x` 1 万层:parse_unary 守卫必须拦住(只守二元链会漏此处)
    #[test]
    fn deep_unary_chain_is_rejected() {
        let mut v = AssistantVars::new();
        let tpl = format!("<%= {}true %>", "!".repeat(10_000));
        let t0 = std::time::Instant::now();
        let (_out, errs) = render_template(&tpl, &mut v, &[]);
        assert!(
            t0.elapsed().as_secs() < 10,
            "深一元链未及时返回,疑似爆栈/挂死"
        );
        assert!(
            errs.iter().any(|e| e.contains("嵌套过深")),
            "应报嵌套过深错误,实际: {errs:?}"
        );
    }

    /// 2000 层括号(原子链):parse_primary 守卫拦住,不崩溃
    #[test]
    fn deep_paren_nesting_is_rejected() {
        let mut v = AssistantVars::new();
        let tpl = format!("<%= {}1{} %>", "(".repeat(2_000), ")".repeat(2_000));
        let t0 = std::time::Instant::now();
        let (_out, errs) = render_template(&tpl, &mut v, &[]);
        assert!(t0.elapsed().as_secs() < 10, "深嵌套未及时返回");
        assert!(
            errs.iter().any(|e| e.contains("嵌套过深")),
            "应报嵌套过深错误,实际: {errs:?}"
        );
    }

    /// 正常量级循环必须照常通过(防预算误杀;同时验证每次渲染预算独立重置)
    #[test]
    fn normal_loop_still_works_and_budget_resets() {
        let mut v = AssistantVars::new();
        let tpl = "<% var s = 0; for (var i = 0; i < 1000; i++) { s = s + i; } %><%= s %>";
        assert_eq!(render(tpl, &mut v), "499500");
        // 连续多次渲染:预算在每次入口重置(RAII 守卫恢复),第二次仍可跑满 1000 次
        assert_eq!(render(tpl, &mut v), "499500");
        assert_eq!(render(tpl, &mut v), "499500");
    }
}
