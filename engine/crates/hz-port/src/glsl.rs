//! A GLSL subset → Metal Shading Language translator.
//!
//! ## Scope, stated up front
//!
//! This is not a general GLSL compiler. It covers the subset that Minecraft
//! resource-pack shaders actually use, and it **refuses** everything else with a
//! line-numbered diagnostic. A translator that silently emits broken MSL is
//! worse than one that says "line 14: loops are not supported".
//!
//! Supported:
//!
//! * `in` / `out` / `uniform` declarations with `layout(location = n)`,
//! * `void main()` with: declarations, assignments (including swizzle targets),
//!   `if` / `else`, `discard`, expression statements,
//! * expressions: literals, identifiers, swizzles, constructors, the built-in
//!   functions listed in [`BUILTINS`], arithmetic/comparison/logical operators,
//!   `gl_Position` and the fragment colour output.
//!
//! Refused, with a reason: loops, user-defined functions, structs, arrays,
//! `#define`/preprocessor conditionals, ternary `?:`, matrices other than
//! `mat4`, and any built-in not in the table.
//!
//! ## Output shape
//!
//! One shared `HzVaryings` struct, a `HzUniforms` block holding exactly the
//! uniforms the engine provides, and a vertex/fragment pair whose entry points
//! match the engine's own pipeline (`hz_vertex` / `hz_fragment`). Attribute
//! indices follow declaration order.
//!
//! ## What is not verified here
//!
//! The generated MSL is **not compiled by this crate** — there is no Metal
//! compiler on Linux or Windows. CI runs `xcrun metal` on the generated output;
//! locally the tests assert structure and refusal behaviour. See
//! docs/SHADERS.md.

use crate::shader::{ShaderPack, StageKind, PROVIDED_UNIFORMS};

// ---------------------------------------------------------------- diagnostics

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub stage: StageKind,
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}: {}", self.stage.as_str(), self.line, self.message)
    }
}

// --------------------------------------------------------------------- output

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranslatedStage {
    pub stage: StageKind,
    /// MSL entry point name.
    pub entry: String,
    /// The function body only, for tests that want to assert on it.
    pub body: String,
}

#[derive(Clone, Debug)]
pub struct TranslatedPack {
    pub name: String,
    /// Complete MSL translation unit.
    pub msl: String,
    pub vertex: TranslatedStage,
    pub fragment: TranslatedStage,
    /// Non-fatal notes: things that translate but behave differently.
    pub notes: Vec<String>,
}

// ------------------------------------------------------------------- built-ins

/// GLSL built-in → MSL, for the functions in the supported subset.
const BUILTINS: &[(&str, &str)] = &[
    ("abs", "fabs"),
    ("acos", "acos"),
    ("asin", "asin"),
    ("atan", "atan"),
    ("ceil", "ceil"),
    ("clamp", "clamp"),
    ("cos", "cos"),
    ("cross", "cross"),
    ("degrees", "degrees"),
    ("distance", "distance"),
    ("dot", "dot"),
    ("degrees", "degrees"),
    ("exp", "exp"),
    ("exp2", "exp2"),
    ("floor", "floor"),
    ("fract", "fract"),
    ("length", "length"),
    ("log", "log"),
    ("log2", "log2"),
    ("max", "max"),
    ("min", "min"),
    ("mix", "mix"),
    ("mod", "fmod"),
    ("normalize", "normalize"),
    ("pow", "pow"),
    ("radians", "radians"),
    ("reflect", "reflect"),
    ("refract", "refract"),
    ("round", "round"),
    ("sign", "sign"),
    ("sin", "sin"),
    ("smoothstep", "smoothstep"),
    ("sqrt", "sqrt"),
    ("step", "step"),
    ("tan", "tan"),
    ("inversesqrt", "rsqrt"),
];

fn msl_builtin(name: &str) -> Option<&'static str> {
    BUILTINS
        .iter()
        .find(|(glsl, _)| *glsl == name)
        .map(|(_, msl)| *msl)
}

// -------------------------------------------------------------------- typing

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ty {
    Void,
    Float,
    Vec2,
    Vec3,
    Vec4,
    Mat4,
    Bool,
    Int,
    Sampler,
    Unknown,
}

