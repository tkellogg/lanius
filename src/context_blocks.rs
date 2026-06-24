//! Context block primitives (docs/context.md increment 6).
//!
//! These types are the shared schema for context components. The current prompt
//! path still assembles the legacy `context::Doc`; this module is the substrate
//! for moving context assembly toward named, hashable blocks with durable build
//! logs.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    System,
    BeforeMessages,
    AfterMessages,
    User,
    Scratch,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    Global,
    Agent,
    Session,
    Run,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextBlock {
    pub name: String,
    pub content: String,
    pub placement: Placement,
    pub priority: i32,
    pub owner: String,
    pub scope: Scope,
    #[serde(default)]
    pub package: Option<String>,
    #[serde(default)]
    pub meta: serde_json::Value,
}

impl ContextBlock {
    pub fn new(
        name: impl Into<String>,
        content: impl Into<String>,
        owner: impl Into<String>,
    ) -> Self {
        ContextBlock {
            name: name.into(),
            content: content.into(),
            placement: Placement::System,
            priority: 0,
            owner: owner.into(),
            scope: Scope::Agent,
            package: None,
            meta: serde_json::Value::Object(Default::default()),
        }
    }

    pub fn content_sha256(&self) -> String {
        sha256_hex(self.content.as_bytes())
    }

    pub fn validate(&self) -> Result<()> {
        validate_name("context block", &self.name)?;
        validate_name("context block owner", &self.owner)?;
        if let Some(package) = &self.package {
            validate_name("context block package", package)?;
        }
        Ok(())
    }
}

// Substrate landed ahead of use (see module docstring): the named-register
// component is not yet wired into the assembly/build-log path.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Register {
    pub name: String,
    pub value: serde_json::Value,
    pub owner: String,
    pub scope: Scope,
    #[serde(default)]
    pub package: Option<String>,
}

impl Register {
    #[allow(dead_code)] // kept with the Register substrate above
    pub fn validate(&self) -> Result<()> {
        validate_name("register", &self.name)?;
        validate_name("register owner", &self.owner)?;
        if let Some(package) = &self.package {
            validate_name("register package", package)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BuildAction {
    Add,
    Remove,
    Rewrite,
    Drop,
    Move,
    Validate,
}

// Substrate landed ahead of use (see module docstring): the durable build-log
// record schema is not yet emitted by the assembly path.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BuildLogRecord {
    pub session_id: String,
    pub run_id: Option<String>,
    pub profile: String,
    pub agent: String,
    pub component: String,
    pub action: BuildAction,
    #[serde(default)]
    pub block_name: Option<String>,
    #[serde(default)]
    pub before_sha256: Option<String>,
    #[serde(default)]
    pub after_sha256: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub meta: serde_json::Value,
}

impl BuildLogRecord {
    #[allow(dead_code)] // kept with the BuildLogRecord substrate above
    pub fn validate(&self) -> Result<()> {
        validate_name("build-log component", &self.component)?;
        if let Some(block_name) = &self.block_name {
            validate_name("build-log block", block_name)?;
        }
        Ok(())
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn validate_name(kind: &str, name: &str) -> Result<()> {
    if name.trim().is_empty() || name.contains('/') || name.contains(char::is_whitespace) {
        bail!("{kind} name {name:?} must be non-empty with no whitespace or slash");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_hash_is_content_only() {
        let mut a = ContextBlock::new("identity", "same", "main");
        let mut b = ContextBlock::new("identity", "same", "other");
        b.priority = 99;
        assert_eq!(a.content_sha256(), b.content_sha256());
        a.content.push('!');
        assert_ne!(a.content_sha256(), b.content_sha256());
    }

    #[test]
    fn names_are_strict() {
        assert!(ContextBlock::new("good-name", "x", "main")
            .validate()
            .is_ok());
        assert!(ContextBlock::new("bad name", "x", "main")
            .validate()
            .is_err());
        assert!(ContextBlock::new("bad/name", "x", "main")
            .validate()
            .is_err());
    }
}
