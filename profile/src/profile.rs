use std::fs::File;
use std::io::{BufReader, Read, Write};
use std::path::Path;
use std::str::FromStr;

use anyhow::{Result, anyhow, bail};
use enum_map::{EnumMap, enum_map};
use log::{debug, warn};
use quick_xml::events::{BytesDecl, BytesStart, Event};
use quick_xml::{Reader, Writer};
use strum::EnumProperty;
use strum::IntoEnumIterator;
use zip::write::SimpleFileOptions;

use crate::SampleButtons::{BottomLeft, BottomRight, Clear, TopLeft, TopRight};
use crate::atomic::atomic_write;
use crate::components::animation::AnimationTree;
use crate::components::browser::BrowserPreviewTree;
use crate::components::context::Context;
use crate::components::echo::EchoEncoderBase;
use crate::components::effects::Effects;
use crate::components::fader::Fader;
use crate::components::gender::GenderEncoderBase;
use crate::components::hardtune::HardtuneEffectBase;
use crate::components::megaphone::MegaphoneEffectBase;
use crate::components::mixer::{InputChannels, Mixers, OutputChannels};
use crate::components::mute::MuteButton;
use crate::components::mute_chat::MuteChat;
use crate::components::pitch::PitchEncoderBase;
use crate::components::preset_writer::PresetWriter;
use crate::components::reverb::ReverbEncoderBase;
use crate::components::robot::RobotEffectBase;
use crate::components::root::RootElement;
use crate::components::sample::SampleBase;
use crate::components::scribble::Scribble;
use crate::components::simple::{SimpleElement, SimpleElements};
use crate::components::submix::mix_routing_tree::{Mix, MixRoutingTree};
use crate::components::submix::submixer::SubMixer;
use crate::{Faders, Preset, SampleButtons};

#[derive(Debug)]
pub struct Profile {
    settings: ProfileSettings,
    scribbles: [Vec<u8>; 4],
}

#[derive(Debug)]
pub struct Attribute {
    pub(crate) name: String,
    pub(crate) value: String,
}

// A screen image is a few KB; anything this large isn't one
const MAX_SCRIBBLE_SIZE: u64 = 1024 * 1024;

impl Profile {
    pub fn load<R: Read + std::io::Seek>(read: R) -> Result<Self> {
        debug!("Loading Profile Archive..");

        let mut archive = zip::ZipArchive::new(read)?;

        let mut scribbles: [Vec<u8>; 4] = Default::default();

        // Load the scribbles if they exist, store them in memory for later fuckery.
        for (i, scribble) in scribbles.iter_mut().enumerate() {
            let filename = format!("scribble{}.png", i + 1);
            if let Ok(file) = archive.by_name(filename.as_str()) {
                // The size comes from the archive itself: read at most a sane amount
                let mut data = Vec::new();
                file.take(MAX_SCRIBBLE_SIZE + 1).read_to_end(&mut data)?;
                if data.len() as u64 > MAX_SCRIBBLE_SIZE {
                    warn!("Ignoring {}, it's too large for a screen image", filename);
                    continue;
                }
                *scribble = data;
            }
        }

        debug!("Attempting to read profile.xml..");
        let result = ProfileSettings::load(archive.by_name("profile.xml")?);
        match result {
            Ok(settings) => Ok(Profile {
                settings,
                scribbles,
            }),
            Err(e) => {
                warn!("Unable to Load Profile: {}", e);
                bail!("Unable to Load Profile");
            }
        }
    }

    // Ok, this is better.
    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<()> {
        debug!("Saving Profile to {:?}", path.as_ref());
        atomic_write(path, |file| {
            // Create a new ZipFile at the requested location
            let mut archive = zip::ZipWriter::new(file);

            // Store the profile..
            archive.start_file("profile.xml", SimpleFileOptions::default())?;
            self.settings.write_to(&mut archive)?;

            // Write the scribbles..
            for (i, scribble) in self.scribbles.iter().enumerate() {
                // Only write if there's actually data stored..
                if !self.scribbles[i].is_empty() {
                    let filename = format!("scribble{}.png", i + 1);
                    archive.start_file(filename, SimpleFileOptions::default())?;
                    archive.write_all(scribble)?;
                }
            }
            archive.finish()?;
            Ok(())
        })
    }

    /// A hash of what `save` would write, to tell whether the profile changed since it was
    /// loaded or saved.
    pub fn fingerprint(&mut self) -> Result<u64> {
        let mut xml = Vec::new();
        self.settings.write_to(&mut xml)?;
        let scribbles: Vec<&[u8]> = self.scribbles.iter().map(|s| s.as_slice()).collect();
        crate::fingerprint::xml_fingerprint(&xml, &scribbles)
    }

    pub fn save_preset(&self, path: impl AsRef<Path>) -> Result<()> {
        self.settings.write_preset(path)?;
        Ok(())
    }

    pub fn settings(&self) -> &ProfileSettings {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut ProfileSettings {
        &mut self.settings
    }

    pub fn get_scribble(&self, id: usize) -> &Vec<u8> {
        &self.scribbles[id]
    }
}

#[derive(Debug)]
pub struct ProfileSettings {
    root: RootElement,
    browser: BrowserPreviewTree,
    animation_tree: AnimationTree,
    mix_routing: MixRoutingTree,
    submix_tree: SubMixer,
    mixer: Mixers,
    context: Context,
    mute_chat: MuteChat,

    faders: EnumMap<Faders, Fader>,
    mute_buttons: EnumMap<Faders, MuteButton>,
    scribbles: EnumMap<Faders, Scribble>,

    sampler_map: EnumMap<SampleButtons, SampleBase>,
    simple_elements: EnumMap<SimpleElements, SimpleElement>,

