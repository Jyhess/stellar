//! Mode console : `stellar.exe --sans-affichage [options]`.
//!
//! Lance une simulation sans fenêtre, l'enregistre et s'arrête à la cible
//! (ou à Ctrl+C), puis clôt l'enregistrement et sauvegarde l'état final.

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

/// Option déclenchant le mode console.
pub const FLAG: &str = "--sans-affichage";

static STOP: AtomicBool = AtomicBool::new(false);

/// Options d'aide.
const HELP: [&str; 3] = ["--help", "-h", "--aide"];

fn usage() -> String {
    tr!("Usage : stellar.exe                         interface graphique
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
sauvegardé (dossiers « {} » et « {} »).", SAVES_DIR, RECORDINGS_DIR)
}

struct Options {
    initial: InitialConditions,
    engine: Option<String>,
    name: Option<String>,
    record: RecordSettings,
    target: RunTarget,
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        initial: InitialConditions {
            velocity: VelocityMode::CircularOrbital { spread: 0.05 },
            ..Default::default()
        },
        engine: None,
        name: None,
        record: RecordSettings {
            interval: 10,
            dust_budget: Some(1_000_000),
        },
        target: RunTarget::Manual,
    };
    let (mut velocity, mut spread, mut vmin, mut vmax) = ("orb".to_owned(), 0.05, 0.0, 10.0);
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
        match a.as_str() {
            FLAG => {}
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
            h if HELP.contains(&h) => return Err(String::new()),
            other => return Err(tr!("argument inconnu : {}", other)),
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
    use windows_sys::Win32::System::Console::{
        AttachConsole, SetConsoleCtrlHandler, SetConsoleOutputCP, ATTACH_PARENT_PROCESS,
    };
    unsafe extern "system" fn on_ctrl(_: u32) -> i32 {
        STOP.store(true, Ordering::Release);
        1
    }
    // SAFETY : appels Win32 sans pointeur à conserver ; le gestionnaire
    // ne fait que lever un drapeau atomique.
    unsafe {
        // Exécutable fenêtré (sous-système Windows) : se rattacher à la
        // console qui l'a lancé pour y écrire.
        AttachConsole(ATTACH_PARENT_PROCESS);
        SetConsoleOutputCP(65001);
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

#[cfg(not(windows))]
fn setup_console() {}

/// Vérifie la ligne de commande : options de la simulation sans fenêtre
/// (`Err("")` : aide demandée ; `Err(message)` : erreur).
fn check(args: &[String]) -> Result<Options, String> {
    if args.iter().any(|a| HELP.contains(&a.as_str())) {
        return Err(String::new());
    }
    if !args.iter().any(|a| a == FLAG) {
        let Some(first) = args.first() else {
            return Err(String::new());
        };
        // Option de simulation (seule, ou suivie de sa valeur) sans le mode.
        let known = parse(std::slice::from_ref(first)).is_ok()
            || parse(&args[..args.len().min(2)]).is_ok();
        return Err(if known {
            tr!("l'option {} demande --sans-affichage", first)
        } else {
            tr!("argument inconnu : {}", first)
        });
    }
    parse(args)
}

/// Exécute le mode console ; renvoie le code de sortie.
pub fn run(args: &[String]) -> i32 {
    setup_console();
    let o = match check(args) {
        Ok(o) => o,
        Err(e) if e.is_empty() => {
            println!("{}", usage());
            return 0;
        }
        Err(e) => {
            eprintln!("{}", tr!("Erreur : {}", e));
            eprintln!("{}", crate::i18n::tr("Voir stellar.exe --help pour la liste des options."));
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
    let name = o
        .name
        .unwrap_or_else(|| crate::home::default_name(&o.initial, chrono::Local::now(), |_| false));
    let config = SimulationConfig {
        initial: o.initial,
        physics: PhysicsParams::suggested(&o.initial, 1.0),
        engine,
    };
    println!(
        "{}",
        tr!(
            "Stellar sans affichage — « {} » : {} corps, moteur {}.",
            name,
            format::count(config.initial.count as u64),
            config.engine
        )
    );
    if let Err(e) = &gpu {
        println!("{}", tr!("Moteur GPU indisponible : {}", e));
    }
    let run = match HeadlessRun::start(&service, config, &name, o.record, o.target) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{e}");
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
    fn parses_console_options() {
        let o = parse(&args(
            "--sans-affichage --corps 2000 --disque --vitesse repos --pas 500 --poussiere toutes --nom essai",
        ))
        .unwrap();
        assert_eq!(o.initial.count, 2000);
        assert!(matches!(o.initial.distribution, Distribution::Disk { .. }));
        assert_eq!(o.initial.velocity, VelocityMode::Rest);
        assert_eq!(o.target, RunTarget::Steps(500));
        assert_eq!(o.record.dust_budget, None);
        assert_eq!(o.name.as_deref(), Some("essai"));
        let o = parse(&args("--duree 1,5 --dispersion 0,1")).unwrap();
        assert_eq!(o.target, RunTarget::WallTime(Duration::from_secs(90)));
        assert_eq!(o.initial.velocity, VelocityMode::CircularOrbital { spread: 0.1 });
        assert!(parse(&args("--inconnue")).is_err());
        assert!(parse(&args("--corps")).is_err());
    }

    #[test]
    fn help_and_unknown_arguments() {
        assert_eq!(check(&args("--help")).err().as_deref(), Some(""));
        assert_eq!(check(&args("--sans-affichage -h")).err().as_deref(), Some(""));
        let e = check(&args("--bidule")).err().unwrap();
        assert!(e.contains("--bidule"), "{e}");
        let e = check(&args("--sans-affichage --pas 10 fichier.txt")).err().unwrap();
        assert!(e.contains("fichier.txt"), "{e}");
        let e = check(&args("--corps 1000")).err().unwrap();
        assert!(e.contains("--sans-affichage"), "{e}");
        assert!(check(&args("--sans-affichage --pas 10")).is_ok());
    }
}
