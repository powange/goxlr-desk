#[cfg(feature = "clap")]
use clap::ValueEnum;
use enum_map::Enum;
#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[cfg(feature = "serde")]
use serde_repr::{Deserialize_repr, Serialize_repr};

use std::fmt::{Display, Formatter};
use strum::{Display, EnumCount, EnumIter};

#[derive(Default, Debug, Copy, Clone, Display, Enum, EnumIter, EnumCount, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ChannelName {
    #[default]
    Mic,
    LineIn,
    Console,
    System,
    Game,
    Chat,
    Sample,
    Music,
    Headphones,
    MicMonitor,
    LineOut,
}

#[derive(Debug, Default, Copy, Clone, Display, Enum, EnumIter, EnumCount, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Mix {
    #[default]
    A,
    B,
}

#[derive(Debug, Copy, Clone, Display, Enum, EnumIter, EnumCount, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SubMixChannelName {
    Mic,
    LineIn,
    Console,
    System,
    Game,
    Chat,
    Sample,
    Music,
}

#[derive(Debug, Copy, Clone, Display, Enum, EnumIter, EnumCount, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FaderName {
    A,
    B,
    C,
    D,
}

#[derive(Copy, Clone, Debug, Display, Enum, EnumIter, EnumCount, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EncoderName {
    Pitch = 0x00,
    Gender = 0x01,
    Reverb = 0x02,
    Echo = 0x03,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FirmwareVersions {
    pub firmware: VersionNumber,
    pub fpga_count: u32,
    pub dice: VersionNumber,
}

#[derive(Clone, Default, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FirmwareDetails {
    pub version: VersionNumber,
    pub change_log: Option<String>,
}

#[derive(Clone, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct VersionNumber(pub u32, pub u32, pub Option<u32>, pub Option<u32>);

impl PartialOrd for VersionNumber {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for VersionNumber {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.0.cmp(&other.0) {
            Ordering::Greater => return Ordering::Greater,
            Ordering::Less => return Ordering::Less,
            Ordering::Equal => {}
        }

        match self.1.cmp(&other.1) {
            Ordering::Greater => return Ordering::Greater,
            Ordering::Less => return Ordering::Less,
            Ordering::Equal => {}
        }

        if let Some(patch) = self.2 {
            if let Some(comparison) = other.2 {
                match patch.cmp(&comparison) {
                    Ordering::Greater => return Ordering::Greater,
                    Ordering::Less => return Ordering::Less,
                    Ordering::Equal => {}
                }
            } else {
                // We have a patch number, but our comparator doesn't, assume we're greater
                return Ordering::Greater;
            }
        } else if other.2.is_some() {
            // Our comparator has a patch number, but we don't, assume we're smaller
            return Ordering::Less;
        }

        if let Some(build) = self.3 {
            if let Some(comparison) = other.3 {
                match build.cmp(&comparison) {
                    Ordering::Greater => return Ordering::Greater,
                    Ordering::Less => return Ordering::Less,
                    Ordering::Equal => {}
                }
            } else {
                return Ordering::Greater;
            }
        } else if other.3.is_some() {
            return Ordering::Less;
        }
        Ordering::Equal
    }
}

impl std::fmt::Display for VersionNumber {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if let Some(patch) = self.2 {
            if let Some(build) = self.3 {
                return write!(f, "{}.{}.{}.{}", self.0, self.1, patch, build);
            }
            return write!(f, "{}.{}.{}", self.0, self.1, patch);
        }

        write!(f, "{}.{}", self.0, self.1)
    }
}

impl std::fmt::Debug for VersionNumber {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(self, f)
    }
}

impl From<String> for VersionNumber {
    fn from(value: String) -> Self {
        let mut version = VersionNumber::default();

        let mut parts = value.split('.');

        // We can't iterate over a tuple, so we need to do this 4 times..
        if let Some(part) = parts.next()
            && let Ok(part) = part.parse()
        {
            version.0 = part;
        }

        if let Some(part) = parts.next()
            && let Ok(part) = part.parse()
        {
            version.1 = part;
        }

        if let Some(part) = parts.next()
            && let Ok(part) = part.parse()
        {
            version.2 = Some(part);
        }

        if let Some(part) = parts.next()
            && let Ok(part) = part.parse()
        {
            version.3 = Some(part);
        }

        version
    }
}

