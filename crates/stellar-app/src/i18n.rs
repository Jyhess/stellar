//! Interface language: French or English.
//!
//! Texts are written in French in the code; [`tr`] returns their English
//! translation (table `EN`) when English is chosen, and the original text
//! otherwise (or if it is missing from the table). [`tr!`](macro@crate::tr) does
//! the same for a template with `{}` placeholders.
//!
//! Messages of the lower layers (domain, application, engines, persistence)
//! are also written in French; [`tr_msg`] translates them from the
//! `MESSAGES` templates, including the messages they quote. Arguments of
//! [`tr!`](macro@crate::tr) go through it.
//!
//! Default language: Windows' (French, or English otherwise); a choice made
//! in the menu is stored in `%APPDATA%\Stellar\langue`.

use std::collections::HashMap;
use std::fmt::Display;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    Fr,
    En,
}

impl Lang {
    pub const ALL: [Lang; 2] = [Lang::Fr, Lang::En];

    /// Name of the language, in that language.
    pub fn label(self) -> &'static str {
        match self {
            Lang::Fr => "Français",
            Lang::En => "English",
        }
    }

    fn code(self) -> &'static str {
        match self {
            Lang::Fr => "fr",
            Lang::En => "en",
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 {
        Lang::En
    } else {
        Lang::Fr
    }
}

fn apply(l: Lang) {
    LANG.store(matches!(l, Lang::En) as u8, Ordering::Relaxed);
}

/// Chooses the language and stores the choice.
pub fn set_lang(l: Lang) {
    apply(l);
    if let Some(path) = settings_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, l.code());
    }
}

/// Language at startup: `STELLAR_LANG` (fr / en), otherwise the stored
/// choice, otherwise Windows'.
pub fn init() {
    let from = |s: &str| match s.trim() {
        "fr" => Some(Lang::Fr),
        "en" => Some(Lang::En),
        _ => None,
    };
    let l = std::env::var("STELLAR_LANG")
        .ok()
        .and_then(|v| from(&v))
        .or_else(|| {
            settings_file()
                .and_then(|p| std::fs::read_to_string(p).ok())
                .and_then(|v| from(&v))
        })
        .unwrap_or_else(system_lang);
    apply(l);
}

fn settings_file() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|d| PathBuf::from(d).join("Stellar").join("langue"))
}

#[cfg(windows)]
fn system_lang() -> Lang {
    use windows_sys::Win32::Globalization::GetUserDefaultUILanguage;
    // SAFETY: function without arguments.
    let id = unsafe { GetUserDefaultUILanguage() };
    // Primary language: the 10 low bits (0x0C: French).
    if id & 0x3FF == 0x0C {
        Lang::Fr
    } else {
        Lang::En
    }
}

#[cfg(not(windows))]
fn system_lang() -> Lang {
    let v = std::env::var("LANG").unwrap_or_default();
    if v.starts_with("fr") {
        Lang::Fr
    } else {
        Lang::En
    }
}

/// Translation of an interface text.
pub fn tr(fr: &'static str) -> &'static str {
    if lang() == Lang::Fr {
        return fr;
    }
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE
        .get_or_init(|| EN.iter().copied().collect())
        .get(fr)
        .copied()
        .unwrap_or(fr)
}

/// Fills the `{}` placeholders of a template, in order.
pub fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16 * args.len());
    let mut rest = template;
    let mut args = args.iter();
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        match args.next() {
            Some(a) => out.push_str(&tr_msg(&a.to_string())),
            None => out.push_str("{}"),
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// Translation of a message of the lower layers, which may quote another
/// one (`fichier invalide : fichier tronqué`). Unknown text is returned as is.
pub fn tr_msg(fr: &str) -> String {
    if lang() == Lang::Fr {
        return fr.to_owned();
    }
    static EXACT: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    let exact = EXACT.get_or_init(|| {
        MESSAGES.iter().copied().filter(|(fr, _)| !fr.contains("{}")).collect()
    });
    if let Some(en) = exact.get(fr) {
        return (*en).to_owned();
    }
    for (template, en) in MESSAGES.iter().filter(|(t, _)| t.contains("{}")) {
        if let Some(parts) = captures(template, fr) {
            let parts: Vec<String> = parts.into_iter().map(tr_msg).collect();
            let args: Vec<&dyn Display> = parts.iter().map(|p| p as &dyn Display).collect();
            return fill(en, &args);
        }
    }
    fr.to_owned()
}

/// Parts of `text` matching the `{}` of `template` (leftmost occurrence of
/// each inner literal part), or `None` if it does not match.
fn captures<'a>(template: &str, text: &'a str) -> Option<Vec<&'a str>> {
    let literals: Vec<&str> = template.split("{}").collect();
    let (first, last) = (literals[0], literals[literals.len() - 1]);
    let mut rest = text.strip_prefix(first)?.strip_suffix(last)?;
    let mut parts = Vec::new();
    for literal in &literals[1..literals.len() - 1] {
        let i = rest.find(literal)?;
        parts.push(&rest[..i]);
        rest = &rest[i + literal.len()..];
    }
    parts.push(rest);
    Some(parts)
}

/// Translated text with `{}` placeholders: `tr!("Masse : {}", m)`.
#[macro_export]
macro_rules! tr {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {
        $crate::i18n::fill($crate::i18n::tr($fmt), &[$(&$arg as &dyn ::std::fmt::Display),*])
    };
}

/// Language choice (drop-down list), stored.
pub fn picker(ui: &mut eframe::egui::Ui) {
    let current = lang();
    eframe::egui::ComboBox::from_id_salt("langue")
        .selected_text(current.label())
        .show_ui(ui, |ui| {
            for l in Lang::ALL {
                if ui.selectable_label(l == current, l.label()).clicked() && l != current {
                    set_lang(l);
                }
            }
        });
}

