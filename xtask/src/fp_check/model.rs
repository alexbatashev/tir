use std::collections::BTreeSet;

use serde::{Deserialize, Deserializer, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub reference: ReferenceProfile,
    #[serde(default)]
    pub inventory: Vec<CoverageItem>,
    pub cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoverageItem {
    pub id: String,
    pub stage: Stage,
    pub area: String,
    pub status: CoverageStatus,
    pub requirements: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Unimplemented,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceProfile {
    pub profile: String,
    pub compiler_version: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub id: String,
    pub stage: Stage,
    pub source: String,
    pub language_mode: String,
    pub target_requirements: Vec<String>,
    pub compiler_args: Vec<String>,
    pub runtime_input_bits: Vec<String>,
    #[serde(default)]
    pub run_args: Vec<String>,
    pub probe: Probe,
    pub expectation: Expectation,
    pub reference_expectation: Option<Expectation>,
    pub oracle: Oracle,
}

impl Case {
    pub fn oracle_detail(&self) -> String {
        format!(
            "matches {} {} {}",
            self.oracle.kind, self.oracle.identity, self.oracle.version
        )
    }
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Probe {
    Execute,
    Assembly,
    CompileDiagnostic,
    ManifestOnly,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Oracle {
    pub kind: String,
    pub identity: String,
    pub version: String,
    pub reference: String,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Expectation {
    ExactBits {
        bits: String,
        #[serde(default)]
        flags: Vec<String>,
    },
    PermittedSet {
        values: Vec<String>,
    },
    CorrelatedResults {
        results: Vec<String>,
    },
    NumericalBound {
        max_error: String,
        metric: String,
        domain: String,
        zero_convention: String,
        subnormal_convention: String,
        exceptional_values: String,
    },
    CodeShape {
        #[serde(default)]
        required: Vec<String>,
        #[serde(default)]
        forbidden: Vec<String>,
    },
    Effects(#[serde(deserialize_with = "deserialize_expected_effects")] Effects),
    Diagnostic {
        contains: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
    #[serde(default)]
    pub manifest_digest: String,
    pub profile: String,
    pub generated_at_unix_seconds: u64,
    pub host: HostIdentity,
    pub results: Vec<CaseResult>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostIdentity {
    pub target: String,
    pub library: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseResult {
    pub case_id: String,
    pub stage: Stage,
    pub compiler: CompilerIdentity,
    pub source_digest: String,
    pub commands: Vec<Vec<String>>,
    pub exit_status: Option<i32>,
    pub observation: Option<Observation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_policy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<String>,
    pub status: Status,
    pub detail: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerIdentity {
    pub version: String,
    pub executable: String,
}

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Reference,
    Scalar,
    Round,
    Fcc,
    Math,
    Rules,
    Vector,
    Release,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Reference => "reference",
            Self::Scalar => "scalar",
            Self::Round => "round",
            Self::Fcc => "fcc",
            Self::Math => "math",
            Self::Rules => "rules",
            Self::Vector => "vector",
            Self::Release => "release",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Observation {
    ExactBits { bits: String, flags: Vec<String> },
    PermittedSet { value: String },
    CorrelatedResults { results: Vec<String> },
    NumericalBound { value: String, error: String },
    CodeShape { instructions: Vec<String> },
    Effects(Effects),
    Diagnostic { message: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResultBits {
    Scalar(String),
    Vector(Vec<String>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Effects {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_bits: Option<ResultBits>,
    pub flags: Vec<String>,
    pub errno: Option<i32>,
    pub events: Vec<String>,
    #[serde(default)]
    pub trapped: bool,
}

impl Effects {
    fn compare(&self, observed: &Self) -> Result<(), String> {
        if observed.result_bits != self.result_bits {
            return Err(format!(
                "expected result bits {:?}, observed {:?}",
                self.result_bits, observed.result_bits
            ));
        }
        compare_flags(&self.flags, &observed.flags)?;
        if observed.errno != self.errno {
            return Err(format!(
                "expected errno {:?}, observed {:?}",
                self.errno, observed.errno
            ));
        }
        if observed.trapped != self.trapped {
            return Err(format!(
                "expected trapped={}, observed trapped={}",
                self.trapped, observed.trapped
            ));
        }
        if observed.events != self.events {
            return Err(format!(
                "expected events {:?}, observed {:?}",
                self.events, observed.events
            ));
        }
        Ok(())
    }
}

fn deserialize_expected_effects<'de, D>(deserializer: D) -> Result<Effects, D::Error>
where
    D: Deserializer<'de>,
{
    const FIELDS: &[&str] = &["result_bits", "flags", "errno", "events", "trapped"];
    let mut fields = serde_json::Map::<String, serde_json::Value>::deserialize(deserializer)?;
    if let Some(field) = fields
        .keys()
        .find(|field| !FIELDS.contains(&field.as_str()))
    {
        return Err(serde::de::Error::unknown_field(field, FIELDS));
    }
    fields
        .entry("flags")
        .or_insert_with(|| serde_json::json!([]));
    fields
        .entry("events")
        .or_insert_with(|| serde_json::json!([]));
    serde_json::from_value(serde_json::Value::Object(fields)).map_err(serde::de::Error::custom)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    UnsupportedCapability,
    MissingInfrastructure,
}

impl Expectation {
    pub fn compare(&self, observation: Option<&Observation>) -> Result<(), String> {
        let observation = observation.ok_or_else(|| "missing observation".to_string())?;
        match (self, observation) {
            (
                Self::ExactBits {
                    bits: expected_bits,
                    flags: expected_flags,
                },
                Observation::ExactBits { bits, flags },
            ) => {
                if bits != expected_bits {
                    return Err(format!("expected bits {expected_bits}, observed {bits}"));
                }
                compare_flags(expected_flags, flags)
            }
            (Self::PermittedSet { values }, Observation::PermittedSet { value }) => {
                if values.contains(value) {
                    Ok(())
                } else {
                    Err(format!("value {value} is not in the permitted set"))
                }
            }
            (
                Self::CorrelatedResults { results: expected },
                Observation::CorrelatedResults { results: observed },
            ) => {
                if observed == expected {
                    Ok(())
                } else {
                    Err(format!(
                        "expected correlated results {expected:?}, observed {observed:?}"
                    ))
                }
            }
            (
                Self::NumericalBound {
                    max_error,
                    metric,
                    domain,
                    zero_convention,
                    subnormal_convention,
                    exceptional_values,
                },
                Observation::NumericalBound { error, .. },
            ) => {
                if [
                    metric,
                    domain,
                    zero_convention,
                    subnormal_convention,
                    exceptional_values,
                ]
                .iter()
                .any(|field| field.is_empty())
                {
                    return Err("numerical bound has an empty contract field".into());
                }
                let maximum = max_error
                    .parse::<f64>()
                    .map_err(|_| format!("invalid expected error bound {max_error}"))?;
                let observed = error
                    .parse::<f64>()
                    .map_err(|_| format!("invalid observed error {error}"))?;
                if observed <= maximum {
                    Ok(())
                } else {
                    Err(format!("error {error} exceeds {max_error}"))
                }
            }
            (
                Self::CodeShape {
                    required,
                    forbidden,
                },
                Observation::CodeShape { instructions },
            ) => {
                let missing = required
                    .iter()
                    .find(|required| !instructions.iter().any(|line| line.contains(*required)));
                if let Some(missing) = missing {
                    return Err(format!("required instruction {missing} is absent"));
                }
                if let Some(forbidden) = forbidden
                    .iter()
                    .find(|forbidden| instructions.iter().any(|line| line.contains(*forbidden)))
                {
                    return Err(format!("forbidden instruction {forbidden} is present"));
                }
                Ok(())
            }
            (Self::Effects(expected), Observation::Effects(observed)) => expected.compare(observed),
            (Self::Diagnostic { contains }, Observation::Diagnostic { message }) => {
                if message.contains(contains) {
                    Ok(())
                } else {
                    Err(format!("diagnostic does not contain {contains:?}"))
                }
            }
            _ => Err("observation kind does not match expectation kind".into()),
        }
    }
}

fn compare_flags(expected: &[String], observed: &[String]) -> Result<(), String> {
    let expected = expected.iter().collect::<BTreeSet<_>>();
    let observed = observed.iter().collect::<BTreeSet<_>>();
    if expected == observed {
        return Ok(());
    }
    let missing = expected.difference(&observed).copied().collect::<Vec<_>>();
    let unexpected = observed.difference(&expected).copied().collect::<Vec<_>>();
    Err(format!(
        "missing flags {missing:?}; unexpected flags {unexpected:?}"
    ))
}