// The ordering here might become important for submixes..
// Under Windows, the Order is Headphones, Broadcast, Chat, Sample, Lineout
#[derive(Debug, Copy, Clone, Display, Enum, EnumIter, EnumCount, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum OutputDevice {
    Headphones,
    BroadcastMix,
    ChatMic,
    Sampler,
    LineOut,
    StreamMix2,
}

#[derive(Debug, Copy, Clone, Display, Enum, EnumIter, EnumCount, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum InputDevice {
    Microphone,
    Chat,
    Music,
    Game,
    Console,
    LineIn,
    System,
    Samples,
}

impl InputDevice {
    pub fn can_from(channel: ChannelName) -> bool {
        InputDevice::try_from(channel).is_ok()
    }
}

/// The input a channel plays from; output channels (headphones, line out, mic monitor) have none.
impl TryFrom<ChannelName> for InputDevice {
    type Error = ChannelName;

    fn try_from(value: ChannelName) -> Result<Self, Self::Error> {
        match value {
            ChannelName::Mic => Ok(InputDevice::Microphone),
            ChannelName::LineIn => Ok(InputDevice::LineIn),
            ChannelName::Console => Ok(InputDevice::Console),
            ChannelName::System => Ok(InputDevice::System),
            ChannelName::Game => Ok(InputDevice::Game),
            ChannelName::Chat => Ok(InputDevice::Chat),
            ChannelName::Sample => Ok(InputDevice::Samples),
            ChannelName::Music => Ok(InputDevice::Music),
            ChannelName::Headphones | ChannelName::MicMonitor | ChannelName::LineOut => Err(value),
        }
    }
}