/// English translations (French text → English text).
const EN: &[(&str, &str)] = &[
    ("Enregistrement suspendu. Il reprendra en rechargeant la sauvegarde « {} ».", "Recording suspended. It will resume when the save “{}” is reloaded."),
    ("La suspension de l'enregistrement a échoué : {}", "Suspending the recording failed: {}"),
    ("Enregistrement impossible : {}", "Recording failed: {}"),
    ("Enregistrement « {} » poursuivi : il reprend dès la relance du calcul (Espace).", "Recording “{}” continued: it resumes as soon as the computation restarts (Space)."),
    ("Reprise de l'enregistrement impossible : {}", "Could not resume the recording: {}"),
    ("Chargement impossible : {}", "Loading failed: {}"),
    ("Relecture impossible : {}", "Playback failed: {}"),
    ("Repos", "At rest"),
    ("Aléatoire (isotrope)", "Random (isotropic)"),
    ("Circulaire — vitesse imposée", "Circular — set speed"),
    ("Circulaire — vitesse orbitale", "Circular — orbital speed"),
    ("Radiale, vers le centre", "Radial, towards the centre"),
    ("manuel (bouton d'arrêt)", "manual (stop button)"),
    ("après un nombre de pas", "after a number of steps"),
    ("après un temps simulé", "after a simulated time"),
    ("après une durée réelle", "after a real duration"),
    ("Stellar — simulateur gravitationnel N-corps (calcul CPU ou GPU, rendu GPU)", "Stellar — gravitational N-body simulator (CPU or GPU computation, GPU rendering)"),
    ("Quitter", "Quit"),
    ("Langue", "Language"),
    ("Effondrement gravitationnel et fusion de particules", "Gravitational collapse and particle mergers"),
    ("enregistrement « {} »", "recording “{}”"),
    ("sauvegarde « {} »", "save “{}”"),
    ("Supprimer", "Delete"),
    ("Renommer", "Rename"),
    ("la sauvegarde", "the save"),
    ("l'enregistrement", "the recording"),
    ("Supprimer {} « {} » ?", "Delete {} “{}”?"),
    ("Le fichier part dans la corbeille de Windows.", "The file goes to the Windows Recycle Bin."),
    ("Supprimer aussi : {}", "Also delete: {}"),
    ("Renommés avec lui : {}.", "Renamed along with it: {}."),
    ("Annuler", "Cancel"),
    ("Opération impossible : {}", "Operation failed: {}"),
    ("Poursuivre un enregistrement", "Continue a recording"),
    ("Sauvegarde « {} » (pas {}). L'enregistrement choisi reprend à ce pas ; ses instantanés postérieurs sont supprimés.", "Save “{}” (step {}). The chosen recording resumes at this step; its later snapshots are deleted."),
    ("Aucun enregistrement compatible : il faut les mêmes conditions initiales et un instantané antérieur à la sauvegarde.", "No compatible recording: it needs the same initial conditions and a snapshot earlier than the save."),
    ("%d/%m/%Y %H:%M", "%Y-%m-%d %H:%M"),
    ("{} · {} instantanés{} · reprise après le pas {}", "{} · {} snapshots{} · resumes after step {}"),
    ("{} instantanés postérieurs seront supprimés", "{} later snapshots will be deleted"),
    ("Poursuivre", "Continue"),
    ("Lecture de la sauvegarde impossible : {}", "Could not read the save: {}"),
    ("Nouvelle simulation", "New simulation"),
    ("Nom", "Name"),
    ("Nom des sauvegardes (F5) et des enregistrements de cette simulation. Par défaut : date, nombre de particules, répartition et vitesse initiale.", "Name of this simulation's saves (F5) and recordings. Default: date, particle count, distribution and initial velocity."),
    ("Revenir au nom tiré des paramètres", "Go back to the name derived from the parameters"),
    ("Nombre de particules", "Number of particles"),
    ("Nombre de corps au départ, tous de masse 1. Le coût d'un pas croît à peu près comme n·log n ; le maximum proposé dépend du moteur et de la mémoire de la machine.", "Number of bodies at the start, all of mass 1. The cost of a step grows roughly as n·log n; the maximum offered depends on the engine and on the machine's memory."),
    ("Rayon du nuage", "Cloud radius"),
    ("Rayon de la sphère ou du disque initial (unité de longueur libre). C'est aussi le rayon de référence du nettoyage.", "Radius of the initial sphere or disc (free length unit). It is also the reference radius of the clean-up."),
    ("Graine aléatoire", "Random seed"),
    ("Mêmes paramètres et même graine : même nuage initial.", "Same parameters and same seed: same initial cloud."),
    ("Nouvelle graine", "New seed"),
    ("Répartition", "Distribution"),
    ("Sphère pleine de densité uniforme, ou disque dans le plan (x, y).", "Solid sphere of uniform density, or disc in the (x, y) plane."),
    ("Sphère", "Sphere"),
    ("Disque", "Disc"),
    ("   Épaisseur (× rayon)", "   Thickness (× radius)"),
    ("Épaisseur du disque, en fraction de son rayon.", "Thickness of the disc, as a fraction of its radius."),
    ("   Profil de densité", "   Density profile"),
    ("Densité surfacique uniforme, ou décroissant exponentiellement avec la distance au centre.", "Uniform surface density, or decreasing exponentially with the distance to the centre."),
    ("Uniforme", "Uniform"),
    ("Exponentiel", "Exponential"),
    ("   Longueur d'échelle (× rayon)", "   Scale length (× radius)"),
    ("Distance, en fraction du rayon, sur laquelle la densité du disque est divisée par e.", "Distance, as a fraction of the radius, over which the disc density is divided by e."),
    ("Vitesse initiale", "Initial velocity"),
    ("Repos : effondrement libre. Aléatoire : directions isotropes. Circulaire : rotation autour de Oz, à vitesse imposée ou à la vitesse d'équilibre orbital (calculée sur la masse intérieure). Radiale : chute vers le centre.", "At rest: free collapse. Random: isotropic directions. Circular: rotation about Oz, at a set speed or at the orbital equilibrium speed (computed from the enclosed mass). Radial: fall towards the centre."),
    ("   Dispersion (±)", "   Spread (±)"),
    ("Écart aléatoire autour de la vitesse orbitale d'équilibre (± ce pourcentage).", "Random deviation around the orbital equilibrium speed (± this percentage)."),
    ("   Facteur", "   Factor"),
    ("Multiple de la vitesse orbitale d'équilibre : 1 donne des orbites circulaires, moins fait s'effondrer le nuage, plus le fait s'étendre.", "Multiple of the equilibrium orbital speed: 1 gives circular orbits, less makes the cloud collapse, more makes it expand."),
    ("   Norme : plage", "   Magnitude: range"),
    ("Norme de la vitesse, tirée uniformément entre le minimum et le maximum.", "Speed magnitude, drawn uniformly between the minimum and the maximum."),
    ("Vitesse caractéristique √(GM/R) ≈ {} · temps de chute libre ≈ {}", "Characteristic speed √(GM/R) ≈ {} · free-fall time ≈ {}"),
    ("Paramètres avancés", "Advanced parameters"),
    ("Sans affichage (enregistrement automatique)", "Without display (automatic recording)"),
    ("Calcule sans dessiner les corps, toute la carte graphique pour le calcul. Un écran de suivi montre l'avancement ; à la cible (ou à l'arrêt), l'enregistrement est clos et l'état final sauvegardé. Aussi en ligne de commande : stellar-cli.exe (stellar-cli --help).", "Computes without drawing the bodies, the whole graphics card for the computation. A monitoring screen shows the progress; at the target (or when stopped), the recording is closed and the final state saved. Also from the command line: stellar-cli.exe (stellar-cli --help)."),
    ("   Arrêt :", "   Stop:"),
    ("pas", "steps"),
    ("(≈ {} pas)", "(≈ {} steps)"),
    ("Enregistrer dès le départ (pour relecture fluide)", "Record from the start (for smooth playback)"),
    ("   Un instantané tous les", "   One snapshot every"),
    ("Intervalle entre deux instantanés enregistrés ; la relecture interpole entre eux.", "Interval between two recorded snapshots; playback interpolates between them."),
    ("Poussière limitée à", "Dust limited to"),
    ("Nombre maximal de particules de masse 1 enregistrées par instantané \
                     (choisies de façon stable) ; les corps issus de fusions sont toujours \
                     gardés.", "Maximum number of mass-1 particles recorded per snapshot (chosen consistently); bodies resulting from mergers are always kept."),
    ("particules", "particles"),
    ("≈ {} Mo par instantané au départ (diminue avec les fusions)", "≈ {} MB per snapshot at the start (decreases with mergers)"),
    ("{} Go", "{} GB"),
    ("{} Mo", "{} MB"),
    ("Mémoire estimée : ≈ {}", "Estimated memory: ≈ {}"),
    (" · mémoire vidéo ≈ {}", " · video memory ≈ {}"),
    ("Durée estimée du premier pas : ≈ {}", "Estimated duration of the first step: ≈ {}"),
    ("Estimation tirée des mesures du banc d'essai (RTX 4060 Ti pour le GPU, processeur de la même machine pour le CPU) ; elle varie avec le matériel, la répartition et θ.", "Estimate based on benchmark measurements (RTX 4060 Ti for the GPU, the same machine's processor for the CPU); it varies with the hardware, the distribution and θ."),
    ("▶ Lancer sans affichage", "▶ Start without display"),
    ("▶ Lancer la simulation", "▶ Start the simulation"),
    ("Constante G", "Constant G"),
    ("Constante de gravitation (unités libres).", "Gravitational constant (free units)."),
    ("Déduire automatiquement dt, ε, r₀ et la liaison des amas des conditions initiales", "Derive dt, ε, r₀ and the cluster linking length automatically from the initial conditions"),
    ("Recalculer les valeurs suggérées", "Recompute the suggested values"),
    ("Pas de temps dt", "Time step dt"),
    ("Pas de temps fixe de l'intégrateur (Leapfrog). Plus petit : plus précis, mais plus lent. Par défaut : temps de chute libre / 500.", "Fixed time step of the (Leapfrog) integrator. Smaller: more accurate, but slower. Default: free-fall time / 500."),
    ("Adoucissement ε", "Softening ε"),
    ("Adoucissement de Plummer : borne l'attraction lors des rencontres très proches. Par défaut : 0,3 × distance moyenne entre voisins.", "Plummer softening: bounds the attraction during very close encounters. Default: 0.3 × mean distance between neighbours."),
    ("Rayon r₀ (masse 1)", "Radius r₀ (mass 1)"),
    ("Rayon d'un corps de masse 1 (r = r₀ · m^⅓) : il décide des contacts et donc des fusions. 0 : aucune fusion.", "Radius of a body of mass 1 (r = r₀ · m^⅓): it decides contacts and hence mergers. 0: no mergers."),
    ("Amas éjectés : liaison", "Ejected clusters: linking length"),
    ("Deux corps lointains plus proches que cette distance appartiennent au même groupe (par défaut : distance moyenne entre voisins au départ).", "Two distant bodies closer than this distance belong to the same group (default: mean distance between neighbours at the start)."),
    ("Précision Barnes-Hut θ", "Barnes-Hut accuracy θ"),
    ("Critère d'ouverture : un groupe de corps vu sous un angle inférieur à θ est remplacé par son centre de masse. Plus petit : plus précis, mais plus lent.", "Opening criterion: a group of bodies seen under an angle smaller than θ is replaced by its centre of mass. Smaller: more accurate, but slower."),
    ("Nettoyage : période (pas)", "Clean-up: period (steps)"),
    ("Tous les combien de pas les corps éjectés sont éliminés (0 : jamais).", "How many steps between removals of ejected bodies (0: never)."),
    ("Nettoyage : distance (× rayon)", "Clean-up: distance (× radius)"),
    ("Seuls les corps non liés situés au-delà de cette distance du barycentre (en rayons du nuage) sont éliminés ; les amas éjectés liés sont conservés.", "Only unbound bodies beyond this distance from the barycentre (in cloud radii) are removed; bound ejected clusters are kept."),
    ("Amas éjectés : corps au moins", "Ejected clusters: at least (bodies)"),
    ("Un amas éjecté lié d'au moins ce nombre de corps est conservé par le nettoyage (0 : critère ignoré).", "A bound ejected cluster with at least this many bodies is kept by the clean-up (0: criterion ignored)."),
    ("… ou masse au moins", "… or at least (mass)"),
    ("… ou d'au moins cette part de la masse initiale, même s'il ne compte qu'un corps (0 : critère ignoré).", "… or with at least this share of the initial mass, even if it has a single body (0: criterion ignored)."),
    ("Moteur de calcul", "Computation engine"),
    ("GPU : carte graphique, simple précision, bien plus rapide. CPU : tous les cœurs sauf un, double précision.", "GPU: graphics card, single precision, much faster. CPU: all cores but one, double precision."),
    ("Simulations sauvegardées", "Saved simulations"),
    ("Actualiser", "Refresh"),
    ("Parcourir…", "Browse…"),
    ("Aucune sauvegarde dans le dossier « saves ». Appuyez sur F5 pendant une simulation.", "No save in the “saves” folder. Press F5 during a simulation."),
    ("{} · {} corps · pas {} · t = {}", "{} · {} bodies · step {} · t = {}"),
    ("Reprend l'enregistrement « {} »", "Resumes the recording “{}”"),
    ("Charger", "Load"),
    ("Poursuivre un enregistrement…", "Continue a recording…"),
    ("Charger cette sauvegarde et prolonger un enregistrement existant à partir de son pas", "Load this save and extend an existing recording from its step"),
    ("Renommer…", "Rename…"),
    ("Supprimer…", "Delete…"),
    ("Enregistrements", "Recordings"),
    ("Aucun enregistrement dans le dossier « recordings ». Appuyez sur F9 pendant une simulation.", "No recording in the “recordings” folder. Press F9 during a simulation."),
    ("{} · {} instantanés · {} corps au départ · durée {} · {}{}", "{} · {} snapshots · {} bodies at the start · duration {} · {}{}"),
    ("Rejouer", "Play"),
    ("Reprendre", "Resume"),
    ("Recharge la sauvegarde « {} » (pas {}) et poursuit l'enregistrement", "Reloads the save “{}” (step {}) and continues the recording"),
    ("Maximum sur cette machine : {}", "Maximum on this machine: {}"),
    ("Valeur exacte", "Exact value"),
    ("Le corps suivi a été éjecté et éliminé.", "The followed body was ejected and removed."),
    ("Traces effacées.", "Traces cleared."),
    ("La simulation n'est pas encore prête.", "The simulation is not ready yet."),
    ("Aucun corps visible à l'écran : rien à tracer.", "No body visible on screen: nothing to trace."),
    ("Traces activées sur les {} corps visibles les plus massifs.", "Traces enabled on the {} most massive visible bodies."),
    ("Référentiel des traces : {}", "Trace reference frame: {}"),
    (" — sélectionnez un corps (barycentre en attendant)", " — select a body (barycentre meanwhile)"),
    ("Barycentre vu de face au disque (axe incliné de {}° sur Oz).", "Barycentre seen face-on to the disc (axis tilted {}° from Oz)."),
    ("Barycentre : aucune rotation d'ensemble, vue selon l'axe Oz.", "Barycentre: no overall rotation, view along the Oz axis."),
    ("Barycentre de la sous-simulation #{} sélectionné.", "Barycentre of sub-simulation #{} selected."),
    ("Sous-simulation réunie : suivi de l'ensemble #{}.", "Sub-simulation merged: following the combined set #{}."),
    ("Sous-simulation réunie à la simulation : suivi du barycentre global.", "Sub-simulation merged back into the simulation: following the overall barycentre."),
    ("Visée selon la normale au disque : rien à redresser.", "Looking along the disc normal: nothing to level."),
    ("Suivi interrompu : vol libre.", "Following stopped: free flight."),
    ("Suivi du barycentre.", "Following the barycentre."),
    ("Suivi du corps sélectionné.", "Following the selected body."),
    ("Rien à suivre : sélectionnez un corps (clic, Origine) ou le barycentre (B).", "Nothing to follow: select a body (click, Home) or the barycentre (B)."),
    ("Vue d'ensemble : 90 % de la masse dans un rayon de {}", "Overview: 90% of the mass within a radius of {}"),
    ("C'est déjà le corps le plus léger.", "This is already the lightest body."),
    ("C'est déjà le corps le plus massif.", "This is already the most massive body."),
    ("Enregistrement indisponible pendant une relecture.", "Recording is unavailable during playback."),
    ("Enregistrement arrêté.", "Recording stopped."),
    ("Enregistrement démarré : un instantané tous les {} pas.", "Recording started: one snapshot every {} steps."),
    ("Sauvegarde indisponible pendant une relecture.", "Saving is unavailable during playback."),
    ("Sauvegarde en cours…", "Saving…"),
    ("Sauvegardé : {}", "Saved: {}"),
    ("Échec de la sauvegarde : {}", "Save failed: {}"),
    ("Génération de {} particules et premier calcul des forces…", "Generating {} particles and computing the first forces…"),
    ("Le moteur de calcul n'a pas pu démarrer :\n{}\n\nÉchap → Retour à l'accueil, puis choisir un autre moteur dans les paramètres avancés.", "The computation engine could not start:\n{}\n\nEsc → Back to the home screen, then choose another engine in the advanced parameters."),
    ("Tab : libérer la souris · Échap : menu · F : suivre / cesser · A / E : bascule · H : horizon · G : grille · C : barycentre · V : vue d'ensemble · T : traces", "Tab: release the mouse · Esc: menu · F: follow / stop · A / E: roll · H: horizon · G: grid · C: barycentre · V: overview · T: traces"),
    ("Souris libre — clic gauche : suivre un corps · clic droit ou Tab : reprendre la visée · Échap : menu", "Mouse released — left click: follow a body · right click or Tab: resume aiming · Esc: menu"),
    ("Relecture", "Playback"),
    ("Corps", "Bodies"),
    ("Barycentre de la sous-simulation #{}", "Barycentre of sub-simulation #{}"),
    ("Barycentre sélectionné", "Barycentre selected"),
    ("Masse", "Mass"),
    ("Vitesse", "Velocity"),
    ("Taille", "Size"),
    ("Distance au barycentre global", "Distance to the overall barycentre"),
    ("Masse présente", "Mass present"),
    ("Distance caméra", "Camera distance"),
    ("Axe de rotation", "Rotation axis"),
    ("{}° sur Oz", "{}° from Oz"),
    ("aucune rotation", "no rotation"),
    ("Vue de face (B)", "Face-on view (B)"),
    ("Cesser le suivi (F)", "Stop following (F)"),
    ("Suivre (F)", "Follow (F)"),
    ("Désélectionner", "Deselect"),
    ("Corps sélectionné", "Selected body"),
    ("Identifiant", "Identifier"),
    ("Rang (masse)", "Rank (mass)"),
    ("Rayon", "Radius"),
    ("Dist. barycentre", "Dist. to barycentre"),
    ("Aucun — visez et cliquez, ou touches Origine ou B (barycentre).", "None — aim and click, or press Home or B (barycentre)."),
    ("Les plus massifs", "Most massive"),
    ("Sous-simulations", "Sub-simulations"),
    ("Amas éjectés détachés : chacun est calculé à part, dans son propre repère, et ressent les autres par leur champ de marée. Deux sous-simulations qui se rapprochent sont réunies.", "Detached ejected clusters: each is computed separately, in its own frame, and feels the others through their tidal field. Two sub-simulations that come close are merged."),
    ("Repère", "Anchor"),
    ("Taille : {}
Vitesse : {}
Cliquer pour centrer la caméra sur son barycentre.", "Size: {}\nSpeed: {}\nClick to centre the camera on its barycentre."),
    ("Amas éjectés", "Ejected clusters"),
    ("Groupes liés de corps lointains, conservés par le nettoyage (seuils dans les paramètres avancés). Cliquer sur un amas sélectionne son corps le plus massif.", "Bound groups of distant bodies, kept by the clean-up (thresholds in the advanced parameters). Clicking a cluster selects its most massive body."),
    ("Aucun nettoyage effectué pour l'instant.", "No clean-up performed yet."),
    ("Aucun au dernier nettoyage (pas {}).", "None at the last clean-up (step {})."),
    ("Au dernier nettoyage (pas {}) :", "At the last clean-up (step {}):"),
    ("Rayon de demi-masse : {}
Vitesse : {}", "Half-mass radius: {}\nVelocity: {}"),
    ("… et {} autres", "… and {} more"),
    ("Effacer", "Clear"),
    ("Tracer les 10 plus gros visibles", "Trace the 10 largest visible"),
    ("Historique", "History"),
    ("Queue", "Tail"),
    ("Référentiel (R)", "Reference frame (R)"),
    ("Orbites vues depuis le centre de gravité de la simulation.", "Orbits seen from the simulation's centre of mass."),
    ("Aucun corps sélectionné : traces relatives au barycentre en attendant.", "No body selected: traces relative to the barycentre meanwhile."),
    ("Centre de gravité de l'amas lié du corps sélectionné ({} corps suivis).", "Centre of mass of the selected body's bound cluster ({} bodies tracked)."),
    ("Orbites vues depuis le corps sélectionné.", "Orbits seen from the selected body."),
    ("#{} (éliminé)", "#{} (removed)"),
    ("échec du moteur", "engine failure"),
    ("en pause", "paused"),
    ("en cours", "running"),
    ("État", "State"),
    ("Moteur", "Engine"),
    ("Pas", "Step"),
    ("Temps simulé", "Simulated time"),
    ("Pas / s", "Steps / s"),
    ("Durée d'un pas", "Step duration"),
    ("Images / s", "Frames / s"),
    ("Énergie totale", "Total energy"),
    ("Masse éjectée", "Ejected mass"),
    ("ΔE inclut les pertes par fusion et éjection.", "ΔE includes the losses through mergers and ejections."),
    ("▶ Reprendre", "▶ Resume"),
    ("Pas à pas", "Single step"),
    ("Limiter à", "Limit to"),
    (" pas/s", " steps/s"),
    ("Luminosité", "Brightness"),
    ("Poussières", "Dust"),
    ("Vitesse caméra", "Camera speed"),
    ("Fraction de la distance au corps sélectionné (ou au barycentre) parcourue chaque seconde.", "Fraction of the distance to the selected body (or to the barycentre) travelled each second."),
    ("(+/−, Maj, Ctrl)", "(+/−, Shift, Ctrl)"),
    ("Grille du plan du disque (G)", "Disc plane grid (G)"),
    ("Enregistrement (F9)", "Recording (F9)"),
    ("{} instantanés · {} · 1 tous les {} pas", "{} snapshots · {} · 1 every {} steps"),
    ("■ Arrêter", "■ Stop"),
    ("Un instantané tous les", "One snapshot every"),
    ("Dernier : {}", "Last: {}"),
    ("Enregistrer", "Record"),
    ("chargement…", "loading…"),
    ("lecture arrière", "playing backwards"),
    ("Instantané", "Snapshot"),
    ("Corps affichés", "Bodies shown"),
    ("Poussière", "Dust"),
    ("1 sur {}", "1 in {}"),
    ("Intervalle", "Interval"),
    ("Budget poussière", "Dust budget"),
    ("temps", "time"),
    ("Début", "Start"),
    ("Instantané précédent (←)", "Previous snapshot (←)"),
    ("Lecture / pause (Espace)", "Play / pause (Space)"),
    ("Instantané suivant (→)", "Next snapshot (→)"),
    ("Fin", "End"),
    ("Arrière", "Backwards"),
    ("Retour arrière (⌫)", "Reverse (⌫)"),
    (" inst./s", " snap./s"),
    ("↑ / ↓ : vitesse · ← / → : instantané · ⌫ : sens", "↑ / ↓: speed · ← / →: snapshot · ⌫: direction"),
    ("Sauvegarder (F5)", "Save (F5)"),
    ("Retour à l'accueil", "Back to the home screen"),
    ("Commandes", "Controls"),
    ("avancer / reculer (en suivi : distance à la cible)", "forward / back (when following: distance to the target)"),
    ("gauche / droite (sans effet en suivi)", "left / right (no effect when following)"),
    ("basculer la vue autour de l'axe de visée", "roll the view around the line of sight"),
    ("redresser l'horizon sur le plan du disque", "level the horizon on the disc plane"),
    ("Maj / Ctrl", "Shift / Ctrl"),
    ("accélérer / ralentir", "speed up / slow down"),
    ("vitesse (proportionnelle à la distance à la sélection)", "speed (proportional to the distance to the selection)"),
    ("Souris", "Mouse"),
    ("orientation (en suivi : tourner autour de la cible)", "orientation (when following: orbit the target)"),
    ("Molette", "Wheel"),
    ("Ctrl + molette", "Ctrl + wheel"),
    ("zoom optique", "optical zoom"),
    ("Clic gauche", "Left click"),
    ("suivre le corps visé", "follow the body aimed at"),
    ("suivre la sélection / cesser le suivi", "follow the selection / stop following"),
    ("grille du plan du disque", "disc plane grid"),
    ("Origine", "Home"),
    ("corps le plus massif", "most massive body"),
    ("corps suivant / précédent par masse", "next / previous body by mass"),
    ("barycentre : sélection, vue de face au disque", "barycentre: selection, face-on view of the disc"),
    ("viser le centre de gravité", "aim at the centre of mass"),
    ("vue d'ensemble (90 % de la masse)", "overview (90% of the mass)"),
    ("tracer les 10 plus massifs visibles / effacer", "trace the 10 most massive visible / clear"),
    ("traces : référentiel corps sélectionné / barycentre / amas local", "traces: reference frame selected body / barycentre / local cluster"),
    ("Espace / P", "Space / P"),
    ("pause / pas à pas", "pause / single step"),
    ("libérer / capturer la souris", "release / capture the mouse"),
    ("Clic droit", "Right click"),
    ("reprendre la visée (souris libre)", "resume aiming (mouse released)"),
    ("sauvegarder", "save"),
    ("démarrer / arrêter l'enregistrement", "start / stop recording"),
    ("quitter", "quit"),
    ("Espace", "Space"),
    ("relecture : lecture / pause", "playback: play / pause"),
    ("relecture : instantané précédent / suivant", "playback: previous / next snapshot"),
    ("relecture : vitesse", "playback: speed"),
    ("relecture : sens de lecture", "playback: direction"),
    ("Échap", "Esc"),
    ("ce menu", "this menu"),
    ("Amas local", "Local cluster"),
    ("[{}] Plantage dans le thread « {} »\n{}\n\n{}\n", "[{}] Crash in thread “{}”\n{}\n\n{}\n"),
    ("Stellar — erreur fatale", "Stellar — fatal error"),
    ("{}\n\nDétails enregistrés dans {}.", "{}\n\nDetails written to {}."),
    ("Carte graphique : {}", "Graphics card: {}"),
    ("Moteur GPU indisponible : {}", "GPU engine unavailable: {}"),
    ("Usage : stellar-cli [options]        nouvelle simulation sans fenêtre
        stellar-cli --load FICHIER        reprendre une sauvegarde (.stellar)
        stellar-cli --resume FICHIER      reprendre un enregistrement (.stellarec)
        stellar-cli --help                cette aide
L'interface graphique est stellar.exe (sans argument).

Nouvelle simulation :
  --bodies N         nombre de particules (défaut : 1000000)
  --radius R         rayon du nuage (défaut : 100)
  --seed S           graine aléatoire (défaut : 42)
  --disc             disque en rotation (sinon sphère)
  --velocity V       rest | random | circ | orb | rad (défaut : orb)
  --spread D         dispersion de la vitesse orbitale (défaut : 0,05)
  --factor F         multiple de la vitesse orbitale : < 1 effondrement,
                     > 1 expansion (défaut : 1)
  --vmin A --vmax B  plage de vitesse (random, circ, rad)
  --engine M         gpu | cpu (défaut : gpu si une carte convient)

Reprise :
  --load F           sauvegarde à reprendre ; son enregistrement lié est poursuivi,
                     sinon un nouvel enregistrement commence
  --continue F       avec --load : enregistrement compatible à prolonger depuis
                     le pas de la sauvegarde (instantanés postérieurs supprimés)
  --resume F         enregistrement à poursuivre depuis sa dernière sauvegarde liée

Dans tous les cas :
  --name NOM         nom de la simulation (défaut : date et paramètres, ou nom du fichier)
  --interval N       un instantané tous les N pas (défaut : 10)
  --dust N           poussière enregistrée par instantané, ou « all » (défaut : 1000000)
  --steps N          s'arrêter après N pas (de plus, en reprise)
  --time T           s'arrêter après un temps simulé T (de plus, en reprise)
  --duration M       s'arrêter après M minutes de calcul
Sans cible : jusqu'à Ctrl+C. À l'arrêt, l'enregistrement est clos et l'état final
sauvegardé (dossiers « {} » et « {} »).", "Usage: stellar-cli [options]        new simulation without a window\n       stellar-cli --load FILE           resume a save (.stellar)\n       stellar-cli --resume FILE         resume a recording (.stellarec)\n       stellar-cli --help                this help\nThe graphical interface is stellar.exe (without arguments).\n\nNew simulation:\n  --bodies N         number of particles (default: 1000000)\n  --radius R         cloud radius (default: 100)\n  --seed S           random seed (default: 42)\n  --disc             rotating disc (otherwise a sphere)\n  --velocity V       rest | random | circ | orb | rad (default: orb)\n  --spread D         spread of the orbital speed (default: 0.05)\n  --factor F         multiple of the orbital speed: < 1 collapse,\n                     > 1 expansion (default: 1)\n  --vmin A --vmax B  speed range (random, circ, rad)\n  --engine M         gpu | cpu (default: gpu if a suitable card is present)\n\nResuming:\n  --load F           save to resume; its linked recording is continued,\n                     otherwise a new recording starts\n  --continue F       with --load: compatible recording to extend from the\n                     save's step (later snapshots deleted)\n  --resume F         recording to continue from its latest linked save\n\nIn all cases:\n  --name NAME        simulation name (default: date and parameters, or file name)\n  --interval N       one snapshot every N steps (default: 10)\n  --dust N           dust recorded per snapshot, or “all” (default: 1000000)\n  --steps N          stop after N steps (more, when resuming)\n  --time T           stop after a simulated time T (more, when resuming)\n  --duration M       stop after M minutes of computation\nWithout a target: until Ctrl+C. When stopped, the recording is closed and the final\nstate saved (folders “{}” and “{}”).\nThe former French option names (--corps, --pas…) are still accepted."),
    ("valeur manquante après {}", "missing value after {}"),
    ("valeur invalide pour {} : {}", "invalid value for {}: {}"),
    ("argument inconnu : {}", "unknown argument: {}"),
    ("--continue s'emploie avec --load", "--continue is used with --load"),
    ("--load et --resume s'excluent", "--load and --resume are mutually exclusive"),
    ("{} ne s'applique qu'à une nouvelle simulation", "{} only applies to a new simulation"),
    ("vitesse inconnue : {}", "unknown velocity: {}"),
    ("stellar.exe est l'interface graphique et ne prend pas d'argument. Ligne de commande : stellar-cli.exe (stellar-cli --help).", "stellar.exe is the graphical interface and takes no argument. Command line: stellar-cli.exe (stellar-cli --help)."),
    ("Erreur : {}", "Error: {}"),
    ("Voir stellar-cli --help pour la liste des options.", "See stellar-cli --help for the list of options."),
    ("Nouvelle simulation « {} » : {} corps, moteur {}.", "New simulation “{}”: {} bodies, engine {}."),
    ("Reprise de la sauvegarde {}.", "Resuming the save {}."),
    ("Reprise de l'enregistrement {} depuis la sauvegarde {}.", "Resuming the recording {} from the save {}."),
    ("Erreur : aucune sauvegarde du dossier « {} » n'est liée à {} ; utiliser --load SAUVEGARDE --continue ENREGISTREMENT.", "Error: no save in the “{}” folder is linked to {}; use --load SAVE --continue RECORDING."),
    ("Enregistrement poursuivi : {}", "Recording continued: {}"),
    ("pas {} · t = {} · {} corps · {} pas/s · {} instantanés · {}", "step {} · t = {} · {} bodies · {} steps/s · {} snapshots · {}"),
    ("Arrêt : clôture de l'enregistrement et sauvegarde…", "Stopping: closing the recording and saving…"),
    ("Terminé au pas {}.", "Finished at step {}."),
    ("Sauvegarde : {}", "Save: {}"),
    ("Enregistrement : {}", "Recording: {}"),
    ("Échec : {}", "Failure: {}"),
    ("Simulation sans affichage", "Simulation without display"),
    ("{} % · reste ≈ {}", "{}% · ≈ {} left"),
    ("préparation…", "preparing…"),
    ("Cible", "Target"),
    ("arrêt manuel", "manual stop"),
    ("{} pas", "{} steps"),
    ("temps simulé {}", "simulated time {}"),
    ("durée {}", "duration {}"),
    ("Durée écoulée", "Elapsed time"),
    ("Enregistrement", "Recording"),
    ("{} instantanés · {}", "{} snapshots · {}"),
    ("■ Arrêter et sauvegarder", "■ Stop and save"),
    ("Afficher la simulation", "Show the simulation"),
    ("Le calcul et l'enregistrement continuent ; la cible n'est plus surveillée.", "The computation and the recording go on; the target is no longer watched."),
    ("À l'arrêt, l'enregistrement est clos et l'état final sauvegardé sous le nom de la simulation.", "When stopped, the recording is closed and the final state saved under the simulation's name."),
    ("Clôture de l'enregistrement et sauvegarde…", "Closing the recording and saving…"),
    ("CPU — Barnes-Hut multithread", "CPU — multithreaded Barnes-Hut"),
    ("Octree de Barnes-Hut sur tous les cœurs (sauf un, réservé à l'affichage).", "Barnes-Hut octree on all cores (but one, kept for the display)."),
    ("GPU — Barnes-Hut (carte graphique)", "GPU — Barnes-Hut (graphics card)"),
    ("Arbre de Barnes-Hut construit et parcouru sur la carte graphique (simple précision).", "Barnes-Hut tree built and traversed on the graphics card (single precision)."),
];

/// Messages of the lower layers (named placeholders of the source written
/// `{}`). A test checks that every French message of those crates is here.
const MESSAGES: &[(&str, &str)] = &[
    // stellar-application
    ("erreur d'entrée/sortie : {}", "input/output error: {}"),
    ("fichier invalide : {}", "invalid file: {}"),
    ("version de format non prise en charge : {}", "unsupported format version: {}"),
    ("moteur de calcul inconnu : « {} »", "unknown computation engine: “{}”"),
    ("la simulation n'est pas encore prête", "the simulation is not ready yet"),
    ("le moteur de calcul n'a pas pu démarrer", "the computation engine could not start"),
    ("thread d'enregistrement interrompu", "recording thread interrupted"),
    ("enregistrement déjà clos", "recording already closed"),
    ("enregistrement vide", "empty recording"),
    ("l'enregistrement provient d'une autre simulation (conditions initiales différentes)", "the recording comes from another simulation (different initial conditions)"),
    ("l'enregistrement commence après le pas de la sauvegarde", "the recording starts after the save's step"),
    ("le corps n° {} de la sauvegarde est absent de l'enregistrement au pas {} : il provient d'une autre simulation", "body no. {} of the save is missing from the recording at step {}: it comes from another simulation"),
    // stellar-persistence
    ("fichier introuvable : {}", "file not found: {}"),
    ("le nom « {} » est déjà pris", "the name “{}” is already taken"),
    ("suppression annulée ou impossible (code {})", "deletion cancelled or impossible (code {})"),
    ("fichier tronqué", "truncated file"),
    ("signature absente : ce n'est pas une sauvegarde Stellar", "missing signature: this is not a Stellar save"),
    ("en-tête démesuré", "oversized header"),
    ("tableaux de corps incohérents", "inconsistent body arrays"),
    ("instantané tronqué", "truncated snapshot"),
    ("varint invalide", "invalid varint"),
    ("nombre de corps incohérent", "inconsistent body count"),
    ("nombre de sous-systèmes invalide", "invalid subsystem count"),
    ("sous-système inconnu", "unknown subsystem"),
    ("devenir inconnu", "unknown fate"),
    ("ce n'est pas un enregistrement Stellar", "this is not a Stellar recording"),
    ("instantané hors limites", "snapshot out of range"),
    ("l'instantané du pas {} est introuvable dans l'enregistrement", "the snapshot of step {} is missing from the recording"),
    // stellar-engine-gpu
    ("la carte graphique ne gère pas les shaders de calcul", "the graphics card does not support compute shaders"),
    ("aucune carte graphique compatible n'a été trouvée", "no compatible graphics card was found"),
    ("ouverture de la carte graphique impossible : {}", "cannot open the graphics card: {}"),
    ("le moteur GPU est limité à {} corps ({} demandés)", "the GPU engine is limited to {} bodies ({} requested)"),
    ("identifiants de corps trop grands pour le moteur GPU", "body identifiers too large for the GPU engine"),
    ("trop de corps pour cette carte graphique ({} : tampon de {} Mo, limite {} Mo)", "too many bodies for this graphics card ({}: buffer of {} MB, limit {} MB)"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_placeholders_in_order() {
        assert_eq!(fill("{} sur {}", &[&1, &"deux"]), "1 sur deux");
        assert_eq!(fill("sans", &[]), "sans");
        assert_eq!(fill("{} {}", &[&1]), "1 {}");
    }

    #[test]
    fn table_has_no_duplicates_nor_missing_placeholders() {
        let mut seen = std::collections::HashSet::new();
        for (fr, en) in EN {
            assert!(seen.insert(fr), "en double : {fr}");
            assert_eq!(fr.matches("{}").count(), en.matches("{}").count(), "{fr}");
        }
        let mut seen = std::collections::HashSet::new();
        for (fr, en) in MESSAGES {
            assert!(seen.insert(fr), "duplicate: {fr}");
            assert!(!fr.contains("{}{}"), "adjacent placeholders: {fr}");
            assert_eq!(fr.matches("{}").count(), en.matches("{}").count(), "{fr}");
        }
    }

    #[test]
    fn translates_nested_messages() {
        apply(Lang::En);
        assert_eq!(tr_msg("fichier invalide : fichier tronqué"), "invalid file: truncated file");
        assert_eq!(
            tr_msg("le moteur GPU est limité à 16776960 corps (20000000 demandés)"),
            "the GPU engine is limited to 16776960 bodies (20000000 requested)"
        );
        assert_eq!(tr_msg("erreur d'entrée/sortie : Access denied"), "input/output error: Access denied");
        assert_eq!(tr_msg("galaxie"), "galaxie");
        assert_eq!(
            tr!("Chargement impossible : {}", "fichier invalide : varint invalide"),
            fill(tr("Chargement impossible : {}"), &[&"invalid file: invalid varint"])
        );
        apply(Lang::Fr);
        assert_eq!(tr_msg("fichier tronqué"), "fichier tronqué");
    }

    /// Every French string literal of the lower crates (outside their tests)
    /// has an English translation in `MESSAGES` or `EN`.
    #[test]
    fn lower_layer_messages_are_translated() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let known: std::collections::HashSet<&str> =
            MESSAGES.iter().chain(EN).map(|(fr, _)| *fr).collect();
        let mut missing = Vec::new();
        for krate in [
            "stellar-domain",
            "stellar-application",
            "stellar-persistence",
            "stellar-engine-cpu",
            "stellar-engine-gpu",
        ] {
            let mut dirs = vec![root.join(krate).join("src")];
            while let Some(dir) = dirs.pop() {
                for entry in std::fs::read_dir(&dir).unwrap() {
                    let path = entry.unwrap().path();
                    if path.is_dir() {
                        dirs.push(path);
                    } else if path.extension().is_some_and(|e| e == "rs") {
                        let source = std::fs::read_to_string(&path).unwrap();
                        let code = source.split("#[cfg(test)]").next().unwrap();
                        for literal in literals(code) {
                            let template = named_placeholders(&literal);
                            if looks_french(&template) && !known.contains(template.as_str()) {
                                missing.push(format!("{}: {template}", path.display()));
                            }
                        }
                    }
                }
            }
        }
        assert!(missing.is_empty(), "untranslated messages:\n{}", missing.join("\n"));
    }

    /// String literals of Rust code, outside comments (character literals
    /// and lifetimes skipped).
    fn literals(code: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut chars = code.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '/' if chars.peek() == Some(&'/') => {
                    for c in chars.by_ref() {
                        if c == '\n' {
                            break;
                        }
                    }
                }
                '\'' => {
                    // '"', '\"' or a lifetime: never the start of a string.
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                        chars.next();
                    } else if chars.peek() == Some(&'"') {
                        chars.next();
                    }
                }
                '"' => {
                    let mut s = String::new();
                    while let Some(c) = chars.next() {
                        match c {
                            '\\' => {
                                // Line continuation: skip the indentation.
                                if chars.next() == Some('\n') {
                                    while chars.peek().is_some_and(|c| c.is_whitespace()) {
                                        chars.next();
                                    }
                                }
                            }
                            '"' => break,
                            c => s.push(c),
                        }
                    }
                    out.push(s);
                }
                _ => {}
            }
        }
        out
    }

    /// `{name}` and `{0}` written `{}`.
    fn named_placeholders(s: &str) -> String {
        let mut out = String::new();
        let mut rest = s;
        while let Some(i) = rest.find('{') {
            out.push_str(&rest[..i]);
            match rest[i..].find('}') {
                Some(j) if rest[i + 1..i + j].chars().all(|c| c.is_alphanumeric() || c == '_') => {
                    out.push_str("{}");
                    rest = &rest[i + j + 1..];
                }
                _ => {
                    out.push('{');
                    rest = &rest[i + 1..];
                }
            }
        }
        out.push_str(rest);
        out
    }

    fn looks_french(s: &str) -> bool {
        s.chars().any(|c| "éèêàùçôîâœ«»".contains(c))
            || [" de ", " le ", " la ", " du ", " des ", " est ", "n'", "l'", "d'"]
                .iter()
                .any(|w| s.contains(w))
    }
}
