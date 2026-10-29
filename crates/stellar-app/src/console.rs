//! Ligne de commande (`stellar-cli.exe`) : simulation sans fenêtre, reprise
//! d'une sauvegarde ou d'un enregistrement.
//!
//! Lance la simulation, l'enregistre et s'arrête à la cible (ou à Ctrl+C),
//! puis clôt l'enregistrement et sauvegarde l'état final.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use stellar_application::{
    EngineRegistry, HeadlessRun, RecordSettings, RunTarget, SimulatorService,
};
use stellar_domain::{
    DensityProfile, Distribution, InitialConditions, PhysicsParams, SimulationConfig, VelocityMode,
};
use stellar_engine_cpu::CpuEngineFactory;
use stellar_engine_gpu::GpuEngineFactory;
use stellar_persistence::{FileRecordingRepository, FileRepository};

use crate::headless::hms;
use crate::{format, RECORDINGS_DIR, SAVES_DIR};

static STOP: AtomicBool = AtomicBool::new(false);

/// Options d'aide.
const HELP: [&str; 3] = ["--help", "-h", "--aide"];

fn usage() -> String {
    tr!("Usage : stellar-cli [options]        nouvelle simulation sans fenêtre
        stellar-cli --charger FICHIER     reprendre une sauvegarde (.stellar)
        stellar-cli --reprendre FICHIER   reprendre un enregistrement (.stellarec)
        stellar-cli --help                cette aide
L'interface graphique est stellar.exe (sans argument).

Nouvelle simulation :
  --corps N          nombre de particules (défaut : 1000000)
  --rayon R          rayon du nuage (défaut : 100)
  --graine S         graine aléatoire (défaut : 42)
  --disque           disque en rotation (sinon sphère)
  --vitesse V        repos | alea | circ | orb | rad (défaut : orb)
  --dispersion D     dispersion de la vitesse orbitale (défaut : 0,05)
  --vmin A --vmax B  plage de vitesse (alea, circ, rad)
  --moteur M         gpu | cpu (défaut : gpu si une carte convient)

Reprise :
  --charger F        sauvegarde à reprendre ; son enregistrement lié est poursuivi,
                     sinon un nouvel enregistrement commence
  --poursuivre F     avec --charger : enregistrement compatible à prolonger depuis
                     le pas de la sauvegarde (instantanés postérieurs supprimés)
  --reprendre F      enregistrement à poursuivre depuis sa dernière sauvegarde liée

Dans tous les cas :
  --nom NOM          nom de la simulation (défaut : date et paramètres, ou nom du fichier)
  --intervalle N     un instantané tous les N pas (défaut : 10)
  --poussiere N      poussière enregistrée par instantané, ou « toutes » (défaut : 1000000)
  --pas N            s'arrêter après N pas (de plus, en reprise)
  --temps T          s'arrêter après un temps simulé T (de plus, en reprise)
  --duree M          s'arrêter après M minutes de calcul
Sans cible : jusqu'à Ctrl+C. À l'arrêt, l'enregistrement est clos et l'état final
sauvegardé (dossiers « {} » et « {} »).", SAVES_DIR, RECORDINGS_DIR)
}

/// Point de départ de la simulation.
#[derive(Debug, Clone, PartialEq)]
enum Start {
    New,
    /// Sauvegarde, et enregistrement à poursuivre éventuel.
    Save(PathBuf, Option<PathBuf>),
    /// Enregistrement à poursuivre depuis sa dernière sauvegarde liée.
    Recording(PathBuf),
}

#[derive(Debug)]
struct Options {
    start: Start,
    initial: InitialConditions,
    /// Options de nouvelle simulation données (refusées en reprise).
    new_options: Vec<String>,
    engine: Option<String>,
    name: Option<String>,
    record: RecordSettings,
    target: RunTarget,
}

/// Analyse la ligne de commande (`Err("")` : aide demandée ; `Err(message)` :
/// erreur).
fn parse(args: &[String]) -> Result<Options, String> {
    if args.iter().any(|a| HELP.contains(&a.as_str())) {
        return Err(String::new());
    }
    let mut o = Options {
        start: Start::New,
        initial: InitialConditions {
            velocity: VelocityMode::CircularOrbital { spread: 0.05 },
            ..Default::default()
        },
        new_options: Vec::new(),
        engine: None,
        name: None,
        record: RecordSettings {
            interval: 10,
            dust_budget: Some(1_000_000),
        },
        target: RunTarget::Manual,
    };
    let (mut velocity, mut spread, mut vmin, mut vmax) = ("orb".to_owned(), 0.05, 0.0, 10.0);
    let (mut load, mut follow, mut resume) = (None, None, None);
    let mut it = args.iter();
    let value = |it: &mut std::slice::Iter<String>, flag: &str| {
        it.next()
            .cloned()
            .ok_or_else(|| tr!("valeur manquante après {}", flag))
    };
    fn num<T: std::str::FromStr>(v: &str, flag: &str) -> Result<T, String> {
        v.replace(',', ".")
            .replace(['\u{00A0}', ' ', '_'], "")
            .parse()
            .map_err(|_| tr!("valeur invalide pour {} : {}", flag, v))
    }
    while let Some(a) = it.next() {
        let new_option = matches!(
            a.as_str(),
            "--corps" | "--rayon" | "--graine" | "--disque" | "--vitesse" | "--dispersion"
                | "--vmin" | "--vmax" | "--moteur"
        );
        if new_option {
            o.new_options.push(a.clone());
        }
        match a.as_str() {
            // Ancienne forme (`stellar.exe --sans-affichage`) : sans effet.
            "--sans-affichage" => {}
            "--corps" => o.initial.count = num(&value(&mut it, a)?, a)?,
            "--rayon" => o.initial.radius = num(&value(&mut it, a)?, a)?,
            "--graine" => o.initial.seed = num(&value(&mut it, a)?, a)?,
            "--disque" => {
                o.initial.distribution = Distribution::Disk {
                    thickness: 0.05,
                    profile: DensityProfile::Exponential { scale: 0.3 },
                }
            }
            "--vitesse" => velocity = value(&mut it, a)?,
            "--dispersion" => spread = num(&value(&mut it, a)?, a)?,
            "--vmin" => vmin = num(&value(&mut it, a)?, a)?,
            "--vmax" => vmax = num(&value(&mut it, a)?, a)?,
            "--moteur" => o.engine = Some(value(&mut it, a)?),
            "--charger" => load = Some(PathBuf::from(value(&mut it, a)?)),
            "--poursuivre" => follow = Some(PathBuf::from(value(&mut it, a)?)),
            "--reprendre" => resume = Some(PathBuf::from(value(&mut it, a)?)),
            "--nom" => o.name = Some(value(&mut it, a)?),
            "--intervalle" => o.record.interval = num::<u32>(&value(&mut it, a)?, a)?.max(1),
            "--poussiere" => {
                let v = value(&mut it, a)?;
                o.record.dust_budget = if v == "toutes" { None } else { Some(num(&v, a)?) };
            }
            "--pas" => o.target = RunTarget::Steps(num(&value(&mut it, a)?, a)?),
            "--temps" => o.target = RunTarget::SimTime(num(&value(&mut it, a)?, a)?),
            "--duree" => {
                let m: f64 = num(&value(&mut it, a)?, a)?;
                o.target = RunTarget::WallTime(Duration::from_secs_f64(m.max(0.0) * 60.0));
            }
            other => return Err(tr!("argument inconnu : {}", other)),
        }
    }
    o.start = match (load, follow, resume) {
        (None, None, None) => Start::New,
        (Some(save), follow, None) => Start::Save(save, follow),
        (None, None, Some(rec)) => Start::Recording(rec),
        (None, Some(_), _) => return Err(crate::i18n::tr("--poursuivre s'emploie avec --charger").to_owned()),
        (Some(_), _, Some(_)) => {
            return Err(crate::i18n::tr("--charger et --reprendre s'excluent").to_owned())
        }
    };
    if o.start != Start::New {
        if let Some(opt) = o.new_options.first() {
            return Err(tr!("{} ne s'applique qu'à une nouvelle simulation", opt));
        }
    }
    o.initial.velocity = match velocity.as_str() {
        "repos" => VelocityMode::Rest,
        "alea" => VelocityMode::Random { min: vmin, max: vmax },
        "circ" => VelocityMode::CircularFixed { min: vmin, max: vmax },
        "orb" => VelocityMode::CircularOrbital { spread },
        "rad" => VelocityMode::Radial { min: vmin, max: vmax },
        v => return Err(tr!("vitesse inconnue : {}", v)),
    };
    o.initial.count = o.initial.count.max(2);
    Ok(o)
}

#[cfg(windows)]
fn setup_console() {
    use windows_sys::Win32::System::Console::{SetConsoleCtrlHandler, SetConsoleOutputCP};
    unsafe extern "system" fn on_ctrl(_: u32) -> i32 {
        STOP.store(true, Ordering::Release);
        1
    }
    // SAFETY : appels Win32 sans pointeur à conserver ; le gestionnaire
    // ne fait que lever un drapeau atomique.
    unsafe {
        SetConsoleOutputCP(65001);
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

#[cfg(not(windows))]
fn setup_console() {}

/// `stellar.exe` lancé avec des arguments : l'exécutable fenêtré ne peut pas
/// tenir l'invite ; renvoi vers `stellar-cli`. Renvoie le code de sortie.
pub fn redirect() -> i32 {
    #[cfg(windows)]
    // SAFETY : rattachement à la console du parent, sans pointeur.
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
    crate::i18n::init();
    eprintln!(
        "{}",
        crate::i18n::tr(
            "stellar.exe est l'interface graphique et ne prend pas d'argument. Ligne de commande : stellar-cli.exe (stellar-cli --help)."
        )
    );
    2
}

/// Exécute la ligne de commande ; renvoie le code de sortie.
pub fn run(args: &[String]) -> i32 {
    setup_console();
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) if e.is_empty() => {
            println!("{}", usage());
            return 0;
        }
        Err(e) => {
            eprintln!("{}", tr!("Erreur : {}", e));
            eprintln!(
                "{}",
                crate::i18n::tr("Voir stellar-cli --help pour la liste des options.")
            );
            return 2;
        }
    };
    let mut registry = EngineRegistry::new();
    let gpu = GpuEngineFactory::probe();
    if gpu.is_ok() {
        registry.register(Arc::new(GpuEngineFactory::new()));
    }
    registry.register(Arc::new(CpuEngineFactory));
    let engine = match o.engine.as_deref() {
        Some("gpu") => "gpu-barnes-hut".to_owned(),
        Some("cpu") => "cpu-barnes-hut".to_owned(),
        Some(other) => other.to_owned(),
        None => registry.default_id().unwrap_or("cpu-barnes-hut").to_owned(),
    };
    let service = SimulatorService::new(
        registry,
        Arc::new(FileRepository::new(SAVES_DIR)),
        Arc::new(FileRecordingRepository::new(RECORDINGS_DIR)),
    );
    if let Err(e) = &gpu {
        println!("{}", tr!("Moteur GPU indisponible : {}", e));
    }
    let started = match &o.start {
        Start::New => {
            let name = o.name.clone().unwrap_or_else(|| {
                crate::home::default_name(&o.initial, chrono::Local::now(), |_| false)
            });
            let config = SimulationConfig {
                initial: o.initial,
                physics: PhysicsParams::suggested(&o.initial, 1.0),
                engine,
            };
            println!(
                "{}",
                tr!(
                    "Nouvelle simulation « {} » : {} corps, moteur {}.",
                    name,
                    format::count(config.initial.count as u64),
                    config.engine
                )
            );
            HeadlessRun::start(&service, config, &name, o.record, o.target).map(|r| (r, None))
        }
        Start::Save(save, follow) => {
            println!("{}", tr!("Reprise de la sauvegarde {}.", save.display()));
            HeadlessRun::resume(&service, save, follow.as_deref(), o.name.as_deref(), o.record, o.target)
        }
        Start::Recording(rec) => {
            // Dernière sauvegarde liée à l'enregistrement.
            let save = service
                .linked_saves(rec)
                .into_iter()
                .max_by_key(|s| (s.saved_at_unix, s.step))
                .map(|s| s.path);
            match save {
                Some(save) => {
                    println!(
                        "{}",
                        tr!("Reprise de l'enregistrement {} depuis la sauvegarde {}.", rec.display(), save.display())
                    );
                    HeadlessRun::resume(&service, &save, Some(rec), o.name.as_deref(), o.record, o.target)
                }
                None => {
                    eprintln!(
                        "{}",
                        tr!(
                            "Erreur : aucune sauvegarde du dossier « {} » n'est liée à {} ; utiliser --charger SAUVEGARDE --poursuivre ENREGISTREMENT.",
                            SAVES_DIR,
                            rec.display()
                        )
                    );
                    return 1;
                }
            }
        }
    };
    let run = match started {
        Ok((run, continued)) => {
            if let Some(p) = continued {
                println!("{}", tr!("Enregistrement poursuivi : {}", p.display()));
            }
            run
        }
        Err(e) => {
            eprintln!("{}", tr!("Erreur : {}", e));
            return 1;
        }
    };
    let mut last_report = Instant::now();
    loop {
        if run.done() || STOP.load(Ordering::Acquire) {
            break;
        }
        if last_report.elapsed() >= Duration::from_secs(5) {
            last_report = Instant::now();
            let p = run.poll();
            let mut line = tr!(
                "pas {} · t = {} · {} corps · {} pas/s · {} instantanés · {}",
                format::count(p.step),
                format::real(p.time),
                format::count(p.bodies as u64),
                format!("{:.1}", p.steps_per_second).replace('.', format::decimal_sep()),
                format::count(p.frames),
                hms(p.elapsed)
            );
            if let Some(f) = p.fraction {
                line.push_str(&format!(" · {:.1} %", 100.0 * f).replace('.', format::decimal_sep()));
            }
            println!("{line}");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    println!("{}", crate::i18n::tr("Arrêt : clôture de l'enregistrement et sauvegarde…"));
    match run.finish(&service) {
        Ok(out) => {
            println!("{}", tr!("Terminé au pas {}.", format::count(out.step)));
            println!("{}", tr!("Sauvegarde : {}", out.save.display()));
            if let Some(r) = out.recording {
                println!("{}", tr!("Enregistrement : {}", r.display()));
            }
            0
        }
        Err(e) => {
            eprintln!("{}", tr!("Échec : {}", e));
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn parses_new_simulation_options() {
        let o = parse(&args(
            "--corps 2000 --disque --vitesse repos --pas 500 --poussiere toutes --nom essai",
        ))
        .unwrap();
        assert_eq!(o.start, Start::New);
        assert_eq!(o.initial.count, 2000);
        assert!(matches!(o.initial.distribution, Distribution::Disk { .. }));
        assert_eq!(o.initial.velocity, VelocityMode::Rest);
        assert_eq!(o.target, RunTarget::Steps(500));
        assert_eq!(o.record.dust_budget, None);
        assert_eq!(o.name.as_deref(), Some("essai"));
        let o = parse(&args("--sans-affichage --duree 1,5 --dispersion 0,1")).unwrap();
        assert_eq!(o.target, RunTarget::WallTime(Duration::from_secs(90)));
        assert_eq!(o.initial.velocity, VelocityMode::CircularOrbital { spread: 0.1 });
    }

    #[test]
    fn parses_resume_options() {
        let o = parse(&args("--charger saves/a.stellar --pas 100")).unwrap();
        assert_eq!(o.start, Start::Save("saves/a.stellar".into(), None));
        let o = parse(&args("--charger a.stellar --poursuivre r.stellarec")).unwrap();
        assert_eq!(
            o.start,
            Start::Save("a.stellar".into(), Some("r.stellarec".into()))
        );
        let o = parse(&args("--reprendre r.stellarec --duree 10")).unwrap();
        assert_eq!(o.start, Start::Recording("r.stellarec".into()));
        assert!(parse(&args("--poursuivre r.stellarec")).is_err());
        assert!(parse(&args("--charger a --reprendre b")).is_err());
        let e = parse(&args("--charger a --corps 10")).unwrap_err();
        assert!(e.contains("--corps"), "{e}");
    }

    #[test]
    fn help_and_unknown_arguments() {
        assert_eq!(parse(&args("--help")).err().as_deref(), Some(""));
        assert_eq!(parse(&args("--pas 10 -h")).err().as_deref(), Some(""));
        let e = parse(&args("--bidule")).unwrap_err();
        assert!(e.contains("--bidule"), "{e}");
        let e = parse(&args("--pas 10 fichier.txt")).unwrap_err();
        assert!(e.contains("fichier.txt"), "{e}");
        assert!(parse(&args("--corps")).is_err());
        assert!(parse(&args("--pas 10")).is_ok());
    }
}
