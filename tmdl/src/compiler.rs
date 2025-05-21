use core::fmt;
use std::io::Write;
use std::path::PathBuf;
use std::{fs, io};

use ariadne::{Color, Label, Report, ReportKind, sources};
use chumsky::error::Rich;
use clap::{ArgMatches, CommandFactory, FromArgMatches, Parser, ValueEnum};

use crate::Span;
use crate::lexer::lex;
use crate::parser::parse;

pub struct Compiler {
    action: Action,
    inputs: Vec<String>,
    output: OutputKind,
    dialect: Option<String>,
}

pub struct CompilerBuilder {
    action: Option<Action>,
    inputs: Vec<String>,
    output: Option<OutputKind>,
    dialect: Option<String>,
}

#[derive(Clone, Debug)]
pub enum OutputKind {
    File(String),
    Batch(String),
    Stdout,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum Action {
    EmitTokens,
    EmitAst,
    EmitRust,
}

#[derive(Debug, Parser)]
pub struct Cli {
    #[arg(value_enum, long)]
    pub action: Action,
    pub input: String,
    #[arg(short, long)]
    pub output: String,
    #[arg(short, long)]
    pub dialect: Option<String>,
}

impl Compiler {
    pub fn builder() -> CompilerBuilder {
        CompilerBuilder {
            action: None,
            inputs: vec![],
            output: None,
            dialect: None,
        }
    }

    pub fn compile(&self) -> Result<(), Box<dyn std::error::Error>> {
        for input in &self.inputs {
            let mut output: Box<dyn Write> = match &self.output {
                OutputKind::Stdout => Box::new(io::BufWriter::new(io::stdout())),
                OutputKind::File(path) => {
                    let file = fs::OpenOptions::new()
                        .create(true)
                        .truncate(true)
                        .open(path)?;
                    Box::new(io::BufWriter::new(file))
                }
                OutputKind::Batch(out_dir) => {
                    let mut path = PathBuf::from(out_dir);
                    path.push(input.replace(".tmdl", ".rs"));

                    fs::create_dir_all(path.parent().as_ref().unwrap())?;

                    let file = fs::OpenOptions::new()
                        .create(true)
                        .write(true)
                        .truncate(true)
                        .read(true)
                        .open(&path)?;
                    Box::new(io::BufWriter::new(file))
                }
            };

            let source = std::fs::read_to_string(input)?;

            match &self.action {
                Action::EmitTokens => {
                    // TODO print errors if any
                    let (tokens, _errors) = lex(&source);
                    writeln!(output, "{:#?}", tokens)?;
                }
                Action::EmitAst => {
                    let (tokens, errors) = lex(&source);

                    if !errors.is_empty() {
                        // print_errors(input, &source, errors);
                        return Ok(());
                    }

                    let (file, errors) = parse(&source, &tokens);
                    if !errors.is_empty() {
                        print_errors(input, &source, errors);
                        return Ok(());
                    }

                    writeln!(output, "{:#?}", file)?;
                    // let file = parse()
                    // let tokens = lex(&source).unwrap();
                    // let root = parse(&tokens);
                    // let red_root = SyntaxNodeData::new(root);
                    // let translation_unit = ast::SourceFile::new(red_root);
                    // writeln!(output, "{:#?}", translation_unit)?;
                }
                Action::EmitRust => {
                    // if self.dialect.is_none() {
                    //     let mut cmd = Cli::command();
                    //     cmd.error(
                    //         clap::error::ErrorKind::ArgumentConflict,
                    //         "--dialect must be specified with --action=emit-rust",
                    //     )
                    //     .exit();
                    // }
                    // let tokens = lex(&source).unwrap();
                    // let root = parse(&tokens);
                    // let red_root = SyntaxNodeData::new(root);
                    // let translation_unit = ast::SourceFile::new(red_root);
                    // emit_rust(
                    //     &mut output,
                    //     &translation_unit.unwrap(),
                    //     self.dialect.as_ref().unwrap(),
                    // )?;
                }
            }
        }

        Ok(())
    }
}

impl CompilerBuilder {
    pub fn action(self, action: Action) -> Self {
        Self {
            action: Some(action),
            inputs: self.inputs,
            output: self.output,
            dialect: self.dialect,
        }
    }

    pub fn add_input(self, path: &str) -> Self {
        let mut inputs = self.inputs;
        inputs.push(path.to_string());

        Self {
            action: self.action,
            inputs,
            output: self.output,
            dialect: self.dialect,
        }
    }

    pub fn output(self, output: OutputKind) -> Self {
        Self {
            action: self.action,
            inputs: self.inputs,
            output: Some(output),
            dialect: self.dialect,
        }
    }

    pub fn dialect(self, dialect: Option<String>) -> Self {
        Self {
            action: self.action,
            inputs: self.inputs,
            output: self.output,
            dialect,
        }
    }

    pub fn build(self) -> Compiler {
        Compiler {
            action: self.action.unwrap(),
            inputs: self.inputs,
            output: self.output.unwrap(),
            dialect: self.dialect,
        }
    }
}

pub fn compiler_main(args: Option<&ArgMatches>) -> Result<(), Box<dyn std::error::Error>> {
    let args = match args {
        Some(args) => Cli::from_arg_matches(args),
        None => Ok(Cli::parse()),
    }?;

    let output = match args.output.as_str() {
        "-" => OutputKind::Stdout,
        _ => OutputKind::File(args.output.clone()),
    };
    let compiler = Compiler::builder()
        .action(args.action)
        .add_input(&args.input)
        .dialect(args.dialect.clone())
        .output(output)
        .build();

    compiler.compile()
}

fn print_errors<'src, T>(file_name: &str, source: &'src str, errors: Vec<Rich<'src, T, Span>>)
where
    T: fmt::Display,
{
    errors.into_iter().for_each(|e| {
        Report::build(
            ReportKind::Error,
            (file_name.to_string(), e.span().into_range()),
        )
        .with_config(ariadne::Config::new().with_index_type(ariadne::IndexType::Byte))
        .with_message(e.to_string())
        .with_label(
            Label::new((file_name.to_string(), e.span().into_range()))
                .with_message(e.reason().to_string())
                .with_color(Color::Red),
        )
        .with_labels(e.contexts().map(|(label, span)| {
            Label::new((file_name.to_string(), span.into_range()))
                .with_message(format!("while parsing this {}", label))
                .with_color(Color::Yellow)
        }))
        .finish()
        .print(sources([(file_name.to_string(), source.to_string())]))
        .unwrap()
    })
}
