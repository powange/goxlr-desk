use crate::DaemonState;
use crate::events::EventTriggers;
use anyhow::Result;
use goxlr_ipc::DaemonStatus;
use goxlr_types::MuteState;
use std::sync::{LazyLock, Mutex};
use tokio::sync::mpsc;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "windows")]
mod windows;

pub fn handle_tray(state: DaemonState, tx: mpsc::Sender<EventTriggers>) -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::sync::atomic::Ordering;
        use tokio::task;
        if state.show_tray.load(Ordering::Relaxed) {
            // We'll just spawn the tray and return.
            task::spawn(linux::handle_tray(state.shutdown, tx));
        }
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        macos::handle_tray(state, tx)
    }
    #[cfg(target_os = "windows")]
    {
        windows::handle_tray(state, tx)
    }

    // For all other platforms, don't attempt to spawn a Tray Icon
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        // For now, don't spawn a tray icon.
        Ok(())
    }
}

/// What the tray menus show, refreshed from each new daemon status (the first device).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrayState {
    pub profiles: Vec<String>,
    pub mic_profiles: Vec<String>,
    pub profile: Option<String>,
    pub mic_profile: Option<String>,
    // None: no device, or its Mute button is in Hold mode
    pub mic_muted: Option<bool>,
    pub language: String,
}

static TRAY_STATE: LazyLock<Mutex<TrayState>> = LazyLock::new(Default::default);

pub fn update_tray_state(status: &DaemonStatus) {
    let mut serials: Vec<&String> = status.mixers.keys().collect();
    serials.sort();
    let mixer = serials
        .first()
        .and_then(|serial| status.mixers.get(*serial));

    let mut profiles: Vec<String> = status.files.profiles.to_vec();
    profiles.sort_by_key(|name| name.to_lowercase());
    let mut mic_profiles: Vec<String> = status.files.mic_profiles.to_vec();
    mic_profiles.sort_by_key(|name| name.to_lowercase());

    let locale = &status.config.locale;
    let language = locale
        .user_locale
        .clone()
        .unwrap_or_else(|| locale.system_locale.clone());

    let state = TrayState {
        profiles,
        mic_profiles,
        profile: mixer.map(|m| m.profile_name.clone()),
        mic_profile: mixer.map(|m| m.mic_profile_name.clone()),
        mic_muted: mixer
            .filter(|m| m.cough_button.is_toggle)
            .map(|m| m.cough_button.state != MuteState::Unmuted),
        language: language.chars().take(2).collect::<String>().to_lowercase(),
    };
    if let Ok(mut current) = TRAY_STATE.lock() {
        *current = state;
    }
}

pub fn tray_state() -> TrayState {
    TRAY_STATE
        .lock()
        .map(|state| state.clone())
        .unwrap_or_default()
}

/// The menu's words in the interface language (English otherwise).
#[allow(dead_code)]
pub fn tray_label(language: &str, key: &str) -> &'static str {
    let index = match language {
        "fr" => 1,
        "de" => 2,
        "es" => 3,
        "it" => 4,
        "nl" => 5,
        "pl" => 6,
        "ru" => 7,
        _ => 0,
    };
    let labels: [&'static str; 8] = match key {
        "configure" => [
            "Configure GoXLR",
            "Configurer la GoXLR",
            "GoXLR konfigurieren",
            "Configurar GoXLR",
            "Configura GoXLR",
            "GoXLR configureren",
            "Konfiguruj GoXLR",
            "Настроить GoXLR",
        ],
        "profiles" => [
            "Profiles",
            "Profils",
            "Profile",
            "Perfiles",
            "Profili",
            "Profielen",
            "Profile",
            "Профили",
        ],
        "mic_profiles" => [
            "Mic Profiles",
            "Profils micro",
            "Mikrofonprofile",
            "Perfiles de micrófono",
            "Profili microfono",
            "Microfoonprofielen",
            "Profile mikrofonu",
            "Профили микрофона",
        ],
        "mute" => [
            "Mute mic",
            "Couper le micro",
            "Mikro stumm",
            "Silenciar micro",
            "Silenzia microfono",
            "Microfoon dempen",
            "Wycisz mikrofon",
            "Выключить микрофон",
        ],
        "unmute" => [
            "Unmute mic",
            "Rétablir le micro",
            "Mikro einschalten",
            "Activar micro",
            "Riattiva microfono",
            "Microfoon aanzetten",
            "Włącz mikrofon",
            "Включить микрофон",
        ],
        "open_path" => [
            "Open Path",
            "Ouvrir un dossier",
            "Ordner öffnen",
            "Abrir carpeta",
            "Apri cartella",
            "Map openen",
            "Otwórz folder",
            "Открыть папку",
        ],
        "presets" => [
            "Presets",
            "Préréglages",
            "Presets",
            "Preajustes",
            "Preset",
            "Presets",
            "Presety",
            "Пресеты",
        ],
        "samples" => [
            "Samples",
            "Sons",
            "Samples",
            "Samples",
            "Campioni",
            "Samples",
            "Sample",
            "Сэмплы",
        ],
        "icons" => [
            "Icons",
            "Icônes",
            "Symbole",
            "Iconos",
            "Icone",
            "Pictogrammen",
            "Ikony",
            "Иконки",
        ],
        "logs" => [
            "Logs",
            "Journaux",
            "Protokolle",
            "Registros",
            "Log",
            "Logs",
            "Dzienniki",
            "Журналы",
        ],
        "quit" => [
            "Quit",
            "Quitter",
            "Beenden",
            "Salir",
            "Esci",
            "Afsluiten",
            "Zakończ",
            "Выход",
        ],
        _ => [""; 8],
    };
    labels[index]
}
