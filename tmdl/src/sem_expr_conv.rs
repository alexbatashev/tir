use crate::ast::{BinOp, BuiltinFunction, Expr as AstExpr, Lit};
use std::collections::HashMap;
use tir::sem_expr::Expr;
use tir::utils::APInt;

/// Information about a symbol in the expression
#[derive(Debug, Clone, PartialEq)]
pub enum SymbolInfo {
    /// A register reference: (register_class, register_number)
    Register { class: String, number: u32 },
    /// A variable/operand reference by name
    Variable { name: String },
}

/// Result of converting an AST expression to a semantic expression
pub struct ConversionResult {
    /// The converted semantic expression
    pub expr: Expr,
    /// Map from symbol IDs to their information
    pub symbols: HashMap<u32, SymbolInfo>,
}

/// Context for expression conversion
pub struct ConversionContext {
    /// Static parameters that can be resolved during conversion
    params: HashMap<String, i64>,
    /// Next symbol ID to allocate
    next_symbol_id: u32,
    /// Symbol information
    symbols: HashMap<u32, SymbolInfo>,
}

impl ConversionContext {
    /// Create a new conversion context with the given parameters
    pub fn new(params: HashMap<String, i64>) -> Self {
        Self {
            params,
            next_symbol_id: 0,
            symbols: HashMap::new(),
        }
    }

    /// Allocate a new symbol ID and register its information
    fn alloc_symbol(&mut self, info: SymbolInfo) -> u32 {
        let id = self.next_symbol_id;
        self.next_symbol_id += 1;
        self.symbols.insert(id, info);
        id
    }

    /// Get or create a symbol for a register reference
    fn get_or_create_register_symbol(&mut self, class: String, number: u32) -> u32 {
        // Check if we already have this register as a symbol
        for (id, info) in &self.symbols {
            if let SymbolInfo::Register {
                class: c,
                number: n,
            } = info
            {
                if c == &class && n == &number {
                    return *id;
                }
            }
        }

        // Create new symbol
        self.alloc_symbol(SymbolInfo::Register { class, number })
    }

    /// Convert a TMDL AST expression to a semantic expression
    pub fn convert(&mut self, ast_expr: &AstExpr) -> Result<Expr, String> {
        match ast_expr {
            AstExpr::Lit(lit) => self.convert_literal(lit),
            AstExpr::Ident(ident) => self.convert_ident(&ident.name),
            AstExpr::Binary(binary) => self.convert_binary(binary),
            AstExpr::Call(call) => self.convert_call(call),
            AstExpr::Slice(slice) => self.convert_slice(slice),
            AstExpr::IndexAccess(index) => self.convert_index_access(index),
            AstExpr::Field(field) => self.convert_field(field),
            AstExpr::Path(path) => self.convert_path(path),
            AstExpr::If(if_expr) => self.convert_if(if_expr),
            AstExpr::Block(block) => self.convert_block(block),
            AstExpr::Assign(_) => {
                Err("Assignment expressions not supported in sem_expr".to_string())
            }
            AstExpr::BuiltinFunction(builtin) => self.convert_builtin_function(builtin),
            AstExpr::Invalid => Err("Invalid expression".to_string()),
        }
    }

    fn convert_literal(&mut self, lit: &Lit) -> Result<Expr, String> {
        match lit {
            Lit::Int(lit_int) => {
                let value_str = lit_int.value();

                // Parse integer literal (support decimal, hex, binary)
                let value = if value_str.starts_with("0x") || value_str.starts_with("0X") {
                    u64::from_str_radix(&value_str[2..], 16)
                        .map_err(|e| format!("Invalid hex literal: {}", e))?
                } else if value_str.starts_with("0b") || value_str.starts_with("0B") {
                    u64::from_str_radix(&value_str[2..], 2)
                        .map_err(|e| format!("Invalid binary literal: {}", e))?
                } else {
                    value_str
                        .parse::<u64>()
                        .map_err(|e| format!("Invalid integer literal: {}", e))?
                };

                // Determine the bit width needed
                let width = if value == 0 {
                    1
                } else {
                    64 - value.leading_zeros()
                };

                Ok(Expr::Int(APInt::new(width, value)))
            }
            Lit::Str(_) => Err("String literals not supported in sem_expr".to_string()),
        }
    }

