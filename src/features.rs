use std::process::Command;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CargoFeatures {
    pub all_features: bool,
    pub no_default_features: bool,
    pub features: Vec<String>,
}

impl CargoFeatures {
    pub fn validate(&self) -> Result<(), String> {
        if self.all_features && (self.no_default_features || !self.features.is_empty()) {
            return Err(
                "--all-features cannot be combined with --features or --no-default-features".into(),
            );
        }
        for feature in &self.features {
            if feature.is_empty()
                || !feature
                    .chars()
                    .all(|value| value.is_alphanumeric() || matches!(value, '_' | '-' | '+' | '.' | '/'))
            {
                return Err(format!("invalid Cargo feature name: {feature}"));
            }
        }
        Ok(())
    }

    pub fn cargo_arguments(&self) -> Vec<String> {
        let mut output = Vec::new();
        if self.all_features {
            output.push("--all-features".into());
            return output;
        }
        if self.no_default_features {
            output.push("--no-default-features".into());
        }
        if !self.features.is_empty() {
            output.push("--features".into());
            output.push(self.features.join(","));
        }
        output
    }

    pub(crate) fn apply_to_command(&self, command: &mut Command) {
        command.args(self.cargo_arguments());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_features_add_no_cargo_flags() {
        assert!(CargoFeatures::default().cargo_arguments().is_empty());
    }

    #[test]
    fn selected_features_can_disable_defaults() {
        let value = CargoFeatures {
            no_default_features: true,
            features: vec!["alpha".into(), "dep/beta".into()],
            ..CargoFeatures::default()
        };
        assert_eq!(
            value.cargo_arguments(),
            vec!["--no-default-features", "--features", "alpha,dep/beta"]
        );
        assert!(value.validate().is_ok());
    }

    #[test]
    fn all_features_rejects_conflicting_selection() {
        let value = CargoFeatures {
            all_features: true,
            features: vec!["alpha".into()],
            ..CargoFeatures::default()
        };
        assert!(value.validate().is_err());
    }

    #[test]
    fn shell_metacharacters_are_rejected() {
        let value = CargoFeatures {
            features: vec!["x;touch-pwned".into()],
            ..CargoFeatures::default()
        };
        assert!(value.validate().is_err());
    }
}
