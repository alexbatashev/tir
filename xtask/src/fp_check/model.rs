use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub reference: ReferenceProfile,
    pub cases: Vec<Case>,
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
    Effects {
        #[serde(default)]
        result_bits: Option<String>,
        #[serde(default)]
        flags: Vec<String>,
        errno: Option<i32>,
        #[serde(default)]
        events: Vec<String>,
        #[serde(default)]
        trapped: bool,
    },
    Diagnostic {
        contains: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Observation {
    ExactBits {
        bits: String,
        flags: Vec<String>,
    },
    PermittedSet {
        value: String,
    },
    NumericalBound {
        value: String,
        error: String,
    },
    CodeShape {
        instructions: Vec<String>,
    },
    Effects {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result_bits: Option<String>,
        flags: Vec<String>,
        errno: Option<i32>,
        events: Vec<String>,
        #[serde(default)]
        trapped: bool,
    },
    Diagnostic {
        message: String,
    },
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
            (
                Self::Effects {
                    result_bits: expected_bits,
                    flags: expected_flags,
                    errno: expected_errno,
                    events: expected_events,
                    trapped: expected_trap,
                },
                Observation::Effects {
                    result_bits,
                    flags,
                    errno,
                    events,
                    trapped,
                },
            ) => {
                if result_bits != expected_bits {
                    return Err(format!(
                        "expected result bits {expected_bits:?}, observed {result_bits:?}"
                    ));
                }
                compare_flags(expected_flags, flags)?;
                if errno != expected_errno {
                    return Err(format!(
                        "expected errno {expected_errno:?}, observed {errno:?}"
                    ));
                }
                if trapped != expected_trap {
                    return Err(format!(
                        "expected trapped={expected_trap}, observed trapped={trapped}"
                    ));
                }
                if events != expected_events {
                    return Err(format!(
                        "expected events {expected_events:?}, observed {events:?}"
                    ));
                }
                Ok(())
            }
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
