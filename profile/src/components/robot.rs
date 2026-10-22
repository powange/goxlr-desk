use std::collections::HashMap;
use std::io::Write;

use enum_map::EnumMap;
use strum::{EnumIter, EnumProperty, IntoEnumIterator};

use anyhow::Result;
use quick_xml::Writer;
use quick_xml::events::{BytesEnd, BytesStart, Event};

use crate::Preset;
use crate::components::UnknownAttributes;
use crate::components::colours::{Colour, ColourMap, ColourOffStyle};
use crate::components::robot::RobotStyle::Robot1;
use crate::components::{check_range, style_from_index};
use crate::profile::Attribute;
use goxlr_types::effect_ranges as ranges;

#[derive(thiserror::Error, Debug)]
#[allow(clippy::enum_variant_names)]
pub enum ParseError {
    #[error("[ROBOT] Expected int: {0}")]
    ExpectedInt(#[from] std::num::ParseIntError),

    #[error("[ROBOT] Expected float: {0}")]
    ExpectedFloat(#[from] std::num::ParseFloatError),

    #[error("[ROBOT] Expected enum: {0}")]
    ExpectedEnum(#[from] strum::ParseError),

    #[error("[ROBOT] Invalid colours: {0}")]
    InvalidColours(#[from] crate::components::colours::ParseError),
}

/**
 * This is relatively static, main tag contains standard colour mapping, subtags contain various
 * presets, we'll use an EnumMap to define the 'presets' as they'll be useful for the other various
 * 'types' of presets (encoders and effects).
 */
#[derive(Debug)]
pub struct RobotEffectBase {
    colour_map: ColourMap,
    preset_map: EnumMap<Preset, RobotEffect>,

    // Attributes this version doesn't know, written back as they were
    unknown: UnknownAttributes,
}

impl RobotEffectBase {
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
            unknown: UnknownAttributes::default(),
        }
    }

    pub fn parse_robot_root(&mut self, attributes: &Vec<Attribute>) -> Result<()> {
        for attr in attributes {
            if !self.colour_map.read_colours(attr)? {
                self.unknown.keep(attr);
            }
        }

        Ok(())
    }

    pub fn parse_robot_preset(
        &mut self,
        preset_enum: Preset,
        attributes: &Vec<Attribute>,
    ) -> Result<(), ParseError> {
        let mut preset = RobotEffect::new();
        for attr in attributes {
            if preset.read_numeric(&attr.name, &attr.value)? {
                continue;
            }

            if attr.name == "robotEffectstate" {
                preset.state = matches!(attr.value.as_str(), "1");
                continue;
            }
            if attr.name == "ROBOT_STYLE" {
                if let Some(style) = style_from_index::<RobotStyle>(&attr.value) {
                    preset.style = style;
                }
                continue;
            }

            /* Same as Microphone, I haven't seen any random float values in the config for robot
             * but I'm not gonna rule it out.. */

            preset.unknown.keep(attr);
        }

        self.preset_map[preset_enum] = preset;
        Ok(())
    }

    pub fn write_robot<W: Write>(&self, writer: &mut Writer<W>) -> Result<()> {
        let mut elem = BytesStart::new("robotEffect");

        let mut attributes: HashMap<String, String> = HashMap::default();
        self.colour_map.write_colours(&mut attributes);

        // Write out the attributes etc for this element, but don't close it yet..
        self.unknown.write_to(&mut attributes);
        for (key, value) in &attributes {
            elem.push_attribute((key.as_str(), value.as_str()));
        }

        writer.write_event(Event::Start(elem))?;

        // Because all of these are seemingly 'guaranteed' to exist, we can straight dump..
        for preset in Preset::iter() {
            let tag_name = format!("robotEffect{}", preset.get_str("tagSuffix").unwrap());
            let mut sub_elem = BytesStart::new(tag_name.as_str());

            let sub_attributes = self.get_preset_attributes(preset);
            for (key, value) in &sub_attributes {
                sub_elem.push_attribute((key.as_str(), value.as_str()));
            }

            writer.write_event(Event::Empty(sub_elem))?;
        }

        // Finally, close the 'main' tag.
        writer.write_event(Event::End(BytesEnd::new("robotEffect")))?;
        Ok(())
    }

    pub fn get_preset_attributes(&self, preset: Preset) -> HashMap<String, String> {
        let mut attributes = HashMap::new();
        let value = &self.preset_map[preset];

        attributes.insert(
            "robotEffectstate".to_string(),
            if value.state {
                "1".to_string()
            } else {
                "0".to_string()
            },
        );
        attributes.insert(
            "ROBOT_STYLE".to_string(),
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

    pub fn get_preset(&self, preset: Preset) -> &RobotEffect {
        &self.preset_map[preset]
    }
    pub fn get_preset_mut(&mut self, preset: Preset) -> &mut RobotEffect {
        &mut self.preset_map[preset]
    }
}

#[derive(Debug, Default, Clone)]
pub struct RobotEffect {
    // State here determines if the robot effect is on or off when this preset is loaded.
    state: bool,

    style: RobotStyle,
    synthosc_pulse_width: u8,
    synthosc_waveform: u8,
    vocoder_gate_threshold: i8,
    dry_mix: i8,

    vocoder_low_freq: u8,
    vocoder_low_gain: i8,
    vocoder_low_bw: u8,

    vocoder_mid_freq: u8,
    vocoder_mid_gain: i8,
    vocoder_mid_bw: u8,

    vocoder_high_freq: u8,
    vocoder_high_gain: i8,
    vocoder_high_bw: u8,

    // Attributes this version doesn't know, written back as they were
    unknown: UnknownAttributes,
}

impl RobotEffect {
    pub fn new() -> Self {
        Self {
            state: false,
            style: Default::default(),

            synthosc_pulse_width: 0,
            synthosc_waveform: 0,
            vocoder_gate_threshold: 0,
            dry_mix: 0,
            vocoder_low_freq: 0,
            vocoder_low_gain: 0,
            vocoder_low_bw: 0,
            vocoder_mid_freq: 0,
            vocoder_mid_gain: 0,
            vocoder_mid_bw: 0,
            vocoder_high_freq: 0,
            vocoder_high_gain: 0,
            vocoder_high_bw: 0,
            unknown: UnknownAttributes::default(),
        }
    }

    pub fn state(&self) -> bool {
        self.state
    }
    pub fn set_state(&mut self, state: bool) {
        self.state = state;
    }

    pub fn style(&self) -> &RobotStyle {
        &self.style
    }
    pub fn set_style(&mut self, style: RobotStyle) -> Result<()> {
        self.style = style;

        let preset = RobotPresets::get_preset(style);
        self.set_synthosc_pulse_width(preset.synthosc_pulse_width)?;
        self.set_synthosc_waveform(preset.synthosc_waveform)?;
        self.set_vocoder_gate_threshold(preset.vocoder_gate_threshold)?;
        self.set_dry_mix(preset.dry_mix)?;
        self.set_vocoder_low_freq(preset.vocoder_low_freq)?;
        self.set_vocoder_low_gain(preset.vocoder_low_gain)?;
        self.set_vocoder_low_bw(preset.vocoder_low_bw)?;
        self.set_vocoder_mid_freq(preset.vocoder_mid_freq)?;
        self.set_vocoder_mid_gain(preset.vocoder_mid_gain)?;
        self.set_vocoder_mid_bw(preset.vocoder_mid_bw)?;
        self.set_vocoder_high_freq(preset.vocoder_high_freq)?;
        self.set_vocoder_high_gain(preset.vocoder_high_gain)?;
        self.set_vocoder_high_bw(preset.vocoder_high_bw)?;

        Ok(())
    }

    pub fn synthosc_pulse_width(&self) -> u8 {
        self.synthosc_pulse_width
    }
    pub fn set_synthosc_pulse_width(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::PERCENTAGE, "Pulse Width")?;
        self.synthosc_pulse_width = value;
        Ok(())
    }

    pub fn synthosc_waveform(&self) -> u8 {
        self.synthosc_waveform
    }
    pub fn set_synthosc_waveform(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::ROBOT_WAVEFORM, "Waveform")?;
        self.synthosc_waveform = value;
        Ok(())
    }

    pub fn vocoder_gate_threshold(&self) -> i8 {
        self.vocoder_gate_threshold
    }
    pub fn set_vocoder_gate_threshold(&mut self, value: i8) -> Result<()> {
        check_range(value, ranges::ROBOT_LEVEL, "Threshold")?;
        self.vocoder_gate_threshold = value;
        Ok(())
    }

    pub fn dry_mix(&self) -> i8 {
        self.dry_mix
    }
    pub fn set_dry_mix(&mut self, value: i8) -> Result<()> {
        check_range(value, ranges::ROBOT_LEVEL, "Dry Mix")?;
        self.dry_mix = value;
        Ok(())
    }

    pub fn vocoder_low_freq(&self) -> u8 {
        self.vocoder_low_freq
    }
    pub fn set_vocoder_low_freq(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::ROBOT_LOW_FREQ, "Low Freq")?;
        self.vocoder_low_freq = value;
        Ok(())
    }

    pub fn vocoder_low_gain(&self) -> i8 {
        self.vocoder_low_gain
    }
    pub fn set_vocoder_low_gain(&mut self, value: i8) -> Result<()> {
        check_range(value, ranges::ROBOT_GAIN, "Low Gain")?;
        self.vocoder_low_gain = value;
        Ok(())
    }

    pub fn vocoder_low_bw(&self) -> u8 {
        self.vocoder_low_bw
    }
    pub fn set_vocoder_low_bw(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::ROBOT_WIDTH, "Low Width")?;
        self.vocoder_low_bw = value;
        Ok(())
    }

