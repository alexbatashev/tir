use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    pub schema_version: u32,
    pub profile: String,
    pub generated_at_unix_seconds: u64,
    pub host: HostIdentity,
    pub results: Vec<CaseResult>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HostIdentity {
    pub target: String,
    pub library: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CaseResult {
    pub case_id: String,
    pub stage: Stage,
    pub compiler: CompilerIdentity,
    pub source_digest: String,
    pub commands: Vec<Vec<String>>,
    pub exit_status: Option<i32>,
    pub observation: Option<Observation>,
    pub status: Status,
    pub detail: String,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CompilerIdentity {
    pub version: String,
    pub executable: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Observation {
    ExactBits {
        bits: String,
        flags: Vec<String>,
    },
    PermittedSet {
        values: Vec<String>,
    },
    NumericalBound {
        value: String,
        error: String,
    },
    CodeShape {
        instructions: Vec<String>,
    },
    Effects {
        flags: Vec<String>,
        errno: Option<i32>,
        events: Vec<String>,
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