    effects: EnumMap<Preset, Effects>,
    megaphone_effect: MegaphoneEffectBase,
    robot_effect: RobotEffectBase,
    hardtune_effect: HardtuneEffectBase,
    reverb_encoder: ReverbEncoderBase,
    echo_encoder: EchoEncoderBase,
    pitch_encoder: PitchEncoderBase,
    gender_encoder: GenderEncoderBase,
}

impl ProfileSettings {
    pub fn load<R: Read>(read: R) -> Result<Self> {
        // Wrap our reader into a Buffered Reader for parsing..
        let buf_reader = BufReader::new(read);
        let mut reader = Reader::from_reader(buf_reader);

        debug!("Preparing Structure..");

        let mut root = RootElement::new();
        let mut browser = BrowserPreviewTree::new("browserPreviewTree".to_string());

        let mut animation_tree = AnimationTree::new("animationTree".to_string());

        let mut mix_routing = MixRoutingTree::new();
        let mut submix_tree = SubMixer::new();

        let mut mixer = Mixers::new();
        let mut context = Context::new("selectedContext".to_string());
        let mut mute_chat = MuteChat::new("muteChat".to_string());

        let mut faders = enum_map! {
            Faders::A => Fader::new(Faders::A),
            Faders::B => Fader::new(Faders::B),
            Faders::C => Fader::new(Faders::C),
            Faders::D => Fader::new(Faders::D),
        };

        let mut mute_buttons = enum_map! {
            Faders::A => MuteButton::new(Faders::A),
            Faders::B => MuteButton::new(Faders::B),
            Faders::C => MuteButton::new(Faders::C),
            Faders::D => MuteButton::new(Faders::D),
        };

        // Create Defaults For the Scribbles..
        let mut scribbles = enum_map! {
            Faders::A => Scribble::new(Faders::A),
            Faders::B => Scribble::new(Faders::B),
            Faders::C => Scribble::new(Faders::C),
            Faders::D => Scribble::new(Faders::D)
        };

        let mut effects = enum_map! {
            Preset::Preset1 => Effects::new(Preset::Preset1),
            Preset::Preset2 => Effects::new(Preset::Preset2),
            Preset::Preset3 => Effects::new(Preset::Preset3),
            Preset::Preset4 => Effects::new(Preset::Preset4),
            Preset::Preset5 => Effects::new(Preset::Preset5),
            Preset::Preset6 => Effects::new(Preset::Preset6),
        };

        let mut simple_elements = enum_map! {
            SimpleElements::SampleBankA => SimpleElement::new(SimpleElements::SampleBankA),
            SimpleElements::SampleBankB => SimpleElement::new(SimpleElements::SampleBankB),
            SimpleElements::SampleBankC => SimpleElement::new(SimpleElements::SampleBankC),
            SimpleElements::FxClear => SimpleElement::new(SimpleElements::FxClear),
            SimpleElements::Swear => SimpleElement::new(SimpleElements::Swear),
            SimpleElements::GlobalColour => SimpleElement::new(SimpleElements::GlobalColour),
            SimpleElements::LogoX => SimpleElement::new(SimpleElements::LogoX),
        };

        //let mut simple: EnumMap<SimpleElements, Option<SimpleElement>> = Default::default();

        let mut megaphone_effect = MegaphoneEffectBase::new("megaphoneEffect".to_string());
        let mut robot_effect = RobotEffectBase::new("robotEffect".to_string());
        let mut hardtune_effect = HardtuneEffectBase::new("hardtuneEffect".to_string());
        let mut reverb_encoder = ReverbEncoderBase::new("reverbEncoder".to_string());
        let mut echo_encoder = EchoEncoderBase::new("echoEncoder".to_string());
        let mut pitch_encoder = PitchEncoderBase::new("pitchEncoder".to_string());
        let mut gender_encoder = GenderEncoderBase::new("genderEncoder".to_string());

        let mut sampler_map = enum_map! {
            TopLeft => SampleBase::new(TopLeft),
            TopRight => SampleBase::new(TopRight),
            BottomLeft => SampleBase::new(BottomLeft),
            BottomRight => SampleBase::new(BottomRight),
            Clear => SampleBase::new(Clear),
        };

        // This value isn't stored in the struct.
        let mut active_sample_button: Option<&mut SampleBase> = None;

        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_event_into(&mut buf) {
                // Applies to most tags, represents a tag with no child
                Ok(Event::Empty(ref e)) => {
                    let (name, attributes) = wrap_start_event(e)?;
                    if name == "browserPreviewTree" {
                        browser.parse_browser(&attributes)?;
                        continue;
                    }

                    if name == "animationTree" {
                        animation_tree.parse_animation(&attributes)?;
                        continue;
                    }

                    if name == "mixRoutingTree" {
                        mix_routing.parse_mix_tree(&attributes)?;
                        continue;
                    }

                    if name == "monitorTree" {
                        submix_tree.parse_monitor(&attributes)?;
                        continue;
                    }

                    if name == "linkingTree" {
                        submix_tree.parse_linking(&attributes)?;
                        continue;
                    }

                    if name == "mixerTree" {
                        mixer.parse_mixers(&attributes)?;
                        continue;
                    }

                    if name == "selectedContext" {
                        context.parse_context(&attributes)?;
                        continue;
                    }

                    if name == "muteChat" {
                        mute_chat.parse_mute_chat(&attributes)?;
                        continue;
                    }

                    if name.starts_with("FaderMeter") {
                        for fader in Faders::iter() {
                            if fader.get_str("faderContext").unwrap() == name {
                                faders[fader].parse_fader(&attributes)?;
                                break;
                            }
                        }
                        continue;
                    }

                    // Might need to pattern match this..
                    if name.starts_with("mute") && name != "muteChat" {
                        for fader in Faders::iter() {
                            if fader.get_str("muteContext").unwrap() == name {
                                mute_buttons[fader].parse_button(&attributes)?;
                                break;
                            }
                        }
                        continue;
                    }

                    if name.starts_with("scribble") {
                        for fader in Faders::iter() {
                            if fader.get_str("scribbleContext").unwrap() == name {
                                scribbles[fader].parse_scribble(&attributes)?;
                                break;
                            }
                        }

                        continue;
                    }

                    if name.starts_with("effects") {
                        for preset in Preset::iter() {
                            if preset.get_str("contextTitle").unwrap() == name {
                                effects[preset].parse_effect(&attributes)?;
                                break;
                            }
                        }
                        continue;
                    }

                    if name.starts_with("megaphoneEffectpreset")
                        && let Ok(preset) = ProfileSettings::parse_preset(name.clone())
                    {
                        megaphone_effect.parse_megaphone_preset(preset, &attributes)?;
                        continue;
                    }

                    if name.starts_with("robotEffectpreset")
                        && let Ok(preset) = ProfileSettings::parse_preset(name.clone())
                    {
                        robot_effect.parse_robot_preset(preset, &attributes)?;
                        continue;
                    }

                    if name.starts_with("hardtuneEffectpreset")
                        && let Ok(preset) = ProfileSettings::parse_preset(name.clone())
                    {
                        hardtune_effect.parse_hardtune_preset(preset, &attributes)?;
                        continue;
                    }

                    if name.starts_with("reverbEncoderpreset")
                        && let Ok(preset) = ProfileSettings::parse_preset(name.clone())
                    {
                        reverb_encoder.parse_reverb_preset(preset, &attributes)?;
                        continue;
                    }

                    if name.starts_with("echoEncoderpreset")
                        && let Ok(preset) = ProfileSettings::parse_preset(name.clone())
                    {
                        echo_encoder.parse_echo_preset(preset, &attributes)?;
                        continue;
                    }

                    if name.starts_with("pitchEncoderpreset")
                        && let Ok(preset) = ProfileSettings::parse_preset(name.clone())
                    {
                        pitch_encoder.parse_pitch_preset(preset, &attributes)?;
                        continue;
                    }

                    if name.starts_with("genderEncoderpreset")
                        && let Ok(preset) = ProfileSettings::parse_preset(name.clone())
                    {
                        gender_encoder.parse_gender_preset(preset, &attributes)?;
                        continue;
                    }

                    if name.starts_with("sampleStack")
                        && let Some(id) = name.chars().last()
                        && let Some(button) = &mut active_sample_button
                    {
                        button.parse_sample_stack(id, &attributes)?;
                        continue;
                    }

                    if name.starts_with("sampleBank")
                        || name == "fxClear"
                        || name == "swear"
                        || name == "globalColour"
                        || name == "logoX"
                    {
                        // In this case, the tag name, and attribute prefixes are the same.. An
                        // element this version doesn't know (a 'sampleBankD') is skipped rather
                        // than failing the whole profile.
                        match SimpleElements::from_str(&name) {
                            Ok(element) => simple_elements[element].parse_simple(&attributes)?,
                            Err(_) => warn!("Skipping unknown element {}", name),
                        }

                        continue;
                    }

                    if name == "AppTree" {
                        // This is handled by ValueTreeRoot
                        continue;
                    }

                    warn!("Unhandled Tag: {}", name);
                }

                // Represents a tag which has children
                Ok(Event::Start(ref e)) => {
                    let (name, attributes) = wrap_start_event(e)?;

                    if name == "ValueTreeRoot" {
                        // This also handles <AppTree, due to a single shared value.
                        root.parse_root(&attributes)?;

                        // This code was made for XML version 2, v1 not currently supported.
                        if root.get_version() > 3 {
                            bail!("Unsupported Profile Version {}", root.get_version());
                        }
                        continue;
                    }

                    if name == "submixerTree" {
                        submix_tree.parse_submixer(&attributes)?;
                        continue;
                    }

                    if name == "megaphoneEffect" {
                        megaphone_effect.parse_megaphone_root(&attributes)?;
                        continue;
                    }

                    if name == "robotEffect" {
                        robot_effect.parse_robot_root(&attributes)?;
                        continue;
                    }

                    if name == "hardtuneEffect" {
                        hardtune_effect.parse_hardtune_root(&attributes)?;
                        continue;
                    }

                    if name == "reverbEncoder" {
                        reverb_encoder.parse_reverb_root(&attributes)?;
                        continue;
                    }

                    if name == "echoEncoder" {
                        echo_encoder.parse_echo_root(&attributes)?;
                        continue;
                    }

                    if name == "pitchEncoder" {
                        pitch_encoder.parse_pitch_root(&attributes)?;
                        continue;
                    }

                    if name == "genderEncoder" {
                        gender_encoder.parse_gender_root(&attributes)?;
                        continue;
                    }

                    // These can probably be a little cleaner..
                    if name == "sampleTopLeft" {
                        sampler_map[TopLeft].parse_sample_root(&attributes)?;
                        active_sample_button = Some(&mut sampler_map[TopLeft]);
                        continue;
                    }

                    if name == "sampleTopRight" {
                        sampler_map[TopRight].parse_sample_root(&attributes)?;
                        active_sample_button = Some(&mut sampler_map[TopRight]);
                        continue;
                    }

                    if name == "sampleBottomLeft" {
                        sampler_map[BottomLeft].parse_sample_root(&attributes)?;
                        active_sample_button = Some(&mut sampler_map[BottomLeft]);
                        continue;
                    }

                    if name == "sampleBottomRight" {
                        sampler_map[BottomRight].parse_sample_root(&attributes)?;
                        active_sample_button = Some(&mut sampler_map[BottomRight]);
                        continue;
                    }

                    if name == "sampleClear" {
                        sampler_map[Clear].parse_sample_root(&attributes)?;
                        active_sample_button = Some(&mut sampler_map[Clear]);
                        continue;
                    }
                }

                // Ends a tag with children
                Ok(Event::End(_)) => {}
                Ok(Event::Eof) => {
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    bail!("Error Parsing Profile: {}", e);
                }
            }
        }

