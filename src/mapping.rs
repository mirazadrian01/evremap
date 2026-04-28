use anyhow::Context;
pub use evdev_rs::enums::{EventCode, EventType, EV_KEY as KeyCode};
use serde::Deserialize;
use std::collections::HashSet;
use std::path::Path;
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct MappingConfig {
    pub device_name: Option<String>,
    pub phys: Option<String>,
    pub mappings: Vec<Mapping>,
}

impl MappingConfig {
    pub fn from_file<P: AsRef<Path>>(path: P) -> anyhow::Result<Self> {
        let path = path.as_ref();
        let toml_data = std::fs::read_to_string(path)
            .context(format!("reading toml from {}", path.display()))?;
        let config_file: ConfigFile =
            toml::from_str(&toml_data).context(format!("parsing toml from {}", path.display()))?;
        let mut mappings = vec![];
        for dual in config_file.dual_role {
            mappings.push(dual.into());
        }
        for remap in config_file.remap {
            mappings.push(remap.into());
        }
        for modifier_key in config_file.modifier_keys {
            mappings.push(modifier_key.into());
        }
        Ok(Self {
            device_name: config_file.device_name,
            phys: config_file.phys,
            mappings,
        })
    }
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum Mapping {
    DualRole {
        input: KeyCode,
        hold: Vec<KeyCode>,
        tap: Vec<KeyCode>,
    },
    Remap {
        input: HashSet<KeyCode>,
        output: HashSet<KeyCode>,
    },
    ModifierKey {
        keys: HashSet<KeyCode>,
    }
}

#[derive(Debug, Deserialize)]
#[serde(try_from = "String")]
struct KeyCodeWrapper {
    pub code: KeyCode,
}

impl Into<KeyCode> for KeyCodeWrapper {
    fn into(self) -> KeyCode {
        self.code
    }
}

#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Invalid key `{0}`.  Use `evremap list-keys` to see possible keys.")]
    InvalidKey(String),
    #[error("Impossible: parsed KEY_XXX but not into an EV_KEY")]
    ImpossibleParseKey,
}

impl std::convert::TryFrom<String> for KeyCodeWrapper {
    type Error = ConfigError;
    fn try_from(s: String) -> Result<KeyCodeWrapper, Self::Error> {
        match EventCode::from_str(&EventType::EV_KEY, &s) {
            Some(code) => match code {
                EventCode::EV_KEY(code) => Ok(KeyCodeWrapper { code }),
                _ => Err(ConfigError::ImpossibleParseKey),
            },
            None => Err(ConfigError::InvalidKey(s)),
        }
    }
}

#[derive(Debug, Deserialize)]
struct DualRoleConfig {
    input: KeyCodeWrapper,
    hold: Vec<KeyCodeWrapper>,
    tap: Vec<KeyCodeWrapper>,
}

impl Into<Mapping> for DualRoleConfig {
    fn into(self) -> Mapping {
        Mapping::DualRole {
            input: self.input.into(),
            hold: self.hold.into_iter().map(Into::into).collect(),
            tap: self.tap.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct RemapConfig {
    input: Vec<KeyCodeWrapper>,
    output: Vec<KeyCodeWrapper>,
}

impl Into<Mapping> for RemapConfig {
    fn into(self) -> Mapping {
        Mapping::Remap {
            input: self.input.into_iter().map(Into::into).collect(),
            output: self.output.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ModifierKeyConfig {
    keys: Vec<KeyCodeWrapper>,
} 

impl Into<Mapping> for ModifierKeyConfig {
    fn into(self) -> Mapping {
        Mapping::ModifierKey { 
            keys: self.keys.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(Debug, Deserialize)]
struct ConfigFile {
    #[serde(default)]
    device_name: Option<String>,

    #[serde(default)]
    phys: Option<String>,

    #[serde(default)]
    dual_role: Vec<DualRoleConfig>,

    #[serde(default)]
    remap: Vec<RemapConfig>,

    #[serde(default)]
    modifier_keys: Vec<ModifierKeyConfig>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_keys_initialization() {
        let toml_data = "[[modifier_keys]]
            keys = [\"KEY_FN\", \"KEY_LEFTCTRL\"]";
        let config_file: ConfigFile =
            toml::from_str(&toml_data).unwrap();
        let mut mappings: Vec<Mapping> = vec![];
        for dual in config_file.dual_role {
            mappings.push(dual.into());
        }
        for remap in config_file.remap {
            mappings.push(remap.into());
        }
        for modifier_key in config_file.modifier_keys {
            mappings.push(modifier_key.into());
        }

        assert!(mappings.contains(&Mapping::ModifierKey { 
            keys: HashSet::from([KeyCode::KEY_FN, KeyCode::KEY_LEFTCTRL]) 
        }));
    }
}
