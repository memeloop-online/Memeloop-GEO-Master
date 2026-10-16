//! Host-scoped operator appearance, independent of customer project brands.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{AppError, Operator};

pub const DEFAULT_PRIMARY_COLOR: &str = "#2563EB";
pub const DEFAULT_LOCALE: &str = "zh-CN";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct OperatorAppearance {
    pub display_name: String,
    pub logo_url: Option<String>,
    pub primary_color: String,
    pub default_locale: String,
    pub revision: i64,
}

impl OperatorAppearance {
    pub fn for_operator(operator: &Operator) -> Self {
        Self {
            display_name: operator.display_name.clone(),
            logo_url: None,
            primary_color: DEFAULT_PRIMARY_COLOR.to_owned(),
            default_locale: DEFAULT_LOCALE.to_owned(),
            revision: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateOperatorAppearance {
    pub display_name: String,
    pub primary_color: String,
    pub default_locale: String,
}

impl UpdateOperatorAppearance {
    pub fn validate(mut self) -> Result<Self, AppError> {
        self.display_name = self.display_name.trim().to_owned();
        if self.display_name.is_empty()
            || self.display_name.chars().count() > 200
            || self.display_name.chars().any(char::is_control)
        {
            return Err(AppError::invalid_request(
                "display_name must contain 1–200 printable characters",
            ));
        }
        if !self.primary_color.starts_with('#')
            || self.primary_color.len() != 7
            || !self.primary_color[1..]
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(AppError::invalid_request(
                "primary_color must be a six-digit hexadecimal color",
            ));
        }
        self.primary_color.make_ascii_uppercase();
        if !matches!(self.default_locale.as_str(), "zh-CN" | "en") {
            return Err(AppError::invalid_request(
                "default_locale must be zh-CN or en",
            ));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn defaults_use_operator_not_project_or_product() {
        let operator = Operator::new(Uuid::new_v4().into(), "local", "Example Operator").unwrap();
        let appearance = OperatorAppearance::for_operator(&operator);
        assert_eq!(appearance.display_name, "Example Operator");
        assert_eq!(appearance.revision, 1);
        assert_eq!(appearance.logo_url, None);
    }

    #[test]
    fn validates_color_locale_and_name() {
        let input = UpdateOperatorAppearance {
            display_name: "  Example Operator  ".to_owned(),
            primary_color: "#abc123".to_owned(),
            default_locale: "en".to_owned(),
        };
        let update = input.validate().unwrap();
        assert_eq!(update.display_name, "Example Operator");
        assert_eq!(update.primary_color, "#ABC123");
        for color in ["red", "#12345G", "#12345600", "url(foo)"] {
            assert!(
                UpdateOperatorAppearance {
                    primary_color: color.to_owned(),
                    ..update.clone()
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            UpdateOperatorAppearance {
                default_locale: "fr".to_owned(),
                ..update.clone()
            }
            .validate()
            .is_err()
        );
        assert!(
            UpdateOperatorAppearance {
                display_name: "Bad\nName".to_owned(),
                ..update
            }
            .validate()
            .is_err()
        );
    }
}
