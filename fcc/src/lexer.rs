use logos::Logos;

use tir::utils::APInt;

#[derive(Logos, Debug, Clone, PartialEq)]
#[logos(skip r"[ \t\n\f]+")]
pub enum Token {
    #[token("alignas")]
    KwAlignas,
    #[token("alignof")]
    KwAlignof,
    #[token("auto")]
    KwAuto,
    #[token("bool")]
    KwBool,
    #[token("break")]
    KwBreak,
    #[token("case")]
    KwCase,
    #[token("char")]
    KwChar,
    #[token("const")]
    KwConst,
    #[token("constexpr")]
    KwConstexpr,
    #[token("continue")]
    KwContinue,
    #[token("default")]
    KwDefault,
    #[token("do")]
    KwDo,
    #[token("double")]
    KwDouble,
    #[token("else")]
    KwElse,
    #[token("enum")]
    KwEnum,
    #[token("extern")]
    KwExtern,
    #[token("false")]
    KwFalse,
    #[token("float")]
    KwFloat,
    #[token("for")]
    KwFor,
    #[token("goto")]
    KwGoto,
    #[token("if")]
    KwIf,
    #[token("inline")]
    KwInline,
    #[token("int")]
    KwInt,
    #[token("long")]
    KwLong,
    #[token("nullptr")]
    KwNullptr,
    #[token("register")]
    KwRegister,
    #[token("restrict")]
    KwRestrict,
    #[token("return")]
    KwReturn,
    #[token("short")]
    KwShort,
    #[token("signed")]
    KwSigned,
    #[token("sizeof")]
    KwSizeof,
    #[token("static")]
    KwStatic,
    #[token("static_assert")]
    KwStaticAssert,
    #[token("struct")]
    KwStruct,
    #[token("switch")]
    KwSwitch,
    #[token("thread_local")]
    KwThreadLocal,
    #[token("true")]
    KwTrue,
    #[token("typedef")]
    KwTypedef,
    #[token("typeof")]
    KwTypeof,
    #[token("typeof_unqual")]
    KwTypeofUnqual,
    #[token("union")]
    KwUnion,
    #[token("unsigned")]
    KwUnsigned,
    #[token("void")]
    KwVoid,
    #[token("volatile")]
    KwVolatile,
    #[token("while")]
    KwWhile,

    // TODO C11 underscore keywords?

    // Preprocessor punctuation (must come before Hash so ## wins on longest-match).
    #[token("##")]
    HashHash,
    #[token("#")]
    Hash,

    // Or regular expressions.
    #[regex("[a-zA-Z_][a-zA-Z0-9_]*")]
    Identifier,
    #[regex("[0-9][0-9_]*|0[xX][0-9a-fA-F][0-9a-fA-F_]*|0[oO][0-7][0-7_]*|0[bB][01][01_]*", |lex| lex.slice().parse::<APInt>().ok())]
    IntegerLiteral(APInt),

    #[token("(")]
    LParen,
    #[token(")")]
    RParen,
    #[token("{")]
    LBrace,
    #[token("}")]
    RBrace,
    #[token(";")]
    Semicolon,
}