#[derive(Debug, Eq, Copy, Clone, Display, EnumIter, EnumCount, PartialEq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EffectKey {
    MicInputMute = 0x0158,
    BleepLevel = 0x0073,
    GateMode = 0x0010,
    GateThreshold = 0x0011,
    GateEnabled = 0x0014,
    GateAttenuation = 0x0015,
    GateAttack = 0x0016,
    GateRelease = 0x0017,
    MicCompSelect = 0x014b,
    Equalizer31HzFrequency = 0x0126,
    Equalizer31HzGain = 0x0127,
    Equalizer63HzFrequency = 0x00f8,
    Equalizer63HzGain = 0x00f9,
    Equalizer125HzFrequency = 0x0113,
    Equalizer125HzGain = 0x0114,
    Equalizer250HzFrequency = 0x0129,
    Equalizer250HzGain = 0x012a,
    Equalizer500HzFrequency = 0x0116,
    Equalizer500HzGain = 0x0117,
    Equalizer1KHzFrequency = 0x011d,
    Equalizer1KHzGain = 0x011e,
    Equalizer2KHzFrequency = 0x012c,
    Equalizer2KHzGain = 0x012d,
    Equalizer4KHzFrequency = 0x0120,
    Equalizer4KHzGain = 0x0121,
    Equalizer8KHzFrequency = 0x0109,
    Equalizer8KHzGain = 0x010a,
    Equalizer16KHzFrequency = 0x012f,
    Equalizer16KHzGain = 0x0130,
    CompressorThreshold = 0x013d,
    CompressorRatio = 0x013c,
    CompressorAttack = 0x013e,
    CompressorRelease = 0x013f,
    CompressorMakeUpGain = 0x0140,
    DeEsser = 0x000b,
    ReverbAmount = 0x0076,
    ReverbDecay = 0x002f,
    ReverbEarlyLevel = 0x0037,
    ReverbTailLevel = 0x0039, // Always sent as 0.
    ReverbPredelay = 0x0030,
    ReverbLowColor = 0x0032,
    ReverbHighColor = 0x0033,
    ReverbHighFactor = 0x0034,
    ReverbDiffuse = 0x0031,
    ReverbModSpeed = 0x0035,
    ReverbModDepth = 0x0036,
    ReverbType = 0x002e,
    EchoAmount = 0x0075,
    EchoFeedback = 0x0028,
    EchoTempo = 0x001f,
    EchoDelayL = 0x0022,
    EchoDelayR = 0x0023,
    EchoFeedbackL = 0x0024,
    EchoFeedbackR = 0x0025,
    EchoXFBLtoR = 0x0026,
    EchoXFBRtoL = 0x0027,
    EchoSource = 0x001e,
    EchoDivL = 0x0020,
    EchoDivR = 0x0021,
    EchoFilterStyle = 0x002a,
    PitchAmount = 0x005d,
    PitchCharacter = 0x0167,
    PitchThreshold = 0x0159,
    GenderAmount = 0x0060,
    MegaphoneAmount = 0x003c,
    MegaphonePostGain = 0x0040,
    MegaphoneStyle = 0x003a,
    MegaphoneHP = 0x003d,
    MegaphoneLP = 0x003e,
    MegaphonePreGain = 0x003f,
    MegaphoneDistType = 0x0041,
    MegaphonePresenceGain = 0x0042,
    MegaphonePresenceFC = 0x0043,
    MegaphonePresenceBW = 0x0044,
    MegaphoneBeatboxEnable = 0x0045,
    MegaphoneFilterControl = 0x0046,
    MegaphoneFilter = 0x0047,
    MegaphoneDrivePotGainCompMid = 0x0048,
    MegaphoneDrivePotGainCompMax = 0x0049,
    RobotLowGain = 0x0134,
    RobotLowFreq = 0x0133,
    RobotLowWidth = 0x0135,
    RobotMidGain = 0x013a,
    RobotMidFreq = 0x0139,
    RobotMidWidth = 0x013b,
    RobotHiGain = 0x0137,
    RobotHiFreq = 0x0136,
    RobotHiWidth = 0x0138,
    RobotWaveform = 0x0147,
    RobotPulseWidth = 0x0146,
    RobotThreshold = 0x0157,
    RobotDryMix = 0x014d,
    RobotStyle = 0x0000,
    HardTuneKeySource = 0x0059, // Always sent as 0.
    HardTuneAmount = 0x005a,
    HardTuneRate = 0x005c,
    HardTuneWindow = 0x005b,
    HardTuneScale = 0x005e,
    HardTunePitchAmount = 0x005f,

    RobotEnabled = 0x014e,
    MegaphoneEnabled = 0x00d7,
    HardTuneEnabled = 0x00d8,

    Encoder1Enabled = 0x00d5,
    Encoder2Enabled = 0x00d6,
    Encoder3Enabled = 0x0150,
    Encoder4Enabled = 0x0151,
}

// Eq and Hash allow for these to be added to a HashSet (the values make EnumSet unusable)
#[derive(Debug, Copy, Clone, Eq, Display, EnumIter, EnumCount, PartialEq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MicrophoneParamKey {
    MicType = 0x000,
    DynamicGain = 0x001,
    CondenserGain = 0x002,
    JackGain = 0x003,
    GateThreshold = 0x30200,
    GateAttack = 0x30400,
    GateRelease = 0x30600,
    GateAttenuation = 0x30900,
    CompressorThreshold = 0x60200,
    CompressorRatio = 0x60300,
    CompressorAttack = 0x60400,
    CompressorRelease = 0x60600,
    CompressorMakeUpGain = 0x60700,
    BleepLevel = 0x70100,

    /*
     These are the values for the GoXLR mini, it seems there's a difference in how the two
     are setup, The Mini does EQ via mic parameters, where as the full does it via effects.
    */
    Equalizer90HzFrequency = 0x40000,
    Equalizer90HzGain = 0x40001,
    Equalizer250HzFrequency = 0x40003,
    Equalizer250HzGain = 0x40004,
    Equalizer500HzFrequency = 0x40006,
    Equalizer500HzGain = 0x40007,
    Equalizer1KHzFrequency = 0x50000,
    Equalizer1KHzGain = 0x50001,
    Equalizer3KHzFrequency = 0x50003,
    Equalizer3KHzGain = 0x50004,
    Equalizer8KHzFrequency = 0x50006,
    Equalizer8KHzGain = 0x50007,
}

#[derive(Debug, Copy, Clone, Display, EnumIter, EnumCount, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum FaderDisplayStyle {
    TwoColour,
    Gradient,
    Meter,
    GradientMeter,
}

