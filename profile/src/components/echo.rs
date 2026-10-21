use std::collections::HashMap;
use std::io::Write;
use std::os::raw::c_float;

use enum_map::{Enum, EnumMap};
use strum::{EnumIter, EnumProperty, IntoEnumIterator};

use anyhow::{Result, anyhow};
use quick_xml::Writer;
use quick_xml::events::{BytesEnd, BytesStart, Event};

use crate::components::colours::{Colour, ColourMap};

use crate::Preset;
use crate::components::UnknownAttributes;
use crate::components::{check_range, style_from_index};
use crate::profile::Attribute;
use goxlr_types::effect_ranges as ranges;

#[derive(thiserror::Error, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum ParseError {
    #[error("[ECHO] Expected int: {0}")]
    ExpectedInt(#[from] std::num::ParseIntError),

    #[error("[ECHO] Expected float: {0}")]
    ExpectedFloat(#[from] std::num::ParseFloatError),

    #[error("[ECHO] Expected enum: {0}")]
    ExpectedEnum(#[from] strum::ParseError),

    #[error("[ECHO] Invalid colours: {0}")]
    InvalidColours(#[from] crate::components::colours::ParseError),
}

/**
 * This is relatively static, main tag contains standard colour mapping, subtags contain various
 * presets, we'll use an EnumMap to define the 'presets' as they'll be useful for the other various
 * 'types' of presets (encoders and effects).
 */
#[derive(Debug)]
pub struct EchoEncoderBase {
    colour_map: ColourMap,
    preset_map: EnumMap<Preset, EchoEncoder>,
    active_set: u8, // Not sure what this does?

    // Attributes this version doesn't know, written back as they were
    unknown: UnknownAttributes,
}

impl EchoEncoderBase {
    pub fn new(element_name: String) -> Self {
        let mut colour_map = ColourMap::new(element_name.clone());
        colour_map.set_colour(0, Colour::fromrgb("000000").unwrap());
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

    pub fn parse_echo_root(&mut self, attributes: &Vec<Attribute>) -> Result<(), ParseError> {
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

    pub fn parse_echo_preset(
        &mut self,
        preset_enum: Preset,
        attributes: &Vec<Attribute>,
    ) -> Result<()> {
        let mut preset = EchoEncoder::new();
        for attr in attributes {
            if preset.read_numeric(&attr.name, &attr.value)? {
                continue;
            }

            if attr.name == "DELAY_STYLE" {
                if let Some(style) = style_from_index::<EchoStyle>(&attr.value) {
                    preset.style = style;
                }
                continue;
            }

            if attr.name == "DELAY_KNOB_POSITION" {
                preset.set_knob_position(attr.value.parse::<c_float>()? as i8)?;
                continue;
            }

            preset.unknown.keep(attr);
        }

        self.preset_map[preset_enum] = preset;
        Ok(())
    }

    pub fn write_echo<W: Write>(&self, writer: &mut Writer<W>) -> Result<()> {
        let mut elem = BytesStart::new("echoEncoder");

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
            let tag_name = format!("echoEncoder{}", preset.get_str("tagSuffix").unwrap());
            let mut sub_elem = BytesStart::new(tag_name.as_str());

            let sub_attributes = self.get_preset_attributes(preset);
            for (key, value) in &sub_attributes {
                sub_elem.push_attribute((key.as_str(), value.as_str()));
            }

            writer.write_event(Event::Empty(sub_elem))?;
        }

        // Finally, close the 'main' tag.
        writer.write_event(Event::End(BytesEnd::new("echoEncoder")))?;
        Ok(())
    }

    pub fn get_preset_attributes(&self, preset: Preset) -> HashMap<String, String> {
        let mut attributes = HashMap::new();
        let value = &self.preset_map[preset];

        attributes.insert(
            "DELAY_KNOB_POSITION".to_string(),
            format!("{}", value.knob_position),
        );
        attributes.insert(
            "DELAY_STYLE".to_string(),
            value.style.get_str("uiIndex").unwrap().to_string(),
        );
        value.write_numeric(&mut attributes);

        value.unknown.write_to(&mut attributes);
        attributes
    }

    pub fn colour_map(&self) -> &ColourMap {
        &self.colour_map
    }

    pub fn colour_map_mut(&mut self) -> &mut ColourMap {
        &mut self.colour_map
    }

    pub fn get_preset(&self, preset: Preset) -> &EchoEncoder {
        &self.preset_map[preset]
    }

    pub fn get_preset_mut(&mut self, preset: Preset) -> &mut EchoEncoder {
        &mut self.preset_map[preset]
    }
}

#[derive(Debug, Default, Clone)]
pub struct EchoEncoder {
    knob_position: i8,
    style: EchoStyle,
    source: u8,
    div_l: u8,
    div_r: u8,
    feedback_left: u8,
    feedback_right: u8,
    feedback_control: u8,
    xfb_l_to_r: u8,
    xfb_r_to_l: u8,
    filter_style: u8,
    time_left: u16,
    time_right: u16,
    tempo: u16,

    // Attributes this version doesn't know, written back as they were
    unknown: UnknownAttributes,
}

impl EchoEncoder {
    pub fn new() -> Self {
        Self {
            knob_position: 0,
            style: EchoStyle::Quarter,
            source: 1,
            div_l: 0,
            div_r: 0,
            feedback_left: 0,
            feedback_right: 0,
            feedback_control: 0,
            xfb_l_to_r: 0,
            xfb_r_to_l: 0,
            filter_style: 0,
            time_left: 0,
            time_right: 0,
            tempo: 0,
            unknown: UnknownAttributes::default(),
        }
    }

    pub fn amount(&self) -> i8 {
        ((36 * self.knob_position as i32) / 24 - 36) as i8
    }

    // TODO: This should probably be handled by UIs like basically everything else!
    pub fn get_percentage_amount(&self) -> u8 {
        ((self.knob_position as u16 * 100) / 24) as u8
    }
    pub fn set_percentage_value(&mut self, percentage: u8) -> Result<()> {
        check_range(percentage, ranges::PERCENTAGE, "Amount")?;
        self.set_knob_position(((percentage as i16 * 24) / 100) as i8)?;
        Ok(())
    }

    pub fn knob_position(&self) -> i8 {
        self.knob_position
    }
    pub fn set_knob_position(&mut self, knob_position: i8) -> Result<()> {
        check_range(knob_position, ranges::KNOB, "Echo Knob Position")?;

        self.knob_position = knob_position;
        Ok(())
    }

    pub fn style(&self) -> &EchoStyle {
        &self.style
    }
    pub fn set_style(&mut self, style: EchoStyle) -> Result<()> {
        self.style = style;

        // Load a preset and set variables..
        let preset = EchoPreset::get_preset(style);
        self.set_source(preset.source);
        self.set_div_l(preset.div_l);
        self.set_div_r(preset.div_r);
        self.set_feedback_left(preset.feedback_left)?;
        self.set_feedback_right(preset.feedback_right)?;
        self.set_feedback(preset.feedback_control)?;
        self.set_xfb_l_to_r(preset.xfb_l_to_r)?;
        self.set_xfb_r_to_l(preset.xfb_r_to_l)?;
        self.set_filter_style(preset.filter_style);
        if let Some(time_left) = preset.time_left {
            self.set_time_left(time_left)?;
        }
        if let Some(time_right) = preset.time_right {
            self.set_time_right(time_right)?;
        }

        Ok(())
    }

    pub fn source(&self) -> u8 {
        self.source
    }
    fn set_source(&mut self, source: u8) {
        self.source = source;
    }

    pub fn div_l(&self) -> u8 {
        self.div_l
    }
    fn set_div_l(&mut self, value: u8) {
        self.div_l = value;
    }
    pub fn div_r(&self) -> u8 {
        self.div_r
    }
    fn set_div_r(&mut self, value: u8) {
        self.div_r = value;
    }

    pub fn feedback_left(&self) -> u8 {
        self.feedback_left
    }
    pub fn set_feedback_left(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::PERCENTAGE, "Feedback Left")?;
        self.feedback_left = value;
        Ok(())
    }

    pub fn feedback_right(&self) -> u8 {
        self.feedback_right
    }
    pub fn set_feedback_right(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::PERCENTAGE, "Feedback Right")?;
        self.feedback_right = value;
        Ok(())
    }
    pub fn feedback_control(&self) -> u8 {
        self.feedback_control
    }
    pub fn set_feedback(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::PERCENTAGE, "Feedback")?;
        self.feedback_control = value;
        Ok(())
    }
    pub fn xfb_l_to_r(&self) -> u8 {
        self.xfb_l_to_r
    }
    pub fn set_xfb_l_to_r(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::PERCENTAGE, "XFB L to R")?;
        self.xfb_l_to_r = value;
        Ok(())
    }

    pub fn xfb_r_to_l(&self) -> u8 {
        self.xfb_r_to_l
    }
    pub fn set_xfb_r_to_l(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::PERCENTAGE, "XFB R to L")?;
        self.xfb_r_to_l = value;
        Ok(())
    }

    pub fn filter_style(&self) -> u8 {
        self.filter_style
    }
    fn set_filter_style(&mut self, value: u8) {
        self.filter_style = value;
    }

    pub fn time_left(&self) -> u16 {
        self.time_left
    }
    pub fn set_time_left(&mut self, value: u16) -> Result<()> {
        check_range(value, ranges::ECHO_DELAY, "Delay Left")?;
        if self.style != EchoStyle::ClassicSlap {
            return Err(anyhow!("Delay can only be set if Style is ClassicSlap"));
        }

        self.time_left = value;
        Ok(())
    }

    pub fn time_right(&self) -> u16 {
        self.time_right
    }
    pub fn set_time_right(&mut self, value: u16) -> Result<()> {
        check_range(value, ranges::ECHO_DELAY, "Delay Right")?;
        if self.style != EchoStyle::ClassicSlap {
            return Err(anyhow!("Delay can only be set if Style is ClassicSlap"));
        }

        self.time_right = value;
        Ok(())
    }

    pub fn tempo(&self) -> u16 {
        self.tempo
    }
    pub fn set_tempo(&mut self, value: u16) -> Result<()> {
        check_range(value, ranges::ECHO_TEMPO, "Tempo")?;
        if self.style == EchoStyle::ClassicSlap {
            return Err(anyhow!("Tempo cannot be set if Style is ClassicSlap"));
        }
        self.tempo = value;
        Ok(())
    }
}