        debug!("{:?}", mix_routing);
        debug!("{:?}", submix_tree);

        Ok(Self {
            root,
            browser,
            animation_tree,
            mix_routing,
            submix_tree,
            mixer,
            context,
            mute_chat,
            faders,
            mute_buttons,
            scribbles,
            effects,
            sampler_map,
            simple_elements,
            megaphone_effect,
            robot_effect,
            hardtune_effect,
            reverb_encoder,
            echo_encoder,
            pitch_encoder,
            gender_encoder,
        })
    }

    /// Loads an effect preset into the active effect bank. It's all or nothing: a preset that
    /// can't be read completely leaves the bank as it was.
    pub fn load_preset<R: Read>(&mut self, read: R) -> Result<()> {
        let current = self.context().selected_effects();
        let before = (
            self.reverb_encoder.get_preset(current).clone(),
            self.echo_encoder.get_preset(current).clone(),
            self.pitch_encoder.get_preset(current).clone(),
            self.gender_encoder.get_preset(current).clone(),
            self.megaphone_effect.get_preset(current).clone(),
            self.robot_effect.get_preset(current).clone(),
            self.hardtune_effect.get_preset(current).clone(),
            self.effects(current).name().to_string(),
        );
        let result = self.read_preset(read);
        if result.is_err() {
            *self.reverb_encoder.get_preset_mut(current) = before.0;
            *self.echo_encoder.get_preset_mut(current) = before.1;
            *self.pitch_encoder.get_preset_mut(current) = before.2;
            *self.gender_encoder.get_preset_mut(current) = before.3;
            *self.megaphone_effect.get_preset_mut(current) = before.4;
            *self.robot_effect.get_preset_mut(current) = before.5;
            *self.hardtune_effect.get_preset_mut(current) = before.6;
            self.effects_mut(current).set_loaded_name(before.7);
        }
        result
    }

