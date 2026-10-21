use enum_map::Enum;
use json_patch::Patch;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod client;
pub mod clients;
mod device;

pub use device::*;
use goxlr_types::{
    AnimationMode, Button, ButtonColourGroups, ButtonColourOffStyle, ChannelName,
    CompressorAttackTime, CompressorRatio, CompressorReleaseTime, DeviceType, DisplayMode,
    DisplayModeComponents, EchoStyle, EffectBankPresets, EncoderColourTargets, EqFrequencies,
    FaderDisplayStyle, FaderName, GateTimes, GenderStyle, HardTuneSource, HardTuneStyle,
    InputDevice, MegaphoneStyle, MicrophoneType, MiniEqFrequencies, Mix, MuteFunction, MuteState,
    OutputDevice, PitchStyle, ReverbStyle, RobotRange, RobotStyle, SampleBank, SampleButtons,
    SamplePlayOrder, SamplePlaybackMode, SamplerColourTargets, SimpleColourTargets, VersionNumber,
    VodMode, WaterfallDirection,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonRequest {
    Ping,
    GetStatus,
    Daemon(DaemonCommand),
    GetMicLevel(String),
    Command(String, GoXLRCommand),
    RunFirmwareUpdate(String, Option<PathBuf>, bool),
    ContinueFirmwareUpdate(String),
    ClearFirmwareState(String),

    // Which profiles use the sample file(s) at this path (a file, or a folder's content),
    // relative to the samples directory. Answered with SampleUsage.
    GetSampleUsage(String),

    // The links other machines on the network use to reach the interface. Answered with
    // NetworkAccess. Only from this computer.
    GetNetworkAccess,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[allow(clippy::large_enum_variant)]
pub enum DaemonResponse {
    Ok,
    Error(String),
    MicLevel(f64),
    Status(DaemonStatus),
    Patch(Patch),
    SampleUsage(SampleUsage),
    NetworkAccess(NetworkAccess),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct NetworkAccess {
    // Whether the interface is reachable from the network (the setting applies at start)
    pub enabled: bool,
    // Links with the access token, one per address of this machine
    pub urls: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SampleUsage {
    // How many sample files are at the path (1 for a file, the content of a folder)
    pub files: usize,
    // The profiles using any of them
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebsocketRequest {
    pub id: u64,
    pub data: DaemonRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebsocketResponse {
    pub id: u64,
    pub data: DaemonResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum ColourWay {
    Black,
    White,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum PathTypes {
    Profiles,
    MicProfiles,
    Presets,
    Samples,
    Icons,
    Logs,
    Backups,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub enum UpdateState {
    Failed,

    Starting,
    Manifest,
    Download,
    Pause(FirmwareInfo),
    ClearNVR,
    UploadFirmware,
    ValidateUpload,
    HardwareValidate,
    HardwareWrite,
    Complete,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct FirmwareInfo {
    pub path: PathBuf,
    pub device_type: DeviceType,
    pub version: VersionNumber,
}

#[derive(Debug, Copy, Clone, Default, Enum, Serialize, Deserialize, Eq, PartialEq)]
pub enum FirmwareSource {
    #[default]
    Live,
    Beta,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Eq, PartialEq)]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum DaemonCommand {
    OpenUi,
    Activate,
    StopDaemon,
    OpenPath(PathTypes),
    SetLogLevel(LogLevel),
    SetFirmwareSource(FirmwareSource),
    SetShowTrayIcon(bool),
    SetLocale(Option<String>),
    SetTTSEnabled(bool),
    SetAutoStartEnabled(bool),
    SetAllowNetworkAccess(bool),
    SetUiLaunchOnLoad(bool),
    RecoverDefaults(PathTypes),
    SetActivatorPath(Option<PathBuf>),

    SetSampleGainPct(String, u8),
    ApplySampleChange,

    // Sample file management, paths are relative to the samples directory ('/' separated).
    CreateSampleFolder(String),
    // Moves or renames a sample file or folder: (from, to)
    MoveSamplePath(String, String),
    // Moves a sample file, or a folder and everything in it, to the sample trash
    DeleteSamplePath(String),
    // Puts an item from the sample trash back where it was (trash item id)
    RestoreSampleTrash(String),
    // Deletes an item from the sample trash for good, or everything when None
    EmptySampleTrash(Option<String>),
    // Puts a deleted profile or mic profile back, by trash id
    RestoreProfileTrash(String),
    // Deletes a profile from the trash for good (or everything with None)
    EmptyProfileTrash(Option<String>),
    // A new network access token, the links given out before stop working
    RegenerateNetworkToken,
    // Copies a sample next to itself, with a free name such as 'name (2).wav'
    DuplicateSample(String),
    // Edits a sample and saves it as WAV, either replacing the file (the original goes to the
    // trash), or as a new file with the given name in the same folder: (path, edit, new name)
    EditSampleFile(String, SampleEdit, Option<String>),

    HandleMacOSAggregates(bool),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SampleEdit {
    // Parts to remove, as (start, end) times in seconds
    #[serde(default)]
    pub remove: Vec<(f64, f64)>,
    // Raise or lower the volume so the loudest peak is at -1dB
    #[serde(default)]
    pub normalize: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum GoXLRCommand {
    SetShutdownCommands(Vec<GoXLRCommand>),
    SetSleepCommands(Vec<GoXLRCommand>),
    SetWakeCommands(Vec<GoXLRCommand>),
    SetSamplerPreBufferDuration(u16),

    SetFader(FaderName, ChannelName),
    SetFaderMuteFunction(FaderName, MuteFunction),

    SetVolume(ChannelName, u8),
    SetMicrophoneType(MicrophoneType),
    SetMicrophoneGain(MicrophoneType, u16),
    SetRouter(InputDevice, OutputDevice, bool),

    // Cough Button
    SetCoughMuteFunction(MuteFunction),
    SetCoughIsHold(bool),

    // Bleep Button
    SetSwearButtonVolume(i8),

    // EQ Settings
    SetEqMiniGain(MiniEqFrequencies, i8),
    SetEqMiniFreq(MiniEqFrequencies, f32),
    SetEqGain(EqFrequencies, i8),
    SetEqFreq(EqFrequencies, f32),

    // Gate Settings
    SetGateThreshold(i8),
    SetGateAttenuation(u8),
    SetGateAttack(GateTimes),
    SetGateRelease(GateTimes),
    SetGateActive(bool),

    // Compressor..
    SetCompressorThreshold(i8),
    SetCompressorRatio(CompressorRatio),
    SetCompressorAttack(CompressorAttackTime),
    SetCompressorReleaseTime(CompressorReleaseTime),
    SetCompressorMakeupGain(i8),

    // Used to switch between display modes..
    SetElementDisplayMode(DisplayModeComponents, DisplayMode),

    // DeEss
    SetDeeser(u8),

    // Colour Related Settings..
    SetAnimationMode(AnimationMode),
    SetAnimationMod1(u8),
    SetAnimationMod2(u8),
    SetAnimationWaterfall(WaterfallDirection),

    SetGlobalColour(String),

    SetFaderDisplayStyle(FaderName, FaderDisplayStyle),
    SetFaderColours(FaderName, String, String),
    SetAllFaderColours(String, String),
    SetAllFaderDisplayStyle(FaderDisplayStyle),

    SetButtonColours(Button, String, Option<String>),
    SetButtonOffStyle(Button, ButtonColourOffStyle),
    SetButtonGroupColours(ButtonColourGroups, String, Option<String>),
    SetButtonGroupOffStyle(ButtonColourGroups, ButtonColourOffStyle),

    SetSimpleColour(SimpleColourTargets, String),
    SetEncoderColour(EncoderColourTargets, String, String, String),
    SetSampleColour(SamplerColourTargets, String, String, String),
    SetSampleOffStyle(SamplerColourTargets, ButtonColourOffStyle),

    // Effect Related Settings..
    LoadEffectPreset(String),
    RenameActivePreset(String),
    SaveActivePreset(),

    // Reverb
    SetReverbStyle(ReverbStyle),
    SetReverbAmount(u8),
    SetReverbDecay(u16),
    SetReverbEarlyLevel(i8),
    SetReverbTailLevel(i8),
    SetReverbPreDelay(u8),
    SetReverbLowColour(i8),
    SetReverbHighColour(i8),
    SetReverbHighFactor(i8),
    SetReverbDiffuse(i8),
    SetReverbModSpeed(i8),
    SetReverbModDepth(i8),

    // Echo..
    SetEchoStyle(EchoStyle),
    SetEchoAmount(u8),
    SetEchoFeedback(u8),
    SetEchoTempo(u16),
    SetEchoDelayLeft(u16),
    SetEchoDelayRight(u16),
    SetEchoFeedbackLeft(u8),
    SetEchoFeedbackRight(u8),
    SetEchoFeedbackXFBLtoR(u8),
    SetEchoFeedbackXFBRtoL(u8),

    // Pitch
    SetPitchStyle(PitchStyle),
    SetPitchAmount(i8),
    SetPitchCharacter(u8),

    // Gender
    SetGenderStyle(GenderStyle),
    SetGenderAmount(i8),

    // Megaphone
    SetMegaphoneStyle(MegaphoneStyle),
    SetMegaphoneAmount(u8),
    SetMegaphonePostGain(i8),

    // Robot
    SetRobotStyle(RobotStyle),
    SetRobotGain(RobotRange, i8),
    SetRobotFreq(RobotRange, u8),
    SetRobotWidth(RobotRange, u8),
    SetRobotWaveform(u8),
    SetRobotPulseWidth(u8),
    SetRobotThreshold(i8),
    SetRobotDryMix(i8),

    // Hardtune
    SetHardTuneStyle(HardTuneStyle),
    SetHardTuneAmount(u8),
    SetHardTuneRate(u8),
    SetHardTuneWindow(u16),
    SetHardTuneSource(HardTuneSource),

    // Sampler..
    ClearSampleProcessError(),
    SetSamplerFunction(SampleBank, SampleButtons, SamplePlaybackMode),
    SetSamplerOrder(SampleBank, SampleButtons, SamplePlayOrder),
    AddSample(SampleBank, SampleButtons, String),
    SetSampleStartPercent(SampleBank, SampleButtons, usize, f32),
    SetSampleStopPercent(SampleBank, SampleButtons, usize, f32),
    RemoveSampleByIndex(SampleBank, SampleButtons, usize),
    PlaySampleByIndex(SampleBank, SampleButtons, usize),
    PlayNextSample(SampleBank, SampleButtons),
    StopSamplePlayback(SampleBank, SampleButtons),

    // Scribbles
    SetScribbleIcon(FaderName, Option<String>),
    SetScribbleText(FaderName, String),
    SetScribbleNumber(FaderName, String),
    SetScribbleInvert(FaderName, bool),

    // Profile Handling..
    NewProfile(String),
    LoadProfile(String, bool),
    LoadProfileColours(String),
    SaveProfile(),
    SaveProfileAs(String),
    DeleteProfile(String),
    // Renames or copies a profile file: (current name, new name)
    RenameProfile(String, String),
    DuplicateProfile(String, String),
    ReloadSettings(),

    NewMicProfile(String),
    LoadMicProfile(String, bool),
    SaveMicProfile(),
    SaveMicProfileAs(String),
    DeleteMicProfile(String),
    RenameMicProfile(String, String),
    DuplicateMicProfile(String, String),

    // General Settings
    SetMuteHoldDuration(u16),
    SetVCMuteAlsoMuteCM(bool),
    SetMonitorWithFx(bool),
    SetSamplerResetOnClear(bool),
    SetSamplerFadeDuration(u32),
    SetLockFaders(bool),
    SetVodMode(VodMode),

    // These control the current GoXLR 'State'..
    SetActiveEffectPreset(EffectBankPresets),
    SetActiveSamplerBank(SampleBank),
    SetMegaphoneEnabled(bool),
    SetRobotEnabled(bool),
    SetHardTuneEnabled(bool),
    SetFXEnabled(bool),
    SetFaderMuteState(FaderName, MuteState),
    SetCoughMuteState(MuteState),

    // Submix Commands
    SetSubMixEnabled(bool),
    SetSubMixVolume(ChannelName, u8),
    SetSubMixLinked(ChannelName, bool),
    SetSubMixOutputMix(OutputDevice, Mix),

    // Mix Monitoring
    SetMonitorMix(OutputDevice),
}

#[cfg(test)]
mod tests {
    use super::*;
    use goxlr_types::ChannelName;

    // The web UI and third party tools send these exact JSON shapes, changing them breaks them.
    #[test]
    fn request_wire_format() {
        let request = DaemonRequest::Command(
            "S220202153DI7".to_string(),
            GoXLRCommand::SetVolume(ChannelName::Mic, 100),
        );
        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json,
            r#"{"Command":["S220202153DI7",{"SetVolume":["Mic",100]}]}"#
        );

        let parsed: DaemonRequest = serde_json::from_str(&json).unwrap();
        assert!(matches!(
            parsed,
            DaemonRequest::Command(_, GoXLRCommand::SetVolume(ChannelName::Mic, 100))
        ));

        assert_eq!(
            serde_json::to_string(&DaemonRequest::GetStatus).unwrap(),
            r#""GetStatus""#
        );
    }

    #[test]
    fn websocket_wire_format() {
        let request: WebsocketRequest =
            serde_json::from_str(r#"{"id":7,"data":{"Daemon":"OpenUi"}}"#).unwrap();
        assert_eq!(request.id, 7);
        assert!(matches!(
            request.data,
            DaemonRequest::Daemon(DaemonCommand::OpenUi)
        ));

        let response = WebsocketResponse {
            id: 7,
            data: DaemonResponse::Error("Nope".to_string()),
        };
        assert_eq!(
            serde_json::to_string(&response).unwrap(),
            r#"{"id":7,"data":{"Error":"Nope"}}"#
        );
    }
}
