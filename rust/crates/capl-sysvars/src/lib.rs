//! System variables are shared application state with a stable, namespaced
//! identity. The manager is UI- and runtime-independent so native and WASM
//! frontends can expose the same CAPL-facing model.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum SysVarValue {
    Integer(i64),
    Unsigned(u64),
    Float(f64),
    Boolean(bool),
    Text(String),
    Bytes(Vec<u8>),
}

impl Default for SysVarValue {
    fn default() -> Self {
        Self::Integer(0)
    }
}

impl SysVarValue {
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(v) => Some(*v),
            Self::Unsigned(v) => i64::try_from(*v).ok(),
            Self::Float(v) => Some(*v as i64),
            Self::Boolean(v) => Some(i64::from(*v)),
            _ => None,
        }
    }
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(v) => Some(*v as f64),
            Self::Unsigned(v) => Some(*v as f64),
            Self::Float(v) => Some(*v),
            Self::Boolean(v) => Some(f64::from(*v)),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SysVarKey {
    pub namespace: String,
    pub name: String,
}

impl SysVarKey {
    pub fn new(namespace: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            namespace: namespace.into(),
            name: name.into(),
        }
    }
    pub fn parse(path: &str) -> Self {
        path.split_once("::").map_or_else(
            || Self::new("", path),
            |(namespace, name)| Self::new(namespace, name),
        )
    }
    pub fn path(&self) -> String {
        if self.namespace.is_empty() {
            self.name.clone()
        } else {
            format!("{}::{}", self.namespace, self.name)
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SysVar {
    pub key: SysVarKey,
    pub value: SysVarValue,
    pub writable: bool,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SysVarChange {
    pub key: SysVarKey,
    pub old: Option<SysVarValue>,
    pub new: SysVarValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SysVarError {
    Missing(SysVarKey),
    ReadOnly(SysVarKey),
}
impl fmt::Display for SysVarError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(k) => write!(f, "unknown system variable {}", k.path()),
            Self::ReadOnly(k) => write!(f, "system variable {} is read-only", k.path()),
        }
    }
}
impl std::error::Error for SysVarError {}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct SysVarStore {
    vars: BTreeMap<SysVarKey, SysVar>,
}

impl SysVarStore {
    pub fn declare(
        &mut self,
        key: SysVarKey,
        value: SysVarValue,
        writable: bool,
        description: Option<String>,
    ) {
        self.vars.insert(
            key.clone(),
            SysVar {
                key,
                value,
                writable,
                description,
            },
        );
    }
    pub fn get(&self, key: &SysVarKey) -> Option<&SysVar> {
        self.vars.get(key)
    }
    pub fn get_path(&self, path: &str) -> Option<&SysVar> {
        self.get(&SysVarKey::parse(path))
    }
    pub fn iter(&self) -> impl Iterator<Item = &SysVar> {
        self.vars.values()
    }
    pub fn set(
        &mut self,
        key: &SysVarKey,
        value: SysVarValue,
    ) -> Result<SysVarChange, SysVarError> {
        let var = self
            .vars
            .get_mut(key)
            .ok_or_else(|| SysVarError::Missing(key.clone()))?;
        if !var.writable {
            return Err(SysVarError::ReadOnly(key.clone()));
        }
        let old = std::mem::replace(&mut var.value, value.clone());
        Ok(SysVarChange {
            key: key.clone(),
            old: Some(old),
            new: value,
        })
    }
    pub fn set_path(
        &mut self,
        path: &str,
        value: SysVarValue,
    ) -> Result<SysVarChange, SysVarError> {
        self.set(&SysVarKey::parse(path), value)
    }
    pub fn snapshot(&self) -> BTreeMap<SysVarKey, SysVarValue> {
        self.vars
            .iter()
            .map(|(key, var)| (key.clone(), var.value.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespaced_values_change_with_a_previous_value() {
        let key = SysVarKey::new("Powertrain", "Enable");
        let mut store = SysVarStore::default();
        store.declare(key.clone(), SysVarValue::Boolean(false), true, None);
        let change = store.set(&key, SysVarValue::Boolean(true)).unwrap();
        assert_eq!(change.old, Some(SysVarValue::Boolean(false)));
        assert_eq!(store.get(&key).unwrap().value, SysVarValue::Boolean(true));
    }
    #[test]
    fn read_only_variables_are_rejected() {
        let key = SysVarKey::parse("Diag::Counter");
        let mut store = SysVarStore::default();
        store.declare(key.clone(), SysVarValue::Unsigned(1), false, None);
        assert!(matches!(
            store.set_path("Diag::Counter", SysVarValue::Unsigned(2)),
            Err(SysVarError::ReadOnly(_))
        ));
    }
}