impl Ty {
    fn msl(self) -> &'static str {
        match self {
            Ty::Void => "void",
            Ty::Float => "float",
            Ty::Vec2 => "float2",
            Ty::Vec3 => "float3",
            Ty::Vec4 => "float4",
            Ty::Mat4 => "float4x4",
            Ty::Bool => "bool",
            Ty::Int => "int",
            Ty::Sampler => "texture2d_array<float>",
            Ty::Unknown => "auto",
        }
    }

    fn from_glsl(name: &str) -> Option<Ty> {
        Some(match name {
            "void" => Ty::Void,
            "float" | "double" => Ty::Float,
            "vec2" => Ty::Vec2,
            "vec3" => Ty::Vec3,
            "vec4" => Ty::Vec4,
            "mat4" => Ty::Mat4,
            "bool" => Ty::Bool,
            "int" => Ty::Int,
            "sampler2D" | "sampler2DArray" => Ty::Sampler,
            _ => return None,
        })
    }

    /// Number of components; 1 for scalars, 0 when not numeric.
    fn components(self) -> u8 {
        match self {
            Ty::Float | Ty::Int => 1,
            Ty::Vec2 => 2,
            Ty::Vec3 => 3,
            Ty::Vec4 => 4,
            _ => 0,
        }
    }

    fn from_components(n: u8) -> Ty {
        match n {
            1 => Ty::Float,
            2 => Ty::Vec2,
            3 => Ty::Vec3,
            _ => Ty::Vec4,
        }
    }

    /// Result of mixing two numeric operands the way GLSL does: the wider one.
    fn broadcast(a: Ty, b: Ty) -> Ty {
        if a.components() >= b.components() {
            a
        } else {
            b
        }
    }
}

// ------------------------------------------------------------------- emitter

struct Emitter {
    stage: StageKind,
    diagnostics: Vec<Diagnostic>,
    symbols: std::collections::HashMap<String, Ty>,
    /// Identifier → MSL spelling: inputs become `in.x`, engine uniforms `u.x`,
    /// varyings `out.x` / `in.x`. Renaming where the identifier is emitted keeps
    /// the generated body honest instead of patching strings afterwards.
    renames: std::collections::HashMap<String, String>,
    notes: Vec<String>,
    /// Name the fragment stage writes its colour into.
    colour_var: String,
    indent: usize,
}

impl Emitter {
    fn error(&mut self, line: usize, message: impl Into<String>) {
        self.diagnostics.push(Diagnostic {
            stage: self.stage,
            line,
            message: message.into(),
        });
    }

    fn push(&mut self, out: &mut String) {
        for _ in 0..self.indent {
            out.push_str("    ");
        }
    }
}

// ------------------------------------------------------------------ tokenizer

#[derive(Clone, Debug, PartialEq)]
struct Token {
    text: String,
    line: usize,
}

fn tokenize(src: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let bytes = src.as_bytes();
    let mut line = 1usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\n' {
            line += 1;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        // comments
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            i += 2;
            while i + 1 < bytes.len() && !(bytes[i] == b'*' && bytes[i + 1] == b'/') {
                if bytes[i] == b'\n' {
                    line += 1;
                }
                i += 1;
            }
            i = (i + 2).min(bytes.len());
            continue;
        }
        // preprocessor lines are kept whole so the caller can reject them
        if c == b'#' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
            out.push(Token {
                text: String::from_utf8_lossy(&bytes[start..i]).into_owned(),
                line,
            });
            continue;
        }
        // Identifier: letters, digits and underscore. A dot is *not* part of an
        // identifier — `base.rgb` must tokenize as three tokens so the swizzle
        // parser can see it.
        if c.is_ascii_alphabetic() || c == b'_' {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            out.push(Token {
                text: String::from_utf8_lossy(&bytes[start..i]).into_owned(),
                line,
            });
            continue;
        }
        // Number: digits, a decimal point, an exponent. A leading dot counts
        // only when a digit follows (`.5`).
        let starts_number = c.is_ascii_digit()
            || (c == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit());
        if starts_number {
            let start = i;
            while i < bytes.len()
                && (bytes[i].is_ascii_digit()
                    || bytes[i] == b'.'
                    || bytes[i] == b'e'
                    || bytes[i] == b'E')
            {
                i += 1;
            }
            // exponent sign, e.g. 1e-3
            if i < bytes.len()
                && (bytes[i] == b'-' || bytes[i] == b'+')
                && i > start
                && (bytes[i - 1] == b'e' || bytes[i - 1] == b'E')
            {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_digit() {
                    i += 1;
                }
            }
            out.push(Token {
                text: String::from_utf8_lossy(&bytes[start..i]).into_owned(),
                line,
            });
            continue;
        }
        // two-character operators
        let two = if i + 1 < bytes.len() {
            &src[i..(i + 2).min(src.len())]
        } else {
            ""
        };
        if matches!(
            two,
            "<=" | ">=" | "==" | "!=" | "&&" | "||" | "++" | "--" | "+=" | "-=" | "*=" | "/="
        ) {
            out.push(Token {
                text: two.to_string(),
                line,
            });
            i += 2;
            continue;
        }
        out.push(Token {
            text: (c as char).to_string(),
            line,
        });
        i += 1;
    }
    out
}

/// Peel off `void main()` and return its body tokens.
fn main_body(tokens: &[Token]) -> Option<&[Token]> {
    let mut i = 0;
    while i + 3 < tokens.len() {
        if tokens[i].text == "void" && tokens[i + 1].text == "main" && tokens[i + 2].text == "(" {
            let mut j = i + 2;
            while j < tokens.len() && tokens[j].text != ")" {
                j += 1;
            }
            if j + 1 < tokens.len() && tokens[j + 1].text == "{" {
                let start = j + 2;
                let mut depth = 1;
                let mut k = start;
                while k < tokens.len() {
                    match tokens[k].text.as_str() {
                        "{" => depth += 1,
                        "}" => {
                            depth -= 1;
                            if depth == 0 {
                                return Some(&tokens[start..k]);
                            }
                        }
                        _ => {}
                    }
                    k += 1;
                }
            }
        }
        i += 1;
    }
    None
}

