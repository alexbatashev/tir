use std::ops::Range;

use ariadne::{Color, Label, Report, ReportKind, Source};

/// Print a user-friendly parse error using ariadne, highlighting the given span.
///
/// - `source_name`: logical name of the source (e.g., file path or `"<stdin>"`)
/// - `source`: full source text
/// - `span`: byte offset into `source` where the error occurred
/// - `err`: the semantic error to display
pub fn print_parse_error(
    source_name: &str,
    source: &str,
    span: crate::parse::Span,
    err: &crate::Error,
) -> std::io::Result<()> {
    let start = span.0 as usize;
    let end = start.saturating_add(1);
    print_error_range(source_name, source, start..end, format!("{}", err))
}

/// Print an error for an arbitrary byte range in the source.
pub fn print_error_range(
    source_name: &str,
    source: &str,
    range: Range<usize>,
    message: impl std::fmt::Display,
) -> std::io::Result<()> {
    let source_id = source_name.to_string();

    Report::build(ReportKind::Error, (source_id.clone(), range.clone()))
        .with_config(ariadne::Config::new().with_index_type(ariadne::IndexType::Byte))
        .with_message(message.to_string())
        .with_label(
            Label::new((source_id.clone(), range))
                .with_message("here")
                .with_color(Color::Red),
        )
        .finish()
        .eprint((source_id, Source::from(source)))
}
