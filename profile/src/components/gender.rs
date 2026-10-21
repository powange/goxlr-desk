use std::collections::HashMap;
use std::io::Write;

use enum_map::{Enum, EnumMap};
use strum::{EnumIter, EnumProperty, IntoEnumIterator};

use anyhow::{Result, anyhow};
use quick_xml::Writer;
use quick_xml::events::{BytesEnd, BytesStart, Event};

use crate::Preset;
use crate::components::UnknownAttributes;
use crate::components::colours::{Colour, ColourMap};
use crate::components::{check_range, style_from_index};
use crate::profile::Attribute;
use goxlr_types::effect_ranges as ranges;

#[derive(thiserror::Error, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum ParseError {
    #[error("[Gender] Expected int: {0}")]
    ExpectedInt(#[from] std::num::ParseIntError),

    #[error("[Gender] Expected float: {0}")]
    ExpectedFloat(#[from] std::num::ParseFloatError),

    #[error("[Gender] Expected enum: {0}")]
    ExpectedEnum(#[from] strum::ParseError),

    #[error("[Gender] Invalid colours: {0}")]
    InvalidColours(#[from] crate::components::colours::ParseError),
}

/**
 * This is relatively static, main tag contains standard colour mapping, subtags contain various
 * presets, we'll use an EnumMap to define the 'presets' as they'll be useful for the other various
 * 'types' of presets (encoders and effects).
 */
#[derive(Debug)]
pub struct GenderEncoderBase {
    colour_map: ColourMap,
    preset_map: EnumMap<Preset, GenderEncoder>,
    active_set: u8, // Not sure what this does?

    // Attributes this version doesn't know, written back as they were
    unknown: UnknownAttributes,
}

impl GenderEncoderBase {
    pub fn new(element_name: String) -> Self {
        let mut colour_map = ColourMap::new(element_name.clone());
        colour_map.set_colour(0, Colour::fromrgb("00FFFF").unwrap());
        colour_map.set_colour(1, Colour::fromrgb("00FFFF").unwrap());
        colour_map.set_colour(2, Colour::fromrgb("00FFFF").unwrap());
        colour_map.set_colour_group("encoderGroup".to_string());

        Self {
            colour_map,
            preset_map: EnumMap::default(),
            active_set: 0,
            unknown: UnknownAttributes::default(),
        }
    }

    pub fn parse_gender_root(&mut self, attributes: &Vec<Attribute>) -> Result<(), ParseError> {
        for attr in attributes {
            if attr.name == "active_set" {
                self.active_set = attr.value.parse()?;
                continue;
            }

            if !self.colour_map.read_colours(attr)? {
                self.unknown.keep(attr);
            }
        }

        Ok(())
    }

    pub fn parse_gender_preset(
        &mut self,
        preset_enum: Preset,
        attributes: &Vec<Attribute>,
    ) -> Result<(), ParseError> {
        let mut preset = GenderEncoder::new();
        for attr in attributes {
            if preset.read_numeric(&attr.name, &attr.value)? {
                continue;
            }

            if attr.name == "GENDER_STYLE" {
                if let Some(style) = style_from_index::<GenderStyle>(&attr.value) {
                    preset.style = style;
                }
                continue;
            }

            preset.unknown.keep(attr);
        }

        self.preset_map[preset_enum] = preset;
        Ok(())
    }

    pub fn write_gender<W: Write>(&self, writer: &mut Writer<W>) -> Result<()> {
        let mut elem = BytesStart::new("genderEncoder");

        let mut attributes: HashMap<String, String> = HashMap::default();
        attributes.insert("active_set".to_string(), format!("{}", self.active_set));
        self.colour_map.write_colours(&mut attributes);

        // Write out the attributes etc for this element, but don't close it yet..
        self.unknown.write_to(&mut attributes);
        for (key, value) in &attributes {
            elem.push_attribute((key.as_str(), value.as_str()));
        }

        writer.write_event(Event::Start(elem))?;

        // Because all of these are seemingly 'guaranteed' to exist, we can straight dump..
        for preset in Preset::iter() {
            let tag_name = format!("genderEncoder{}", preset.get_str("tagSuffix").unwrap());
            let mut sub_elem = BytesStart::new(tag_name.as_str());

            let sub_attributes = self.get_preset_attributes(preset);
            for (key, value) in &sub_attributes {
                sub_elem.push_attribute((key.as_str(), value.as_str()));
            }

            writer.write_event(Event::Empty(sub_elem))?;
        }

        // Finally, close the 'main' tag.
        writer.write_event(Event::End(BytesEnd::new("genderEncoder")))?;
        Ok(())
    }

    pub fn get_preset_attributes(&self, preset: Preset) -> HashMap<String, String> {
        let mut attributes = HashMap::new();
        let value = &self.preset_map[preset];

        value.write_numeric(&mut attributes);
        attributes.insert(
            "GENDER_STYLE".to_string(),
            value.style.get_str("uiIndex").unwrap().to_string(),
        );

        value.unknown.write_to(&mut attributes);
        attributes
    }

    pub fn colour_map(&self) -> &ColourMap {
        &self.colour_map
    }

    pub fn colour_map_mut(&mut self) -> &mut ColourMap {
        &mut self.colour_map
    }

    pub fn get_preset(&self, preset: Preset) -> &GenderEncoder {
        &self.preset_map[preset]
    }

    pub fn get_preset_mut(&mut self, preset: Preset) -> &mut GenderEncoder {
        &mut self.preset_map[preset]
    }
}

#[derive(Debug, Default, Clone)]
pub struct GenderEncoder {
    knob_position: i8,
    style: GenderStyle,
    range: u8,

    // Attributes this version doesn't know, written back as they were
    unknown: UnknownAttributes,
}

impl GenderEncoder {
    pub fn new() -> Self {
        Self {
            knob_position: 0,
            style: GenderStyle::Narrow,
            range: 0,
            unknown: UnknownAttributes::default(),
        }
    }

    pub fn amount(&self) -> i8 {
        // Amount is dependent on Style, and knob position, lets work with positive numbers.
        let knob_position = self.knob_position as f32; // Between -24 and 24..

        match self.style {
            GenderStyle::Narrow => ((12. * knob_position) / 24.).round() as i8,
            GenderStyle::Medium => ((25. * knob_position) / 24.).round() as i8,
            GenderStyle::Wide => ((50. * knob_position) / 24.).round() as i8,
        }
    }

    pub fn set_amount(&mut self, amount: i8) -> Result<()> {
        match self.style {
            GenderStyle::Narrow => {
                if !(-12..=12).contains(&amount) {
                    return Err(anyhow!(
                        "Amount should be between -12 and 12 (Style: Narrow)"
                    ));
                }
                let base = amount as i32 + 12;
                let percent = base * 48 / 24;
                self.knob_position = (percent - 24) as i8;
                Ok(())
            }
            GenderStyle::Medium => {
                if !(-25..=25).contains(&amount) {
                    return Err(anyhow!(
                        "Amount should be between -25 and 25 (Style: Medium)"
                    ));
                }
                let base = amount as i32 + 25;
                let percent = base * 48 / 50;
                self.knob_position = (percent - 24) as i8;
                Ok(())
            }
            GenderStyle::Wide => {
                if !(-50..=50).contains(&amount) {
                    return Err(anyhow!("Amount should be between -50 and 50 (Style: Wide)"));
                }
                let base = amount as i32 + 50;
                let percent = base * 48 / 100;
                self.knob_position = (percent - 24) as i8;
                Ok(())
            }
        }
    }

    pub fn knob_position(&self) -> i8 {
        self.knob_position
    }

    pub fn set_knob_position(&mut self, knob_position: i8) -> Result<()> {
        check_range(knob_position, ranges::KNOB_CENTRED, "Gender Knob Position")?;

        self.knob_position = knob_position;
        Ok(())
    }

    pub fn style(&self) -> &GenderStyle {
        &self.style
    }
    pub fn set_style(&mut self, style: GenderStyle) {
        self.style = style;
    }
    pub fn range(&self) -> u8 {
        self.range
    }
}

#[derive(Default, Debug, EnumIter, Enum, EnumProperty, Clone, Copy)]
pub enum GenderStyle {
    #[default]
    #[strum(props(uiIndex = "0"))]
    Narrow,

    #[strum(props(uiIndex = "1"))]
    Medium,

    #[strum(props(uiIndex = "2"))]
    Wide,
}

numeric_attributes!(GenderEncoder {
    knob_position: i8 = "GENDER_KNOB_POSITION" in ranges::KNOB_CENTRED,
    range: u8 = "GENDER_RANGE",
});