// --------------------------------------------------------- expression parser

struct Parser<'t, 'e> {
    tokens: &'t [Token],
    pos: usize,
    emitter: &'e mut Emitter,
    failures: usize,
}

impl Parser<'_, '_> {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn line(&self) -> usize {
        self.peek().map(|t| t.line).unwrap_or(0)
    }

    fn bump(&mut self) -> Option<Token> {
        let token = self.tokens.get(self.pos).cloned();
        self.pos += 1;
        token
    }

    fn eat(&mut self, text: &str) -> bool {
        if self.peek().map(|t| t.text == text).unwrap_or(false) {
            self.pos += 1;
            true
        } else {
            false
        }
    }

    /// Precedence climbing over the subset's operators.
    fn expression(&mut self) -> (String, Ty) {
        self.ternary_or_binary(0)
    }

    fn ternary_or_binary(&mut self, min_prec: u8) -> (String, Ty) {
        let (mut left, mut left_ty) = self.unary();
        while let Some(token) = self.peek() {
            let (op, prec) = match token.text.as_str() {
                "||" => ("||", 1),
                "&&" => ("&&", 2),
                "==" | "!=" => (token.text.as_str(), 3),
                "<" | ">" | "<=" | ">=" => (token.text.as_str(), 4),
                "+" | "-" => (token.text.as_str(), 5),
                "*" | "/" => (token.text.as_str(), 6),
                "?" => {
                    self.emitter.error(
                        self.line(),
                        "the ternary operator is outside the supported subset",
                    );
                    self.failures += 1;
                    return (left, left_ty);
                }
                _ => break,
            };
            if prec < min_prec {
                break;
            }
            let op = op.to_string();
            self.pos += 1;
            let (right, right_ty) = self.ternary_or_binary(prec + 1);
            let ty = match op.as_str() {
                "==" | "!=" | "<" | ">" | "<=" | ">=" | "&&" | "||" => Ty::Bool,
                "*" => {
                    if left_ty == Ty::Mat4 || right_ty == Ty::Mat4 {
                        if left_ty == Ty::Mat4 && right_ty == Ty::Mat4 {
                            Ty::Mat4
                        } else if left_ty == Ty::Mat4 {
                            right_ty
                        } else {
                            left_ty
                        }
                    } else {
                        Ty::broadcast(left_ty, right_ty)
                    }
                }
                _ => Ty::broadcast(left_ty, right_ty),
            };
            // GLSL `mat4 * vec4` and `vec4 * mat4` both exist; MSL keeps both.
            left = format!("({left} {op} {right})");
            left_ty = ty;
        }
        (left, left_ty)
    }

    fn unary(&mut self) -> (String, Ty) {
        if self.eat("-") {
            let (inner, ty) = self.unary();
            return (format!("(-{inner})"), ty);
        }
        if self.eat("!") {
            let (inner, _) = self.unary();
            return (format!("(!{inner})"), Ty::Bool);
        }
        if self.eat("+") {
            return self.unary();
        }
        self.postfix()
    }

    fn postfix(&mut self) -> (String, Ty) {
        let (mut text, mut ty) = self.primary();
        while let Some(token) = self.peek().cloned() {
            if token.text == "." {
                self.pos += 1;
                let field = match self.bump() {
                    Some(f) => f,
                    None => break,
                };
                let comps = field
                    .text
                    .chars()
                    .filter(|c| "xyzwrgbastpq".contains(*c))
                    .count();
                let valid = field.text.chars().all(|c| "xyzwrgbastpq".contains(c))
                    && !field.text.is_empty();
                if !valid {
                    self.emitter.error(
                        field.line,
                        format!("unsupported member access .{}", field.text),
                    );
                    self.failures += 1;
                    return (text, ty);
                }
                text = format!("{text}.{}", field.text);
                ty = if comps == 0 {
                    Ty::Unknown
                } else {
                    Ty::from_components(comps.min(4) as u8)
                };
                continue;
            }
            if token.text == "[" {
                self.emitter
                    .error(token.line, "array indexing is outside the supported subset");
                self.failures += 1;
                break;
            }
            break;
        }
        (text, ty)
    }

