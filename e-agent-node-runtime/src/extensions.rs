use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExtensionPolicyMode {
    Strict,
    Prompt,
    Permissive,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ExtensionOverride {
    pub mode: Option<ExtensionPolicyMode>,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecMediationPolicy {
    pub enabled: bool,
    pub deny_patterns: Vec<String>,
    pub allow_patterns: Vec<String>,
}

impl Default for ExecMediationPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            deny_patterns: Vec::new(),
            allow_patterns: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DangerousCommandClass {
    PolicyMatch,
}

impl DangerousCommandClass {
    pub const fn label(self) -> &'static str {
        "policy_match"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecMediationResult {
    Allow,
    AllowWithAudit {
        class: DangerousCommandClass,
        reason: String,
    },
    Deny {
        class: Option<DangerousCommandClass>,
        reason: String,
    },
}

pub fn evaluate_exec_mediation(
    policy: &ExecMediationPolicy,
    cmd: &str,
    args: &[String],
) -> ExecMediationResult {
    if !policy.enabled {
        return ExecMediationResult::Allow;
    }
    let command = std::iter::once(cmd)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if policy
        .allow_patterns
        .iter()
        .any(|p| command.starts_with(&p.to_ascii_lowercase()))
    {
        return ExecMediationResult::Allow;
    }
    if let Some(pattern) = policy
        .deny_patterns
        .iter()
        .find(|p| command.starts_with(&p.to_ascii_lowercase()))
    {
        return ExecMediationResult::Deny {
            class: Some(DangerousCommandClass::PolicyMatch),
            reason: format!("Command matches deny pattern: {pattern}"),
        };
    }
    ExecMediationResult::Allow
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecretBrokerPolicy {
    pub enabled: bool,
    pub disclosure_allowlist: Vec<String>,
}

impl Default for SecretBrokerPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            disclosure_allowlist: Vec::new(),
        }
    }
}

impl SecretBrokerPolicy {
    pub fn is_secret(&self, name: &str) -> bool {
        if !self.enabled
            || self
                .disclosure_allowlist
                .iter()
                .any(|item| item.eq_ignore_ascii_case(name))
        {
            return false;
        }
        let name = name.to_ascii_uppercase();
        [
            "_KEY",
            "_SECRET",
            "_TOKEN",
            "_PASSWORD",
            "_PASSWD",
            "_CREDENTIAL",
            "_CREDENTIALS",
            "_AUTH",
            "DATABASE_URL",
            "REDIS_URL",
        ]
        .iter()
        .any(|pattern| name.ends_with(pattern))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtensionPolicy {
    pub mode: ExtensionPolicyMode,
    pub default_caps: Vec<String>,
    pub deny_caps: Vec<String>,
    pub per_extension: HashMap<String, ExtensionOverride>,
    pub exec_mediation: ExecMediationPolicy,
    pub secret_broker: SecretBrokerPolicy,
}

impl Default for ExtensionPolicy {
    fn default() -> Self {
        Self {
            mode: ExtensionPolicyMode::Prompt,
            default_caps: vec![
                "read".into(),
                "write".into(),
                "http".into(),
                "events".into(),
                "session".into(),
            ],
            deny_caps: vec!["exec".into(), "env".into()],
            per_extension: HashMap::new(),
            exec_mediation: ExecMediationPolicy::default(),
            secret_broker: SecretBrokerPolicy::default(),
        }
    }
}

pub fn hostcall_params_hash(method: &str, params: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(method.as_bytes());
    hasher.update(serde_json::to_vec(params).unwrap_or_default());
    format!("{:x}", hasher.finalize())
}

pub fn safe_canonicalize(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .map(strip_unc_prefix)
        .unwrap_or_else(|_| {
            let path = if path.is_absolute() {
                path.to_path_buf()
            } else {
                std::env::current_dir().unwrap_or_default().join(path)
            };
            let mut normalized = PathBuf::new();
            for component in path.components() {
                match component {
                    Component::CurDir => {}
                    Component::ParentDir => {
                        normalized.pop();
                    }
                    other => normalized.push(other.as_os_str()),
                }
            }
            strip_unc_prefix(normalized)
        })
}

pub fn strip_unc_prefix(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let value = path.to_string_lossy();
        if let Some(value) = value.strip_prefix(r"\\?\") {
            return PathBuf::from(value);
        }
        if let Some(value) = value.strip_prefix("//?/") {
            return PathBuf::from(value);
        }
    }
    path
}
