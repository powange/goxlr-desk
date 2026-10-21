/// The plain numeric attributes of an effect preset, described once (field, type, attribute, and
/// the range when the value is stored as the setter takes it): `read_numeric` and
/// `write_numeric` are generated from that list, so every value is read and written the same
/// way (a float in the file, an integer here). The policy, decided once: a setting out of range
/// is refused (see `check_range`), a value out of range in a file is brought back into it.
macro_rules! numeric_attributes {
    ($encoder:ty { $($field:ident: $ty:ty = $name:literal $(in $range:expr)?),+ $(,)? }) => {
        impl $encoder {
            /// Reads `value` when `name` is one of the numeric attributes (true), false otherwise.
            fn read_numeric(
                &mut self,
                name: &str,
                value: &str,
            ) -> Result<bool, std::num::ParseFloatError> {
                match name {
                    $($name => {
                        let read = value.parse::<std::os::raw::c_float>()?;
                        $(let read = $crate::components::clamp_loaded(read, $range, $name);)?
                        self.$field = read as $ty;
                    })+
                    _ => return Ok(false),
                }
                Ok(true)
            }

            fn write_numeric(&self, attributes: &mut std::collections::HashMap<String, String>) {
                $(attributes.insert($name.to_string(), format!("{}", self.$field));)+
            }
        }
    };
}

pub mod animation;
pub mod browser;
pub mod colours;
pub mod context;
pub mod echo;
pub mod effects;
pub mod fader;
pub mod gender;
pub mod hardtune;
pub mod megaphone;
pub mod mixer;
pub mod mute;
pub mod mute_chat;
pub mod pitch;
pub mod preset_writer;
pub mod reverb;
pub mod robot;
pub mod root;
pub mod sample;
pub mod scribble;
pub mod simple;
pub mod submix;

/// A setting checked against its range (see goxlr_types::effect_ranges).
pub(crate) fn check_range<T: Into<i64>>(
    value: T,
    range: std::ops::RangeInclusive<i64>,
    name: &str,
) -> anyhow::Result<()> {
    goxlr_types::effect_ranges::check(value, range, name).map_err(anyhow::Error::msg)
}

/// A value read from a file, brought back into its range (with a warning) when it's outside.
pub(crate) fn clamp_loaded(
    value: std::os::raw::c_float,
    range: std::ops::RangeInclusive<i64>,
    name: &str,
) -> std::os::raw::c_float {
    let (low, high) = (
        *range.start() as std::os::raw::c_float,
        *range.end() as std::os::raw::c_float,
    );
    if value < low || value > high || value.is_nan() {
        log::warn!("{name}: {value} is out of range, using the closest valid value");
        return if value.is_nan() {
            low
        } else {
            value.clamp(low, high)
        };
    }
    value
}

/// Attributes this version doesn't know (from a newer official app, for example). They're kept
/// and written back as they were, so saving a profile here doesn't lose them.
#[derive(Debug, Default, Clone)]
pub struct UnknownAttributes(Vec<(String, String)>);

impl UnknownAttributes {
    pub fn keep(&mut self, attribute: &crate::profile::Attribute) {
        log::debug!("Keeping unknown attribute {}", attribute.name);
        self.0.retain(|(name, _)| name != &attribute.name);
        self.0
            .push((attribute.name.clone(), attribute.value.clone()));
    }

    /// Adds them to the attributes being written, without replacing a known one.
    pub fn write_to<S: std::hash::BuildHasher>(
        &self,
        attributes: &mut std::collections::HashMap<String, String, S>,
    ) {
        for (name, value) in &self.0 {
            attributes
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
    }
}

/// An effect style from its index in the file (its "uiIndex"), None when it isn't one.
pub(crate) fn style_from_index<T>(value: &str) -> Option<T>
where
    T: strum::IntoEnumIterator + strum::EnumProperty,
{
    T::iter().find(|style| style.get_str("uiIndex") == Some(value))
}