    fn convert_ident(&mut self, name: &str) -> Result<Expr, String> {
        // Check if this is a parameter that can be resolved statically
        if let Some(&value) = self.params.get(name) {
            let (width, abs_value) = if value < 0 {
                let abs = value.abs() as u64;
                let width = if abs == 0 {
                    1
                } else {
                    64 - abs.leading_zeros() + 1
                };
                (width, abs)
            } else {
                let v = value as u64;
                let width = if v == 0 { 1 } else { 64 - v.leading_zeros() };
                (width, v)
            };

            if value < 0 {
                Ok(Expr::Int(APInt::new_signed(width, value)))
            } else {
                Ok(Expr::Int(APInt::new(width, abs_value)))
            }
        } else {
            // Treat unknown identifiers as symbolic variables/operands.
            let symbol_id = self.alloc_symbol(SymbolInfo::Variable {
                name: name.to_string(),
            });
            Ok(Expr::Symbol(symbol_id))
        }
    }

    fn convert_path(&mut self, path: &crate::ast::Path) -> Result<Expr, String> {
        if path.remainder.len() != 1 {
            return Err("path expressions must have exactly one register component".to_string());
        }
        let reg_name = &path.remainder[0];
        let number = if path.base == "PC" && reg_name == "pc" {
            0
        } else {
            let digits_start = reg_name.find(|c: char| c.is_ascii_digit()).ok_or_else(|| {
                format!(
                    "could not infer register index from path '{}::{}'",
                    path.base, reg_name
                )
            })?;
            reg_name[digits_start..].parse::<u32>().map_err(|_| {
                format!(
                    "invalid register index in path '{}::{}'",
                    path.base, reg_name
                )
            })?
        };
        let symbol_id = self.get_or_create_register_symbol(path.base.clone(), number);
        Ok(Expr::Symbol(symbol_id))
    }

    fn convert_binary(&mut self, binary: &crate::ast::Binary) -> Result<Expr, String> {
        let lhs = self.convert(&binary.lhs)?;
        let rhs = self.convert(&binary.rhs)?;

        Ok(match binary.op {
            BinOp::Add => Expr::Add(Box::new(lhs), Box::new(rhs)),
            BinOp::Sub => Expr::Sub(Box::new(lhs), Box::new(rhs)),
            BinOp::Mul => Expr::Mul(Box::new(lhs), Box::new(rhs)),
            BinOp::Div => Expr::Div(Box::new(lhs), Box::new(rhs)),
            BinOp::UnsignedDiv => Expr::UDiv(Box::new(lhs), Box::new(rhs)),
            BinOp::Equal => Expr::Eq(Box::new(lhs), Box::new(rhs)),
            BinOp::NotEqual => Expr::Ne(Box::new(lhs), Box::new(rhs)),
            BinOp::LessThan => Expr::Lt(Box::new(lhs), Box::new(rhs)),
            BinOp::GreaterThan => Expr::Gt(Box::new(lhs), Box::new(rhs)),
            BinOp::LessThenEqual => Expr::Le(Box::new(lhs), Box::new(rhs)),
            BinOp::GreaterThanEqual => Expr::Ge(Box::new(lhs), Box::new(rhs)),
            BinOp::UnsignedLessThan => Expr::ULt(Box::new(lhs), Box::new(rhs)),
            BinOp::UnsignedGreaterThan => Expr::UGt(Box::new(lhs), Box::new(rhs)),
            BinOp::UnsignedLessThenEqual => Expr::ULe(Box::new(lhs), Box::new(rhs)),
            BinOp::UnsignedGreaterThanEqual => Expr::UGe(Box::new(lhs), Box::new(rhs)),
            BinOp::BitwiseAnd => Expr::And(Box::new(lhs), Box::new(rhs)),
            BinOp::BitwiseOr => Expr::Or(Box::new(lhs), Box::new(rhs)),
            BinOp::BitwiseXor => Expr::Xor(Box::new(lhs), Box::new(rhs)),
            BinOp::ShiftLeftLogical => Expr::ShiftLeft(Box::new(lhs), Box::new(rhs)),
            BinOp::ShiftRightLogical => Expr::ShiftRightLogic(Box::new(lhs), Box::new(rhs)),
            BinOp::ShiftRightArithmetic => Expr::ShiftRightArithmetic(Box::new(lhs), Box::new(rhs)),
        })
    }

