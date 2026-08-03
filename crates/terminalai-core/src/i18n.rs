//! Shared Fluent catalog loading for the control plane.
//!
//! The source is intentionally the same `.ftl` file imported by the web
//! renderer. Rust loads it at compile time so a malformed catalog fails in the
//! daemon's startup path and both sides format the same message identifiers.

use fluent_bundle::concurrent::FluentBundle;
use fluent_bundle::{FluentArgs, FluentResource};
use unic_langid::LanguageIdentifier;

pub const DEFAULT_LOCALE: &str = "en-US";
pub const CATALOG_SOURCE: &str = include_str!("../../../web/src/i18n/terminalai.ftl");

#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("the shared Fluent catalog could not be parsed: {0}")]
    Parse(String),
    #[error("the shared Fluent catalog could not be registered: {0}")]
    Register(String),
}

pub struct Catalog {
    bundle: FluentBundle<FluentResource>,
}

impl Catalog {
    pub fn english() -> Result<Self, CatalogError> {
        let locale: LanguageIdentifier = DEFAULT_LOCALE
            .parse()
            .map_err(|error| CatalogError::Parse(format!("invalid locale: {error}")))?;
        let resource = FluentResource::try_new(CATALOG_SOURCE.to_owned())
            .map_err(|errors| CatalogError::Parse(format!("{errors:?}")))?;
        let mut bundle = FluentBundle::new_concurrent(vec![locale]);
        bundle.set_use_isolating(false);
        bundle
            .add_resource(resource)
            .map_err(|error| CatalogError::Register(format!("{error:?}")))?;
        Ok(Self { bundle })
    }

    pub fn format(&self, id: &str, args: Option<&FluentArgs<'_>>) -> String {
        let Some(message) = self.bundle.get_message(id) else {
            return id.to_owned();
        };
        let Some(pattern) = message.value() else {
            return id.to_owned();
        };
        let mut errors = Vec::new();
        self.bundle
            .format_pattern(pattern, args, &mut errors)
            .into_owned()
    }
}

pub fn default_catalog() -> Result<Catalog, CatalogError> {
    Catalog::english()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_the_shared_catalog_and_formats_a_status() {
        let catalog = Catalog::english().expect("shared catalog");
        assert_eq!(catalog.format("status-working", None), "Working");
    }

    #[test]
    fn rust_formats_fluent_arguments_and_plural_selectors() {
        let catalog = Catalog::english().expect("shared catalog");
        let mut args = FluentArgs::new();
        args.set("count", 2);
        assert_eq!(catalog.format("sessions-count", Some(&args)), "2 sessions");
    }

    #[test]
    fn an_unknown_message_is_safe_for_forward_compatibility() {
        let catalog = Catalog::english().expect("shared catalog");
        assert_eq!(catalog.format("future-message", None), "future-message");
    }
}
