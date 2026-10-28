//! Langue de l'interface : français ou anglais.
//!
//! Les textes sont écrits en français dans le code ; [`tr`] renvoie leur
//! traduction anglaise (table [`EN`]) quand l'anglais est choisi, et le texte
//! d'origine sinon (ou s'il manque à la table). [`tr!`](crate::tr) fait de même
//! pour un modèle à emplacements `{}`.
//!
//! Langue par défaut : celle de Windows (français ou, à défaut, anglais) ;
//! un choix fait dans le menu est mémorisé dans `%APPDATA%\Stellar\langue`.

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

    /// Nom de la langue, dans cette langue.
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

/// Choisit la langue et mémorise ce choix.
pub fn set_lang(l: Lang) {
    apply(l);
    if let Some(path) = settings_file() {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::write(path, l.code());
    }
}

/// Langue au démarrage : `STELLAR_LANG` (fr / en), sinon le choix mémorisé,
/// sinon celle de Windows.
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
    // SAFETY : fonction sans argument.
    let id = unsafe { GetUserDefaultUILanguage() };
    // Langue principale : 10 bits de poids faible (0x0C : français).
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

/// Traduction d'un texte de l'interface.
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

/// Remplit les emplacements `{}` d'un modèle, dans l'ordre.
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

/// Texte traduit, à emplacements `{}` : `tr!("Masse : {}", m)`.
#[macro_export]
macro_rules! tr {
    ($fmt:literal $(, $arg:expr)* $(,)?) => {
        $crate::i18n::fill($crate::i18n::tr($fmt), &[$(&$arg as &dyn ::std::fmt::Display),*])
    };
}

/// Choix de la langue (liste déroulante), mémorisé.
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

/// Traductions anglaises (texte français → texte anglais).
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
    ("   Norme : plage", "   Magnitude: range"),
    ("Norme de la vitesse, tirée uniformément entre le minimum et le maximum.", "Speed magnitude, drawn uniformly between the minimum and the maximum."),
    ("Vitesse caractéristique √(GM/R) ≈ {} · temps de chute libre ≈ {}", "Characteristic speed √(GM/R) ≈ {} · free-fall time ≈ {}"),
    ("Paramètres avancés", "Advanced parameters"),
    ("Sans affichage (enregistrement automatique)", "Without display (automatic recording)"),
    ("Calcule sans dessiner les corps, toute la carte graphique pour le calcul. Un écran de suivi montre l'avancement ; à la cible (ou à l'arrêt), l'enregistrement est clos et l'état final sauvegardé. Aussi en console : stellar.exe --sans-affichage (voir le README).", "Computes without drawing the bodies, the whole graphics card for the computation. A monitoring screen shows the progress; at the target (or when stopped), the recording is closed and the final state saved. Also from the console: stellar.exe --sans-affichage (see the README)."),
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
    ("Barycentre sélectionné", "Barycentre selected"),
    ("Masse présente", "Mass present"),
    ("Vitesse", "Velocity"),
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
    ("Masse", "Mass"),
    ("Rayon", "Radius"),
    ("Dist. barycentre", "Dist. to barycentre"),
    ("Aucun — visez et cliquez, ou touches Origine ou B (barycentre).", "None — aim and click, or press Home or B (barycentre)."),
    ("Les plus massifs", "Most massive"),
    ("Sous-simulations", "Sub-simulations"),
    ("Amas éjectés détachés : chacun est calculé à part, dans son propre repère, et ressent les autres par leur champ de marée. Deux sous-simulations qui se rapprochent sont réunies.", "Detached ejected clusters: each is computed separately, in its own frame, and feels the others through their tidal field. Two sub-simulations that come close are merged."),
    ("Repère", "Anchor"),
    ("Taille : {}
Vitesse : {}", "Size: {}\nVelocity: {}"),
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
    ("démarrer / arrêter l'enregistrement", "start / stop recording"),
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
    ("Usage : stellar.exe                         interface graphique
        stellar.exe --sans-affichage [options]   simulation sans fenêtre
        stellar.exe --help                       cette aide

Options de --sans-affichage :

  --corps N          nombre de particules (défaut : 1000000)
  --rayon R          rayon du nuage (défaut : 100)
  --graine S         graine aléatoire (défaut : 42)
  --disque           disque en rotation (sinon sphère)
  --vitesse V        repos | alea | circ | orb | rad (défaut : orb)
  --dispersion D     dispersion de la vitesse orbitale (défaut : 0,05)
  --vmin A --vmax B  plage de vitesse (alea, circ, rad)
  --moteur M         gpu | cpu (défaut : gpu si une carte convient)
  --nom NOM          nom de la simulation (défaut : date et paramètres)
  --intervalle N     un instantané tous les N pas (défaut : 10)
  --poussiere N      poussière enregistrée par instantané, ou « toutes » (défaut : 1000000)
  --pas N            s'arrêter après N pas
  --temps T          s'arrêter après un temps simulé T
  --duree M          s'arrêter après M minutes de calcul
Sans cible : jusqu'à Ctrl+C. À l'arrêt, l'enregistrement est clos et l'état final
sauvegardé (dossiers « {} » et « {} »).", "Usage: stellar.exe                         graphical interface\n       stellar.exe --sans-affichage [options]   simulation without a window\n       stellar.exe --help                       this help\n\nOptions of --sans-affichage:\n\n  --corps N          number of particles (default: 1000000)\n  --rayon R          cloud radius (default: 100)\n  --graine S         random seed (default: 42)\n  --disque           rotating disc (otherwise a sphere)\n  --vitesse V        repos | alea | circ | orb | rad (default: orb)\n  --dispersion D     spread of the orbital speed (default: 0.05)\n  --vmin A --vmax B  speed range (alea, circ, rad)\n  --moteur M         gpu | cpu (default: gpu if a suitable card is present)\n  --nom NOM          simulation name (default: date and parameters)\n  --intervalle N     one snapshot every N steps (default: 10)\n  --poussiere N      dust recorded per snapshot, or “toutes” for all (default: 1000000)\n  --pas N            stop after N steps\n  --temps T          stop after a simulated time T\n  --duree M          stop after M minutes of computation\nWithout a target: until Ctrl+C. When stopped, the recording is closed and the final\nstate saved (folders “{}” and “{}”)."),
    ("valeur manquante après {}", "missing value after {}"),
    ("valeur invalide pour {} : {}", "invalid value for {}: {}"),
    ("argument inconnu : {}", "unknown argument: {}"),
    ("vitesse inconnue : {}", "unknown velocity: {}"),
    ("l'option {} demande --sans-affichage", "option {} requires --sans-affichage"),
    ("Erreur : {}", "Error: {}"),
    ("Voir stellar.exe --help pour la liste des options.", "See stellar.exe --help for the list of options."),
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
    }
}
