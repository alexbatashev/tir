#[cfg(test)]
mod tests {
    use logos::Logos;

    use crate::lexer::Token;

    fn lex(input: &str) -> Vec<Token> {
        Token::lexer(input).map(|r| r.unwrap()).collect()
    }

    #[test]
    fn test_simple_function() {
        let tokens = lex("int main() { return 0; }");
        insta::assert_debug_snapshot!(tokens);
    }
}
