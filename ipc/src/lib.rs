use enum_map::Enum;
use json_patch::Patch;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod client;
pub mod clients;
mod device;
pub mod location;

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

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct NetworkAccess {
    // Whether the interface is reachable from the network (the setting applies at start)
    pub enabled: bool,
    // Links with the access token, one per address of this machine
    pub urls: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SampleUsage {
    // How many sample files are at the path (1 for a file, the content of a folder)
    pub files: usize,
    // The profiles using any of them
    pub profiles: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WebsocketRequest {
    pub id: u64,
    pub data: DaemonRequest,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebsocketResponse {
    pub id: u64,
    pub data: DaemonResponse,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub enum ColourWay {
    Black,
    White,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub enum PathTypes {
    Profiles,
    MicProfiles,
    Presets,
    Samples,
    Icons,
    Logs,
    Backups,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub struct FirmwareInfo {
    pub path: PathBuf,
    pub device_type: DeviceType,
    pub version: VersionNumber,
}

#[derive(Debug, Copy, Clone, Default, Enum, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub enum FirmwareSource {
    #[default]
    Live,
    Beta,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, Eq, PartialEq, JsonSchema)]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    #[default]
    Info,
    Debug,
    Trace,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum DaemonCommand {
    OpenUi,
    Activate,
    StopDaemon,
    // Stops, then starts again with the same arguments (to apply network access, for example)
    RestartDaemon,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct SampleEdit {
    // Parts to remove, as (start, end) times in seconds
    #[serde(default)]
    pub remove: Vec<(f64, f64)>,
    // Raise or lower the volume so the loudest peak is at -1dB
    #[serde(default)]
    pub normalize: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub enum GoXLRCommand {
    SetShutdownCommands(Vec<GoXLRCommand>),
    SetSleepCommands(Vec<GoXLRCommand>),
    SetWakeCommands(Vec<GoXLRCommand>),

    // Scenes: named lists of commands, run in one go (volumes, mutes, routing, effects..)
    SaveScene(String, Vec<GoXLRCommand>),
    RunScene(String),
    DeleteScene(String),
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
    // Records into an empty button of the active bank, as holding it on the device does
    StartSampleRecording(SampleBank, SampleButtons),
    StopSampleRecording(SampleBank, SampleButtons),

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

impl GoXLRCommand {
    /// Whether running this command writes files or the settings. Every command is listed (no
    /// catch-all), so a new one needs a decision here; command lists run at shutdown must not
    /// write (see the daemon's execute_command_list).
    pub fn writes_to_disk(&self) -> bool {
        match self {
            GoXLRCommand::LoadProfile(_, save) | GoXLRCommand::LoadMicProfile(_, save) => *save,

            GoXLRCommand::SetShutdownCommands(..)
            | GoXLRCommand::SetSleepCommands(..)
            | GoXLRCommand::SetWakeCommands(..)
            | GoXLRCommand::SaveScene(..)
            | GoXLRCommand::DeleteScene(..)
            | GoXLRCommand::SetSamplerPreBufferDuration(..)
            | GoXLRCommand::SaveActivePreset(..)
            | GoXLRCommand::NewProfile(..)
            | GoXLRCommand::SaveProfile(..)
            | GoXLRCommand::SaveProfileAs(..)
            | GoXLRCommand::DeleteProfile(..)
            | GoXLRCommand::RenameProfile(..)
            | GoXLRCommand::DuplicateProfile(..)
            | GoXLRCommand::NewMicProfile(..)
            | GoXLRCommand::SaveMicProfile(..)
            | GoXLRCommand::SaveMicProfileAs(..)
            | GoXLRCommand::DeleteMicProfile(..)
            | GoXLRCommand::RenameMicProfile(..)
            | GoXLRCommand::DuplicateMicProfile(..)
            | GoXLRCommand::SetMuteHoldDuration(..)
            | GoXLRCommand::SetVCMuteAlsoMuteCM(..)
            | GoXLRCommand::SetMonitorWithFx(..)
            | GoXLRCommand::SetSamplerResetOnClear(..)
            | GoXLRCommand::SetSamplerFadeDuration(..)
            | GoXLRCommand::SetLockFaders(..)
            | GoXLRCommand::SetVodMode(..) => true,

            GoXLRCommand::RunScene(..)
            | GoXLRCommand::SetFader(..)
            | GoXLRCommand::SetFaderMuteFunction(..)
            | GoXLRCommand::SetVolume(..)
            | GoXLRCommand::SetMicrophoneType(..)
            | GoXLRCommand::SetMicrophoneGain(..)
            | GoXLRCommand::SetRouter(..)
            | GoXLRCommand::SetCoughMuteFunction(..)
            | GoXLRCommand::SetCoughIsHold(..)
            | GoXLRCommand::SetSwearButtonVolume(..)
            | GoXLRCommand::SetEqMiniGain(..)
            | GoXLRCommand::SetEqMiniFreq(..)
            | GoXLRCommand::SetEqGain(..)
            | GoXLRCommand::SetEqFreq(..)
            | GoXLRCommand::SetGateThreshold(..)
            | GoXLRCommand::SetGateAttenuation(..)
            | GoXLRCommand::SetGateAttack(..)
            | GoXLRCommand::SetGateRelease(..)
            | GoXLRCommand::SetGateActive(..)
            | GoXLRCommand::SetCompressorThreshold(..)
            | GoXLRCommand::SetCompressorRatio(..)
            | GoXLRCommand::SetCompressorAttack(..)
            | GoXLRCommand::SetCompressorReleaseTime(..)
            | GoXLRCommand::SetCompressorMakeupGain(..)
            | GoXLRCommand::SetElementDisplayMode(..)
            | GoXLRCommand::SetDeeser(..)
            | GoXLRCommand::SetAnimationMode(..)
            | GoXLRCommand::SetAnimationMod1(..)
            | GoXLRCommand::SetAnimationMod2(..)
            | GoXLRCommand::SetAnimationWaterfall(..)
            | GoXLRCommand::SetGlobalColour(..)
            | GoXLRCommand::SetFaderDisplayStyle(..)
            | GoXLRCommand::SetFaderColours(..)
            | GoXLRCommand::SetAllFaderColours(..)
            | GoXLRCommand::SetAllFaderDisplayStyle(..)
            | GoXLRCommand::SetButtonColours(..)
            | GoXLRCommand::SetButtonOffStyle(..)
            | GoXLRCommand::SetButtonGroupColours(..)
            | GoXLRCommand::SetButtonGroupOffStyle(..)
            | GoXLRCommand::SetSimpleColour(..)
            | GoXLRCommand::SetEncoderColour(..)
            | GoXLRCommand::SetSampleColour(..)
            | GoXLRCommand::SetSampleOffStyle(..)
            | GoXLRCommand::LoadEffectPreset(..)
            | GoXLRCommand::RenameActivePreset(..)
            | GoXLRCommand::SetReverbStyle(..)
            | GoXLRCommand::SetReverbAmount(..)
            | GoXLRCommand::SetReverbDecay(..)
            | GoXLRCommand::SetReverbEarlyLevel(..)
            | GoXLRCommand::SetReverbTailLevel(..)
            | GoXLRCommand::SetReverbPreDelay(..)
            | GoXLRCommand::SetReverbLowColour(..)
            | GoXLRCommand::SetReverbHighColour(..)
            | GoXLRCommand::SetReverbHighFactor(..)
            | GoXLRCommand::SetReverbDiffuse(..)
            | GoXLRCommand::SetReverbModSpeed(..)
            | GoXLRCommand::SetReverbModDepth(..)
            | GoXLRCommand::SetEchoStyle(..)
            | GoXLRCommand::SetEchoAmount(..)
            | GoXLRCommand::SetEchoFeedback(..)
            | GoXLRCommand::SetEchoTempo(..)
            | GoXLRCommand::SetEchoDelayLeft(..)
            | GoXLRCommand::SetEchoDelayRight(..)
            | GoXLRCommand::SetEchoFeedbackLeft(..)
            | GoXLRCommand::SetEchoFeedbackRight(..)
            | GoXLRCommand::SetEchoFeedbackXFBLtoR(..)
            | GoXLRCommand::SetEchoFeedbackXFBRtoL(..)
            | GoXLRCommand::SetPitchStyle(..)
            | GoXLRCommand::SetPitchAmount(..)
            | GoXLRCommand::SetPitchCharacter(..)
            | GoXLRCommand::SetGenderStyle(..)
            | GoXLRCommand::SetGenderAmount(..)
            | GoXLRCommand::SetMegaphoneStyle(..)
            | GoXLRCommand::SetMegaphoneAmount(..)
            | GoXLRCommand::SetMegaphonePostGain(..)
            | GoXLRCommand::SetRobotStyle(..)
            | GoXLRCommand::SetRobotGain(..)
            | GoXLRCommand::SetRobotFreq(..)
            | GoXLRCommand::SetRobotWidth(..)
            | GoXLRCommand::SetRobotWaveform(..)
            | GoXLRCommand::SetRobotPulseWidth(..)
            | GoXLRCommand::SetRobotThreshold(..)
            | GoXLRCommand::SetRobotDryMix(..)
            | GoXLRCommand::SetHardTuneStyle(..)
            | GoXLRCommand::SetHardTuneAmount(..)
            | GoXLRCommand::SetHardTuneRate(..)
            | GoXLRCommand::SetHardTuneWindow(..)
            | GoXLRCommand::SetHardTuneSource(..)
            | GoXLRCommand::ClearSampleProcessError(..)
            | GoXLRCommand::SetSamplerFunction(..)
            | GoXLRCommand::SetSamplerOrder(..)
            | GoXLRCommand::AddSample(..)
            | GoXLRCommand::SetSampleStartPercent(..)
            | GoXLRCommand::SetSampleStopPercent(..)
            | GoXLRCommand::RemoveSampleByIndex(..)
            | GoXLRCommand::PlaySampleByIndex(..)
            | GoXLRCommand::PlayNextSample(..)
            | GoXLRCommand::StopSamplePlayback(..)
            | GoXLRCommand::StartSampleRecording(..)
            | GoXLRCommand::StopSampleRecording(..)
            | GoXLRCommand::SetScribbleIcon(..)
            | GoXLRCommand::SetScribbleText(..)
            | GoXLRCommand::SetScribbleNumber(..)
            | GoXLRCommand::SetScribbleInvert(..)
            | GoXLRCommand::LoadProfileColours(..)
            | GoXLRCommand::ReloadSettings(..)
            | GoXLRCommand::SetActiveEffectPreset(..)
            | GoXLRCommand::SetActiveSamplerBank(..)
            | GoXLRCommand::SetMegaphoneEnabled(..)
            | GoXLRCommand::SetRobotEnabled(..)
            | GoXLRCommand::SetHardTuneEnabled(..)
            | GoXLRCommand::SetFXEnabled(..)
            | GoXLRCommand::SetFaderMuteState(..)
            | GoXLRCommand::SetCoughMuteState(..)
            | GoXLRCommand::SetSubMixEnabled(..)
            | GoXLRCommand::SetSubMixVolume(..)
            | GoXLRCommand::SetSubMixLinked(..)
            | GoXLRCommand::SetSubMixOutputMix(..)
            | GoXLRCommand::SetMonitorMix(..) => false,
        }
    }
}

/// The JSON Schema of what /api/command accepts (a DaemonRequest), for the people writing
/// integrations (Stream Deck, OBS, scripts) and for their tools.
pub fn request_schema() -> schemars::Schema {
    schemars::schema_for!(DaemonRequest)
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

    #[test]
    fn the_request_schema_describes_the_commands() {
        let schema = serde_json::to_string(&request_schema()).unwrap();
        assert!(schema.contains("SetVolume"));
        assert!(schema.contains("LoadProfile"));
        assert!(schema.contains("SetCompressorRatio"));
    }
}