    pub fn vocoder_mid_freq(&self) -> u8 {
        self.vocoder_mid_freq
    }
    pub fn set_vocoder_mid_freq(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::ROBOT_MID_FREQ, "Mid Freq")?;
        self.vocoder_mid_freq = value;
        Ok(())
    }

    pub fn vocoder_mid_gain(&self) -> i8 {
        self.vocoder_mid_gain
    }
    pub fn set_vocoder_mid_gain(&mut self, value: i8) -> Result<()> {
        check_range(value, ranges::ROBOT_GAIN, "Mid Gain")?;
        self.vocoder_mid_gain = value;
        Ok(())
    }

    pub fn vocoder_mid_bw(&self) -> u8 {
        self.vocoder_mid_bw
    }
    pub fn set_vocoder_mid_bw(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::ROBOT_WIDTH, "Mid Width")?;
        self.vocoder_mid_bw = value;
        Ok(())
    }

    pub fn vocoder_high_freq(&self) -> u8 {
        self.vocoder_high_freq
    }
    pub fn set_vocoder_high_freq(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::ROBOT_HIGH_FREQ, "High Freq")?;
        self.vocoder_high_freq = value;
        Ok(())
    }

    pub fn vocoder_high_gain(&self) -> i8 {
        self.vocoder_high_gain
    }
    pub fn set_vocoder_high_gain(&mut self, value: i8) -> Result<()> {
        check_range(value, ranges::ROBOT_GAIN, "High Gain")?;
        self.vocoder_high_gain = value;
        Ok(())
    }

    pub fn vocoder_high_bw(&self) -> u8 {
        self.vocoder_high_bw
    }
    pub fn set_vocoder_high_bw(&mut self, value: u8) -> Result<()> {
        check_range(value, ranges::ROBOT_WIDTH, "High Width")?;
        self.vocoder_high_bw = value;
        Ok(())
    }
}

