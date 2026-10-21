//! User-interface strings. English is the default; Italian is selectable.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    #[default]
    En,
    It,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::En, Lang::It];

    pub fn name(self) -> &'static str {
        match self {
            Lang::En => "English",
            Lang::It => "Italiano",
        }
    }

    /// Picks the language from the environment (`LC_ALL`, `LANG`…), else English.
    pub fn from_env() -> Self {
        for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(v) = std::env::var(var)
                && !v.is_empty()
            {
                return if v.starts_with("it") {
                    Lang::It
                } else {
                    Lang::En
                };
            }
        }
        Lang::En
    }
}

macro_rules! strings {
    ($($key:ident => $en:literal, $it:literal;)*) => {
        /// Every translatable string, by key.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum T { $($key),* }

        impl T {
            pub const ALL: &'static [T] = &[$(T::$key),*];

            pub fn get(self, lang: Lang) -> &'static str {
                match (self, lang) {
                    $((T::$key, Lang::En) => $en, (T::$key, Lang::It) => $it,)*
                }
            }
        }
    };
}

pub fn tr(lang: Lang, key: T) -> &'static str {
    key.get(lang)
}

strings! {
    TabOverview => "Overview", "Panoramica";
    TabTuning => "Tuning", "Regolazioni";
    TabFans => "Fans", "Ventole";
    TabProfiles => "Profiles", "Profili";
    TabSettings => "Settings", "Impostazioni";

    Temperature => "Temperature", "Temperatura";
    CoreClock => "Core clock", "Clock core";
    MemClock => "Memory clock", "Clock memoria";
    Power => "Power", "Potenza";
    Fan => "Fan", "Ventola";
    GpuLoad => "GPU load", "Carico GPU";
    Vram => "VRAM", "VRAM";
    MemLoad => "memory controller", "controller memoria";
    Slowdown => "slowdown at", "rallenta a";
    Limit => "limit", "limite";
    PState => "P-state", "P-state";
    LimitedBy => "Limited by", "Limitata da";
    NotLimited => "Running free", "Nessun limite attivo";
    History => "History", "Storico";
    Driver => "Driver", "Driver";
    Vbios => "VBIOS", "VBIOS";
    Bus => "PCI bus", "Bus PCI";
    Arch => "Architecture", "Architettura";
    NoGpu => "No NVIDIA GPU found", "Nessuna GPU NVIDIA trovata";
    NoGpuHint => "wnvctl needs the proprietary NVIDIA driver (libnvidia-ml.so.1).", "wnvctl richiede il driver NVIDIA proprietario (libnvidia-ml.so.1).";

    PowerLimit => "Power limit", "Limite di potenza";
    PowerLimitHint => "Lower = cooler, quieter and more efficient; higher = more headroom.", "Più basso = più fresca, silenziosa ed efficiente; più alto = più margine.";
    Clocks => "Clock offsets", "Offset dei clock";
    ClocksHint => "Raise in small steps (+15–25 MHz) and test with a game or benchmark.", "Alza a piccoli passi (+15–25 MHz) e prova con un gioco o un benchmark.";
    CoreOffset => "Core offset", "Offset core";
    MemOffset => "Memory offset", "Offset memoria";
    Undervolt => "Undervolt (clock limit)", "Undervolt (limite di clock)";
    UndervoltHint => "NVIDIA has no voltage slider on Linux. A positive core offset plus a clock limit reaches the same clock at a lower voltage and never goes above it: less power and heat at the same performance.", "Su Linux NVIDIA non offre un cursore della tensione. Un offset core positivo più un limite di clock raggiunge lo stesso clock a una tensione più bassa senza mai superarlo: meno consumo e calore a parità di prestazioni.";
    ClockLimit => "Max core clock", "Clock core massimo";
    UndervoltWizard => "Suggest an undervolt", "Suggerisci un undervolt";
    UndervoltWizardHint => "Starting point: +150 MHz and a limit at 92% of the highest clock seen. Test it, then raise the offset or lower the limit.", "Punto di partenza: +150 MHz e un limite al 92% del clock più alto osservato. Provalo, poi alza l'offset o abbassa il limite.";
    TargetTemp => "Target temperature", "Temperatura obiettivo";
    TargetTempHint => "The driver adjusts clocks and fans to stay below this temperature.", "Il driver regola clock e ventole per restare sotto questa temperatura.";
    Unsupported => "Not supported by this GPU", "Non supportato da questa GPU";
    Enabled => "Enabled", "Attivo";
    DefaultValue => "default", "predefinito";

    FanMode => "Fan control", "Controllo ventole";
    FanAuto => "Automatic", "Automatico";
    FanFixed => "Fixed", "Fisso";
    FanCurve => "Curve", "Curva";
    FanAutoHint => "The GPU firmware controls the fans (driver default).", "Le ventole sono gestite dal firmware della GPU (predefinito del driver).";
    FanFixedHint => "The fans always run at this speed.", "Le ventole girano sempre a questa velocità.";
    FanCurveHint => "Drag points to edit · double-click to add · right-click to remove.", "Trascina i punti per modificarli · doppio clic per aggiungere · clic destro per rimuovere.";
    FanSpeed => "Fan speed", "Velocità ventole";
    FanMinNote => "This GPU's fans cannot go below", "Le ventole di questa GPU non scendono sotto";
    Advanced => "Advanced", "Avanzate";
    Hysteresis => "Hysteresis", "Isteresi";
    HysteresisHint => "Slow down only after the temperature dropped this much.", "Rallenta solo dopo che la temperatura è scesa di tanto.";
    Spindown => "Slow-down delay", "Ritardo rallentamento";
    Interval => "Update interval", "Intervallo di aggiornamento";
    ExternalFans => "Another program (e.g. CoolerControl) is controlling the fans. Disable its GPU fan control to avoid conflicts.", "Un altro programma (es. CoolerControl) sta controllando le ventole. Disattiva il suo controllo ventole GPU per evitare conflitti.";
    PresetSilent => "Silent", "Silenziosa";
    PresetBalanced => "Balanced", "Bilanciata";
    PresetCool => "Cool", "Fresca";

    Apply => "Apply", "Applica";
    Applying => "Applying…", "Applicazione…";
    Revert => "Discard", "Annulla";
    ResetStock => "Reset to stock", "Ripristina predefiniti";
    Unsaved => "Changes not applied", "Modifiche non applicate";
    InSync => "Applied", "Applicato";
    SaveAs => "Save as profile", "Salva come profilo";
    ProfileName => "Profile name", "Nome profilo";
    Save => "Save", "Salva";
    Cancel => "Cancel", "Annulla";
    Applied => "Applied", "Applicato";
    NothingToChange => "Already applied: nothing to change", "Già applicato: nessuna modifica";

    Profiles => "Profiles", "Profili";
    ProfilesHint => "Profiles are TOML files you can also edit by hand.", "I profili sono file TOML modificabili anche a mano.";
    NoProfiles => "No profiles yet: set things up in Tuning and Fans, then \"Save as profile\".", "Ancora nessun profilo: regola in Regolazioni e Ventole, poi \"Salva come profilo\".";
    Edit => "Edit", "Modifica";
    Delete => "Delete", "Elimina";
    ConfirmDelete => "Click again to delete", "Clicca di nuovo per eliminare";
    AtBoot => "At boot", "All'avvio";
    SetAtBoot => "Apply at boot", "Applica all'avvio";
    RemoveAtBoot => "Remove from boot", "Togli dall'avvio";
    OpenFolder => "Open folder", "Apri cartella";
    Active => "active", "attivo";
    Stock => "Stock", "Predefinito";

    Language => "Language", "Lingua";
    Refresh => "Refresh rate", "Frequenza di aggiornamento";
    HistoryLen => "Chart history", "Durata grafici";
    CloseToTray => "Keep running in the tray when the window is closed", "Resta nel vassoio di sistema quando chiudi la finestra";
    StartHidden => "Start hidden in the tray", "Avvia nascosto nel vassoio";
    Autostart => "Start at login", "Avvia all'accesso";
    TrayRestart => "Tray changes take effect at the next start.", "Le modifiche al vassoio valgono dal prossimo avvio.";
    About => "About", "Informazioni";
    AboutText => "Overclocking, undervolting, power limit and fan curves for NVIDIA GPUs on Linux. Unofficial, not affiliated with NVIDIA.", "Overclock, undervolt, limite di potenza e curve ventole per GPU NVIDIA su Linux. Non ufficiale, non affiliato a NVIDIA.";
    Credits => "Credits", "Riconoscimenti";

    Daemon => "Daemon", "Servizio";
    DaemonOk => "Service running", "Servizio attivo";
    DaemonMissing => "Service not running", "Servizio non attivo";
    DaemonHint => "Monitoring works without it; to change settings enable the service:", "Il monitoraggio funziona anche senza; per cambiare le impostazioni attiva il servizio:";
    PermissionHint => "Your user must be in the wheel or wnvctl group.", "Il tuo utente deve far parte del gruppo wheel o wnvctl.";
    Show => "Show wnvctl", "Mostra wnvctl";
    Quit => "Quit", "Esci";
    Demo => "demo", "demo";
    Error => "Error", "Errore";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_string_is_translated() {
        for &k in T::ALL {
            assert!(!k.get(Lang::En).is_empty(), "{k:?}");
            assert!(!k.get(Lang::It).is_empty(), "{k:?}");
        }
    }
}