#[derive(Debug, Copy, Clone, Display, Enum, EnumIter, EnumCount, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum Button {
    // These are all the buttons from the GoXLR Mini.
    Fader1Mute,
    Fader2Mute,
    Fader3Mute,
    Fader4Mute,
    Bleep,
    Cough,

    // The rest are GoXLR Full Buttons. On the mini, they will simply be ignored.
    EffectSelect1,
    EffectSelect2,
    EffectSelect3,
    EffectSelect4,
    EffectSelect5,
    EffectSelect6,

    // FX Button labelled as 'fxClear' in config?
    EffectFx,
    EffectMegaphone,
    EffectRobot,
    EffectHardTune,

    SamplerSelectA,
    SamplerSelectB,
    SamplerSelectC,

    SamplerTopLeft,
    SamplerTopRight,
    SamplerBottomLeft,
    SamplerBottomRight,
    SamplerClear,
}

#[derive(Debug, Copy, Clone, Display, EnumIter, EnumCount, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SimpleColourTargets {
    Global,
    Accent,
    Scribble1,
    Scribble2,
    Scribble3,
    Scribble4,
}

#[derive(Debug, Copy, Clone, Display, EnumIter, EnumCount, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SamplerColourTargets {
    SamplerSelectA,
    SamplerSelectB,
    SamplerSelectC,
}

#[derive(Debug, Copy, Clone, Display, EnumIter, EnumCount, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EncoderColourTargets {
    Reverb,
    Pitch,
    Echo,
    Gender,
}

#[derive(Debug, Copy, Clone, Display, EnumIter, EnumCount, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ButtonColourGroups {
    FaderMute,
    EffectSelector,
    EffectTypes,
}

#[derive(Debug, Copy, Clone, Display, EnumIter, EnumCount, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ButtonColourOffStyle {
    Dimmed,
    Colour2,
    DimmedColour2,
}

// MuteChat
#[derive(Debug, Copy, Clone, Display, EnumIter, EnumCount, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MuteFunction {
    All,
    ToStream,
    ToVoiceChat,
    ToPhones,
    ToLineOut,
    ToStream2,
    ToStreams,
}

#[derive(Debug, Copy, Clone, Display, Enum, EnumIter, EnumCount, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MicrophoneType {
    Dynamic,
    Condenser,
    Jack,
}

impl MicrophoneType {
    pub fn get_gain_param(&self) -> MicrophoneParamKey {
        match self {
            MicrophoneType::Dynamic => MicrophoneParamKey::DynamicGain,
            MicrophoneType::Condenser => MicrophoneParamKey::CondenserGain,
            MicrophoneType::Jack => MicrophoneParamKey::JackGain,
        }
    }

    pub fn has_phantom_power(&self) -> bool {
        matches!(self, MicrophoneType::Condenser)
    }
}

#[derive(Debug, Copy, Clone, Display, EnumIter, EnumCount, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EffectBankPresets {
    Preset1,
    Preset2,
    Preset3,
    Preset4,
    Preset5,
    Preset6,
}

#[derive(Debug, Copy, Clone, Display, Enum, EnumIter, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SampleBank {
    A,
    B,
    C,
}

#[derive(Debug, Copy, Clone, Display, EnumIter, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MiniEqFrequencies {
    Equalizer90Hz,
    Equalizer250Hz,
    Equalizer500Hz,
    Equalizer1KHz,
    Equalizer3KHz,
    Equalizer8KHz,
}

#[derive(Debug, Copy, Clone, Display, EnumIter, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EqFrequencies {
    Equalizer31Hz,
    Equalizer63Hz,
    Equalizer125Hz,
    Equalizer250Hz,
    Equalizer500Hz,
    Equalizer1KHz,
    Equalizer2KHz,
    Equalizer4KHz,
    Equalizer8KHz,
    Equalizer16KHz,
}