    fn primary(&mut self) -> (String, Ty) {
        let token = match self.bump() {
            Some(t) => t,
            None => return ("0.0".to_string(), Ty::Unknown),
        };

        if token.text == "(" {
            let (inner, ty) = self.expression();
            self.eat(")");
            return (format!("({inner})"), ty);
        }

        // numeric literal
        if token
            .text
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
        {
            let text = token.text.trim_end_matches('u').to_string();
            let text = if text.contains('.') || text.contains('e') || text.contains('E') {
                text
            } else {
                format!("{text}.0")
            };
            return (text, Ty::Float);
        }

        // constructor or call
        if matches!(self.peek(), Some(t) if t.text == "(") {
            return self.call(&token);
        }

        // identifier
        let renamed = self
            .emitter
            .renames
            .get(&token.text)
            .cloned()
            .unwrap_or_else(|| token.text.clone());
        let ty = self
            .emitter
            .symbols
            .get(&token.text)
            .copied()
            .unwrap_or(Ty::Unknown);
        if ty == Ty::Unknown {
            // Built-in varyings get renamed by the caller through the symbol
            // table; anything else is a real error.
            if !matches!(token.text.as_str(), "true" | "false") {
                self.emitter
                    .error(token.line, format!("unknown identifier `{}`", token.text));
                self.failures += 1;
            } else {
                return (token.text, Ty::Bool);
            }
        }
        (renamed, ty)
    }

    fn call(&mut self, name: &Token) -> (String, Ty) {
        self.eat("(");
        let mut args: Vec<(String, Ty)> = Vec::new();
        if !self.eat(")") {
            loop {
                let (text, ty) = self.expression();
                args.push((text, ty));
                if self.eat(",") {
                    continue;
                }
                self.eat(")");
                break;
            }
        }

        // Constructors: vecN(...) / mat4(...) / float(...)
        if let Some(ty) = Ty::from_glsl(&name.text) {
            if ty == Ty::Sampler {
                self.emitter
                    .error(name.line, "cannot construct a sampler in shader code");
                self.failures += 1;
                return ("0.0".to_string(), Ty::Unknown);
            }
            let joined: Vec<String> = args.into_iter().map(|(t, _)| t).collect();
            return (format!("{}({})", ty.msl(), joined.join(", ")), ty);
        }

        if name.text == "texture" || name.text == "texture2D" {
            // texture(sampler, uv) → array sample at slice 0.
            if args.len() < 2 {
                self.emitter
                    .error(name.line, "texture() needs a sampler and coordinates");
                self.failures += 1;
                return ("float4(0.0)".to_string(), Ty::Vec4);
            }
            self.emitter.notes.push(format!(
                "line {}: texture() maps to slice 0 of the engine's texture array",
                name.line
            ));
            let uv = args[1].0.clone();
            return (format!("hz_tex.sample(hz_smp, {uv}, 0)"), Ty::Vec4);
        }

        if let Some(msl) = msl_builtin(&name.text) {
            let joined: Vec<String> = args.iter().map(|(t, _)| t.clone()).collect();
            let ty = match name.text.as_str() {
                "dot" | "length" | "distance" => Ty::Float,
                "cross" => Ty::Vec3,
                "normalize" | "reflect" | "refract" => {
                    args.first().map(|(_, t)| *t).unwrap_or(Ty::Unknown)
                }
                _ => args.iter().map(|(_, t)| *t).fold(Ty::Float, Ty::broadcast),
            };
            return (format!("{}({})", msl, joined.join(", ")), ty);
        }

        self.emitter.error(
            name.line,
            format!(
                "`{}` is not a supported built-in (see docs/SHADERS.md for the subset)",
                name.text
            ),
        );
        self.failures += 1;
        ("0.0".to_string(), Ty::Unknown)
    }
}

// ------------------------------------------------------------------ statements