#[derive(Default, Debug, EnumIter, Enum, EnumProperty, Eq, PartialEq, Clone, Copy)]
pub enum EchoStyle {
    #[default]
    #[strum(props(uiIndex = "0"))]
    #[strum(to_string = "QUARTER")]
    Quarter,

    #[strum(props(uiIndex = "1"))]
    #[strum(to_string = "EIGHTH")]
    Eighth,

    #[strum(props(uiIndex = "2"))]
    #[strum(to_string = "TRIPLET")]
    Triplet,

    #[strum(props(uiIndex = "3"))]
    #[strum(to_string = "PING_PONG")]
    PingPong,

    #[strum(props(uiIndex = "4"))]
    #[strum(to_string = "CLASSIC_SLAP")]
    ClassicSlap,

    #[strum(props(uiIndex = "5"))]
    #[strum(to_string = "MULTI_TAP")]
    MultiTap,
}

struct EchoPreset {
    source: u8,
    div_l: u8,
    div_r: u8,
    feedback_left: u8,
    feedback_right: u8,
    feedback_control: u8,
    xfb_l_to_r: u8,
    xfb_r_to_l: u8,
    filter_style: u8,
    time_left: Option<u16>,
    time_right: Option<u16>,
}

impl EchoPreset {
    fn get_preset(style: EchoStyle) -> EchoPreset {
        match style {
            EchoStyle::Quarter => EchoPreset {
                source: 1,
                div_l: 9,
                div_r: 9,
                feedback_left: 50,
                feedback_right: 50,
                feedback_control: 30,
                xfb_l_to_r: 0,
                xfb_r_to_l: 0,
                filter_style: 0,
                time_left: None,
                time_right: None,
            },
            EchoStyle::Eighth => EchoPreset {
                source: 1,
                div_l: 12,
                div_r: 12,
                feedback_left: 50,
                feedback_right: 50,
                feedback_control: 30,
                xfb_l_to_r: 0,
                xfb_r_to_l: 0,
                filter_style: 0,
                time_left: None,
                time_right: None,
            },
            EchoStyle::Triplet => EchoPreset {
                source: 1,
                div_l: 13,
                div_r: 13,
                feedback_left: 50,
                feedback_right: 50,
                feedback_control: 30,
                xfb_l_to_r: 0,
                xfb_r_to_l: 0,
                filter_style: 0,
                time_left: None,
                time_right: None,
            },
            EchoStyle::PingPong => EchoPreset {
                source: 1,
                div_l: 10,
                div_r: 13,
                feedback_left: 50,
                feedback_right: 0,
                feedback_control: 30,
                xfb_l_to_r: 100,
                xfb_r_to_l: 0,
                filter_style: 0,
                time_left: None,
                time_right: None,
            },
            EchoStyle::ClassicSlap => EchoPreset {
                source: 0,
                div_l: 18,
                div_r: 18,
                feedback_left: 50,
                feedback_right: 50,
                feedback_control: 0,
                xfb_l_to_r: 0,
                xfb_r_to_l: 0,
                filter_style: 0,
                time_left: Some(110),
                time_right: Some(110),
            },
            EchoStyle::MultiTap => EchoPreset {
                source: 1,
                div_l: 9,
                div_r: 11,
                feedback_left: 25,
                feedback_right: 50,
                feedback_control: 30,
                xfb_l_to_r: 0,
                xfb_r_to_l: 0,
                filter_style: 0,
                time_left: None,
                time_right: None,
            },
        }
    }
}

numeric_attributes!(EchoEncoder {
    source: u8 = "DELAY_SOURCE",
    div_l: u8 = "DELAY_DIV_L",
    div_r: u8 = "DELAY_DIV_R",
    feedback_left: u8 = "DELAY_FB_L" in ranges::PERCENTAGE,
    feedback_right: u8 = "DELAY_FB_R" in ranges::PERCENTAGE,
    xfb_l_to_r: u8 = "DELAY_XFB_L_R" in ranges::PERCENTAGE,
    xfb_r_to_l: u8 = "DELAY_XFB_R_L" in ranges::PERCENTAGE,
    feedback_control: u8 = "DELAY_FB_CONTROL" in ranges::PERCENTAGE,
    filter_style: u8 = "DELAY_FILTER_STYLE",
    time_left: u16 = "DELAY_TIME_L" in ranges::ECHO_DELAY,
    time_right: u16 = "DELAY_TIME_R" in ranges::ECHO_DELAY,
    tempo: u16 = "DELAY_TEMPO" in ranges::ECHO_TEMPO,
});
