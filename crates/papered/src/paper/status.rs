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
#[serde(rename_all = "snake_case")]
#[strum(serialize_all = "snake_case")]
pub enum PaperStatus {
    #[default]
    Indexed,
    Processing,
    Failed,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_display_from_str_round_trip() {
        for value in [
            PaperStatus::Indexed,
            PaperStatus::Processing,
            PaperStatus::Failed,
        ] {
            let label: &str = value.into();
            assert_eq!(label.parse::<PaperStatus>().unwrap(), value);
            assert_eq!(value.to_string(), label);
        }
    }
}
