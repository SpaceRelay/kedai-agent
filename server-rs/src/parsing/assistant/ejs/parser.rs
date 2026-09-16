// 递归下降解析器:token 流 → AST(Parser 结构 + 各层优先级解析函数 + expr_to_target)。
// 语句级容错:单条语句出错跳过到下一个分号继续,错误收集到 self.errors;
// 模板字符串 `${...}` 内部用子 Parser 解析子 token 流。
use super::ast::*;
use super::lexer::*;
use super::value::*;

pub(super) struct Parser<'a> {
    toks: &'a [Tok],
    pos: usize,
    /// 语句级解析错误(容错收集,不影响其余语句)
    pub(super) errors: Vec<String>,
    /// 表达式递归深度(防深嵌套输入爆栈)。递增点见 parse_assign/parse_unary/parse_primary
    /// —— 三者覆盖「二元链 / 一元链 / 原子链」全部递归路径,子 Parser 继承深度。
    depth: usize,
}

/// 表达式嵌套深度上限。正常角色卡表达式嵌套在 10 层以内(成员链/下标走 parse_postfix
/// 的循环,不计深度),64 层对真实模板绰绰有余。
///
/// 取值依据:每层嵌套要经过 parse_assign→…→parse_primary 约 13 个递归函数,且 debug
/// 构建的栈帧很大;Rust 测试线程默认栈仅 2MB。取值过高会导致**守卫来不及触发就已爆栈**
/// (实测 256 层即 1664 帧仍溢出),故取 64(约 400 余帧,release/debug 均安全)。
const MAX_PARSE_DEPTH: usize = 64;

impl<'a> Parser<'a> {
    pub(super) fn new(toks: &'a [Tok]) -> Self {
        Parser {
            toks,
            pos: 0,
            errors: Vec::new(),
            depth: 0,
        }
    }

    /// 以指定初始深度构造(子 token 流解析时继承父深度,防止 `((((...))))`
    /// 或模板字符串嵌套通过新建 Parser 重置计数绕过限制)。
    fn with_depth(toks: &'a [Tok], depth: usize) -> Self {
        Parser {
            toks,
            pos: 0,
            errors: Vec::new(),
            depth,
        }
    }