    fn read_preset<R: Read>(&mut self, read: R) -> Result<()> {
        let buf_reader = BufReader::new(read);
        let mut reader = Reader::from_reader(buf_reader);

        // So, in principle here, all we need to do is loop over the tags, check on the
        // tag name, and load it directly into the relevant effect. This should force a
        // replace of the current effect, and bam, done.

        // Firstly, we need the current preset to overwrite.
        let current = self.context().selected_effects();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_event_into(&mut buf) {
                Ok(Event::Empty(ref e)) => {
                    let (name, attributes) = wrap_start_event(e)?;

                    match name.as_str() {
                        "reverbEncoder" => self
                            .reverb_encoder
                            .parse_reverb_preset(current, &attributes)?,
                        "echoEncoder" => {
                            self.echo_encoder.parse_echo_preset(current, &attributes)?
                        }
                        "pitchEncoder" => self
                            .pitch_encoder
                            .parse_pitch_preset(current, &attributes)?,
                        "genderEncoder" => self
                            .gender_encoder
                            .parse_gender_preset(current, &attributes)?,
                        "megaphoneEffect" => self
                            .megaphone_effect
                            .parse_megaphone_preset(current, &attributes)?,
                        "robotEffect" => {
                            self.robot_effect.parse_robot_preset(current, &attributes)?
                        }
                        "hardtuneEffect" => self
                            .hardtune_effect
                            .parse_hardtune_preset(current, &attributes)?,
                        _ => warn!("Unexpected Start Tag {}", name),
                    }
                }

                Ok(Event::Start(ref e)) => {
                    let (_name, attributes) = wrap_start_event(e)?;
                    let mut found = false;

                    // We can cheese this a little, there's only one tag in a preset that has
                    // children, and that's the top level element. So if this is going, we
                    // already know what to do.
                    for attribute in attributes {
                        if attribute.name == "name" {
                            found = true;
                            self.effects_mut(current).set_loaded_name(attribute.value);
                            break;
                        }
                    }
                    if !found {
                        bail!("Preset Name not found, cannot proceed.");
                    }
                }

                // Ends a tag with children
                Ok(Event::End(_)) => {}
                Ok(Event::Eof) => {
                    break;
                }

                Ok(_) => {}
                Err(e) => bail!("The preset can't be read: {}", e),
            }
        }
        Ok(())
    }

    pub fn write<P: AsRef<Path>>(&mut self, path: P) -> Result<()> {
        let out_file = File::create(path)?;
        self.write_to(out_file)
    }

    pub fn write_to<W: Write>(&mut self, sink: W) -> Result<()> {
        let mut writer = Writer::new_with_indent(sink, u8::try_from('\t')?, 1);
        writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))?;

        // For compatibility with the 'Release' version of the official app, we need to adjust
        // the config and reset channel monitoring back to the headphones (along with associated
        // routing), so we'll pull some data out, make some changes, then reload the settings once
        // writing is complete.
        let monitored_output = self.submix_tree.monitor_tree().monitored_output();
        let routing = self.mixer.mixer_table_mut();
        let headphone_routing = self.submix_tree.monitor_tree_mut().routing();
        let headphone_mix = self.submix_tree.monitor_tree_mut().headphone_mix();

        if monitored_output != OutputChannels::Headphones {
            for input in InputChannels::iter() {
                routing[input][OutputChannels::Headphones] = headphone_routing[input];
            }
            self.submix_tree
                .monitor_tree_mut()
                .set_monitored_output(OutputChannels::Headphones);
            self.submix_tree
                .monitor_tree_mut()
                .set_headphone_mix(Mix::A);
        }

        // Write everything, then restore the monitor settings even if writing failed, so the
        // profile in memory always matches the device.
        let result = self.write_elements(&mut writer);

        let routing = self.mixer.mixer_table_mut();
        // Everything's written, restore the original monitor settings..
        if monitored_output != OutputChannels::Headphones {
            for input in InputChannels::iter() {
                routing[input][OutputChannels::Headphones] = routing[input][monitored_output];
            }
            self.submix_tree
                .monitor_tree_mut()
                .set_monitored_output(monitored_output);
            self.submix_tree
                .monitor_tree_mut()
                .set_headphone_mix(headphone_mix);
        }

        result
    }

    fn write_elements<W: Write>(&self, writer: &mut Writer<W>) -> Result<()> {
        self.root.write_initial(writer)?;
        self.browser.write_browser(writer)?;
        self.animation_tree.write_animation(writer)?;

        self.mix_routing.write_mix_tree(writer)?;
        self.submix_tree.write_submixer(writer)?;

        self.mixer.write_mixers(writer)?;
        self.context.write_context(writer)?;

        self.mute_chat.write_mute_chat(writer)?;

        // The following three iters need the FaderName to be defined and passed
        // forward, the tag names need to be dynamically generated to prevent breakage
        // when .swap() is used to move them around.
        for (faders, fader) in &self.faders {
            fader.write_fader(writer, faders)?;
        }

        for (fader, button) in &self.mute_buttons {
            button.write_button(writer, fader)?;
        }

        for (fader, scribble) in &self.scribbles {
            scribble.write_scribble(writer, fader)?;
        }

        for effect in self.effects.values() {
            effect.write_effects(writer)?;
        }

        self.megaphone_effect.write_megaphone(writer)?;
        self.robot_effect.write_robot(writer)?;
        self.hardtune_effect.write_hardtune(writer)?;

        self.reverb_encoder.write_reverb(writer)?;
        self.echo_encoder.write_echo(writer)?;
        self.pitch_encoder.write_pitch(writer)?;
        self.gender_encoder.write_gender(writer)?;

        for sampler in self.sampler_map.values() {
            sampler.write_sample(writer)?;
        }

        for element in self.simple_elements.values() {
            element.write_simple(writer)?;
        }

        // Finalise the XML..
        self.root.write_final(writer)?;
        Ok(())
    }

    pub fn write_preset<P: AsRef<Path>>(&self, path: P) -> Result<()> {
        atomic_write(path, |file| self.write_preset_to(file))
    }

    pub fn write_preset_to<W: Write>(&self, sink: W) -> Result<()> {
        let mut writer = Writer::new_with_indent(sink, u8::try_from('\t')?, 1);
        writer.write_event(Event::Decl(BytesDecl::new("1.0", Some("utf-8"), None)))?;

        let current = self.context().selected_effects();
        let preset_writer = PresetWriter::new(String::from(self.effects(current).name()));
        preset_writer.write_initial(&mut writer)?;
        preset_writer.write_tag(
            &mut writer,
            "reverbEncoder",
            self.reverb_encoder.get_preset_attributes(current),
        )?;

        preset_writer.write_tag(
            &mut writer,
            "echoEncoder",
            self.echo_encoder.get_preset_attributes(current),
        )?;

        preset_writer.write_tag(
            &mut writer,
            "pitchEncoder",
            self.pitch_encoder.get_preset_attributes(current),
        )?;

        preset_writer.write_tag(
            &mut writer,
            "genderEncoder",
            self.gender_encoder.get_preset_attributes(current),
        )?;

        preset_writer.write_tag(
            &mut writer,
            "megaphoneEffect",
            self.megaphone_effect.get_preset_attributes(current),
        )?;

        preset_writer.write_tag(
            &mut writer,
            "robotEffect",
            self.robot_effect.get_preset_attributes(current),
        )?;

        preset_writer.write_tag(
            &mut writer,
            "hardtuneEffect",
            self.hardtune_effect.get_preset_attributes(current),
        )?;

        preset_writer.write_final(&mut writer)?;
        Ok(())
    }

    pub fn parse_preset(key: String) -> Result<Preset> {
        if let Some(id) = key
            .chars()
            .last()
            .map(|s| u8::from_str(&s.to_string()))
            .transpose()?
            && let Some(index) = id.checked_sub(1)
            && let Some(preset) = Preset::iter().nth(index as usize)
        {
            return Ok(preset);
        }
        Err(anyhow!("Unable to Parse Preset from Number"))
    }

    pub fn animation(&self) -> &AnimationTree {
        &self.animation_tree
    }

    pub fn animation_mut(&mut self) -> &mut AnimationTree {
        &mut self.animation_tree
    }

    pub fn mixer_mut(&mut self) -> &mut Mixers {
        &mut self.mixer
    }

    pub fn mixer(&self) -> &Mixers {
        &self.mixer
    }

    pub fn faders_mut(&mut self) -> &mut EnumMap<Faders, Fader> {
        &mut self.faders
    }

    pub fn fader_mut(&mut self, fader: Faders) -> &mut Fader {
        &mut self.faders[fader]
    }

    pub fn fader(&self, fader: Faders) -> &Fader {
        &self.faders[fader]
    }

    pub fn mute_buttons(&mut self) -> &mut EnumMap<Faders, MuteButton> {
        &mut self.mute_buttons
    }

    pub fn mute_button_mut(&mut self, fader: Faders) -> &mut MuteButton {
        &mut self.mute_buttons[fader]
    }

    pub fn mute_button(&self, fader: Faders) -> &MuteButton {
        &self.mute_buttons[fader]
    }

    pub fn scribbles_mut(&mut self) -> &mut EnumMap<Faders, Scribble> {
        &mut self.scribbles
    }

    pub fn scribble(&self, fader: Faders) -> &Scribble {
        &self.scribbles[fader]
    }

    pub fn scribble_mut(&mut self, fader: Faders) -> &mut Scribble {
        &mut self.scribbles[fader]
    }

    pub fn effects(&self, effect: Preset) -> &Effects {
        &self.effects[effect]
    }

    pub fn effects_mut(&mut self, effect: Preset) -> &mut Effects {
        &mut self.effects[effect]
    }

    pub fn mute_chat_mut(&mut self) -> &mut MuteChat {
        &mut self.mute_chat
    }

    pub fn mute_chat(&self) -> &MuteChat {
        &self.mute_chat
    }

    pub fn megaphone_effect(&self) -> &MegaphoneEffectBase {
        &self.megaphone_effect
    }

    pub fn megaphone_effect_mut(&mut self) -> &mut MegaphoneEffectBase {
        &mut self.megaphone_effect
    }

    pub fn robot_effect(&self) -> &RobotEffectBase {
        &self.robot_effect
    }

    pub fn robot_effect_mut(&mut self) -> &mut RobotEffectBase {
        &mut self.robot_effect
    }

    pub fn hardtune_effect(&self) -> &HardtuneEffectBase {
        &self.hardtune_effect
    }

    pub fn hardtune_effect_mut(&mut self) -> &mut HardtuneEffectBase {
        &mut self.hardtune_effect
    }

    pub fn sample_button(&self, button: SampleButtons) -> &SampleBase {
        &self.sampler_map[button]
    }

    pub fn sample_button_mut(&mut self, button: SampleButtons) -> &mut SampleBase {
        &mut self.sampler_map[button]
    }

    /// Whether any sampler button plays one of these sample files.
    pub fn uses_sample_file(&self, names: &std::collections::HashSet<String>) -> bool {
        self.sampler_map
            .values()
            .any(|button| button.uses_track_file(names))
    }

    /// Points every sampler track using the sample file `old` at `new` instead (a sample file
    /// was renamed). Returns how many tracks were changed.
    pub fn reset_sample_file(&mut self, name: &str, gain: f64) -> usize {
        self.sampler_map
            .values_mut()
            .map(|button| button.reset_track_file(name, gain))
            .sum()
    }

    pub fn rename_sample_file(&mut self, old: &str, new: &str) -> usize {
        self.sampler_map
            .values_mut()
            .map(|button| button.rename_track_file(old, new))
            .sum()
    }

    pub fn pitch_encoder(&self) -> &PitchEncoderBase {
        &self.pitch_encoder
    }

    pub fn pitch_encoder_mut(&mut self) -> &mut PitchEncoderBase {
        &mut self.pitch_encoder
    }

    pub fn echo_encoder(&self) -> &EchoEncoderBase {
        &self.echo_encoder
    }

    pub fn echo_encoder_mut(&mut self) -> &mut EchoEncoderBase {
        &mut self.echo_encoder
    }

    pub fn gender_encoder(&self) -> &GenderEncoderBase {
        &self.gender_encoder
    }

    pub fn gender_encoder_mut(&mut self) -> &mut GenderEncoderBase {
        &mut self.gender_encoder
    }

    pub fn reverb_encoder(&self) -> &ReverbEncoderBase {
        &self.reverb_encoder
    }

    pub fn reverb_encoder_mut(&mut self) -> &mut ReverbEncoderBase {
        &mut self.reverb_encoder
    }

    pub fn simple_element_mut(&mut self, name: SimpleElements) -> &mut SimpleElement {
        &mut self.simple_elements[name]
    }

    pub fn simple_element(&self, name: SimpleElements) -> &SimpleElement {
        &self.simple_elements[name]
    }

    pub fn context(&self) -> &Context {
        &self.context
    }

    pub fn context_mut(&mut self) -> &mut Context {
        &mut self.context
    }

    pub fn submixes(&self) -> &SubMixer {
        &self.submix_tree
    }
    pub fn submixes_mut(&mut self) -> &mut SubMixer {
        &mut self.submix_tree
    }

    pub fn mix_routing(&self) -> &MixRoutingTree {
        &self.mix_routing
    }
    pub fn mix_routing_mut(&mut self) -> &mut MixRoutingTree {
        &mut self.mix_routing
    }
}