#[derive(Default, Debug, EnumIter, EnumProperty, Copy, Clone)]
pub enum RobotStyle {
    #[default]
    #[strum(props(uiIndex = "0"))]
    Robot1,

    #[strum(props(uiIndex = "1"))]
    Robot2,

    #[strum(props(uiIndex = "2"))]
    Robot3,
}

struct RobotPresets {
    synthosc_pulse_width: u8,
    synthosc_waveform: u8,
    vocoder_gate_threshold: i8,
    dry_mix: i8,

    vocoder_low_freq: u8,
    vocoder_low_gain: i8,
    vocoder_low_bw: u8,

    vocoder_mid_freq: u8,
    vocoder_mid_gain: i8,
    vocoder_mid_bw: u8,

    vocoder_high_freq: u8,
    vocoder_high_gain: i8,
    vocoder_high_bw: u8,
}

impl RobotPresets {
    fn get_preset(style: RobotStyle) -> RobotPresets {
        match style {
            Robot1 => RobotPresets {
                synthosc_pulse_width: 50,
                synthosc_waveform: 0,
                vocoder_gate_threshold: -36,
                dry_mix: -6,
                vocoder_low_freq: 88,
                vocoder_low_gain: -10,
                vocoder_low_bw: 0,
                vocoder_mid_freq: 173,
                vocoder_mid_gain: 5,
                vocoder_mid_bw: 32,
                vocoder_high_freq: 182,
                vocoder_high_gain: 0,
                vocoder_high_bw: 0,
            },
            RobotStyle::Robot2 => RobotPresets {
                synthosc_pulse_width: 50,
                synthosc_waveform: 1,
                vocoder_gate_threshold: -36,
                dry_mix: -6,
                vocoder_low_freq: 88,
                vocoder_low_gain: -10,
                vocoder_low_bw: 0,
                vocoder_mid_freq: 173,
                vocoder_mid_gain: 5,
                vocoder_mid_bw: 25,
                vocoder_high_freq: 182,
                vocoder_high_gain: 0,
                vocoder_high_bw: 0,
            },
            RobotStyle::Robot3 => RobotPresets {
                synthosc_pulse_width: 50,
                synthosc_waveform: 2,
                vocoder_gate_threshold: -36,
                dry_mix: -6,
                vocoder_low_freq: 87,
                vocoder_low_gain: 3,
                vocoder_low_bw: 32,
                vocoder_mid_freq: 155,
                vocoder_mid_gain: -2,
                vocoder_mid_bw: 23,
                vocoder_high_freq: 240,
                vocoder_high_gain: 12,
                vocoder_high_bw: 0,
            },
        }
    }
}

