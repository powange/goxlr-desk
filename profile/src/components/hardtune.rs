use std::collections::HashMap;
use std::io::Write;
use std::str::FromStr;

use enum_map::EnumMap;
use strum::{Display, EnumIter, EnumProperty, EnumString, IntoEnumIterator};

use anyhow::{Result, anyhow};
use quick_xml::Writer;
use quick_xml::events::{BytesEnd, BytesStart, Event};

use crate::Preset;
use crate::components::UnknownAttributes;
use crate::components::colours::{Colour, ColourMap, ColourOffStyle};
use crate::components::hardtune::HardTuneSource::All;
use crate::components::hardtune::HardTuneStyle::Natural;
use crate::components::{check_range, style_from_index};
use crate::profile::Attribute;
use goxlr_types::effect_ranges as ranges;

#[derive(thiserror::Error, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum ParseError {
    #[error("[HardTune] Expected int: {0}")]
    ExpectedInt(#[from] std::num::ParseIntError),

    #[error("[HardTune] Expected float: {0}")]
    ExpectedFloat(#[from] std::num::ParseFloatError),

    #[error("[HardTune] Expected enum: {0}")]
    ExpectedEnum(#[from] strum::ParseError),

    #[error("[HardTune] Invalid colours: {0}")]
    InvalidColours(#[from] crate::components::colours::ParseError),
}

/**
 * This is relatively static, main tag contains standard colour mapping, subtags contain various
 * presets, we'll use an EnumMap to define the 'presets' as they'll be useful for the other various
 * 'types' of presets (encoders and effects).
 */
#[derive(Debug)]
pub struct HardtuneEffectBase {
    colour_map: ColourMap,
    preset_map: EnumMap<Preset, HardTuneEffect>,
    source: HardTuneSource,

    // Attributes this version doesn't know, written back as they were
    unknown: UnknownAttributes,
}

impl HardtuneEffectBase {
    pub fn new(element_name: String) -> Self {
        let mut colour_map = ColourMap::new(element_name.clone());
        colour_map.set_off_style(ColourOffStyle::Dimmed);
        colour_map.set_blink_on(false);
        colour_map.set_state_on(false);
        colour_map.set_colour(0, Colour::fromrgb("00FFFF").unwrap());
        colour_map.set_colour(1, Colour::fromrgb("FFFFFF").unwrap());
        colour_map.set_velocity(127);

        Self {
            colour_map,
            preset_map: EnumMap::default(),
            source: HardTuneSource::Music,
            unknown: UnknownAttributes::default(),
        }
    }

    pub fn parse_hardtune_root(&mut self, attributes: &Vec<Attribute>) -> Result<(), ParseError> {
        for attr in attributes {
            // I honestly have no idea why this lives here :D
            if attr.name == "HARDTUNE_SOURCE" {
                self.source = HardTuneSource::from_str(&attr.value)?;
                continue;
            }

            if !self.colour_map.read_colours(attr)? {
                self.unknown.keep(attr);
            }
        }

        Ok(())
    }

    pub fn parse_hardtune_preset(
        &mut self,
        preset_enum: Preset,
        attributes: &Vec<Attribute>,
    ) -> Result<(), ParseError> {
        let mut preset = HardTuneEffect::new();
        for attr in attributes {
            if preset.read_numeric(&attr.name, &attr.value)? {
                continue;
            }

            if attr.name == "hardtuneEffectstate" {
                preset.state = matches!(attr.value.as_str(), "1");
                continue;
            }
            if attr.name == "HARDTUNE_STYLE" {
                if let Some(style) = style_from_index::<HardTuneStyle>(&attr.value) {
                    preset.style = style;
                }
                continue;
            }

            if attr.name == "HARDTUNE_SOURCE" {
                preset.source = Some(HardTuneSource::from_str(&attr.value)?);
                continue;
            }

            preset.unknown.keep(attr);
        }

        self.preset_map[preset_enum] = preset;
        Ok(())
    }

    pub fn write_hardtune<W: Write>(&self, writer: &mut Writer<W>) -> Result<()> {
        let mut elem = BytesStart::new("hardtuneEffect");

        let mut attributes: HashMap<String, String> = HashMap::default();
        attributes.insert("HARDTUNE_SOURCE".to_string(), self.source.to_string());
        self.colour_map.write_colours(&mut attributes);

        // Write out the attributes etc for this element, but don't close it yet..
        self.unknown.write_to(&mut attributes);
        for (key, value) in &attributes {
            elem.push_attribute((key.as_str(), value.as_str()));
        }

        writer.write_event(Event::Start(elem))?;

        // Because all of these are seemingly 'guaranteed' to exist, we can straight dump..
        for preset in Preset::iter() {
            let tag_name = format!("hardtuneEffect{}", preset.get_str("tagSuffix").unwrap());
            let mut sub_elem = BytesStart::new(tag_name.as_str());

            let sub_attributes = self.get_preset_attributes(preset);
            for (key, value) in &sub_attributes {
                sub_elem.push_attribute((key.as_str(), value.as_str()));
            }

            writer.write_event(Event::Empty(sub_elem))?;
        }

        // Finally, close the 'main' tag.
        writer.write_event(Event::End(BytesEnd::new("hardtuneEffect")))?;
        Ok(())
    }

    pub fn get_preset_attributes(&self, preset: Preset) -> HashMap<String, String> {
        let mut attributes = HashMap::new();
        let value = &self.preset_map[preset];

        attributes.insert(
            "hardtuneEffectstate".to_string(),
            if value.state {
                "1".to_string()
            } else {
                "0".to_string()
            },
        );
        attributes.insert(
            "HARDTUNE_STYLE".to_string(),
            value.style.get_str("uiIndex").unwrap().to_string(),
        );
        value.write_numeric(&mut attributes);

        if let Some(source) = &value.source {
            attributes.insert("HARDTUNE_SOURCE".to_string(), source.to_string());
        }

        value.unknown.write_to(&mut attributes);
        attributes
    }

    pub fn colour_map(&self) -> &ColourMap {
        &self.colour_map
    }

    pub fn colour_map_mut(&mut self) -> &mut ColourMap {
        &mut self.colour_map
    }

    pub fn get_preset(&self, preset: Preset) -> &HardTuneEffect {
        &self.preset_map[preset]
    }

    pub fn get_preset_mut(&mut self, preset: Preset) -> &mut HardTuneEffect {
        &mut self.preset_map[preset]
    }
}

#[derive(Debug, Default, Clone)]
pub struct HardTuneEffect {
    // State here determines if the hardtune is on or off when this preset is loaded.
    state: bool,

    style: HardTuneStyle,
    key_source: u8,
    amount: u8,
    window: u16,
    rate: u8,
    scale: u8,
    pitch_amt: u8,
    source: Option<HardTuneSource>,

    // Attributes this version doesn't know, written back as they were
    unknown: UnknownAttributes,
}

impl HardTuneEffect {
    pub fn new() -> Self {
        Self {
            state: false,
            style: Default::default(),
            key_source: 0,
            amount: 0,
            window: 0,
            rate: 0,
            scale: 0,
            pitch_amt: 0,
            source: None,
            unknown: UnknownAttributes::default(),
        }
    }

    pub fn state(&self) -> bool {
        self.state
    }
    pub fn set_state(&mut self, state: bool) {
        self.state = state;
    }

    pub fn style(&self) -> &HardTuneStyle {
        &self.style
    }
    pub fn set_style(&mut self, style: HardTuneStyle) -> Result<()> {
        self.style = style;

        let preset = HardtunePreset::get_preset(style);
        self.set_amount(preset.amount)?;
        self.set_window(preset.window)?;
        self.set_rate(preset.rate)?;
        self.set_scale(preset.scale);
        self.set_pitch_amt(preset.pitch_amt)?;

        Ok(())
    }

    pub fn key_source(&self) -> u8 {
        self.key_source
    }

    pub fn amount(&self) -> u8 {
        self.amount
    }
    pub fn set_amount(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::PERCENTAGE, "Amount")?;
        self.amount = value;
        Ok(())
    }

    pub fn window(&self) -> u16 {
        self.window
    }
    pub fn set_window(&mut self, value: u16) -> Result<()> {
        check_range(value, ranges::HARDTUNE_WINDOW, "Window")?;
        self.window = value;
        Ok(())
    }

    pub fn rate(&self) -> u8 {
        self.rate
    }
    pub fn set_rate(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::PERCENTAGE, "Rate")?;
        self.rate = value;
        Ok(())
    }

    pub fn scale(&self) -> u8 {
        self.scale
    }
    fn set_scale(&mut self, value: u8) {
        self.scale = value;
    }

    pub fn pitch_amt(&self) -> u8 {
        self.pitch_amt
    }
    fn set_pitch_amt(&mut self, value: u8) -> Result<()> {
        if value != 0 {
            return Err(anyhow!("Hardtune Pitch Amt should be 0.."));
        }
        self.pitch_amt = value;
        Ok(())
    }

    pub fn source(&self) -> &Option<HardTuneSource> {
        &self.source
    }
    pub fn get_source_value(&self) -> HardTuneSource {
        if let Some(source) = self.source {
            return source;
        }
        All
    }
    pub fn set_source(&mut self, source: HardTuneSource) {
        self.source = Some(source);
    }

    pub fn get_source(&self) -> HardTuneSource {
        if let Some(source) = self.source {
            return source;
        }
        All
    }
}

#[derive(Default, Debug, EnumIter, EnumProperty, Clone, Copy)]
pub enum HardTuneStyle {
    #[default]
    #[strum(props(uiIndex = "0"))]
    Natural,

    #[strum(props(uiIndex = "1"))]
    Medium,

    #[strum(props(uiIndex = "2"))]
    Hard,
}

#[derive(Default, Debug, Display, EnumString, PartialEq, Eq, Copy, Clone)]
pub enum HardTuneSource {
    #[default]
    #[strum(to_string = "ALL")]
    All,

    #[strum(to_string = "MUSIC")]
    Music,

    #[strum(to_string = "GAME")]
    Game,

    #[strum(to_string = "LINEIN")]
    LineIn,

    #[strum(to_string = "SYSTEM")]
    System,
}

struct HardtunePreset {
    amount: u8,
    window: u16,
    rate: u8,
    scale: u8,
    pitch_amt: u8,
}

impl HardtunePreset {
    fn get_preset(style: HardTuneStyle) -> HardtunePreset {
        match style {
            Natural => HardtunePreset {
                amount: 70,
                window: 20,
                rate: 20,
                scale: 5,
                pitch_amt: 0,
            },
            HardTuneStyle::Medium => HardtunePreset {
                amount: 53,
                window: 20,
                rate: 99,
                scale: 5,
                pitch_amt: 0,
            },
            HardTuneStyle::Hard => HardtunePreset {
                amount: 100,
                window: 60,
                rate: 100,
                scale: 5,
                pitch_amt: 0,
            },
        }
    }
}

numeric_attributes!(HardTuneEffect {
    key_source: u8 = "HARDTUNE_KEYSOURCE",
    amount: u8 = "HARDTUNE_AMOUNT" in ranges::PERCENTAGE,
    window: u16 = "HARDTUNE_WINDOW" in ranges::HARDTUNE_WINDOW,
    rate: u8 = "HARDTUNE_RATE" in ranges::PERCENTAGE,
    scale: u8 = "HARDTUNE_SCALE",
    pitch_amt: u8 = "HARDTUNE_PITCH_AMT",
});