/// This will wrap a 'Start' XML event into a name, and attribute Vec. We're using
/// our own Attribute Struct here to allow easy moving between XML libraries in future.
/// TODO: If we're doing this, we might as well make the attributes a HashMap
pub(crate) fn wrap_start_event(event: &BytesStart) -> Result<(String, Vec<Attribute>)> {
    let mut attributes = Vec::new();

    let name = event.local_name().into_inner().to_string();
    for attribute in event.attributes() {
        match attribute {
            Ok(a) => {
                // unescape_value() is deprecated in favour of normalized_value(), which also
                // rewrites whitespace in values. Keep the previous behaviour so profiles load and
                // save exactly as before.
                #[allow(deprecated)]
                let value = a.unescape_value()?.to_string();
                attributes.push(Attribute {
                    name: a.key.local_name().into_inner().to_string(),
                    value,
                });
            }
            Err(e) => {
                bail!("Error Processing Attribute: {}", e);
            }
        }
    }
    Ok((name, attributes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    const TEST_PROFILE: &str = include_str!("../test-data/profile.xml");

    #[test]
    fn loads_test_profile() {
        assert!(ProfileSettings::load(TEST_PROFILE.as_bytes()).is_ok());
    }

    /// Every element with its attributes sorted (they're written from HashMaps, in any order).
    fn normalised(xml: &[u8]) -> Vec<String> {
        let mut reader = Reader::from_reader(xml);
        let (mut lines, mut buf) = (Vec::new(), Vec::new());
        loop {
            match reader.read_event_into(&mut buf).unwrap() {
                Event::Start(e) | Event::Empty(e) => {
                    let mut attributes: Vec<String> = e
                        .attributes()
                        .map(|a| {
                            let a = a.unwrap();
                            let key = a.key.as_ref().to_string();
                            format!("{key}={}", a.value)
                        })
                        .collect();
                    attributes.sort();
                    let name = e.name().as_ref().to_string();
                    lines.push(format!("{name} {}", attributes.join(" ")));
                }
                Event::Eof => break,
                _ => {}
            }
            buf.clear();
        }
        lines
    }

    fn written(settings: &mut ProfileSettings) -> Vec<u8> {
        let mut out = Vec::new();
        settings.write_to(&mut out).unwrap();
        out
    }

    #[test]
    fn test_profile_round_trip() {
        // What's written reads back to the same thing (load -> write -> load -> write)
        let mut first = ProfileSettings::load(TEST_PROFILE.as_bytes()).unwrap();
        let once = written(&mut first);
        let mut second = ProfileSettings::load(&once[..]).unwrap();
        let twice = written(&mut second);
        assert_eq!(normalised(&once), normalised(&twice));

        // And every effect preset value of the source is kept
        let source = normalised(TEST_PROFILE.as_bytes());
        let once = normalised(&once);
        for prefix in [
            "reverbEncoder",
            "echoEncoder",
            "pitchEncoder",
            "genderEncoder",
            "megaphoneEffect",
            "robotEffect",
            "hardtuneEffect",
        ] {
            let elements: Vec<&String> = source.iter().filter(|l| l.starts_with(prefix)).collect();
            assert!(
                !elements.is_empty(),
                "{prefix} missing from the test profile"
            );
            for element in elements {
                let name = element.split(' ').next().unwrap();
                let out = once
                    .iter()
                    .find(|l| l.split(' ').next() == Some(name))
                    .unwrap_or_else(|| panic!("{name} not written"));
                for attribute in element.split(' ').skip(1) {
                    let key = attribute.split('=').next().unwrap();
                    assert!(
                        out.split(' ').any(|a| a.starts_with(&format!("{key}="))),
                        "{name}: {key} not written"
                    );
                }
            }
        }
    }

    #[test]
    fn bundled_profiles_keep_their_effect_values() {
        // Their values are inside the ranges: loading and writing them changes nothing
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let mut files = vec![root.join("daemon/profiles/Default.goxlr")];
        for entry in fs::read_dir(root.join("defaults/resources/profiles")).unwrap() {
            files.push(entry.unwrap().path());
        }
        for file in files {
            let mut archive = zip::ZipArchive::new(File::open(&file).unwrap()).unwrap();
            let mut source = String::new();
            archive
                .by_name("profile.xml")
                .unwrap()
                .read_to_string(&mut source)
                .unwrap();
            let mut settings = ProfileSettings::load(source.as_bytes()).unwrap();
            let out = normalised(&written(&mut settings));
            for element in normalised(source.as_bytes()) {
                let mut parts = element.split(' ');
                let name = parts.next().unwrap();
                let effects = [
                    "reverb",
                    "echo",
                    "pitch",
                    "gender",
                    "megaphone",
                    "robot",
                    "hardtune",
                ];
                if !effects.iter().any(|effect| name.starts_with(effect)) {
                    continue;
                }
                let Some(line) = out.iter().find(|l| l.split(' ').next() == Some(name)) else {
                    continue;
                };
                for attribute in parts {
                    let Some((key, value)) = attribute.split_once('=') else {
                        continue;
                    };
                    // Derived from the style when loading, not read
                    if key == "PITCH_SHIFT_THRESHOLD" {
                        continue;
                    }
                    let Ok(value) = value.parse::<f64>() else {
                        continue;
                    };
                    let prefix = format!("{key}=");
                    let Some(now) = line.split(' ').find(|a| a.starts_with(&prefix)) else {
                        continue;
                    };
                    if let Ok(now) = now[prefix.len()..].parse::<f64>() {
                        assert_eq!(
                            value.trunc(),
                            now,
                            "{}: {name} {key} changed",
                            file.display()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn unknown_attributes_are_written_back() {
        // Attributes from a newer app, on an effect preset and on a root element
        let profile = TEST_PROFILE
            .replacen(
                "<echoEncoderpreset1 DELAY_KNOB_POSITION=\"0\"",
                "<echoEncoderpreset1 FUTURE_DELAY=\"7\" DELAY_KNOB_POSITION=\"0\"",
                1,
            )
            .replacen(
                "<mixerTree headphoneLevel",
                "<mixerTree futureLevel=\"3\" headphoneLevel",
                1,
            );
        let mut settings = ProfileSettings::load(profile.as_bytes()).unwrap();
        let out = normalised(&written(&mut settings));
        let line = |name: &str| out.iter().find(|l| l.starts_with(name)).unwrap().clone();
        assert!(line("echoEncoderpreset1 ").contains("FUTURE_DELAY=7"));
        assert!(line("mixerTree ").contains("futureLevel=3"));
    }

    #[test]
    fn sample_stacks_keep_what_they_dont_know() {
        use crate::components::sample::SampleBank;
        let profile = TEST_PROFILE
            .replacen(
                "track_0NormalizedGain=\"11.26597213745117187500\" playbackMode=\"1\"",
                "futureFade=\"4\" playbackMode=\"1\"",
                1,
            )
            .replacen(
                "<sampleStackC/>\n  </sampleTopLeft>",
                "<sampleStackC/>\n    <sampleStackD sampleStackDstackSize=\"1\" track_0=\"x.wav\"/>\n  </sampleTopLeft>",
                1,
            );
        assert_ne!(profile, TEST_PROFILE);
        // A bank this version doesn't know doesn't fail the profile
        let mut settings = ProfileSettings::load(profile.as_bytes()).unwrap();

        // The track without its gain is kept, playing whole at an unchanged gain
        let tracks = settings
            .sample_button(SampleButtons::TopLeft)
            .get_stack(SampleBank::A)
            .get_tracks();
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].normalized_gain(), 1.);
        assert_eq!(tracks[0].end_position(), 100.);

        let out = normalised(&written(&mut settings));
        let line = out.iter().find(|l| l.starts_with("sampleStackA ")).unwrap();
        assert!(line.contains("futureFade=4"), "{line}");
        assert!(line.contains("track_0=Walkie Talkie Static.wav"), "{line}");
    }

    #[test]
    fn a_mic_fader_id_out_of_range_means_no_fader() {
        let with = |id: &str| {
            let profile = TEST_PROFILE.replacen(
                "micIsAnActiveFader=\"4\"",
                &format!("micIsAnActiveFader=\"{id}\""),
                1,
            );
            ProfileSettings::load(profile.as_bytes())
                .unwrap()
                .mute_chat()
                .mic_fader_id()
        };
        assert_eq!(with("2"), 2);
        assert_eq!(with("7"), 4);
        assert_eq!(with("-1"), 4);
        assert_eq!(with("x"), 4);
    }

    #[test]
    fn a_broken_preset_changes_nothing() {
        let mut settings = ProfileSettings::load(TEST_PROFILE.as_bytes()).unwrap();
        let current = settings.context().selected_effects();
        let before = format!("{:?}", settings.reverb_encoder.get_preset(current));

        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let preset = fs::read_to_string(root.join("defaults/resources/presets/Big Echo.preset"))
            .unwrap()
            .replace("TRANS_DIST_AMT=\"30\"", "TRANS_DIST_AMT=\"thirty\"");
        // The reverb comes first in the file, the error is in the megaphone after it
        assert!(settings.load_preset(preset.as_bytes()).is_err());
        assert_eq!(
            format!("{:?}", settings.reverb_encoder.get_preset(current)),
            before
        );

        // The bank keeps its name too, even one `set_name` would refuse
        settings
            .effects_mut(current)
            .set_loaded_name("Lo-Fi".into());
        assert!(settings.load_preset(preset.as_bytes()).is_err());
        assert_eq!(settings.effects(current).name(), "Lo-Fi");

        // A preset named like that loads
        let named = fs::read_to_string(root.join("defaults/resources/presets/Big Echo.preset"))
            .unwrap()
            .replace("name=\"Big Echo\"", "name=\"Big-Echo\"");
        settings.load_preset(named.as_bytes()).unwrap();
        assert_eq!(settings.effects(current).name(), "Big-Echo");
    }

    /// A light fuzz: random attribute values (empty, huge, negative, not numbers..) never make
    /// the loading panic, and what loads also writes and loads again.
    #[test]
    fn random_attribute_values_never_panic() {
        let values = [
            "",
            "-1",
            "0",
            "255",
            "256",
            "65536",
            "-129",
            "999999999999",
            "1e40",
            "-1e40",
            "NaN",
            "inf",
            "abc",
            "1.5",
            "-0.5",
            "\u{e9}",
        ];
        let positions: Vec<usize> = TEST_PROFILE
            .match_indices("=\"")
            .map(|(i, _)| i + 2)
            .collect();
        let mut rng = fastrand::Rng::with_seed(0x60_7c_1e);
        for _ in 0..300 {
            let mut profile = TEST_PROFILE.to_string();
            // Replace a few values, from the end so the earlier positions stay valid
            let mut chosen: Vec<usize> = (0..rng.usize(1..8))
                .map(|_| positions[rng.usize(..positions.len())])
                .collect();
            chosen.sort_unstable();
            chosen.dedup();
            for start in chosen.into_iter().rev() {
                let end = start + profile[start..].find('"').unwrap();
                profile.replace_range(start..end, values[rng.usize(..values.len())]);
            }
            if let Ok(mut settings) = ProfileSettings::load(profile.as_bytes()) {
                let out = written(&mut settings);
                assert!(ProfileSettings::load(&out[..]).is_ok());
            }
        }
    }

    /// Writes the normalised output of the test profile, to compare before and after a change
    /// of the parsing or writing code: GOXLR_PROFILE_DUMP=<file> cargo test dump_normalised
    #[test]
    fn dump_normalised() {
        if let Ok(path) = std::env::var("GOXLR_PROFILE_DUMP") {
            let mut settings = ProfileSettings::load(TEST_PROFILE.as_bytes()).unwrap();
            fs::write(path, normalised(&written(&mut settings)).join("\n")).unwrap();
        }
    }

    #[test]
    fn default_profile_round_trip() {
        let default = include_bytes!("../../daemon/profiles/Default.goxlr");
        let mut profile = Profile::load(std::io::Cursor::new(&default[..])).unwrap();

        let path =
            std::env::temp_dir().join(format!("goxlr-test-profile-{}.goxlr", std::process::id()));
        profile.save(&path).unwrap();
        let reloaded = Profile::load(File::open(&path).unwrap());
        fs::remove_file(&path).unwrap();
        assert!(reloaded.is_ok());
    }

    /// Accepts a few bytes, then fails (like a disk filling up during a save).
    struct FailAfter(usize);

    impl Write for FailAfter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            if self.0 < buf.len() {
                return Err(std::io::Error::other("disk full"));
            }
            self.0 -= buf.len();
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn failed_write_keeps_monitor_settings() {
        let mut settings = ProfileSettings::load(TEST_PROFILE.as_bytes()).unwrap();
        settings
            .submix_tree
            .monitor_tree_mut()
            .set_monitored_output(OutputChannels::LineOut);

        assert!(settings.write_to(FailAfter(2000)).is_err());
        assert_eq!(
            settings.submix_tree.monitor_tree().monitored_output(),
            OutputChannels::LineOut
        );
    }

    #[test]
    fn renames_sample_tracks() {
        let mut settings = ProfileSettings::load(TEST_PROFILE.as_bytes()).unwrap();
        let track = settings
            .sample_button(SampleButtons::TopLeft)
            .get_stack(crate::components::sample::SampleBank::A)
            .get_tracks()
            .first()
            .map(|track| track.track.clone());

        // The test profile has a sample on the first button of bank A.
        let old = track.expect("test profile should have a sample on TopLeft / A");
        let changed = settings.rename_sample_file(&old, "renamed.wav");
        assert!(changed >= 1);
        assert_eq!(settings.rename_sample_file(&old, "other.wav"), 0);
        assert_eq!(
            settings
                .sample_button(SampleButtons::TopLeft)
                .get_stack(crate::components::sample::SampleBank::A)
                .get_tracks()[0]
                .track,
            "renamed.wav"
        );
    }

    #[test]
    fn edited_samples_play_whole_at_their_new_gain() {
        let mut settings = ProfileSettings::load(TEST_PROFILE.as_bytes()).unwrap();
        let name = settings
            .sample_button(SampleButtons::TopLeft)
            .get_stack(crate::components::sample::SampleBank::A)
            .get_tracks()[0]
            .track
            .clone();
        assert!(settings.reset_sample_file(&name, 0.5) >= 1);
        let track = settings
            .sample_button(SampleButtons::TopLeft)
            .get_stack(crate::components::sample::SampleBank::A)
            .get_tracks()[0]
            .clone();
        assert_eq!(
            (
                track.start_position,
                track.end_position,
                track.normalized_gain
            ),
            (0.0, 100.0, 0.5)
        );
        assert_eq!(settings.reset_sample_file("not-used.wav", 0.5), 0);
    }

    #[test]
    fn out_of_range_enum_indexes_do_not_panic() {
        let profile = TEST_PROFILE
            .replacen("playbackMode=\"1\"", "playbackMode=\"9\"", 1)
            .replacen(
                "coughButtonMuteSourceSelection=\"0\"",
                "coughButtonMuteSourceSelection=\"7\"",
                1,
            );
        assert!(ProfileSettings::load(profile.as_bytes()).is_ok());
    }
}