/*
Ok, before we get started with these next couple of enums, lemme explain how the GoXLR works for
certain values. While the UI under windows appears to display a range, these values are all mapped
to fixed values in an array (eg. goxlr_shared.h line 415), and the index of that value is sent to
the GoXLR. This will most often occur for values that aren't linear, the ratio starts at increments
of 0.1, and by the end it's hitting increments of 16 and 32.

These enums are essentially the same maps, and use 'as usize' and strum::iter().nth to convert.
 */

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize_repr, Deserialize_repr))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema_repr))]
#[repr(u8)]
pub enum CompressorRatio {
    Ratio1_0,
    Ratio1_1,
    Ratio1_2,
    Ratio1_4,
    Ratio1_6,
    Ratio1_8,
    Ratio2_0,
    Ratio2_5,
    Ratio3_2,
    Ratio4_0,
    Ratio5_6,
    Ratio8_0,
    Ratio16_0,
    Ratio32_0,
    Ratio64_0,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize_repr, Deserialize_repr))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema_repr))]
#[repr(u8)]
pub enum GateTimes {
    Gate10ms,
    Gate20ms,
    Gate30ms,
    Gate40ms,
    Gate50ms,
    Gate60ms,
    Gate70ms,
    Gate80ms,
    Gate90ms,
    Gate100ms,
    Gate110ms,
    Gate120ms,
    Gate130ms,
    Gate140ms,
    Gate150ms,
    Gate160ms,
    Gate170ms,
    Gate180ms,
    Gate190ms,
    Gate200ms,
    Gate250ms,
    Gate300ms,
    Gate350ms,
    Gate400ms,
    Gate450ms,
    Gate500ms,
    Gate550ms,
    Gate600ms,
    Gate650ms,
    Gate700ms,
    Gate750ms,
    Gate800ms,
    Gate850ms,
    Gate900ms,
    Gate950ms,
    Gate1000ms,
    Gate1100ms,
    Gate1200ms,
    Gate1300ms,
    Gate1400ms,
    Gate1500ms,
    Gate1600ms,
    Gate1700ms,
    Gate1800ms,
    Gate1900ms,
    Gate2000ms,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize_repr, Deserialize_repr))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema_repr))]
#[repr(u8)]
pub enum CompressorAttackTime {
    // Note: 0ms is technically 0.001ms
    Comp0ms,
    Comp2ms,
    Comp3ms,
    Comp4ms,
    Comp5ms,
    Comp6ms,
    Comp7ms,
    Comp8ms,
    Comp9ms,
    Comp10ms,
    Comp12ms,
    Comp14ms,
    Comp16ms,
    Comp18ms,
    Comp20ms,
    Comp23ms,
    Comp26ms,
    Comp30ms,
    Comp35ms,
    Comp40ms,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize_repr, Deserialize_repr))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema_repr))]