numeric_attributes!(RobotEffect {
    synthosc_pulse_width: u8 = "ROBOT_SYNTHOSC_PULSEWIDTH" in ranges::PERCENTAGE,
    synthosc_waveform: u8 = "ROBOT_SYNTHOSC_WAVEFORM" in ranges::ROBOT_WAVEFORM,
    vocoder_gate_threshold: i8 = "ROBOT_VOCODER_GATE_THRESHOLD" in ranges::ROBOT_LEVEL,
    dry_mix: i8 = "ROBOT_DRY_MIX" in ranges::ROBOT_LEVEL,
    vocoder_low_freq: u8 = "ROBOT_VOCODER_LOW_FREQ" in ranges::ROBOT_LOW_FREQ,
    vocoder_low_gain: i8 = "ROBOT_VOCODER_LOW_GAIN" in ranges::ROBOT_GAIN,
    vocoder_low_bw: u8 = "ROBOT_VOCODER_LOW_BW" in ranges::ROBOT_WIDTH,
    vocoder_mid_freq: u8 = "ROBOT_VOCODER_MID_FREQ" in ranges::ROBOT_MID_FREQ,
    vocoder_mid_gain: i8 = "ROBOT_VOCODER_MID_GAIN" in ranges::ROBOT_GAIN,
    vocoder_mid_bw: u8 = "ROBOT_VOCODER_MID_BW" in ranges::ROBOT_WIDTH,
    vocoder_high_freq: u8 = "ROBOT_VOCODER_HIGH_FREQ" in ranges::ROBOT_HIGH_FREQ,
    vocoder_high_gain: i8 = "ROBOT_VOCODER_HIGH_GAIN" in ranges::ROBOT_GAIN,
    vocoder_high_bw: u8 = "ROBOT_VOCODER_HIGH_BW" in ranges::ROBOT_WIDTH,
});
