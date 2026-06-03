//! Configuration for a FileCheck run.

use clap::Args;

/// Options controlling how check directives are discovered and matched.
///
/// The defaults mirror LLVM's FileCheck: the check prefix is `CHECK`, comment
/// prefixes are `COM` and `RUN`, and horizontal whitespace is canonicalised
/// when matching.
#[derive(Debug, Clone, Default, Args)]
pub struct Config {
    /// Prefix(es) used for check directives. May be given multiple times or as
    /// a comma-separated list. Defaults to `CHECK`.
    #[arg(
        long = "check-prefix",
        alias = "check-prefixes",
        value_name = "PREFIX",
        value_delimiter = ','
    )]
    pub check_prefixes: Vec<String>,

    /// Prefix(es) marking comment lines that are ignored. Defaults to
    /// `COM,RUN`.
    #[arg(
        long = "comment-prefixes",
        value_name = "PREFIX",
        value_delimiter = ','
    )]
    pub comment_prefixes: Vec<String>,

    /// Do not canonicalise whitespace; match it verbatim.
    #[arg(long = "strict-whitespace")]
    pub strict_whitespace: bool,

    /// Require each pattern to match a whole line.
    #[arg(long = "match-full-lines")]
    pub match_full_lines: bool,

    /// Allow the input to be empty.
    #[arg(long = "allow-empty")]
    pub allow_empty: bool,

    /// Patterns that must not appear anywhere in the input (like an implicit
    /// `CHECK-NOT`).
    #[arg(long = "implicit-check-not", value_name = "PATTERN")]
    pub implicit_check_not: Vec<String>,
}

impl Config {
    /// Returns the configured check prefixes, falling back to the default.
    pub fn effective_check_prefixes(&self) -> Vec<String> {
        if self.check_prefixes.is_empty() {
            vec!["CHECK".to_string()]
        } else {
            let mut p = self.check_prefixes.clone();
            p.sort();
            p.dedup();
            p
        }
    }

    /// Returns the configured comment prefixes, falling back to the default.
    pub fn effective_comment_prefixes(&self) -> Vec<String> {
        if self.comment_prefixes.is_empty() {
            vec!["COM".to_string(), "RUN".to_string()]
        } else {
            let mut p = self.comment_prefixes.clone();
            p.sort();
            p.dedup();
            p
        }
    }
}