#[repr(u8)]
pub enum CompressorReleaseTime {
    // Note: 0 is technically 15 :)
    Comp0ms,
    Comp15ms,
    Comp25ms,
    Comp35ms,
    Comp45ms,
    Comp55ms,
    Comp65ms,
    Comp75ms,
    Comp85ms,
    Comp100ms,
    Comp115ms,
    Comp140ms,
    Comp170ms,
    Comp230ms,
    Comp340ms,
    Comp680ms,
    Comp1000ms,
    Comp1500ms,
    Comp2000ms,
    Comp3000ms,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum ReverbStyle {
    Library,
    DarkBloom,
    MusicClub,
    RealPlate,
    Chapel,
    HockeyArena,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum EchoStyle {
    Quarter,
    Eighth,
    Triplet,
    PingPong,
    ClassicSlap,
    MultiTap,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum PitchStyle {
    Narrow,
    Wide,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum GenderStyle {
    Narrow,
    Medium,
    Wide,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MegaphoneStyle {
    Megaphone,
    Radio,
    OnThePhone,
    Overdrive,
    BuzzCutt,
    Tweed,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum RobotStyle {
    Robot1,
    Robot2,
    Robot3,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum RobotRange {
    Low,
    Medium,
    High,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum HardTuneStyle {
    Natural,
    Medium,
    Hard,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum HardTuneSource {
    All,
    Music,
    Game,
    LineIn,
    System,
}

#[derive(Debug, Copy, Clone, Enum, EnumIter, Display, PartialEq, Eq, Hash)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SampleButtons {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SamplePlaybackMode {
    PlayNext,
    PlayStop,
    PlayFade,
    StopOnRelease,
    FadeOnRelease,
    Loop,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum SamplePlayOrder {
    Sequential,
    Random,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DisplayMode {
    Simple,
    Advanced,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DisplayModeComponents {
    NoiseGate,
    Equaliser,
    Compressor,
    EqFineTune,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum MuteState {
    Unmuted,
    MutedToX,
    MutedToAll,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum AnimationMode {
    RetroRainbow,
    RainbowDark,
    RainbowBright,
    Simple,
    Ripple,
    None,
}

#[derive(Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum WaterfallDirection {
    Down,
    Up,
    Off,
}

#[derive(Default, Debug, Copy, Clone, EnumIter, Display, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum VodMode {
    #[default]
    Routable,
    StreamNoMusic,
}

#[derive(Default, Debug, Copy, Clone, Enum, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DeviceType {
    #[default]
    Unknown,
    Full,
    Mini,
}

#[derive(Default, Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "clap", derive(ValueEnum))]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub enum DriverInterface {
    #[default]
    TUSB,
    LIBUSB,
}

/// The values the effect settings accept. The client, the profile setters and the loading of a
/// profile all use these, so a value is accepted (or refused) the same way everywhere.
pub mod effect_ranges {
    use std::ops::RangeInclusive;

    pub const PERCENTAGE: RangeInclusive<i64> = 0..=100;
    // The position of an encoder: 0..=24 for Reverb and Echo, -24..=24 for Pitch and Gender
    pub const KNOB: RangeInclusive<i64> = 0..=24;
    pub const KNOB_CENTRED: RangeInclusive<i64> = -24..=24;

    pub const REVERB_DECAY_MILLIS: RangeInclusive<i64> = 10..=20000;
    /// The decay as stored in a profile (290 is 20 s, see ReverbEncoder::get_decay_millis).
    pub const REVERB_DECAY: RangeInclusive<i64> = 0..=290;
    pub const REVERB_PREDELAY: RangeInclusive<i64> = 0..=100;
    pub const REVERB_DIFFUSE: RangeInclusive<i64> = -50..=50;
    pub const REVERB_COLOUR: RangeInclusive<i64> = -50..=50;
    pub const REVERB_HIGH_FACTOR: RangeInclusive<i64> = -25..=25;
    pub const REVERB_MOD: RangeInclusive<i64> = -25..=25;
    pub const REVERB_LEVEL: RangeInclusive<i64> = -25..=0;

    pub const ECHO_DELAY: RangeInclusive<i64> = 0..=2500;
    pub const ECHO_TEMPO: RangeInclusive<i64> = 45..=300;

    // The widest, Narrow and Hard Tune accept less (checked against the style)
    pub const PITCH_AMOUNT: RangeInclusive<i64> = -24..=24;
    pub const GENDER_AMOUNT: RangeInclusive<i64> = -50..=50;

    pub const MEGAPHONE_POST_GAIN: RangeInclusive<i64> = -20..=20;

    pub const ROBOT_GAIN: RangeInclusive<i64> = -12..=12;
    pub const ROBOT_LOW_FREQ: RangeInclusive<i64> = 0..=88;
    pub const ROBOT_MID_FREQ: RangeInclusive<i64> = 86..=184;
    pub const ROBOT_HIGH_FREQ: RangeInclusive<i64> = 182..=240;
    // Any of the three (the range given with it picks the one that applies)
    pub const ROBOT_FREQ: RangeInclusive<i64> = 0..=240;
    pub const ROBOT_WIDTH: RangeInclusive<i64> = 0..=32;
    pub const ROBOT_WAVEFORM: RangeInclusive<i64> = 0..=3;
    pub const ROBOT_LEVEL: RangeInclusive<i64> = -36..=0;

    pub const HARDTUNE_WINDOW: RangeInclusive<i64> = 0..=600;

    /// Checks `value` against `range`, the error names the setting.
    pub fn check<T: Into<i64>>(
        value: T,
        range: RangeInclusive<i64>,
        name: &str,
    ) -> Result<(), String> {
        if range.contains(&value.into()) {
            Ok(())
        } else {
            Err(format!(
                "{name} should be between {} and {}",
                range.start(),
                range.end()
            ))
        }
    }
}