fn translate_body(tokens: &[Token], emitter: &mut Emitter, failures: &mut usize) -> String {
    let mut out = String::new();
    let mut index = 0usize;
    while index < tokens.len() {
        // find one statement: up to `;` or a brace block
        let token = &tokens[index];
        if token.text.starts_with('#') {
            emitter.error(
                token.line,
                "preprocessor directives are not translated; write plain GLSL",
            );
            *failures += 1;
            index += 1;
            continue;
        }

        // block statement for if/else
        if token.text == "if" {
            // The parser borrows the emitter, so it lives in its own scope: the
            // code below needs the emitter back for indentation.
            let (cond, body_start, mut cursor, parser_failures) = {
                let mut parser = Parser {
                    tokens,
                    pos: index + 1,
                    emitter,
                    failures: *failures,
                };
                let cond = if parser.eat("(") {
                    let (c, _) = parser.expression();
                    parser.eat(")");
                    c
                } else {
                    parser.emitter.error(token.line, "if without a condition");
                    "false".to_string()
                };
                (cond, parser.pos, parser.pos, parser.failures)
            };
            *failures = parser_failures;
            let mut body_tokens: Vec<Token> = Vec::new();
            if tokens.get(cursor).map(|t| t.text.as_str()) == Some("{") {
                let mut depth = 0;
                let start = cursor;
                while cursor < tokens.len() {
                    match tokens[cursor].text.as_str() {
                        "{" => depth += 1,
                        "}" => {
                            depth -= 1;
                            if depth == 0 {
                                cursor += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    cursor += 1;
                }
                body_tokens.extend_from_slice(&tokens[start + 1..cursor.saturating_sub(1)]);
            } else {
                // single statement
                while cursor < tokens.len() && tokens[cursor].text != ";" {
                    cursor += 1;
                }
                if cursor < tokens.len() {
                    cursor += 1;
                }
                body_tokens.extend_from_slice(&tokens[body_start..cursor]);
            }

            emitter.push(&mut out);
            out.push_str(&format!("if ({cond}) {{\n"));
            emitter.indent += 1;
            let inner = translate_body(&body_tokens, emitter, failures);
            out.push_str(&inner);
            emitter.indent -= 1;
            emitter.push(&mut out);
            out.push_str("}\n");

            let mut after = cursor;
            if tokens.get(after).map(|t| t.text.as_str()) == Some("else") {
                after += 1;
                let mut else_tokens: Vec<Token> = Vec::new();
                if tokens.get(after).map(|t| t.text.as_str()) == Some("{") {
                    let mut depth = 0;
                    let start = after;
                    while after < tokens.len() {
                        match tokens[after].text.as_str() {
                            "{" => depth += 1,
                            "}" => {
                                depth -= 1;
                                if depth == 0 {
                                    after += 1;
                                    break;
                                }
                            }
                            _ => {}
                        }
                        after += 1;
                    }
                    else_tokens.extend_from_slice(&tokens[start + 1..after.saturating_sub(1)]);
                } else {
                    while after < tokens.len() && tokens[after].text != ";" {
                        after += 1;
                    }
                    if after < tokens.len() {
                        after += 1;
                    }
                    else_tokens.extend_from_slice(&tokens[cursor + 1..after]);
                }
                emitter.push(&mut out);
                out.push_str("else {\n");
                emitter.indent += 1;
                let inner = translate_body(&else_tokens, emitter, failures);
                out.push_str(&inner);
                emitter.indent -= 1;
                emitter.push(&mut out);
                out.push_str("}\n");
            }
            index = after;
            continue;
        }

        if token.text == "for" || token.text == "while" || token.text == "do" {
            emitter.error(
                token.line,
                format!("`{}` loops are outside the supported subset", token.text),
            );
            *failures += 1;
            // skip to the end of the loop so we do not cascade errors
            let mut depth = 0;
            while index < tokens.len() {
                match tokens[index].text.as_str() {
                    "{" => depth += 1,
                    "}" => {
                        depth -= 1;
                        if depth == 0 {
                            index += 1;
                            break;
                        }
                    }
                    ";" if depth == 0 => {
                        index += 1;
                        break;
                    }
                    _ => {}
                }
                index += 1;
            }
            continue;
        }

        if token.text == "discard" {
            emitter.push(&mut out);
            out.push_str("discard_fragment();\n");
            index += 1;
            if tokens.get(index).map(|t| t.text.as_str()) == Some(";") {
                index += 1;
            }
            continue;
        }

        if token.text == "return" {
            index += 1;
            if tokens.get(index).map(|t| t.text.as_str()) == Some(";") {
                index += 1;
            }
            continue; // main's return is implicit in the generated epilogue
        }

        if token.text == "struct" {
            emitter.error(token.line, "structs are outside the supported subset");
            *failures += 1;
            while index < tokens.len() && tokens[index].text != "}" {
                index += 1;
            }
            index += 1;
            continue;
        }

        if token.text == "{" || token.text == "}" {
            index += 1;
            continue;
        }

        // one simple statement: up to the next `;`
        let start = index;
        let mut cursor = index;
        while cursor < tokens.len() && tokens[cursor].text != ";" {
            if tokens[cursor].text == "(" {
                let mut depth = 0;
                while cursor < tokens.len() {
                    match tokens[cursor].text.as_str() {
                        "(" => depth += 1,
                        ")" => {
                            depth -= 1;
                            if depth == 0 {
                                cursor += 1;
                                break;
                            }
                        }
                        _ => {}
                    }
                    cursor += 1;
                }
                continue;
            }
            cursor += 1;
        }
        let stmt = &tokens[start..cursor.min(tokens.len())];
        index = (cursor + 1).min(tokens.len());
        if stmt.is_empty() {
            continue;
        }

        let translated = translate_statement(stmt, emitter, failures);
        if let Some(text) = translated {
            emitter.push(&mut out);
            out.push_str(&text);
            out.push('\n');
        }
    }
    out
}

/// Declaration, assignment or expression statement.
fn translate_statement(
    stmt: &[Token],
    emitter: &mut Emitter,
    failures: &mut usize,
) -> Option<String> {
    let first = &stmt[0];
    let line = first.line;

    // declaration: `<type> <name> [= expr]`
    if stmt.len() >= 2 {
        if let Some(ty) = Ty::from_glsl(&first.text) {
            let name = stmt[1].text.clone();
            if stmt.len() > 2 && stmt[2].text == "[" {
                emitter.error(line, "arrays are outside the supported subset");
                *failures += 1;
                return None;
            }
            let mut parser = Parser {
                tokens: stmt,
                pos: 2,
                emitter,
                failures: *failures,
            };
            let init = if parser.eat("=") {
                let (text, _) = parser.expression();
                text
            } else {
                match ty {
                    Ty::Float => "0.0".to_string(),
                    Ty::Int => "0".to_string(),
                    Ty::Bool => "false".to_string(),
                    Ty::Vec2 => "float2(0.0)".to_string(),
                    Ty::Vec3 => "float3(0.0)".to_string(),
                    Ty::Vec4 => "float4(0.0)".to_string(),
                    Ty::Mat4 => "float4x4(1.0)".to_string(),
                    _ => "0.0".to_string(),
                }
            };
            *failures = parser.failures;
            emitter.symbols.insert(name.clone(), ty);
            return Some(format!("{} {} = {};", ty.msl(), name, init));
        }
    }

    // assignment: target `=` expr
    enum Outcome {
        Assigned {
            target: String,
            value: String,
            ty: Ty,
        },
        Empty,
        Expression(String),
    }

    let outcome = {
        let mut parser = Parser {
            tokens: stmt,
            pos: 0,
            emitter,
            failures: *failures,
        };
        let (target, target_ty) = parser.postfix();
        let outcome = if parser.eat("=") {
            let (value, value_ty) = parser.expression();
            Outcome::Assigned {
                target,
                value,
                ty: value_ty,
            }
        } else if parser.eat(";") {
            Outcome::Empty
        } else {
            let (text, _) = parser.expression();
            Outcome::Expression(text)
        };
        *failures = parser.failures;
        let _ = target_ty;
        outcome
    };

    match outcome {
        Outcome::Assigned { target, value, ty } => {
            // `gl_FragColor` / a declared `out vec4` both land in the colour var.
            let resolved = if target == "gl_FragColor" {
                emitter.colour_var.clone()
            } else {
                target.clone()
            };
            if emitter.symbols.get(&target).copied().unwrap_or(Ty::Unknown) == Ty::Unknown
                && ty != Ty::Unknown
            {
                emitter.symbols.insert(target, ty);
            }
            Some(format!("{resolved} = {value};"))
        }
        Outcome::Empty => None,
        Outcome::Expression(text) => {
            if text.trim().is_empty() {
                None
            } else {
                Some(format!("{text};"))
            }
        }
    }
}

// ------------------------------------------------------------------ pack level

/// Vertex attribute slots the engine's buffer actually has, by name.
///
/// `position` is 3 floats at offset 0, `uv` 2 at 3, `normal` 3 at 5, `color` 4
/// at 8 and the texture index 1 float at 12 — see `hz_game::frame`.
pub const ENGINE_ATTRIBUTES: &[(&str, u32, &str)] = &[
    ("position", 0, "float3"),
    ("uv", 1, "float2"),
    ("normal", 2, "float3"),
    ("color", 3, "float4"),
    ("texture", 4, "float"),
];

fn engine_attribute(name: &str) -> Option<(u32, &'static str)> {
    ENGINE_ATTRIBUTES
        .iter()
        .find(|(n, _, _)| *n == name)
        .map(|(_, slot, ty)| (*slot, *ty))
}

/// Uniforms the engine binds, in declaration order, with the MSL field type.
const UNIFORM_TYPES: &[(&str, &str)] = &[
    ("mvp", "float4x4"),
    ("cameraPosition", "float4"),
    ("fogColor", "float4"),
    ("fogParams", "float4"),
    ("time", "float"),
    ("daylight", "float"),
];

fn uniform_struct(used: &[String]) -> String {
    let mut out = String::from("struct HzUniforms {\n");
    for (name, ty) in UNIFORM_TYPES {
        // Declared for every provided uniform: MSL has no optional fields and
        // an unused one costs nothing.
        let _ = used;
        out.push_str(&format!("    {ty} {name};\n"));
    }
    out.push_str("};\n");
    out
}

/// Translate a whole pack (vertex + fragment) into one MSL translation unit.
pub fn translate(
    pack: &ShaderPack,
    sources: &[(StageKind, String)],
) -> Result<TranslatedPack, Vec<Diagnostic>> {
    let mut diagnostics: Vec<Diagnostic> = Vec::new();
    let mut notes: Vec<String> = Vec::new();

    // Translating a shader whose uniforms we cannot bind would emit MSL that
    // references undeclared identifiers. Refuse instead of shipping that.
    if !pack.requires.is_empty() {
        diagnostics.push(Diagnostic {
            stage: StageKind::Fragment,
            line: 0,
            message: format!(
                "pack needs uniforms the engine does not provide: {}",
                pack.requires.join(", ")
            ),
        });
        return Err(diagnostics);
    }

    let vertex_src = sources
        .iter()
        .find(|(k, _)| *k == StageKind::Vertex)
        .map(|(_, s)| s.clone());
    let fragment_src = sources
        .iter()
        .find(|(k, _)| *k == StageKind::Fragment)
        .map(|(_, s)| s.clone());
    let (vertex_src, fragment_src) = match (vertex_src, fragment_src) {
        (Some(v), Some(f)) => (v, f),
        _ => {
            diagnostics.push(Diagnostic {
                stage: StageKind::Vertex,
                line: 0,
                message: "pack needs both a vertex and a fragment stage".to_string(),
            });
            return Err(diagnostics);
        }
    };

    // Vertex outputs define the varying struct and the fragment inputs must map
    // onto it by name (the link check in `shader::compile` covers the rest).
    let vertex_out = pack
        .stage(StageKind::Vertex)
        .map(|s| s.outputs.clone())
        .unwrap_or_default();
    let fragment_in = pack
        .stage(StageKind::Fragment)
        .map(|s| s.inputs.clone())
        .unwrap_or_default();

    let mut varyings = String::from("struct HzVaryings {\n    float4 position [[position]];\n");
    for (index, decl) in vertex_out.iter().enumerate() {
        let ty = Ty::from_glsl(&decl.glsl_type).unwrap_or(Ty::Vec4);
        varyings.push_str(&format!(
            "    {} {} [[user(locn{})]];\n",
            ty.msl(),
            decl.name,
            index + 1
        ));
    }
    varyings.push_str("};\n");

    let mut msl = String::new();
    msl.push_str("// Generated by hz-port from a GLSL subset; do not edit.\n");
    msl.push_str(&format!(
        "// pack: {}\n#include <metal_stdlib>\nusing namespace metal;\n\n",
        pack.name
    ));
    msl.push_str(&uniform_struct(&[]));
    msl.push('\n');
    msl.push_str(&varyings);
    msl.push('\n');

    // ---- vertex ----
    let vertex_tokens = tokenize(&vertex_src);
    let mut vertex_emitter = Emitter {
        stage: StageKind::Vertex,
        diagnostics: Vec::new(),
        symbols: std::collections::HashMap::new(),
        renames: std::collections::HashMap::new(),
        notes: Vec::new(),
        colour_var: String::new(),
        indent: 0,
    };
    // Inputs become attributes in declaration order.
    let vertex_in = pack
        .stage(StageKind::Vertex)
        .map(|s| s.inputs.clone())
        .unwrap_or_default();
    let mut vs_in = String::from("struct HzVertexIn {\n");
    for decl in vertex_in.iter() {
        let ty = Ty::from_glsl(&decl.glsl_type).unwrap_or(Ty::Vec3);
        // Bind by name to the engine's fixed attribute slots. Declaration order
        // is meaningless here: the buffer layout is the engine's, not the
        // pack's, and a wrong index reads garbage that still renders.
        let (slot, expected) = match engine_attribute(&decl.name) {
            Some(pair) => pair,
            None => {
                diagnostics.push(Diagnostic {
                    stage: StageKind::Vertex,
                    line: 0,
                    message: format!(
                        "unknown vertex input `{}`; the engine provides position, uv, normal, color, texture",
                        decl.name
                    ),
                });
                continue;
            }
        };
        if ty.msl() != expected {
            diagnostics.push(Diagnostic {
                stage: StageKind::Vertex,
                line: 0,
                message: format!(
                    "vertex input `{}` must be {} (the engine's layout), not {}",
                    decl.name,
                    expected,
                    ty.msl()
                ),
            });
            continue;
        }
        vs_in.push_str(&format!(
            "    {} {} [[attribute({})]];\n",
            ty.msl(),
            decl.name,
            slot
        ));
        vertex_emitter.symbols.insert(decl.name.clone(), ty);
    }
    vs_in.push_str("};\n");

    // Engine-provided uniforms and the built-in position.
    for (name, ty) in UNIFORM_TYPES {
        vertex_emitter
            .symbols
            .insert((*name).to_string(), Ty::from_glsl(ty).unwrap_or(Ty::Vec4));
    }
    for decl in pack
        .stage(StageKind::Vertex)
        .map(|s| &s.uniforms)
        .into_iter()
        .flatten()
    {
        if let Some(ty) = Ty::from_glsl(&decl.glsl_type) {
            vertex_emitter.symbols.insert(decl.name.clone(), ty);
        }
    }
    vertex_emitter
        .symbols
        .insert("gl_Position".to_string(), Ty::Vec4);
    for decl in &vertex_out {
        vertex_emitter.symbols.insert(
            decl.name.clone(),
            Ty::from_glsl(&decl.glsl_type).unwrap_or(Ty::Vec4),
        );
    }
    // Everything the body may name gets its MSL spelling here.
    for name in PROVIDED_UNIFORMS {
        vertex_emitter
            .renames
            .insert((*name).to_string(), format!("u.{name}"));
    }
    for decl in &vertex_in {
        vertex_emitter
            .renames
            .insert(decl.name.clone(), format!("in.{}", decl.name));
    }
    for decl in &vertex_out {
        vertex_emitter
            .renames
            .insert(decl.name.clone(), format!("out.{}", decl.name));
    }
    vertex_emitter
        .renames
        .insert("gl_Position".to_string(), "out.position".to_string());

    let mut vertex_failures = 0usize;
    let vertex_body = match main_body(&vertex_tokens) {
        Some(body) => translate_body(body, &mut vertex_emitter, &mut vertex_failures),
        None => {
            diagnostics.push(Diagnostic {
                stage: StageKind::Vertex,
                line: 0,
                message: "no `void main()` found".to_string(),
            });
            String::new()
        }
    };
    notes.append(&mut vertex_emitter.notes);
    diagnostics.append(&mut vertex_emitter.diagnostics);

    // ---- fragment ----
    let fragment_tokens = tokenize(&fragment_src);
    let mut fragment_emitter = Emitter {
        stage: StageKind::Fragment,
        diagnostics: Vec::new(),
        symbols: std::collections::HashMap::new(),
        renames: std::collections::HashMap::new(),
        notes: Vec::new(),
        colour_var: "hz_colour".to_string(),
        indent: 0,
    };
    for (name, ty) in UNIFORM_TYPES {
        fragment_emitter
            .symbols
            .insert((*name).to_string(), Ty::from_glsl(ty).unwrap_or(Ty::Vec4));
    }
    for decl in &fragment_in {
        fragment_emitter.symbols.insert(
            decl.name.clone(),
            Ty::from_glsl(&decl.glsl_type).unwrap_or(Ty::Vec4),
        );
    }
    for decl in pack
        .stage(StageKind::Fragment)
        .map(|s| &s.uniforms)
        .into_iter()
        .flatten()
    {
        if let Some(ty) = Ty::from_glsl(&decl.glsl_type) {
            fragment_emitter.symbols.insert(decl.name.clone(), ty);
        }
    }
    // A declared `out vec4 name;` is the colour output.
    if let Some(out) = pack
        .stage(StageKind::Fragment)
        .and_then(|s| s.outputs.first())
    {
        fragment_emitter.symbols.insert(
            out.name.clone(),
            Ty::from_glsl(&out.glsl_type).unwrap_or(Ty::Vec4),
        );
        fragment_emitter.colour_var = "hz_colour".to_string();
    }
    fragment_emitter
        .symbols
        .insert("gl_FragColor".to_string(), Ty::Vec4);
    for name in PROVIDED_UNIFORMS {
        fragment_emitter
            .renames
            .insert((*name).to_string(), format!("u.{name}"));
    }
    for decl in &fragment_in {
        fragment_emitter
            .renames
            .insert(decl.name.clone(), format!("in.{}", decl.name));
    }
    if let Some(out) = pack
        .stage(StageKind::Fragment)
        .and_then(|s| s.outputs.first())
    {
        fragment_emitter
            .renames
            .insert(out.name.clone(), "hz_colour".to_string());
    }

    let mut fragment_failures = 0usize;
    let fragment_body = match main_body(&fragment_tokens) {
        Some(body) => translate_body(body, &mut fragment_emitter, &mut fragment_failures),
        None => {
            diagnostics.push(Diagnostic {
                stage: StageKind::Fragment,
                line: 0,
                message: "no `void main()` found".to_string(),
            });
            String::new()
        }
    };
    notes.append(&mut fragment_emitter.notes);
    diagnostics.append(&mut fragment_emitter.diagnostics);

    if !diagnostics.is_empty() {
        return Err(diagnostics);
    }

    // A declared `out vec4 name` is assigned in the body; rename to hz_colour.
    let mut fragment_body = fragment_body;
    if let Some(out) = pack
        .stage(StageKind::Fragment)
        .and_then(|s| s.outputs.first())
    {
        fragment_body = fragment_body.replace(&format!(" {} =", out.name), " hz_colour =");
    }

    msl.push_str(&vs_in);
    msl.push('\n');
    msl.push_str("vertex HzVaryings hz_vertex(HzVertexIn in [[stage_in]], constant HzUniforms& u [[buffer(1)]]) {\n");
    msl.push_str("    HzVaryings out;\n");
    msl.push_str("    out.position = float4(0.0);\n");
    msl.push_str(&fragment_prefix(&vertex_body));
    msl.push_str("    return out;\n}\n\n");

    msl.push_str("fragment float4 hz_fragment(HzVaryings in [[stage_in]], constant HzUniforms& u [[buffer(1)]], texture2d_array<float> hz_tex [[texture(0)]], sampler hz_smp [[sampler(0)]]) {\n");
    msl.push_str("    float4 hz_colour = float4(1.0);\n");
    msl.push_str(&fragment_prefix(&fragment_body));
    msl.push_str("    return hz_colour;\n}\n");

    Ok(TranslatedPack {
        name: pack.name.clone(),
        msl,
        vertex: TranslatedStage {
            stage: StageKind::Vertex,
            entry: "hz_vertex".to_string(),
            body: vertex_body,
        },
        fragment: TranslatedStage {
            stage: StageKind::Fragment,
            entry: "hz_fragment".to_string(),
            body: fragment_body,
        },
        notes,
    })
}

/// Re-indent the translated body by one level (it was emitted at indent 2).
fn fragment_prefix(body: &str) -> String {
    let mut out = String::new();
    for line in body.lines() {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

/// The uniforms the engine provides, for callers that want to show them.
pub fn provided_uniforms() -> &'static [&'static str] {
    PROVIDED_UNIFORMS
}

#[cfg(test)]
mod tests;