    fn convert_call(&mut self, call: &crate::ast::Call) -> Result<Expr, String> {
        // Check if this is a builtin function
        if let AstExpr::BuiltinFunction(builtin) = &*call.callee {
            match builtin {
                BuiltinFunction::Clamp => {
                    if call.arguments.len() != 3 {
                        return Err("clamp requires 3 arguments".to_string());
                    }
                    let input = self.convert(&call.arguments[0])?;
                    let min = self.convert(&call.arguments[1])?;
                    let max = self.convert(&call.arguments[2])?;
                    Ok(Expr::Clamp {
                        input: Box::new(input),
                        min: Box::new(min),
                        max: Box::new(max),
                    })
                }
                BuiltinFunction::Extract => {
                    if call.arguments.len() != 3 {
                        return Err("extract requires 3 arguments".to_string());
                    }
                    let input = self.convert(&call.arguments[0])?;
                    let high = self.convert(&call.arguments[1])?;
                    let low = self.convert(&call.arguments[2])?;
                    Ok(Expr::Extract {
                        input: Box::new(input),
                        high: Box::new(high),
                        low: Box::new(low),
                    })
                }
                BuiltinFunction::Log2Ceil => {
                    if call.arguments.len() != 1 {
                        return Err("log2Ceil requires 1 argument".to_string());
                    }
                    let input = self.convert(&call.arguments[0])?;
                    Ok(Expr::Log2Ceil(Box::new(input)))
                }
                BuiltinFunction::SExt => {
                    if call.arguments.len() != 2 {
                        return Err("sext requires 2 arguments".to_string());
                    }
                    let input = self.convert(&call.arguments[0])?;
                    let width = self.convert(&call.arguments[1])?;
                    Ok(Expr::SExt {
                        input: Box::new(input),
                        width: Box::new(width),
                    })
                }
                BuiltinFunction::ZExt => {
                    if call.arguments.len() != 2 {
                        return Err("zext requires 2 arguments".to_string());
                    }
                    let input = self.convert(&call.arguments[0])?;
                    let width = self.convert(&call.arguments[1])?;
                    Ok(Expr::ZExt {
                        input: Box::new(input),
                        width: Box::new(width),
                    })
                }
                BuiltinFunction::Load => {
                    if call.arguments.len() != 3 {
                        return Err("load requires 3 arguments".to_string());
                    }
                    let addr = self.convert(&call.arguments[0])?;
                    let bytes = self.convert(&call.arguments[1])?;
                    let signed = self.convert(&call.arguments[2])?;
                    Ok(Expr::Load {
                        addr: Box::new(addr),
                        bytes: Box::new(bytes),
                        signed: Box::new(signed),
                    })
                }
                BuiltinFunction::Store => {
                    if call.arguments.len() != 3 {
                        return Err("store requires 3 arguments".to_string());
                    }
                    let addr = self.convert(&call.arguments[0])?;
                    let bytes = self.convert(&call.arguments[1])?;
                    let value = self.convert(&call.arguments[2])?;
                    Ok(Expr::Store {
                        addr: Box::new(addr),
                        bytes: Box::new(bytes),
                        value: Box::new(value),
                    })
                }
            }
        } else {
            Err("Only builtin functions are supported".to_string())
        }
    }

    fn convert_slice(&mut self, slice: &crate::ast::Slice) -> Result<Expr, String> {
        let base = self.convert(&slice.base)?;
        let high = Expr::Int(APInt::new(16, slice.end as u64));
        let low = Expr::Int(APInt::new(16, slice.start as u64));

        Ok(Expr::Extract {
            input: Box::new(base),
            high: Box::new(high),
            low: Box::new(low),
        })
    }

