use serde::{Deserialize, Serialize};
use strum::{Display, EnumString, IntoStaticStr};

#[derive(
    Debug,
    Default,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Serialize,
    Deserialize,
    EnumString,
    Display,
    IntoStaticStr,
)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case", ascii_case_insensitive)]
pub enum SearchMethod {
    Semantic,
    Fulltext,
    #[default]
    Hybrid,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_to_snake_case() {
        assert_eq!(
            serde_json::to_string(&SearchMethod::Semantic).unwrap(),
            "\"semantic\""
        );
        assert_eq!(
            serde_json::to_string(&SearchMethod::Fulltext).unwrap(),
            "\"fulltext\""
        );
        assert_eq!(
            serde_json::to_string(&SearchMethod::Hybrid).unwrap(),
            "\"hybrid\""
        );
    }

    #[test]
    fn round_trips() {
        for method in [
            SearchMethod::Semantic,
            SearchMethod::Fulltext,
            SearchMethod::Hybrid,
        ] {
            let json = serde_json::to_string(&method).unwrap();
            let parsed: SearchMethod = serde_json::from_str(&json).unwrap();
            assert_eq!(parsed, method);
        }
    }

    #[test]
    fn rejects_unknown_variant() {
        let result: std::result::Result<SearchMethod, _> = serde_json::from_str("\"unknown\"");
        assert!(result.is_err());
    }

    #[test]
    fn from_str_is_case_insensitive_and_rejects_unknown() {
        assert!("Hybrid".parse::<SearchMethod>().is_ok());
        assert!("bogus".parse::<SearchMethod>().is_err());
    }

    #[test]
    fn default_is_hybrid() {
        assert_eq!(SearchMethod::default(), SearchMethod::Hybrid);
    }

    #[test]
    fn as_str_display_from_str_round_trip() {
        for value in [
            SearchMethod::Semantic,
            SearchMethod::Fulltext,
            SearchMethod::Hybrid,
        ] {
            let label: &str = value.into();
            assert_eq!(label.parse::<SearchMethod>().unwrap(), value);
            assert_eq!(value.to_string(), label);
        }
    }
}
