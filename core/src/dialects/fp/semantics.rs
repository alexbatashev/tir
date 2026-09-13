use std::collections::BTreeMap;

use tir_adt::RoundingMode;

use crate::{Error, attributes::AttributeValue};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Rounding {
    Fixed(RoundingMode),
    Dynamic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Exceptions {
    Ignore,
    Flags,
    FlagsAndTraps,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NaNPolicy {
    AnyQuiet,
    PreservePayload,
    Canonical,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SubnormalMode {
    Gradual,
    FlushInput,
    FlushOutput,
    FlushBoth,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tininess {
    BeforeRounding,
    AfterRounding,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ComparisonBehavior {
    Quiet,
    Signaling,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum InvalidConversion {
    Indeterminate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ArithmeticSemantics {
    pub rounding: Rounding,
    pub exceptions: Exceptions,
    pub nan: NaNPolicy,
    pub subnormals: SubnormalMode,
    pub tininess: Tininess,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ComparisonSemantics {
    pub behavior: ComparisonBehavior,
    pub exceptions: Exceptions,
    pub subnormals: SubnormalMode,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct IntegerConversionSemantics {
    pub rounding: Rounding,
    pub exceptions: Exceptions,
    pub subnormals: SubnormalMode,
    pub invalid: InvalidConversion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Semantics {
    Arithmetic(ArithmeticSemantics),
    Comparison(ComparisonSemantics),
    IntegerConversion(IntegerConversionSemantics),
}

impl ArithmeticSemantics {
    pub const fn strict(rounding: RoundingMode, exceptions: Exceptions) -> Self {
        Self {
            rounding: Rounding::Fixed(rounding),
            exceptions,
            nan: NaNPolicy::AnyQuiet,
            subnormals: SubnormalMode::Gradual,
            tininess: Tininess::AfterRounding,
        }
    }
}

impl Semantics {
    /// Resolved semantics used when an FP operation omits its attribute.
    pub fn default_for_operation(name: &str) -> Self {
        match name {
            "cmp" => Self::Comparison(ComparisonSemantics {
                behavior: ComparisonBehavior::Quiet,
                exceptions: Exceptions::Ignore,
                subnormals: SubnormalMode::Gradual,
            }),
            "to_si" | "to_ui" => Self::IntegerConversion(IntegerConversionSemantics {
                rounding: Rounding::Fixed(RoundingMode::TowardZero),
                exceptions: Exceptions::Ignore,
                subnormals: SubnormalMode::Gradual,
                invalid: InvalidConversion::Indeterminate,
            }),
            _ => Self::Arithmetic(ArithmeticSemantics::strict(
                RoundingMode::TiesToEven,
                Exceptions::Ignore,
            )),
        }
    }

    /// Parse full or default-eliding semantics for an FP operation.
    pub fn parse_for_operation(value: &AttributeValue, name: &str) -> Result<Self, Error> {
        let AttributeValue::Dict(fields) = value else {
            return Err(invalid("semantics must be a dictionary"));
        };
        let mut complete: BTreeMap<_, _> = Self::default_for_operation(name)
            .fields()
            .into_iter()
            .map(|(name, value)| (name.to_string(), AttributeValue::Str(value.into())))
            .collect();
        complete.extend(*fields.clone());
        Self::parse_attribute(&AttributeValue::Dict(Box::new(complete)))
    }

    pub fn parse_attribute(value: &AttributeValue) -> Result<Self, Error> {
        let AttributeValue::Dict(fields) = value else {
            return Err(invalid("semantics must be a dictionary"));
        };
        match string(fields, "kind")? {
            "arithmetic" => {
                exact_fields(
                    fields,
                    &[
                        "kind",
                        "rounding",
                        "exceptions",
                        "nan",
                        "subnormals",
                        "tininess",
                    ],
                )?;
                Ok(Self::Arithmetic(ArithmeticSemantics {
                    rounding: rounding(fields)?,
                    exceptions: exceptions(fields)?,
                    nan: match string(fields, "nan")? {
                        "any_quiet" => NaNPolicy::AnyQuiet,
                        "preserve_payload" => NaNPolicy::PreservePayload,
                        "canonical" => NaNPolicy::Canonical,
                        _ => return Err(invalid("unknown NaN policy")),
                    },
                    subnormals: subnormals(fields)?,
                    tininess: match string(fields, "tininess")? {
                        "before_rounding" => Tininess::BeforeRounding,
                        "after_rounding" => Tininess::AfterRounding,
                        _ => return Err(invalid("unknown tininess policy")),
                    },
                }))
            }
            "comparison" => {
                exact_fields(fields, &["kind", "behavior", "exceptions", "subnormals"])?;
                Ok(Self::Comparison(ComparisonSemantics {
                    behavior: match string(fields, "behavior")? {
                        "quiet" => ComparisonBehavior::Quiet,
                        "signaling" => ComparisonBehavior::Signaling,
                        _ => return Err(invalid("unknown comparison behavior")),
                    },
                    exceptions: exceptions(fields)?,
                    subnormals: subnormals(fields)?,
                }))
            }
            "to_integer" => {
                exact_fields(
                    fields,
                    &["kind", "rounding", "exceptions", "subnormals", "invalid"],
                )?;
                if string(fields, "invalid")? != "indeterminate" {
                    return Err(invalid("unknown invalid conversion policy"));
                }
                Ok(Self::IntegerConversion(IntegerConversionSemantics {
                    rounding: rounding(fields)?,
                    exceptions: exceptions(fields)?,
                    subnormals: subnormals(fields)?,
                    invalid: InvalidConversion::Indeterminate,
                }))
            }
            _ => Err(invalid("unknown floating semantics kind")),
        }
    }

    pub fn arithmetic(&self) -> Result<&ArithmeticSemantics, Error> {
        let Self::Arithmetic(value) = self else {
            return Err(invalid("operation requires arithmetic semantics"));
        };
        Ok(value)
    }

    pub fn comparison(&self) -> Result<&ComparisonSemantics, Error> {
        let Self::Comparison(value) = self else {
            return Err(invalid("operation requires comparison semantics"));
        };
        Ok(value)
    }

    pub fn integer_conversion(&self) -> Result<&IntegerConversionSemantics, Error> {
        let Self::IntegerConversion(value) = self else {
            return Err(invalid("operation requires integer-conversion semantics"));
        };
        Ok(value)
    }

    pub(crate) fn print(&self, fmt: &mut crate::IRFormatter<'_>) -> Result<(), std::fmt::Error> {
        let fields = self.nondefault_fields();
        fmt.write("{")?;
        for (index, (name, value)) in fields.iter().enumerate() {
            if index != 0 {
                fmt.write(", ")?;
            }
            fmt.write(format!("{name} = \"{value}\""))?;
        }
        fmt.write("}")
    }

    pub(crate) fn is_default(&self) -> bool {
        *self == self.defaults()
    }

    fn defaults(self) -> Self {
        let operation = match self {
            Self::Arithmetic(_) => "add",
            Self::Comparison(_) => "cmp",
            Self::IntegerConversion(_) => "to_si",
        };
        Self::default_for_operation(operation)
    }

    fn nondefault_fields(self) -> BTreeMap<&'static str, &'static str> {
        let defaults = self.defaults().fields();
        let mut fields = self.fields();
        fields.retain(|name, value| defaults.get(name) != Some(value));
        fields
    }

    fn fields(self) -> BTreeMap<&'static str, &'static str> {
        let mut fields = BTreeMap::new();
        match self {
            Self::Arithmetic(value) => {
                fields.insert("kind", "arithmetic");
                fields.insert("rounding", rounding_name(value.rounding));
                fields.insert("exceptions", exceptions_name(value.exceptions));
                fields.insert(
                    "nan",
                    match value.nan {
                        NaNPolicy::AnyQuiet => "any_quiet",
                        NaNPolicy::PreservePayload => "preserve_payload",
                        NaNPolicy::Canonical => "canonical",
                    },
                );
                fields.insert("subnormals", subnormal_name(value.subnormals));
                fields.insert(
                    "tininess",
                    match value.tininess {
                        Tininess::BeforeRounding => "before_rounding",
                        Tininess::AfterRounding => "after_rounding",
                    },
                );
            }
            Self::Comparison(value) => {
                fields.insert("kind", "comparison");
                fields.insert(
                    "behavior",
                    match value.behavior {
                        ComparisonBehavior::Quiet => "quiet",
                        ComparisonBehavior::Signaling => "signaling",
                    },
                );
                fields.insert("exceptions", exceptions_name(value.exceptions));
                fields.insert("subnormals", subnormal_name(value.subnormals));
            }
            Self::IntegerConversion(value) => {
                fields.insert("kind", "to_integer");
                fields.insert("rounding", rounding_name(value.rounding));
                fields.insert("exceptions", exceptions_name(value.exceptions));
                fields.insert("subnormals", subnormal_name(value.subnormals));
                fields.insert("invalid", "indeterminate");
            }
        }
        fields
    }
}

impl From<ArithmeticSemantics> for Semantics {
    fn from(value: ArithmeticSemantics) -> Self {
        Self::Arithmetic(value)
    }
}

fn exact_fields(fields: &BTreeMap<String, AttributeValue>, expected: &[&str]) -> Result<(), Error> {
    if fields.len() != expected.len() || expected.iter().any(|name| !fields.contains_key(*name)) {
        return Err(invalid("semantics fields do not match the selected kind"));
    }
    Ok(())
}

fn rounding(fields: &BTreeMap<String, AttributeValue>) -> Result<Rounding, Error> {
    Ok(match string(fields, "rounding")? {
        "nearest_even" => Rounding::Fixed(RoundingMode::TiesToEven),
        "toward_zero" => Rounding::Fixed(RoundingMode::TowardZero),
        "toward_negative" => Rounding::Fixed(RoundingMode::TowardNegative),
        "toward_positive" => Rounding::Fixed(RoundingMode::TowardPositive),
        "ties_away" => Rounding::Fixed(RoundingMode::TiesToAway),
        "dynamic" => Rounding::Dynamic,
        _ => return Err(invalid("unknown rounding policy")),
    })
}

fn exceptions(fields: &BTreeMap<String, AttributeValue>) -> Result<Exceptions, Error> {
    Ok(match string(fields, "exceptions")? {
        "ignore" => Exceptions::Ignore,
        "flags" => Exceptions::Flags,
        "flags_and_traps" => Exceptions::FlagsAndTraps,
        _ => return Err(invalid("unknown exception policy")),
    })
}

fn subnormals(fields: &BTreeMap<String, AttributeValue>) -> Result<SubnormalMode, Error> {
    Ok(match string(fields, "subnormals")? {
        "gradual" => SubnormalMode::Gradual,
        "flush_input" => SubnormalMode::FlushInput,
        "flush_output" => SubnormalMode::FlushOutput,
        "flush_both" => SubnormalMode::FlushBoth,
        _ => return Err(invalid("unknown subnormal policy")),
    })
}

fn string<'a>(fields: &'a BTreeMap<String, AttributeValue>, name: &str) -> Result<&'a str, Error> {
    fields
        .get(name)
        .and_then(AttributeValue::as_str)
        .ok_or_else(|| invalid(&format!("semantics field '{name}' must be a string")))
}

fn rounding_name(value: Rounding) -> &'static str {
    match value {
        Rounding::Fixed(RoundingMode::TiesToEven) => "nearest_even",
        Rounding::Fixed(RoundingMode::TowardZero) => "toward_zero",
        Rounding::Fixed(RoundingMode::TowardNegative) => "toward_negative",
        Rounding::Fixed(RoundingMode::TowardPositive) => "toward_positive",
        Rounding::Fixed(RoundingMode::TiesToAway) => "ties_away",
        Rounding::Dynamic => "dynamic",
    }
}

fn exceptions_name(value: Exceptions) -> &'static str {
    match value {
        Exceptions::Ignore => "ignore",
        Exceptions::Flags => "flags",
        Exceptions::FlagsAndTraps => "flags_and_traps",
    }
}

fn subnormal_name(value: SubnormalMode) -> &'static str {
    match value {
        SubnormalMode::Gradual => "gradual",
        SubnormalMode::FlushInput => "flush_input",
        SubnormalMode::FlushOutput => "flush_output",
        SubnormalMode::FlushBoth => "flush_both",
    }
}

fn invalid(message: &str) -> Error {
    Error::VerificationError(message.to_string())
}