    fn convert_index_access(&mut self, index: &crate::ast::IndexAccess) -> Result<Expr, String> {
        // Index access [n] is equivalent to extracting bit n (slice [n:n])
        let base = self.convert(&index.base)?;
        let idx = Expr::Int(APInt::new(16, index.index as u64));

        Ok(Expr::Extract {
            input: Box::new(base),
            high: Box::new(idx.clone()),
            low: Box::new(idx),
        })
    }

    fn convert_field(&mut self, field: &crate::ast::Field) -> Result<Expr, String> {
        // Field access is used for register references like GPR.x0
        // Base should be an identifier (register class), member is the register name/number

        if let AstExpr::Ident(base_ident) = &*field.base {
            if base_ident.name == "self" {
                if let Some(&value) = self.params.get(&field.member) {
                    let v = value as u64;
                    let width = if v == 0 { 1 } else { 64 - v.leading_zeros() };
                    return Ok(Expr::Int(APInt::new(width, v)));
                }
                let symbol_id = self.alloc_symbol(SymbolInfo::Variable {
                    name: field.member.clone(),
                });
                return Ok(Expr::Symbol(symbol_id));
            }

            let register_class = base_ident.name.clone();

            // Try to parse the member as a register number
            // Support both numeric (x0, x1) and direct numbers (0, 1)
            let register_number = if let Some(num_str) = field.member.strip_prefix('x') {
                num_str
                    .parse::<u32>()
                    .map_err(|_| format!("Invalid register number: {}", field.member))?
            } else {
                field
                    .member
                    .parse::<u32>()
                    .map_err(|_| format!("Invalid register number: {}", field.member))?
            };

            let symbol_id = self.get_or_create_register_symbol(register_class, register_number);
            Ok(Expr::Symbol(symbol_id))
        } else {
            Err("Register field access requires base to be an identifier".to_string())
        }
    }

    fn convert_if(&mut self, if_expr: &crate::ast::If) -> Result<Expr, String> {
        let cond = self.convert(&if_expr.cond)?;
        let then_expr = self.convert(&if_expr.then)?;

        let else_expr = if let Some(else_) = &if_expr.else_ {
            self.convert(else_)?
        } else {
            // If there's no else branch, use 0 as default
            Expr::Int(APInt::new(1, 0))
        };

        Ok(Expr::If {
            cond: Box::new(cond),
            then: Box::new(then_expr),
            else_: Box::new(else_expr),
        })
    }

    fn convert_block(&mut self, block: &crate::ast::Block) -> Result<Expr, String> {
        if block.stmts.is_empty() {
            return Ok(Expr::Int(APInt::new(1, 0)));
        }

        // For now, just convert the last expression
        // TODO: Handle sequences of statements properly
        let last_idx = block.stmts.len() - 1;
        self.convert(&block.stmts[last_idx])
    }

    fn convert_builtin_function(&mut self, _builtin: &BuiltinFunction) -> Result<Expr, String> {
        // Builtin functions should be handled in convert_call
        Err("Builtin functions must be called".to_string())
    }

    /// Finish conversion and return the result with symbol information
    pub fn into_symbols(self) -> HashMap<u32, SymbolInfo> {
        self.symbols
    }
}