    /// 进入一层递归:超限即报错。配对 parse_assign_inner/parse_unary_inner/parse_primary_inner。
    fn enter_depth(&mut self) -> Result<(), String> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            return Err(format!("表达式嵌套过深(超过 {MAX_PARSE_DEPTH} 层)"));
        }
        Ok(())
    }

    fn leave_depth(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    fn peek(&self) -> &Tok {
        &self.toks[self.pos.min(self.toks.len() - 1)]
    }

    fn next(&mut self) -> Tok {
        let t = self.toks[self.pos.min(self.toks.len() - 1)].clone();
        if self.pos + 1 < self.toks.len() {
            self.pos += 1;
        }
        t
    }

    fn expect_p(&mut self, p: P, what: &str) -> Result<(), String> {
        match self.peek() {
            Tok::P(actual) if *actual == p => {
                self.next();
                Ok(())
            }
            other => Err(format!("{what}: 期望 {:?},实际 {:?}", p, other)),
        }
    }

    fn expect_ident(&mut self, what: &str) -> Result<String, String> {
        match self.next() {
            Tok::Ident(s) => Ok(s),
            other => Err(format!("{what}: 期望标识符,实际 {other:?}")),
        }
    }

    /// 解析全部语句;单条语句出错时跳过到下一个分号继续(语句级容错),
    /// 错误收集到 self.errors
    pub(super) fn parse_program(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        loop {
            match self.peek() {
                Tok::Eof | Tok::P(P::RBrace) => break,
                _ => match self.parse_stmt() {
                    Ok(s) => stmts.push(s),
                    Err(e) => {
                        self.errors.push(e);
                        // 尽力恢复:跳过到下一个分号/右括号/末尾
                        while !matches!(self.peek(), Tok::P(P::Semi) | Tok::P(P::RBrace) | Tok::Eof)
                        {
                            self.next();
                        }
                        if matches!(self.peek(), Tok::P(P::Semi)) {
                            self.next();
                        }
                    }
                },
            }
        }
        stmts
    }

    fn parse_stmt(&mut self) -> Result<Stmt, String> {
        self.parse_stmt_inner(true)
    }

    /// consume_semi=false 供 for(;;) 的 init 语句使用(分号由 for 解析器消费)
    fn parse_stmt_inner(&mut self, consume_semi: bool) -> Result<Stmt, String> {
        match self.peek().clone() {
            Tok::P(P::Semi) => {
                self.next();
                Ok(Stmt::Empty)
            }
            Tok::P(P::LBrace) => {
                self.next();
                let body = self.parse_program();
                self.expect_p(P::RBrace, "语句块")?;
                Ok(Stmt::Block(body))
            }
            Tok::Kw(Kw::If) => {
                self.next();
                self.expect_p(P::LParen, "if")?;
                let cond = self.parse_expr()?;
                self.expect_p(P::RParen, "if")?;
                let then = Box::new(self.parse_stmt()?);
                let els = if matches!(self.peek(), Tok::Kw(Kw::Else)) {
                    self.next();
                    Some(Box::new(self.parse_stmt()?))
                } else {
                    None
                };
                Ok(Stmt::If { cond, then, els })
            }
            Tok::Kw(Kw::For) => {
                self.next();
                self.expect_p(P::LParen, "for")?;
                // for (var x of expr) / for (x of expr)
                if matches!(
                    self.peek(),
                    Tok::Kw(Kw::Var) | Tok::Kw(Kw::Let) | Tok::Kw(Kw::Const) | Tok::Ident(_)
                ) {
                    let save = self.pos;
                    let decl_kw = matches!(
                        self.peek(),
                        Tok::Kw(Kw::Var) | Tok::Kw(Kw::Let) | Tok::Kw(Kw::Const)
                    );
                    if decl_kw {
                        self.next();
                    }
                    let name = self.expect_ident("for 循环变量")?;
                    if matches!(self.peek(), Tok::Kw(Kw::Of) | Tok::Kw(Kw::In)) {
                        self.next();
                        let iter = self.parse_expr()?;
                        self.expect_p(P::RParen, "for...of")?;
                        let body = Box::new(self.parse_stmt()?);
                        return Ok(Stmt::ForOf { name, iter, body });
                    }
                    // 不是 for...of,回退经典 for
                    self.pos = save;
                }
                // 经典 for(;;)
                let init = if matches!(self.peek(), Tok::P(P::Semi)) {
                    None
                } else {
                    Some(Box::new(self.parse_stmt_inner(false)?))
                };
                self.expect_p(P::Semi, "for")?;
                let cond = if matches!(self.peek(), Tok::P(P::Semi)) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect_p(P::Semi, "for")?;
                let update = if matches!(self.peek(), Tok::P(P::RParen)) {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                self.expect_p(P::RParen, "for")?;
                let body = Box::new(self.parse_stmt()?);
                Ok(Stmt::For {
                    init,
                    cond,
                    update,
                    body,
                })
            }
            Tok::Kw(Kw::While) => {
                self.next();
                self.expect_p(P::LParen, "while")?;
                let cond = self.parse_expr()?;
                self.expect_p(P::RParen, "while")?;
                let body = Box::new(self.parse_stmt()?);
                Ok(Stmt::While { cond, body })
            }
            Tok::Kw(Kw::Var) | Tok::Kw(Kw::Let) | Tok::Kw(Kw::Const) => {
                self.next();
                let mut decls = Vec::new();
                loop {
                    let name = self.expect_ident("变量声明")?;
                    let value = if matches!(self.peek(), Tok::P(P::Assign)) {
                        self.next();
                        Some(self.parse_expr()?)
                    } else {
                        None
                    };
                    decls.push((name, value));
                    if matches!(self.peek(), Tok::P(P::Comma)) {
                        self.next();
                    } else {
                        break;
                    }
                }
                if consume_semi {
                    let _ = self.next_if(P::Semi);
                }
                Ok(Stmt::Var(decls))
            }
            Tok::Kw(Kw::Function) => {
                self.next();
                let name = self.expect_ident("函数声明")?;
                let (params, body) = self.parse_func_tail()?;
                Ok(Stmt::Function { name, params, body })
            }
            Tok::Kw(Kw::Return) => {
                self.next();
                let value = if matches!(self.peek(), Tok::P(P::Semi) | Tok::P(P::RBrace) | Tok::Eof)
                {
                    None
                } else {
                    Some(self.parse_expr()?)
                };
                if consume_semi {
                    let _ = self.next_if(P::Semi);
                }
                Ok(Stmt::Return(value))
            }
            Tok::Kw(Kw::Break) => {
                self.next();
                if consume_semi {
                    let _ = self.next_if(P::Semi);
                }
                Ok(Stmt::Break)
            }
            Tok::Kw(Kw::Continue) => {
                self.next();
                if consume_semi {
                    let _ = self.next_if(P::Semi);
                }
                Ok(Stmt::Continue)
            }
            _ => {
                let e = self.parse_expr()?;
                if consume_semi {
                    let _ = self.next_if(P::Semi);
                }
                Ok(Stmt::Expr(e))
            }
        }
    }

    fn next_if(&mut self, p: P) -> Option<()> {
        if matches!(self.peek(), Tok::P(actual) if *actual == p) {
            self.next();
            Some(())
        } else {
            None
        }
    }

    /// function 尾部:(params) { body },参数支持默认值 `name = expr`(调用时求值)
    fn parse_func_tail(&mut self) -> Result<(Vec<FuncParam>, Vec<Stmt>), String> {
        self.expect_p(P::LParen, "函数参数")?;
        let params = self.parse_param_list()?;
        self.expect_p(P::RParen, "函数参数")?;
        self.expect_p(P::LBrace, "函数体")?;
        let body = self.parse_program();
        self.expect_p(P::RBrace, "函数体")?;
        Ok((params, body))
    }

    /// 解析圆括号内参数列表:name 或 name = expr(默认参数)
    fn parse_param_list(&mut self) -> Result<Vec<FuncParam>, String> {
        let mut params = Vec::new();
        loop {
            match self.peek() {
                Tok::P(P::RParen) => break,
                Tok::Ident(_) => {
                    let name = self.expect_ident("函数参数")?;
                    let default = if matches!(self.peek(), Tok::P(P::Assign)) {
                        self.next();
                        Some(self.parse_assign()?)
                    } else {
                        None
                    };
                    params.push(FuncParam { name, default });
                }
                _ => return Err("函数参数格式错误".into()),
            }
            if matches!(self.peek(), Tok::P(P::Comma)) {
                self.next();
            } else {
                break;
            }
        }
        Ok(params)
    }

    fn parse_expr(&mut self) -> Result<Expr, String> {
        self.parse_assign()
    }

    /// 赋值表达式(守卫点 ①:二元/右结合/三元链的递归枢纽)
    fn parse_assign(&mut self) -> Result<Expr, String> {
        self.enter_depth()?;
        let r = self.parse_assign_inner();
        self.leave_depth();
        r
    }

    fn parse_assign_inner(&mut self) -> Result<Expr, String> {
        let left = self.parse_cond()?;
        let assign = match self.peek() {
            Tok::P(P::Assign) => Some(AssignOp::Set),
            Tok::P(P::PlusEq) => Some(AssignOp::Add),
            Tok::P(P::MinusEq) => Some(AssignOp::Sub),
            Tok::P(P::StarEq) => Some(AssignOp::Mul),
            Tok::P(P::SlashEq) => Some(AssignOp::Div),
            _ => None,
        };
        if let Some(op) = assign {
            self.next();
            let value = self.parse_assign()?; // 右结合
            let target = expr_to_target(left)?;
            return Ok(Expr::Assign {
                target,
                op,
                value: Box::new(value),
            });
        }
        Ok(left)
    }

    fn parse_cond(&mut self) -> Result<Expr, String> {
        let cond = self.parse_or()?;
        if matches!(self.peek(), Tok::P(P::Question)) {
            self.next();
            let then = self.parse_assign()?;
            self.expect_p(P::Colon, "三元表达式")?;
            let els = self.parse_assign()?;
            return Ok(Expr::Ternary {
                cond: Box::new(cond),
                then: Box::new(then),
                els: Box::new(els),
            });
        }
        Ok(cond)
    }

    fn parse_or(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Tok::P(P::OrOr)) {
            self.next();
            let right = self.parse_and()?;
            left = Expr::Binary {
                op: BinOp::Or,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_and(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_eq()?;
        while matches!(self.peek(), Tok::P(P::AndAnd)) {
            self.next();
            let right = self.parse_eq()?;
            left = Expr::Binary {
                op: BinOp::And,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_eq(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_rel()?;
        loop {
            let op = match self.peek() {
                Tok::P(P::EqEq) => Some(BinOp::Eq),
                Tok::P(P::NotEq) => Some(BinOp::NotEq),
                Tok::P(P::EqEqEq) => Some(BinOp::StrictEq),
                Tok::P(P::NotEqEq) => Some(BinOp::StrictNotEq),
                _ => None,
            };
            let Some(op) = op else { break };
            self.next();
            let right = self.parse_rel()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_rel(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_add()?;
        loop {
            let op = match self.peek() {
                Tok::P(P::Lt) => Some(BinOp::Lt),
                Tok::P(P::Le) => Some(BinOp::Le),
                Tok::P(P::Gt) => Some(BinOp::Gt),
                Tok::P(P::Ge) => Some(BinOp::Ge),
                _ => None,
            };
            let Some(op) = op else { break };
            self.next();
            let right = self.parse_add()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_add(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_mul()?;
        loop {
            let op = match self.peek() {
                Tok::P(P::Plus) => Some(BinOp::Add),
                Tok::P(P::Minus) => Some(BinOp::Sub),
                _ => None,
            };
            let Some(op) = op else { break };
            self.next();
            let right = self.parse_mul()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    fn parse_mul(&mut self) -> Result<Expr, String> {
        let mut left = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Tok::P(P::Star) => Some(BinOp::Mul),
                Tok::P(P::Slash) => Some(BinOp::Div),
                Tok::P(P::Percent) => Some(BinOp::Mod),
                _ => None,
            };
            let Some(op) = op else { break };
            self.next();
            let right = self.parse_unary()?;
            left = Expr::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
        Ok(left)
    }

    /// 一元表达式(守卫点 ②:前缀 `!`/`-`/`+`/`++`/`--` 自递归链的枢纽)。
    /// 若只守二元链而不守此处,`!!!!…x` 这类深一元链仍会爆栈。
    fn parse_unary(&mut self) -> Result<Expr, String> {
        self.enter_depth()?;
        let r = self.parse_unary_inner();
        self.leave_depth();
        r
    }

    fn parse_unary_inner(&mut self) -> Result<Expr, String> {
        match self.peek() {
            Tok::P(P::Bang) => {
                self.next();
                Ok(Expr::Unary {
                    op: UnOp::Not,
                    expr: Box::new(self.parse_unary()?),
                })
            }
            Tok::P(P::Minus) => {
                self.next();
                Ok(Expr::Unary {
                    op: UnOp::Neg,
                    expr: Box::new(self.parse_unary()?),
                })
            }
            Tok::P(P::Plus) => {
                self.next();
                Ok(Expr::Unary {
                    op: UnOp::Pos,
                    expr: Box::new(self.parse_unary()?),
                })
            }
            // 前缀 ++/--(需目标)
            Tok::P(P::PlusPlus) => {
                self.next();
                let target = expr_to_target(self.parse_unary()?)?;
                Ok(Expr::Update {
                    target,
                    op: UpdateOp::Inc,
                    prefix: true,
                })
            }
            Tok::P(P::MinusMinus) => {
                self.next();
                let target = expr_to_target(self.parse_unary()?)?;
                Ok(Expr::Update {
                    target,
                    op: UpdateOp::Dec,
                    prefix: true,
                })
            }
            _ => self.parse_postfix(),
        }
    }

    fn parse_postfix(&mut self) -> Result<Expr, String> {
        let mut expr = self.parse_primary()?;
        loop {
            match self.peek() {
                Tok::P(P::LParen) => {
                    self.next();
                    let mut args = Vec::new();
                    loop {
                        match self.peek() {
                            Tok::P(P::RParen) => break,
                            _ => args.push(self.parse_assign()?),
                        }
                        if matches!(self.peek(), Tok::P(P::Comma)) {
                            self.next();
                        } else {
                            break;
                        }
                    }
                    self.expect_p(P::RParen, "函数调用")?;
                    expr = Expr::Call {
                        callee: Box::new(expr),
                        args,
                    };
                }
                Tok::P(P::Dot) => {
                    self.next();
                    let prop = self.expect_ident("成员访问")?;
                    expr = Expr::Member {
                        obj: Box::new(expr),
                        prop,
                    };
                }
                Tok::P(P::LBracket) => {
                    self.next();
                    let idx = self.parse_expr()?;
                    self.expect_p(P::RBracket, "下标访问")?;
                    expr = Expr::Index {
                        obj: Box::new(expr),
                        idx: Box::new(idx),
                    };
                }
                Tok::P(P::PlusPlus) => {
                    self.next();
                    let target = expr_to_target(expr)?;
                    expr = Expr::Update {
                        target,
                        op: UpdateOp::Inc,
                        prefix: false,
                    };
                }
                Tok::P(P::MinusMinus) => {
                    self.next();
                    let target = expr_to_target(expr)?;
                    expr = Expr::Update {
                        target,
                        op: UpdateOp::Dec,
                        prefix: false,
                    };
                }
                _ => break,
            }
        }
        Ok(expr)
    }

    /// 原子表达式(守卫点 ③:括号分组/数组/对象/模板等所有嵌套最终汇聚于此)
    fn parse_primary(&mut self) -> Result<Expr, String> {
        self.enter_depth()?;
        let r = self.parse_primary_inner();
        self.leave_depth();
        r
    }

    fn parse_primary_inner(&mut self) -> Result<Expr, String> {
        match self.next() {
            Tok::Num(n) => Ok(Expr::Literal(JsValue::Num(n))),
            Tok::Str(s) => Ok(Expr::Literal(JsValue::Str(s))),
            Tok::Tpl { parts, exprs } => {
                // 组装 Template:字面部分与表达式部分交错
                let mut items = Vec::new();
                for (i, part) in parts.iter().enumerate() {
                    if !part.is_empty() {
                        items.push(Expr::Literal(JsValue::Str(part.clone())));
                    }
                    if i < exprs.len() {
                        // 用子 token 列表解析表达式(继承当前深度,防绕开深度上限)
                        let mut sub = Parser::with_depth(&exprs[i], self.depth);
                        let e = sub.parse_assign()?;
                        items.push(e);
                    }
                }
                if items.is_empty() {
                    items.push(Expr::Literal(JsValue::Str(String::new())));
                }
                Ok(Expr::Template(items))
            }
            Tok::Ident(name) => {
                // 单参箭头函数:name => expr
                if matches!(self.peek(), Tok::P(P::Arrow)) {
                    self.next();
                    let body = self.parse_arrow_body()?;
                    return Ok(Expr::Func {
                        params: vec![FuncParam {
                            name,
                            default: None,
                        }],
                        body,
                    });
                }
                Ok(Expr::Ident(name))
            }
            Tok::Kw(Kw::True) => Ok(Expr::Literal(JsValue::Bool(true))),
            Tok::Kw(Kw::False) => Ok(Expr::Literal(JsValue::Bool(false))),
            Tok::Kw(Kw::Null) => Ok(Expr::Literal(JsValue::Null)),
            Tok::Kw(Kw::Undefined) => Ok(Expr::Literal(JsValue::Undefined)),
            Tok::Kw(Kw::Function) => {
                // function 表达式:function (params) { body } 或 function name(...)
                if let Tok::Ident(_) = self.peek() {
                    self.next();
                }
                let (params, body) = self.parse_func_tail()?;
                Ok(Expr::Func {
                    params,
                    body: FnBody::Block(body),
                })
            }
            Tok::P(P::LParen) => {
                // (a, b) => expr 或分组表达式;参数支持默认值 (a = 1) => ...
                // 注意:( 已被 parse_primary 消费,self.pos 指向参数列表第一个 token
                let save = self.pos;
                // 尝试解析为箭头函数参数列表:括号内为 [name (= expr)?] 逗号分隔
                let mut is_arrow = false;
                {
                    let mut probe = Parser::with_depth(self.toks, self.depth);
                    probe.pos = self.pos;
                    let mut ok = true;
                    loop {
                        match probe.next() {
                            Tok::P(P::RParen) => break,
                            Tok::Ident(_) => {
                                if matches!(probe.peek(), Tok::P(P::Assign)) {
                                    probe.next();
                                    // 跳过默认值表达式(粗略:直到逗号/右括号,不嵌套)
                                    let mut terminated = false;
                                    loop {
                                        match probe.next() {
                                            Tok::P(P::Comma) => break,
                                            Tok::P(P::RParen) => {
                                                terminated = true;
                                                break;
                                            }
                                            Tok::Eof => {
                                                ok = false;
                                                break;
                                            }
                                            _ => {}
                                        }
                                    }
                                    if terminated {
                                        break;
                                    }
                                    continue;
                                }
                                if matches!(probe.peek(), Tok::P(P::Comma)) {
                                    probe.next();
                                }
                            }
                            _ => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if ok && matches!(probe.peek(), Tok::P(P::Arrow)) {
                        is_arrow = true;
                    }
                }
                if is_arrow {
                    self.pos = save;
                    // ( 已被 parse_primary 消费,直接解析参数列表
                    let params = self.parse_param_list()?;
                    self.expect_p(P::RParen, "箭头函数参数")?;
                    self.expect_p(P::Arrow, "箭头函数")?;
                    let body = self.parse_arrow_body()?;
                    return Ok(Expr::Func { params, body });
                }
                self.pos = save;
                let e = self.parse_expr()?;
                self.expect_p(P::RParen, "分组表达式")?;
                Ok(e)
            }
            Tok::P(P::LBracket) => {
                let mut items = Vec::new();
                loop {
                    match self.peek() {
                        Tok::P(P::RBracket) => break,
                        _ => items.push(self.parse_assign()?),
                    }
                    if matches!(self.peek(), Tok::P(P::Comma)) {
                        self.next();
                    } else {
                        break;
                    }
                }
                self.expect_p(P::RBracket, "数组字面量")?;
                Ok(Expr::Array(items))
            }
            Tok::P(P::LBrace) => {
                // 对象字面量 { key: value, ... }
                let mut fields = Vec::new();
                loop {
                    match self.peek() {
                        Tok::P(P::RBrace) => break,
                        Tok::Ident(_) | Tok::Str(_) | Tok::Num(_) => {
                            let key = match self.next() {
                                Tok::Ident(s) | Tok::Str(s) => s,
                                Tok::Num(n) => fmt_num(n),
                                _ => unreachable!(),
                            };
                            let value = if matches!(self.peek(), Tok::P(P::Colon)) {
                                self.next();
                                self.parse_assign()?
                            } else {
                                // 简写 { a }
                                Expr::Ident(key.clone())
                            };
                            fields.push((key, value));
                        }
                        other => return Err(format!("对象字面量键格式错误: {other:?}")),
                    }
                    if matches!(self.peek(), Tok::P(P::Comma)) {
                        self.next();
                    } else {
                        break;
                    }
                }
                self.expect_p(P::RBrace, "对象字面量")?;
                Ok(Expr::Object(fields))
            }
            other => Err(format!("无法解析的表达式开头: {other:?}")),
        }
    }

    fn parse_arrow_body(&mut self) -> Result<FnBody, String> {
        if matches!(self.peek(), Tok::P(P::LBrace)) {
            self.next();
            let body = self.parse_program();
            self.expect_p(P::RBrace, "箭头函数体")?;
            Ok(FnBody::Block(body))
        } else {
            Ok(FnBody::Expr(Box::new(self.parse_assign()?)))
        }
    }
}

fn expr_to_target(e: Expr) -> Result<AssignTarget, String> {
    match e {
        Expr::Ident(name) => Ok(AssignTarget::Ident(name)),
        Expr::Member { obj, prop } => Ok(AssignTarget::Member(obj, prop)),
        Expr::Index { obj, idx } => Ok(AssignTarget::Index(obj, idx)),
        other => Err(format!("赋值目标无效: {other:?}")),
    }
}
