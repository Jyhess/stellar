//! Interface language: French or English.
//!
//! Texts are written in English in the code; [`tr`] returns their French
//! translation (table `FR`) when French is chosen, and the original text
//! otherwise. [`tr!`](macro@crate::tr) does the same for a template with `{}`
//! placeholders. A test fails if a text of the interface has no French
//! translation.
//!
//! Errors of the lower layers (domain, application, engines, persistence) are
//! typed; [`crate::errors::Localized`] gives their text.
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

#[cfg(not(test))]
static LANG: AtomicU8 = AtomicU8::new(0);

// Tests run in parallel threads: a test switching the language must not
// change the formatting seen by the others.
#[cfg(test)]
thread_local! {
    static LANG: AtomicU8 = const { AtomicU8::new(0) };
}

fn with_lang<R>(f: impl FnOnce(&AtomicU8) -> R) -> R {
    #[cfg(not(test))]
    return f(&LANG);
    #[cfg(test)]
    LANG.with(f)
}

pub fn lang() -> Lang {
    if with_lang(|l| l.load(Ordering::Relaxed)) == 1 {
        Lang::En
    } else {
        Lang::Fr
    }
}

pub(crate) fn apply(l: Lang) {
    with_lang(|v| v.store(matches!(l, Lang::En) as u8, Ordering::Relaxed));
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
pub fn tr(en: &'static str) -> &'static str {
    if lang() == Lang::En {
        return en;
    }
    static TABLE: OnceLock<HashMap<&'static str, &'static str>> = OnceLock::new();
    TABLE
        .get_or_init(|| FR.iter().copied().collect())
        .get(en)
        .copied()
        .unwrap_or(en)
}

/// Fills the `{}` placeholders of a template, in order.
pub fn fill(template: &str, args: &[&dyn Display]) -> String {
    let mut out = String::with_capacity(template.len() + 16 * args.len());
    let mut rest = template;
    let mut args = args.iter();
    while let Some(i) = rest.find("{}") {
        out.push_str(&rest[..i]);
        match args.next() {
            Some(a) => out.push_str(&a.to_string()),
            None => out.push_str("{}"),
        }
        rest = &rest[i + 2..];
    }
    out.push_str(rest);
    out
}

/// Translated text with `{}` placeholders: `tr!("Mass: {}", m)`.
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
const FR: &[(&str, &str)] = &[
    ("Recording suspended. It will resume when the save “{}” is reloaded.", "Enregistrement suspendu. Il reprendra en rechargeant la sauvegarde « {} »."),
    ("Suspending the recording failed: {}", "La suspension de l'enregistrement a échoué : {}"),
    ("Recording failed: {}", "Enregistrement impossible : {}"),
    ("Recording “{}” continued: it resumes as soon as the computation restarts (Space).", "Enregistrement « {} » poursuivi : il reprend dès la relance du calcul (Espace)."),
    ("Could not resume the recording: {}", "Reprise de l'enregistrement impossible : {}"),
    ("Loading failed: {}", "Chargement impossible : {}"),
    ("Playback failed: {}", "Relecture impossible : {}"),
    ("At rest", "Repos"),
    ("Random (isotropic)", "Aléatoire (isotrope)"),
    ("Circular — set speed", "Circulaire — vitesse imposée"),
    ("Circular — orbital speed", "Circulaire — vitesse orbitale"),
    ("Radial, towards the centre", "Radiale, vers le centre"),
    ("manual (stop button)", "manuel (bouton d'arrêt)"),
    ("after a number of steps", "après un nombre de pas"),
    ("after a simulated time", "après un temps simulé"),
    ("after a real duration", "après une durée réelle"),
    ("Stellar — gravitational N-body simulator (CPU or GPU computation, GPU rendering)", "Stellar — simulateur gravitationnel N-corps (calcul CPU ou GPU, rendu GPU)"),
    ("Quit", "Quitter"),
    ("Language", "Langue"),
    ("Gravitational collapse and particle mergers", "Effondrement gravitationnel et fusion de particules"),
    ("recording “{}”", "enregistrement « {} »"),
    ("save “{}”", "sauvegarde « {} »"),
    ("Delete", "Supprimer"),
    ("Rename", "Renommer"),
    ("the save", "la sauvegarde"),
    ("the recording", "l'enregistrement"),
    ("Delete {} “{}”?", "Supprimer {} « {} » ?"),
    ("The file goes to the Windows Recycle Bin.", "Le fichier part dans la corbeille de Windows."),
    ("Also delete: {}", "Supprimer aussi : {}"),
    ("Renamed along with it: {}.", "Renommés avec lui : {}."),
    ("Cancel", "Annuler"),
    ("Operation failed: {}", "Opération impossible : {}"),
    ("Continue a recording", "Poursuivre un enregistrement"),
    ("Save “{}” (step {}). The chosen recording resumes at this step; its later snapshots are deleted.", "Sauvegarde « {} » (pas {}). L'enregistrement choisi reprend à ce pas ; ses instantanés postérieurs sont supprimés."),
    ("No compatible recording: it needs the same initial conditions and a snapshot earlier than the save.", "Aucun enregistrement compatible : il faut les mêmes conditions initiales et un instantané antérieur à la sauvegarde."),
    ("%Y-%m-%d %H:%M", "%d/%m/%Y %H:%M"),
    ("{} · {} snapshots{} · resumes after step {}", "{} · {} instantanés{} · reprise après le pas {}"),
    ("{} later snapshots will be deleted", "{} instantanés postérieurs seront supprimés"),
    ("Continue", "Poursuivre"),
    ("Could not read the save: {}", "Lecture de la sauvegarde impossible : {}"),
    ("New simulation", "Nouvelle simulation"),
    ("Name", "Nom"),
    ("Name of this simulation's saves (F5) and recordings. Default: date, particle count, distribution and initial velocity.", "Nom des sauvegardes (F5) et des enregistrements de cette simulation. Par défaut : date, nombre de particules, répartition et vitesse initiale."),
    ("Go back to the name derived from the parameters", "Revenir au nom tiré des paramètres"),
    ("Number of particles", "Nombre de particules"),
    ("Number of dust grains at the start (their mass is set below). The cost of a step grows roughly as n·log n; the maximum offered depends on the engine and on the machine's memory.", "Nombre de grains de poussière au départ (leur masse est réglée plus bas). Le coût d'un pas croît à peu près comme n·log n ; le maximum proposé dépend du moteur et de la mémoire de la machine."),
    ("Grain mass", "Masse des grains"),
    ("Maximum mass of the dust grains: their masses are integers from 1 to this maximum (1: all of mass 1).", "Masse maximale des grains de poussière : leurs masses sont des entiers de 1 à ce maximum (1 : tous de masse 1)."),
    ("from 1 to ", "de 1 à "),
    ("   Exponent", "   Exposant"),
    ("Power law of the masses: density ∝ m^exponent. −2: many small grains and few big ones; 0: equally likely masses.", "Loi de puissance des masses : densité ∝ m^exposant. −2 : beaucoup de petits grains et peu de gros ; 0 : masses équiprobables."),
    ("Central sun", "Soleil central"),
    ("Body at rest at the center of the cloud, whose mass is a share of the total mass (sun included).", "Corps au repos au centre du nuage, dont la masse est une part de la masse totale (soleil compris)."),
    (" % of the total mass", " % de la masse totale"),
    ("Massive bodies", "Corps massifs"),
    ("Protoplanets on circular orbits in the (x, y) plane, whatever the velocity mode of the grains, at evenly spaced radii and random angles.", "Protoplanètes en orbite circulaire dans le plan (x, y), quel que soit le mode de vitesse des grains, à des rayons régulièrement espacés et à des angles aléatoires."),
    ("   Mass of each", "   Masse de chacun"),
    ("Mass of each massive body, as a share of the total mass.", "Masse de chaque corps massif, en part de la masse totale."),
    ("   Orbits (× radius)", "   Orbites (× rayon)"),
    ("Radii of the first and last orbits, as a fraction of the cloud radius; the others are evenly spaced in between.", "Rayons de la première et de la dernière orbite, en fraction du rayon du nuage ; les autres sont régulièrement espacées entre les deux."),
    ("from ", "de "),
    ("to ", "à "),
    ("The sun and the massive bodies exceed {} of the total mass.", "Le soleil et les corps massifs dépassent {} de la masse totale."),
    ("Total mass ≈ {} (mean grain mass ≈ {})", "Masse totale ≈ {} (masse moyenne d'un grain ≈ {})"),
    ("Cloud radius", "Rayon du nuage"),
    ("Radius of the initial sphere or disc (free length unit). It is also the reference radius of the clean-up.", "Rayon de la sphère ou du disque initial (unité de longueur libre). C'est aussi le rayon de référence du nettoyage."),
    ("Random seed", "Graine aléatoire"),
    ("Same parameters and same seed: same initial cloud.", "Mêmes paramètres et même graine : même nuage initial."),
    ("New seed", "Nouvelle graine"),
    ("Distribution", "Répartition"),
    ("Solid sphere of uniform density, or disc in the (x, y) plane.", "Sphère pleine de densité uniforme, ou disque dans le plan (x, y)."),
    ("Sphere", "Sphère"),
    ("Disc", "Disque"),
    ("   Thickness (× radius)", "   Épaisseur (× rayon)"),
    ("Thickness of the disc, as a fraction of its radius.", "Épaisseur du disque, en fraction de son rayon."),
    ("   Density profile", "   Profil de densité"),
    ("Uniform surface density, or decreasing exponentially with the distance to the centre.", "Densité surfacique uniforme, ou décroissant exponentiellement avec la distance au centre."),
    ("Uniform", "Uniforme"),
    ("Exponential", "Exponentiel"),
    ("   Scale length (× radius)", "   Longueur d'échelle (× rayon)"),
    ("Distance, as a fraction of the radius, over which the disc density is divided by e.", "Distance, en fraction du rayon, sur laquelle la densité du disque est divisée par e."),
    ("Initial velocity", "Vitesse initiale"),
    ("At rest: free collapse. Random: isotropic directions. Circular: rotation about Oz, at a set speed or at the orbital equilibrium speed (computed from the enclosed mass). Radial: fall towards the centre.", "Repos : effondrement libre. Aléatoire : directions isotropes. Circulaire : rotation autour de Oz, à vitesse imposée ou à la vitesse d'équilibre orbital (calculée sur la masse intérieure). Radiale : chute vers le centre."),
    ("   Spread (±)", "   Dispersion (±)"),
    ("Random deviation around the orbital equilibrium speed (± this percentage).", "Écart aléatoire autour de la vitesse orbitale d'équilibre (± ce pourcentage)."),
    ("   Factor", "   Facteur"),
    ("Multiple of the equilibrium orbital speed: 1 gives circular orbits, less makes the cloud collapse, more makes it expand.", "Multiple de la vitesse orbitale d'équilibre : 1 donne des orbites circulaires, moins fait s'effondrer le nuage, plus le fait s'étendre."),
    ("   Magnitude: range", "   Norme : plage"),
    ("Speed magnitude, drawn uniformly between the minimum and the maximum.", "Norme de la vitesse, tirée uniformément entre le minimum et le maximum."),
    ("Characteristic speed √(GM/R) ≈ {} · free-fall time ≈ {}", "Vitesse caractéristique √(GM/R) ≈ {} · temps de chute libre ≈ {}"),
    ("Advanced parameters", "Paramètres avancés"),
    ("Without display (automatic recording)", "Sans affichage (enregistrement automatique)"),
    ("Computes without drawing the bodies, the whole graphics card for the computation. A monitoring screen shows the progress; at the target (or when stopped), the recording is closed and the final state saved. Also from the command line: stellar-cli.exe (stellar-cli --help).", "Calcule sans dessiner les corps, toute la carte graphique pour le calcul. Un écran de suivi montre l'avancement ; à la cible (ou à l'arrêt), l'enregistrement est clos et l'état final sauvegardé. Aussi en ligne de commande : stellar-cli.exe (stellar-cli --help)."),
    ("   Stop:", "   Arrêt :"),
    ("steps", "pas"),
    ("(≈ {} steps)", "(≈ {} pas)"),
    ("Record from the start (for smooth playback)", "Enregistrer dès le départ (pour relecture fluide)"),
    ("   One snapshot every", "   Un instantané tous les"),
    ("Interval between two recorded snapshots; playback interpolates between them.", "Intervalle entre deux instantanés enregistrés ; la relecture interpole entre eux."),
    ("Dust limited to", "Poussière limitée à"),
    ("Maximum number of mass-1 particles recorded per snapshot (chosen consistently); bodies resulting from mergers are always kept.", "Nombre maximal de particules de masse 1 enregistrées par instantané \
                     (choisies de façon stable) ; les corps issus de fusions sont toujours \
                     gardés."),
    ("particles", "particules"),
    ("≈ {} MB per snapshot at the start (decreases with mergers)", "≈ {} Mo par instantané au départ (diminue avec les fusions)"),
    ("{} GB", "{} Go"),
    ("{} MB", "{} Mo"),
    ("Estimated memory: ≈ {}", "Mémoire estimée : ≈ {}"),
    (" · video memory ≈ {}", " · mémoire vidéo ≈ {}"),
    ("Estimated duration of the first step: ≈ {}", "Durée estimée du premier pas : ≈ {}"),
    ("Estimate based on benchmark measurements (RTX 4060 Ti for the GPU, the same machine's processor for the CPU); it varies with the hardware, the distribution and θ.", "Estimation tirée des mesures du banc d'essai (RTX 4060 Ti pour le GPU, processeur de la même machine pour le CPU) ; elle varie avec le matériel, la répartition et θ."),
    ("▶ Start without display", "▶ Lancer sans affichage"),
    ("▶ Start the simulation", "▶ Lancer la simulation"),
    ("Constant G", "Constante G"),
    ("Gravitational constant (free units).", "Constante de gravitation (unités libres)."),
    ("Derive dt, ε, r₀ and the cluster linking length automatically from the initial conditions", "Déduire automatiquement dt, ε, r₀ et la liaison des amas des conditions initiales"),
    ("Recompute the suggested values", "Recalculer les valeurs suggérées"),
    ("Time step dt", "Pas de temps dt"),
    ("Fixed time step of the (Leapfrog) integrator. Smaller: more accurate, but slower. Default: free-fall time / 500.", "Pas de temps fixe de l'intégrateur (Leapfrog). Plus petit : plus précis, mais plus lent. Par défaut : temps de chute libre / 500."),
    ("Softening ε", "Adoucissement ε"),
    ("Plummer softening: bounds the attraction during very close encounters. Default: 0.3 × mean distance between neighbours.", "Adoucissement de Plummer : borne l'attraction lors des rencontres très proches. Par défaut : 0,3 × distance moyenne entre voisins."),
    ("Radius r₀ (mass 1)", "Rayon r₀ (masse 1)"),
    ("Radius of a body of mass 1 (r = r₀ · m^⅓): it decides contacts and hence mergers. 0: no mergers.", "Rayon d'un corps de masse 1 (r = r₀ · m^⅓) : il décide des contacts et donc des fusions. 0 : aucune fusion."),
    ("Ejected clusters: linking length", "Amas éjectés : liaison"),
    ("Two distant bodies closer than this distance belong to the same group (default: mean distance between neighbours at the start).", "Deux corps lointains plus proches que cette distance appartiennent au même groupe (par défaut : distance moyenne entre voisins au départ)."),
    ("Barnes-Hut accuracy θ", "Précision Barnes-Hut θ"),
    ("Opening criterion: a group of bodies seen under an angle smaller than θ is replaced by its centre of mass. Smaller: more accurate, but slower.", "Critère d'ouverture : un groupe de corps vu sous un angle inférieur à θ est remplacé par son centre de masse. Plus petit : plus précis, mais plus lent."),
    ("Clean-up: period (steps)", "Nettoyage : période (pas)"),
    ("How many steps between removals of ejected bodies (0: never).", "Tous les combien de pas les corps éjectés sont éliminés (0 : jamais)."),
    ("Clean-up: distance (× radius)", "Nettoyage : distance (× rayon)"),
    ("Only unbound bodies beyond this distance from the barycentre (in cloud radii) are removed; bound ejected clusters are kept.", "Seuls les corps non liés situés au-delà de cette distance du barycentre (en rayons du nuage) sont éliminés ; les amas éjectés liés sont conservés."),
    ("Ejected clusters: at least (bodies)", "Amas éjectés : corps au moins"),
    ("A bound ejected cluster with at least this many bodies is kept by the clean-up (0: criterion ignored).", "Un amas éjecté lié d'au moins ce nombre de corps est conservé par le nettoyage (0 : critère ignoré)."),
    ("… or at least (mass)", "… ou masse au moins"),
    ("… or with at least this share of the initial mass, even if it has a single body (0: criterion ignored).", "… ou d'au moins cette part de la masse initiale, même s'il ne compte qu'un corps (0 : critère ignoré)."),
    ("Computation engine", "Moteur de calcul"),
    ("GPU: graphics card, single precision, much faster. CPU: all cores but one, double precision.", "GPU : carte graphique, simple précision, bien plus rapide. CPU : tous les cœurs sauf un, double précision."),
    ("Saved simulations", "Simulations sauvegardées"),
    ("Refresh", "Actualiser"),
    ("Browse…", "Parcourir…"),
    ("No save in the “saves” folder. Press F5 during a simulation.", "Aucune sauvegarde dans le dossier « saves ». Appuyez sur F5 pendant une simulation."),
    ("{} · {} bodies · step {} · t = {}", "{} · {} corps · pas {} · t = {}"),
    ("Resumes the recording “{}”", "Reprend l'enregistrement « {} »"),
    ("Load", "Charger"),
    ("Continue a recording…", "Poursuivre un enregistrement…"),
    ("Load this save and extend an existing recording from its step", "Charger cette sauvegarde et prolonger un enregistrement existant à partir de son pas"),
    ("Rename…", "Renommer…"),
    ("Delete…", "Supprimer…"),
    ("Recordings", "Enregistrements"),
    ("No recording in the “recordings” folder. Press F9 during a simulation.", "Aucun enregistrement dans le dossier « recordings ». Appuyez sur F9 pendant une simulation."),
    ("{} · {} snapshots · {} bodies at the start · duration {} · {}{}", "{} · {} instantanés · {} corps au départ · durée {} · {}{}"),
    ("Play", "Rejouer"),
    ("Resume", "Reprendre"),
    ("Reloads the save “{}” (step {}) and continues the recording", "Recharge la sauvegarde « {} » (pas {}) et poursuit l'enregistrement"),
    ("Maximum on this machine: {}", "Maximum sur cette machine : {}"),
    ("Exact value", "Valeur exacte"),
    ("The followed body was ejected and removed.", "Le corps suivi a été éjecté et éliminé."),
    ("Traces cleared.", "Traces effacées."),
    ("The simulation is not ready yet.", "La simulation n'est pas encore prête."),
    ("No body visible on screen: nothing to trace.", "Aucun corps visible à l'écran : rien à tracer."),
    ("Traces enabled on the {} most massive visible bodies.", "Traces activées sur les {} corps visibles les plus massifs."),
    ("Trace reference frame: {}", "Référentiel des traces : {}"),
    (" — select a body (barycentre meanwhile)", " — sélectionnez un corps (barycentre en attendant)"),
    ("Barycentre seen face-on to the disc (axis tilted {}° from Oz).", "Barycentre vu de face au disque (axe incliné de {}° sur Oz)."),
    ("Barycentre: no overall rotation, view along the Oz axis.", "Barycentre : aucune rotation d'ensemble, vue selon l'axe Oz."),
    ("Barycentre of sub-simulation #{} selected.", "Barycentre de la sous-simulation #{} sélectionné."),
    ("Sub-simulation merged: following the combined set #{}.", "Sous-simulation réunie : suivi de l'ensemble #{}."),
    ("Sub-simulation merged back into the simulation: following the overall barycentre.", "Sous-simulation réunie à la simulation : suivi du barycentre global."),
    ("Looking along the disc normal: nothing to level.", "Visée selon la normale au disque : rien à redresser."),
    ("Following stopped: free flight.", "Suivi interrompu : vol libre."),
    ("Following the barycentre.", "Suivi du barycentre."),
    ("Following the selected body.", "Suivi du corps sélectionné."),
    ("Nothing to follow: select a body (click, Home) or the barycentre (B).", "Rien à suivre : sélectionnez un corps (clic, Origine) ou le barycentre (B)."),
    ("Overview: 90% of the mass within a radius of {}", "Vue d'ensemble : 90 % de la masse dans un rayon de {}"),
    ("This is already the lightest body.", "C'est déjà le corps le plus léger."),
    ("This is already the most massive body.", "C'est déjà le corps le plus massif."),
    ("Recording is unavailable during playback.", "Enregistrement indisponible pendant une relecture."),
    ("Recording stopped.", "Enregistrement arrêté."),
    ("Recording started: one snapshot every {} steps.", "Enregistrement démarré : un instantané tous les {} pas."),
    ("Saving is unavailable during playback.", "Sauvegarde indisponible pendant une relecture."),
    ("Saving…", "Sauvegarde en cours…"),
    ("Saved: {}", "Sauvegardé : {}"),
    ("Save failed: {}", "Échec de la sauvegarde : {}"),
    ("Generating {} particles and computing the first forces…", "Génération de {} particules et premier calcul des forces…"),
    ("The computation engine could not start:\n{}\n\nEsc → Back to the home screen, then choose another engine in the advanced parameters.", "Le moteur de calcul n'a pas pu démarrer :\n{}\n\nÉchap → Retour à l'accueil, puis choisir un autre moteur dans les paramètres avancés."),
    ("Tab: release the mouse · Esc: menu · F: follow / stop · A / E: roll · H: horizon · C: barycentre · V: overview · T: traces", "Tab : libérer la souris · Échap : menu · F : suivre / cesser · A / E : bascule · H : horizon · C : barycentre · V : vue d'ensemble · T : traces"),
    ("Mouse released — left click: follow a body · right click or Tab: resume aiming · Esc: menu", "Souris libre — clic gauche : suivre un corps · clic droit ou Tab : reprendre la visée · Échap : menu"),
    ("Playback", "Relecture"),
    ("Bodies", "Corps"),
    ("Barycentre of sub-simulation #{}", "Barycentre de la sous-simulation #{}"),
    ("Barycentre selected", "Barycentre sélectionné"),
    ("Mass", "Masse"),
    ("Velocity", "Vitesse"),
    ("Size", "Taille"),
    ("Distance to the overall barycentre", "Distance au barycentre global"),
    ("Mass present", "Masse présente"),
    ("Camera distance", "Distance caméra"),
    ("Rotation axis", "Axe de rotation"),
    ("{}° from Oz", "{}° sur Oz"),
    ("no rotation", "aucune rotation"),
    ("Face-on view (B)", "Vue de face (B)"),
    ("Stop following (F)", "Cesser le suivi (F)"),
    ("Follow (F)", "Suivre (F)"),
    ("Deselect", "Désélectionner"),
    ("Selected body", "Corps sélectionné"),
    ("Identifier", "Identifiant"),
    ("Rank (mass)", "Rang (masse)"),
    ("Radius", "Rayon"),
    ("Dist. to barycentre", "Dist. barycentre"),
    ("None — aim and click, or press Home or B (barycentre).", "Aucun — visez et cliquez, ou touches Origine ou B (barycentre)."),
    ("Most massive", "Les plus massifs"),
    ("Sub-simulations", "Sous-simulations"),
    ("Detached ejected clusters: each is computed separately, in its own frame, and feels the others through their tidal field. Two sub-simulations that come close are merged.", "Amas éjectés détachés : chacun est calculé à part, dans son propre repère, et ressent les autres par leur champ de marée. Deux sous-simulations qui se rapprochent sont réunies."),
    ("Anchor", "Repère"),
    ("Size: {}\nSpeed: {}\nClick to centre the camera on its barycentre.", "Taille : {}
Vitesse : {}
Cliquer pour centrer la caméra sur son barycentre."),
    ("Ejected clusters", "Amas éjectés"),
    ("Bound groups of distant bodies, kept by the clean-up (thresholds in the advanced parameters). Clicking a cluster selects its most massive body.", "Groupes liés de corps lointains, conservés par le nettoyage (seuils dans les paramètres avancés). Cliquer sur un amas sélectionne son corps le plus massif."),
    ("No clean-up performed yet.", "Aucun nettoyage effectué pour l'instant."),
    ("None at the last clean-up (step {}).", "Aucun au dernier nettoyage (pas {})."),
    ("At the last clean-up (step {}):", "Au dernier nettoyage (pas {}) :"),
    ("Half-mass radius: {}\nVelocity: {}", "Rayon de demi-masse : {}
Vitesse : {}"),
    ("… and {} more", "… et {} autres"),
    ("Clear", "Effacer"),
    ("Trace the 10 largest visible", "Tracer les 10 plus gros visibles"),
    ("History", "Historique"),
    ("Tail", "Queue"),
    ("Reference frame (R)", "Référentiel (R)"),
    ("Orbits seen from the simulation's centre of mass.", "Orbites vues depuis le centre de gravité de la simulation."),
    ("Orbits seen from the centre of mass of sub-simulation #{}.", "Orbites vues depuis le centre de gravité de la sous-simulation #{}."),
    ("No body selected: traces relative to the barycentre meanwhile.", "Aucun corps sélectionné : traces relatives au barycentre en attendant."),
    ("Centre of mass of the selected body's bound cluster ({} bodies tracked).", "Centre de gravité de l'amas lié du corps sélectionné ({} corps suivis)."),
    ("Orbits seen from the selected body.", "Orbites vues depuis le corps sélectionné."),
    ("#{} (removed)", "#{} (éliminé)"),
    ("engine failure", "échec du moteur"),
    ("paused", "en pause"),
    ("running", "en cours"),
    ("State", "État"),
    ("Engine", "Moteur"),
    ("Step", "Pas"),
    ("Simulated time", "Temps simulé"),
    ("Steps / s", "Pas / s"),
    ("Step duration", "Durée d'un pas"),
    ("Frames / s", "Images / s"),
    ("Total energy", "Énergie totale"),
    ("Ejected mass", "Masse éjectée"),
    ("ΔE includes the losses through mergers and ejections.", "ΔE inclut les pertes par fusion et éjection."),
    ("▶ Resume", "▶ Reprendre"),
    ("Single step", "Pas à pas"),
    ("Limit to", "Limiter à"),
    (" steps/s", " pas/s"),
    ("Brightness", "Luminosité"),
    ("Dust brightness", "Poussières"),
    ("Camera speed", "Vitesse caméra"),
    ("Fraction of the distance to the selected body (or to the barycentre) travelled each second.", "Fraction de la distance au corps sélectionné (ou au barycentre) parcourue chaque seconde."),
    ("(+/−, Shift, Ctrl)", "(+/−, Maj, Ctrl)"),
    ("Recording (F9)", "Enregistrement (F9)"),
    ("{} snapshots · {} · 1 every {} steps", "{} instantanés · {} · 1 tous les {} pas"),
    ("■ Stop", "■ Arrêter"),
    ("One snapshot every", "Un instantané tous les"),
    ("Last: {}", "Dernier : {}"),
    ("Record", "Enregistrer"),
    ("loading…", "chargement…"),
    ("playing backwards", "lecture arrière"),
    ("Snapshot", "Instantané"),
    ("Bodies shown", "Corps affichés"),
    ("Dust", "Poussière"),
    ("1 in {}", "1 sur {}"),
    ("Interval", "Intervalle"),
    ("Dust budget", "Budget poussière"),
    ("time", "temps"),
    ("Start", "Début"),
    ("Previous snapshot (←)", "Instantané précédent (←)"),
    ("Play / pause (Space)", "Lecture / pause (Espace)"),
    ("Next snapshot (→)", "Instantané suivant (→)"),
    ("End", "Fin"),
    ("Backwards", "Arrière"),
    ("Reverse (⌫)", "Retour arrière (⌫)"),
    (" snap./s", " inst./s"),
    ("↑ / ↓: speed · ← / →: snapshot · ⌫: direction", "↑ / ↓ : vitesse · ← / → : instantané · ⌫ : sens"),
    ("Save (F5)", "Sauvegarder (F5)"),
    ("Back to the home screen", "Retour à l'accueil"),
    ("Controls", "Commandes"),
    ("forward / back (when following: distance to the target)", "avancer / reculer (en suivi : distance à la cible)"),
    ("left / right (no effect when following)", "gauche / droite (sans effet en suivi)"),
    ("roll the view around the line of sight", "basculer la vue autour de l'axe de visée"),
    ("level the horizon on the disc plane", "redresser l'horizon sur le plan du disque"),
    ("Shift / Ctrl", "Maj / Ctrl"),
    ("speed up / slow down", "accélérer / ralentir"),
    ("speed (proportional to the distance to the selection)", "vitesse (proportionnelle à la distance à la sélection)"),
    ("Mouse", "Souris"),
    ("orientation (when following: orbit the target)", "orientation (en suivi : tourner autour de la cible)"),
    ("Wheel", "Molette"),
    ("Ctrl + wheel", "Ctrl + molette"),
    ("optical zoom", "zoom optique"),
    ("Left click", "Clic gauche"),
    ("follow the body aimed at", "suivre le corps visé"),
    ("follow the selection / stop following", "suivre la sélection / cesser le suivi"),
    ("Home", "Origine"),
    ("most massive body", "corps le plus massif"),
    ("next / previous body by mass", "corps suivant / précédent par masse"),
    ("barycentre: chosen sub-simulation, then global (face-on view of the disc)", "barycentre : sous-simulation choisie, puis global (vue de face au disque)"),
    ("Remove the negligible lone bodies", "Supprimer les corps isolés négligeables"),
    ("Automatically remove the negligible lone bodies", "Supprimer automatiquement les corps isolés négligeables"),
    ("   Automatically remove the negligible lone bodies", "   Supprimer automatiquement les corps isolés négligeables"),
    ("Negligible lone bodies removed.", "Corps isolés négligeables supprimés."),
    ("Sub-simulations reduced to a single escaping body whose influence on the rest is negligible: (m / M) · (R / d)² < 10⁻⁶, where m is its mass, M and R the mass and size of the rest, d its distance to the rest's barycenter. They are removed like ejected bodies.", "Sous-simulations réduites à un seul corps qui s'échappe et dont l'influence sur le reste est négligeable : (m / M) · (R / d)² < 10⁻⁶, où m est sa masse, M et R la masse et la taille du reste, d sa distance au barycentre du reste. Elles sont retirées comme des corps éjectés."),
    ("Send to the background", "Passer en arrière-plan"),
    ("Without display", "Sans affichage"),
    ("The computation (and the running recording) goes on without display, with the whole graphics card; the simulation can be shown again later.", "Le calcul (et l'enregistrement en cours) continue sans affichage, avec toute la carte graphique ; la simulation peut être réaffichée ensuite."),
    ("none", "aucun"),
    ("aim at the centre of mass", "viser le centre de gravité"),
    ("overview (90% of the mass)", "vue d'ensemble (90 % de la masse)"),
    ("trace the 10 most massive visible / clear", "tracer les 10 plus massifs visibles / effacer"),
    ("traces: reference frame selected body / barycentre / local cluster", "traces : référentiel corps sélectionné / barycentre / amas local"),
    ("Space / P", "Espace / P"),
    ("pause / single step", "pause / pas à pas"),
    ("release / capture the mouse", "libérer / capturer la souris"),
    ("Right click", "Clic droit"),
    ("resume aiming (mouse released)", "reprendre la visée (souris libre)"),
    ("save", "sauvegarder"),
    ("start / stop recording", "démarrer / arrêter l'enregistrement"),
    ("quit", "quitter"),
    ("Space", "Espace"),
    ("playback: play / pause", "relecture : lecture / pause"),
    ("playback: previous / next snapshot", "relecture : instantané précédent / suivant"),
    ("playback: speed", "relecture : vitesse"),
    ("playback: direction", "relecture : sens de lecture"),
    ("Esc", "Échap"),
    ("this menu", "ce menu"),
    ("Local cluster", "Amas local"),
    ("[{}] Crash in thread “{}”\n{}\n\n{}\n", "[{}] Plantage dans le thread « {} »\n{}\n\n{}\n"),
    ("Stellar — fatal error", "Stellar — erreur fatale"),
    ("{}\n\nDetails written to {}.", "{}\n\nDétails enregistrés dans {}."),
    ("Graphics card: {}", "Carte graphique : {}"),
    ("GPU engine unavailable: {}", "Moteur GPU indisponible : {}"),
    ("Usage: stellar-cli [options]        new simulation without a window\n       stellar-cli --load FILE           resume a save (.stellar)\n       stellar-cli --resume FILE         resume a recording (.stellarec)\n       stellar-cli --help                this help\nThe graphical interface is stellar.exe (without arguments).\n\nNew simulation:\n  --bodies N         number of particles (default: 1000000)\n  --radius R         cloud radius (default: 100)\n  --seed S           random seed (default: 42)\n  --disc             rotating disc (otherwise a sphere)\n  --velocity V       rest | random | circ | orb | rad (default: orb)\n  --spread D         spread of the orbital speed (default: 0.05)\n  --factor F         multiple of the orbital speed: < 1 collapse,\n                     > 1 expansion (default: 1)\n  --vmin A --vmax B  speed range (random, circ, rad)\n  --max-mass N       grain masses: integers from 1 to N (default: 1)\n  --mass-exponent A  power law of the masses, density ∝ m^A (default: -2)\n  --sun P            central sun of P % of the total mass\n  --planets N        N massive bodies on circular orbits (from 0.2 to 0.9 × radius)\n  --planet-mass P    mass of each one in % of the total mass (default: 1)\n  --engine M         gpu | cpu (default: gpu if a suitable card is present)\n\nResuming:\n  --load F           save to resume; its linked recording is continued,\n                     otherwise a new recording starts\n  --continue F       with --load: compatible recording to extend from the\n                     save's step (later snapshots deleted)\n  --resume F         recording to continue from its latest linked save\n\nIn all cases:\n  --name NAME        simulation name (default: date and parameters, or file name)\n  --interval N       one snapshot every N steps (default: 10)\n  --dust N           dust recorded per snapshot, or “all” (default: 1000000)\n  --steps N          stop after N steps (more, when resuming)\n  --time T           stop after a simulated time T (more, when resuming)\n  --duration M       stop after M minutes of computation\n  --prune            automatically remove the negligible lone bodies\n                     (single-body sub-simulations escaping)\nWithout a target: until Ctrl+C. When stopped, the recording is closed and the final\nstate saved (folders “{}” and “{}”).", "Usage : stellar-cli [options]        nouvelle simulation sans fenêtre
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
  --max-mass N       masses des grains : entiers de 1 à N (défaut : 1)
  --mass-exponent A  loi de puissance des masses, densité ∝ m^A (défaut : -2)
  --sun P            soleil central de P % de la masse totale
  --planets N        N corps massifs en orbite circulaire (de 0,2 à 0,9 × rayon)
  --planet-mass P    masse de chacun en % de la masse totale (défaut : 1)
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
  --prune            supprimer automatiquement les corps isolés négligeables
                     (sous-simulations d'un seul corps qui s'échappe)
Sans cible : jusqu'à Ctrl+C. À l'arrêt, l'enregistrement est clos et l'état final
sauvegardé (dossiers « {} » et « {} »)."),
    ("missing value after {}", "valeur manquante après {}"),
    ("invalid value for {}: {}", "valeur invalide pour {} : {}"),
    ("unknown argument: {}", "argument inconnu : {}"),
    ("--continue is used with --load", "--continue s'emploie avec --load"),
    ("--load and --resume are mutually exclusive", "--load et --resume s'excluent"),
    ("{} only applies to a new simulation", "{} ne s'applique qu'à une nouvelle simulation"),
    ("unknown velocity: {}", "vitesse inconnue : {}"),
    ("--max-mass must be between 1 and {}", "--max-mass doit être compris entre 1 et {}"),
    ("--planets must be between 0 and {}", "--planets doit être compris entre 0 et {}"),
    ("the sun and the massive bodies must have a positive mass and not exceed {} % of the total mass", "le soleil et les corps massifs doivent avoir une masse positive et ne pas dépasser {} % de la masse totale"),
    ("stellar.exe is the graphical interface and takes no argument. Command line: stellar-cli.exe (stellar-cli --help).", "stellar.exe est l'interface graphique et ne prend pas d'argument. Ligne de commande : stellar-cli.exe (stellar-cli --help)."),
    ("Error: {}", "Erreur : {}"),
    ("See stellar-cli --help for the list of options.", "Voir stellar-cli --help pour la liste des options."),
    ("New simulation “{}”: {} bodies, engine {}.", "Nouvelle simulation « {} » : {} corps, moteur {}."),
    ("Resuming the save {}.", "Reprise de la sauvegarde {}."),
    ("Resuming the recording {} from the save {}.", "Reprise de l'enregistrement {} depuis la sauvegarde {}."),
    ("Error: no save in the “{}” folder is linked to {}; use --load SAVE --continue RECORDING.", "Erreur : aucune sauvegarde du dossier « {} » n'est liée à {} ; utiliser --load SAUVEGARDE --continue ENREGISTREMENT."),
    ("Recording continued: {}", "Enregistrement poursuivi : {}"),
    ("step {} · t = {} · {} bodies · {} steps/s · {} snapshots · {}", "pas {} · t = {} · {} corps · {} pas/s · {} instantanés · {}"),
    ("Stopping: closing the recording and saving…", "Arrêt : clôture de l'enregistrement et sauvegarde…"),
    ("Finished at step {}.", "Terminé au pas {}."),
    ("Save: {}", "Sauvegarde : {}"),
    ("Recording: {}", "Enregistrement : {}"),
    ("Failure: {}", "Échec : {}"),
    ("Simulation without display", "Simulation sans affichage"),
    ("{}% · ≈ {} left", "{} % · reste ≈ {}"),
    ("preparing…", "préparation…"),
    ("Target", "Cible"),
    ("manual stop", "arrêt manuel"),
    ("{} steps", "{} pas"),
    ("simulated time {}", "temps simulé {}"),
    ("duration {}", "durée {}"),
    ("Elapsed time", "Durée écoulée"),
    ("Recording", "Enregistrement"),
    ("{} snapshots · {}", "{} instantanés · {}"),
    ("■ Stop and save", "■ Arrêter et sauvegarder"),
    ("Show the simulation", "Afficher la simulation"),
    ("The computation and the recording go on; the target is no longer watched.", "Le calcul et l'enregistrement continuent ; la cible n'est plus surveillée."),
    ("When stopped, the recording is closed and the final state saved under the simulation's name.", "À l'arrêt, l'enregistrement est clos et l'état final sauvegardé sous le nom de la simulation."),
    ("Closing the recording and saving…", "Clôture de l'enregistrement et sauvegarde…"),
    ("CPU — multithreaded Barnes-Hut", "CPU — Barnes-Hut multithread"),
    ("Barnes-Hut octree on all cores (but one, kept for the display).", "Octree de Barnes-Hut sur tous les cœurs (sauf un, réservé à l'affichage)."),
    ("GPU — Barnes-Hut (graphics card)", "GPU — Barnes-Hut (carte graphique)"),
    ("Barnes-Hut tree built and traversed on the graphics card (single precision).", "Arbre de Barnes-Hut construit et parcouru sur la carte graphique (simple précision)."),
    ("unknown error", "erreur inconnue"),
    // Errors of the lower layers (errors.rs)
    ("no compatible graphics card was found", "aucune carte graphique compatible n'a été trouvée"),
    ("the graphics card does not support compute shaders", "la carte graphique ne gère pas les shaders de calcul"),
    ("cannot open the graphics card: {}", "ouverture de la carte graphique impossible : {}"),
    ("the computation engine is limited to {} bodies ({} requested)", "le moteur de calcul est limité à {} corps ({} demandés)"),
    ("body identifiers too large for the computation engine", "identifiants de corps trop grands pour le moteur de calcul"),
    ("too many bodies for this graphics card ({}: buffer of {} MB, limit {} MB)", "trop de corps pour cette carte graphique ({} : tampon de {} Mo, limite {} Mo)"),
    ("truncated file", "fichier tronqué"),
    ("missing signature: this is not a Stellar save", "signature absente : ce n'est pas une sauvegarde Stellar"),
    ("this is not a Stellar recording", "ce n'est pas un enregistrement Stellar"),
    ("oversized header", "en-tête démesuré"),
    ("inconsistent body arrays", "tableaux de corps incohérents"),
    ("truncated snapshot", "instantané tronqué"),
    ("invalid varint", "varint invalide"),
    ("inconsistent body count", "nombre de corps incohérent"),
    ("invalid subsystem count", "nombre de sous-systèmes invalide"),
    ("unknown subsystem", "sous-système inconnu"),
    ("unknown fate", "devenir inconnu"),
    ("snapshot out of range", "instantané hors limites"),
    ("the snapshot of step {} is missing from the recording", "l'instantané du pas {} est introuvable dans l'enregistrement"),
    ("empty recording", "enregistrement vide"),
    ("input/output error: {}", "erreur d'entrée/sortie : {}"),
    ("file not found: {}", "fichier introuvable : {}"),
    ("the name “{}” is already taken", "le nom « {} » est déjà pris"),
    ("deletion cancelled or impossible (code {})", "suppression annulée ou impossible (code {})"),
    ("recording thread interrupted", "thread d'enregistrement interrompu"),
    ("recording already closed", "enregistrement déjà clos"),
    ("invalid file: {}", "fichier invalide : {}"),
    ("unsupported format version: {}", "version de format non prise en charge : {}"),
    ("the recording comes from another simulation (different initial conditions)", "l'enregistrement provient d'une autre simulation (conditions initiales différentes)"),
    ("the recording starts after the save's step", "l'enregistrement commence après le pas de la sauvegarde"),
    ("body no. {} of the save is missing from the recording at step {}: it comes from another simulation", "le corps n° {} de la sauvegarde est absent de l'enregistrement au pas {} : il provient d'une autre simulation"),
    ("unknown computation engine: “{}”", "moteur de calcul inconnu : « {} »"),
    ("the simulation is not ready yet", "la simulation n'est pas encore prête"),
    ("the computation engine could not start", "le moteur de calcul n'a pas pu démarrer"),
    ("the computation engine could not start: {}", "le moteur de calcul n'a pas pu démarrer : {}"),
    // Same text in both languages, or added with the switch to English sources
    ("Auto", "Auto"),
    ("max ", "max "),
    ("min ", "min "),
    ("minutes", "minutes"),
    (" × dist./s", " × dist./s"),
    ("#{} → #{}", "#{} → #{}"),
    ("A / E", "A / E"),
    ("Barycentre", "Barycentre"),
    ("Distance", "Distance"),
    ("Menu", "Menu"),
    ("PgDn / PgUp", "PgDn / PgUp"),
    ("Position", "Position"),
    ("Q / D", "Q / D"),
    ("REC", "REC"),
    ("Simulation", "Simulation"),
    ("Tab", "Tab"),
    ("Traces (T)", "Traces (T)"),
    ("Z / S", "Z / S"),
    ("barycentre", "barycentre"),
    ("barycentre #{}", "barycentre #{}"),
    ("ΔE / E₀", "ΔE / E₀"),
    ("⏸ Pause", "⏸ Pause"),
    (" · unfinished", " · non clos"),
    (" · unfinished (recovered)", " · non clos (récupéré)"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use stellar_domain::EngineFactory;

    #[test]
    fn fills_placeholders_in_order() {
        assert_eq!(fill("{} of {}", &[&1, &"two"]), "1 of two");
        assert_eq!(fill("none", &[]), "none");
        assert_eq!(fill("{} {}", &[&1]), "1 {}");
    }

    #[test]
    fn table_has_no_duplicates_nor_missing_placeholders() {
        let mut seen = std::collections::HashSet::new();
        for (en, fr) in FR {
            assert!(seen.insert(en), "duplicate: {en}");
            assert_eq!(en.matches("{}").count(), fr.matches("{}").count(), "{en}");
        }
    }

    #[test]
    fn switches_language() {
        apply(Lang::Fr);
        assert_eq!(tr("⏸ Pause"), "⏸ Pause");
        assert_eq!(tr!("Saved: {}", "a.stellar"), "Sauvegardé : a.stellar");
        apply(Lang::En);
        assert_eq!(tr!("Saved: {}", "a.stellar"), "Saved: a.stellar");
    }

    /// Every text given to `tr` / `tr!` in this crate, and every engine label
    /// and description, has a French translation.
    #[test]
    fn every_interface_text_has_a_french_translation() {
        let known: std::collections::HashSet<&str> = FR.iter().map(|(en, _)| *en).collect();
        let mut missing = Vec::new();
        let mut found = 0;
        let mut dirs = vec![std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src")];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let source = std::fs::read_to_string(&path).unwrap();
                    let texts = tr_literals(&source);
                    found += texts.len();
                    for text in texts {
                        if !known.contains(text.as_str()) {
                            missing.push(format!("{}: {text}", path.display()));
                        }
                    }
                }
            }
        }
        assert!(found > 400, "only {found} texts found: the scan is broken");
        let engines: [&dyn EngineFactory; 2] = [
            &stellar_engine_cpu::CpuEngineFactory,
            &stellar_engine_gpu::GpuEngineFactory::new(),
        ];
        for e in engines {
            let d = e.descriptor();
            for text in [d.label, d.description] {
                if !known.contains(text) {
                    missing.push(format!("engine {}: {text}", d.id));
                }
            }
        }
        assert!(missing.is_empty(), "texts without French translation:\n{}", missing.join("\n"));
    }

    /// Literals passed to `tr(` / `tr!(`, outside line comments, unescaped.
    fn tr_literals(source: &str) -> Vec<String> {
        let mut out = Vec::new();
        for (i, _) in source.match_indices("tr") {
            let rest = &source[i + 2..];
            let rest = rest.strip_prefix('!').unwrap_or(rest);
            let Some(rest) = rest.strip_prefix('(') else { continue };
            let Some(rest) = rest.trim_start().strip_prefix('"') else { continue };
            let before = &source[..i];
            if before.ends_with(|c: char| c.is_alphanumeric() || c == '_') {
                continue;
            }
            let line = &before[before.rfind('\n').map_or(0, |n| n + 1)..];
            if line.trim_start().starts_with("//") {
                continue;
            }
            out.push(unescape(rest));
        }
        out
    }

    /// Content of the string literal starting `text` (after its opening quote).
    fn unescape(text: &str) -> String {
        let mut s = String::new();
        let mut chars = text.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '"' => break,
                '\\' => match chars.next() {
                    Some('n') => s.push('\n'),
                    Some('t') => s.push('\t'),
                    Some('\n') => {
                        while chars.peek().is_some_and(|c| c.is_whitespace()) {
                            chars.next();
                        }
                    }
                    Some(c) => s.push(c),
                    None => break,
                },
                c => s.push(c),
            }
        }
        s
    }
}