/// Convert a TMDL AST expression to a semantic expression
pub fn convert_to_sem_expr(
    ast_expr: &AstExpr,
    params: HashMap<String, i64>,
) -> Result<ConversionResult, String> {
    let mut ctx = ConversionContext::new(params);
    let expr = ctx.convert(ast_expr)?;
    let symbols = ctx.into_symbols();

    Ok(ConversionResult { expr, symbols })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{Binary, Ident, LitInt};
    use chumsky::span::SimpleSpan;

    fn make_span() -> SimpleSpan {
        SimpleSpan::from(0..0)
    }

    #[test]
    fn test_convert_literal() {
        let ast = AstExpr::Lit(Lit::Int(LitInt::new("42".to_string(), make_span())));
        let result = convert_to_sem_expr(&ast, HashMap::new()).unwrap();

        match result.expr {
            Expr::Int(i) => {
                assert_eq!(i.to_u64(), 42);
            }
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_convert_hex_literal() {
        let ast = AstExpr::Lit(Lit::Int(LitInt::new("0xFF".to_string(), make_span())));
        let result = convert_to_sem_expr(&ast, HashMap::new()).unwrap();

        match result.expr {
            Expr::Int(i) => {
                assert_eq!(i.to_u64(), 0xFF);
            }
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_convert_binary_add() {
        let ast = AstExpr::Binary(Binary {
            lhs: Box::new(AstExpr::Lit(Lit::Int(LitInt::new(
                "10".to_string(),
                make_span(),
            )))),
            rhs: Box::new(AstExpr::Lit(Lit::Int(LitInt::new(
                "20".to_string(),
                make_span(),
            )))),
            op: BinOp::Add,
            span: make_span(),
        });

        let result = convert_to_sem_expr(&ast, HashMap::new()).unwrap();

        match result.expr {
            Expr::Add(_, _) => {}
            _ => panic!("Expected Add"),
        }
    }

    #[test]
    fn test_convert_binary_lt() {
        let ast = AstExpr::Binary(Binary {
            lhs: Box::new(AstExpr::Lit(Lit::Int(LitInt::new(
                "10".to_string(),
                make_span(),
            )))),
            rhs: Box::new(AstExpr::Lit(Lit::Int(LitInt::new(
                "20".to_string(),
                make_span(),
            )))),
            op: BinOp::LessThan,
            span: make_span(),
        });

        let result = convert_to_sem_expr(&ast, HashMap::new()).unwrap();

        match result.expr {
            Expr::Lt(_, _) => {}
            _ => panic!("Expected Lt"),
        }
    }

    #[test]
    fn test_convert_binary_unsigned_lt() {
        let ast = AstExpr::Binary(Binary {
            lhs: Box::new(AstExpr::Lit(Lit::Int(LitInt::new(
                "10".to_string(),
                make_span(),
            )))),
            rhs: Box::new(AstExpr::Lit(Lit::Int(LitInt::new(
                "20".to_string(),
                make_span(),
            )))),
            op: BinOp::UnsignedLessThan,
            span: make_span(),
        });

        let result = convert_to_sem_expr(&ast, HashMap::new()).unwrap();

        match result.expr {
            Expr::ULt(_, _) => {}
            _ => panic!("Expected ULt"),
        }
    }

    #[test]
    fn test_convert_parameter() {
        let mut params = HashMap::new();
        params.insert("width".to_string(), 32);

        let ast = AstExpr::Ident(Ident::new("width".to_string(), make_span()));
        let result = convert_to_sem_expr(&ast, params).unwrap();

        match result.expr {
            Expr::Int(i) => {
                assert_eq!(i.to_u64(), 32);
            }
            _ => panic!("Expected Int"),
        }
    }

    #[test]
    fn test_convert_register_field() {
        let ast = AstExpr::Field(crate::ast::Field {
            base: Box::new(AstExpr::Ident(Ident::new("GPR".to_string(), make_span()))),
            member: "x5".to_string(),
            span: make_span(),
        });

        let result = convert_to_sem_expr(&ast, HashMap::new()).unwrap();

        match result.expr {
            Expr::Symbol(id) => {
                assert!(result.symbols.contains_key(&id));
                if let Some(SymbolInfo::Register { class, number }) = result.symbols.get(&id) {
                    assert_eq!(class, "GPR");
                    assert_eq!(*number, 5);
                } else {
                    panic!("Expected Register symbol");
                }
            }
            _ => panic!("Expected Symbol"),
        }
    }

    #[test]
    fn test_convert_slice() {
        let ast = AstExpr::Slice(crate::ast::Slice {
            base: Box::new(AstExpr::Lit(Lit::Int(LitInt::new(
                "0xFF".to_string(),
                make_span(),
            )))),
            start: 0,
            end: 3,
            span: make_span(),
        });

        let result = convert_to_sem_expr(&ast, HashMap::new()).unwrap();

        match result.expr {
            Expr::Extract { .. } => {}
            _ => panic!("Expected Extract"),
        }
    }
}
