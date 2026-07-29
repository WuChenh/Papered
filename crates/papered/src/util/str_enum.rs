pub use strum::{AsRefStr, Display, EnumString, IntoStaticStr};

/// Extension trait: returns the canonical string label for the variant.
/// Blanket-implemented for all types that derive `strum::IntoStaticStr`.
pub trait StrLabel {
    fn as_str(&self) -> &str;
}

impl<T> StrLabel for T
where
    for<'a> &'a T: Into<&'static str>,
{
    fn as_str(&self) -> &str {
        self.into()
    }
}
