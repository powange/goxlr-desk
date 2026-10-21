/// The plain numeric attributes of an effect preset, described once (field, type, attribute):
/// `read_numeric` and `write_numeric` are generated from that list, so every value is read and
/// written the same way (a float in the file, an integer here).
macro_rules! numeric_attributes {
    ($encoder:ty { $($field:ident: $ty:ty = $name:literal),+ $(,)? }) => {
        impl $encoder {
            /// Reads `value` when `name` is one of the numeric attributes (true), false otherwise.
            fn read_numeric(
                &mut self,
                name: &str,
                value: &str,
            ) -> Result<bool, std::num::ParseFloatError> {
                match name {
                    $($name => self.$field = value.parse::<std::os::raw::c_float>()? as $ty,)+
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
