//! Command line (`stellar-cli.exe`): simulation without a window, resumption
//! of a save or a recording.
//!
//! Runs the simulation, records it and stops at the target (or on Ctrl+C),
//! then closes the recording and saves the final state.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use stellar_application::{
    EngineRegistry, HeadlessRun, RecordSettings, RunTarget, SimulatorService,
};
use stellar_domain::{
    DensityProfile, Distribution, InitialConditions, PhysicsParams, SimulationConfig, VelocityMode,
    MAX_DUST_MASS, MAX_MASSIVE_FRACTION, MAX_PLANETS,
};
use stellar_engine_cpu::CpuEngineFactory;
use stellar_engine_gpu::GpuEngineFactory;
use stellar_persistence::{FileRecordingRepository, FileRepository};

use crate::errors::Localized;
use crate::headless::hms;
use crate::{format, RECORDINGS_DIR, SAVES_DIR};

static STOP: AtomicBool = AtomicBool::new(false);

fn usage() -> String {
    tr!("Usage: stellar-cli [options]        new simulation without a window\n       stellar-cli --load FILE           resume a save (.stellar)\n       stellar-cli --resume FILE         resume a recording (.stellarec)\n       stellar-cli --help                this help\nThe graphical interface is stellar.exe (without arguments).\n\nNew simulation:\n  --bodies N         number of particles (default: 1000000)\n  --radius R         cloud radius (default: 100)\n  --seed S           random seed (default: 42)\n  --disc             rotating disc (otherwise a sphere)\n  --velocity V       rest | random | circ | orb | rad (default: orb)\n  --spread D         spread of the orbital speed (default: 0.05)\n  --factor F         multiple of the orbital speed: < 1 collapse,\n                     > 1 expansion (default: 1)\n  --vmin A --vmax B  speed range (random, circ, rad)\n  --max-mass N       grain masses: integers from 1 to N (default: 1)\n  --mass-exponent A  power law of the masses, density ∝ m^A (default: -2)\n  --sun P            central sun of P % of the total mass\n  --planets N        N massive bodies on circular orbits (from 0.2 to 0.9 × radius)\n  --planet-mass P    mass of each one in % of the total mass (default: 1)\n  --engine M         gpu | cpu (default: gpu if a suitable card is present)\n\nResuming:\n  --load F           save to resume; its linked recording is continued,\n                     otherwise a new recording starts\n  --continue F       with --load: compatible recording to extend from the\n                     save's step (later snapshots deleted)\n  --resume F         recording to continue from its latest linked save\n\nIn all cases:\n  --name NAME        simulation name (default: date and parameters, or file name)\n  --interval N       one snapshot every N steps (default: 10)\n  --dust N           dust recorded per snapshot, or “all” (default: 1000000)\n  --steps N          stop after N steps (more, when resuming)\n  --time T           stop after a simulated time T (more, when resuming)\n  --duration M       stop after M minutes of computation\n  --prune            automatically remove the negligible lone bodies\n                     (single-body sub-simulations escaping)\nWithout a target: until Ctrl+C. When stopped, the recording is closed and the final\nstate saved (folders “{}” and “{}”).", SAVES_DIR, RECORDINGS_DIR)
}

/// Starting point of the simulation.
#[derive(Debug, Clone, PartialEq)]
enum Start {
    New,
    /// Save, and the recording to continue, if any.
    Save(PathBuf, Option<PathBuf>),
    /// Recording to continue from its latest linked save.
    Recording(PathBuf),
}

#[derive(Debug)]
struct Options {
    start: Start,
    initial: InitialConditions,
    /// New-simulation options given (refused when resuming).
    new_options: Vec<String>,
    engine: Option<String>,
    name: Option<String>,
    record: RecordSettings,
    target: RunTarget,
    /// Automatic removal of negligible lone bodies.
    prune: bool,
}

/// Parses the command line (`Err("")`: help requested; `Err(message)`:
/// error).
fn parse(args: &[String]) -> Result<Options, String> {
    if args.iter().any(|a| a == "--help" || a == "-h") {
        return Err(String::new());
    }
    let mut o = Options {
        start: Start::New,
        initial: InitialConditions {
            velocity: VelocityMode::CircularOrbital { spread: 0.05, factor: 1.0 },
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
        prune: false,
    };
    let (mut velocity, mut spread, mut factor, mut vmin, mut vmax) =
        ("orb".to_owned(), 0.05, 1.0, 0.0, 10.0);
    let (mut load, mut follow, mut resume) = (None, None, None);
    let mut it = args.iter();
    let value = |it: &mut std::slice::Iter<String>, flag: &str| {
        it.next()
            .cloned()
            .ok_or_else(|| tr!("missing value after {}", flag))
    };
    fn num<T: std::str::FromStr>(v: &str, flag: &str) -> Result<T, String> {
        v.replace(',', ".")
            .replace(['\u{00A0}', ' ', '_'], "")
            .parse()
            .map_err(|_| tr!("invalid value for {}: {}", flag, v))
    }
    while let Some(a) = it.next() {
        let new_option = matches!(
            a.as_str(),
            "--bodies" | "--radius" | "--seed" | "--disc" | "--velocity" | "--spread"
                | "--factor" | "--vmin" | "--vmax" | "--engine" | "--max-mass"
                | "--mass-exponent" | "--sun" | "--planets" | "--planet-mass"
        );
        if new_option {
            o.new_options.push(a.clone());
        }
        match a.as_str() {
            "--bodies" => o.initial.count = num(&value(&mut it, a)?, a)?,
            "--radius" => o.initial.radius = num(&value(&mut it, a)?, a)?,
            "--seed" => o.initial.seed = num(&value(&mut it, a)?, a)?,
            "--disc" => {
                o.initial.distribution = Distribution::Disk {
                    thickness: 0.05,
                    profile: DensityProfile::Exponential { scale: 0.3 },
                }
            }
            "--velocity" => velocity = value(&mut it, a)?,
            "--spread" => spread = num(&value(&mut it, a)?, a)?,
            "--factor" => factor = num(&value(&mut it, a)?, a)?,
            "--vmin" => vmin = num(&value(&mut it, a)?, a)?,
            "--vmax" => vmax = num(&value(&mut it, a)?, a)?,
            "--max-mass" => o.initial.dust.max = num(&value(&mut it, a)?, a)?,
            "--mass-exponent" => o.initial.dust.exponent = num(&value(&mut it, a)?, a)?,
            "--sun" => o.initial.sun = Some(num::<f64>(&value(&mut it, a)?, a)? / 100.0),
            "--planets" => o.initial.planets.count = num(&value(&mut it, a)?, a)?,
            "--planet-mass" => {
                o.initial.planets.mass_fraction = num::<f64>(&value(&mut it, a)?, a)? / 100.0
            }
            "--engine" => o.engine = Some(value(&mut it, a)?),
            "--load" => load = Some(PathBuf::from(value(&mut it, a)?)),
            "--continue" => follow = Some(PathBuf::from(value(&mut it, a)?)),
            "--resume" => resume = Some(PathBuf::from(value(&mut it, a)?)),
            "--name" => o.name = Some(value(&mut it, a)?),
            "--interval" => o.record.interval = num::<u32>(&value(&mut it, a)?, a)?.max(1),
            "--dust" => {
                let v = value(&mut it, a)?;
                o.record.dust_budget = if v == "all" { None } else { Some(num(&v, a)?) };
            }
            "--steps" => o.target = RunTarget::Steps(num(&value(&mut it, a)?, a)?),
            "--time" => o.target = RunTarget::SimTime(num(&value(&mut it, a)?, a)?),
            "--prune" => o.prune = true,
            "--duration" => {
                let m: f64 = num(&value(&mut it, a)?, a)?;
                o.target = RunTarget::WallTime(Duration::from_secs_f64(m.max(0.0) * 60.0));
            }
            _ => return Err(tr!("unknown argument: {}", a)),
        }
    }
    o.start = match (load, follow, resume) {
        (None, None, None) => Start::New,
        (Some(save), follow, None) => Start::Save(save, follow),
        (None, None, Some(rec)) => Start::Recording(rec),
        (None, Some(_), _) => return Err(crate::i18n::tr("--continue is used with --load").to_owned()),
        (Some(_), _, Some(_)) => {
            return Err(crate::i18n::tr("--load and --resume are mutually exclusive").to_owned())
        }
    };
    if o.start != Start::New {
        if let Some(opt) = o.new_options.first() {
            return Err(tr!("{} only applies to a new simulation", opt));
        }
    }
    o.initial.velocity = match velocity.as_str() {
        "rest" => VelocityMode::Rest,
        "random" => VelocityMode::Random { min: vmin, max: vmax },
        "circ" => VelocityMode::CircularFixed { min: vmin, max: vmax },
        "orb" => VelocityMode::CircularOrbital { spread, factor },
        "rad" => VelocityMode::Radial { min: vmin, max: vmax },
        v => return Err(tr!("unknown velocity: {}", v)),
    };
    o.initial.count = o.initial.count.max(2);
    let ic = &o.initial;
    if !(1..=MAX_DUST_MASS).contains(&ic.dust.max) {
        return Err(tr!("--max-mass must be between 1 and {}", MAX_DUST_MASS));
    }
    if ic.planets.count > MAX_PLANETS {
        return Err(tr!("--planets must be between 0 and {}", MAX_PLANETS));
    }
    let mut shares = ic.sun.into_iter().chain([ic.planets.mass_fraction]);
    if shares.any(|s| s.is_nan() || s <= 0.0) || !ic.masses_are_valid() {
        return Err(tr!(
            "the sun and the massive bodies must have a positive mass and not exceed {} % of the total mass",
            format!("{:.1}", MAX_MASSIVE_FRACTION * 100.0).replace('.', format::decimal_sep())
        ));
    }
    Ok(o)
}

#[cfg(windows)]
fn setup_console() {
    use windows_sys::Win32::System::Console::{SetConsoleCtrlHandler, SetConsoleOutputCP};
    unsafe extern "system" fn on_ctrl(_: u32) -> i32 {
        STOP.store(true, Ordering::Release);
        1
    }
    // SAFETY: Win32 calls with no pointer to keep; the handler only raises an
    // atomic flag.
    unsafe {
        SetConsoleOutputCP(65001);
        SetConsoleCtrlHandler(Some(on_ctrl), 1);
    }
}

#[cfg(not(windows))]
fn setup_console() {}

/// `stellar.exe` started with arguments: the windowed executable cannot hold
/// the prompt; redirect to `stellar-cli`. Returns the exit code.
pub fn redirect() -> i32 {
    #[cfg(windows)]
    // SAFETY: attaches to the parent's console, no pointer involved.
    unsafe {
        use windows_sys::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
    crate::i18n::init();
    eprintln!(
        "{}",
        crate::i18n::tr(
            "stellar.exe is the graphical interface and takes no argument. Command line: stellar-cli.exe (stellar-cli --help)."
        )
    );
    2
}

/// Runs the command line; returns the exit code.
pub fn run(args: &[String]) -> i32 {
    setup_console();
    let o = match parse(args) {
        Ok(o) => o,
        Err(e) if e.is_empty() => {
            println!("{}", usage());
            return 0;
        }
        Err(e) => {
            eprintln!("{}", tr!("Error: {}", e));
            eprintln!(
                "{}",
                crate::i18n::tr("See stellar-cli --help for the list of options.")
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
        println!("{}", tr!("GPU engine unavailable: {}", e.localized()));
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
                    "New simulation “{}”: {} bodies, engine {}.",
                    name,
                    format::count(config.initial.body_count() as u64),
                    config.engine
                )
            );
            HeadlessRun::start(&service, config, &name, o.record, o.target).map(|r| (r, None))
        }
        Start::Save(save, follow) => {
            println!("{}", tr!("Resuming the save {}.", save.display()));
            HeadlessRun::resume(&service, save, follow.as_deref(), o.name.as_deref(), o.record, o.target)
        }
        Start::Recording(rec) => {
            // Latest save linked to the recording.
            let save = service
                .linked_saves(rec)
                .into_iter()
                .max_by_key(|s| (s.saved_at_unix, s.step))
                .map(|s| s.path);
            match save {
                Some(save) => {
                    println!(
                        "{}",
                        tr!("Resuming the recording {} from the save {}.", rec.display(), save.display())
                    );
                    HeadlessRun::resume(&service, &save, Some(rec), o.name.as_deref(), o.record, o.target)
                }
                None => {
                    eprintln!(
                        "{}",
                        tr!(
                            "Error: no save in the “{}” folder is linked to {}; use --load SAVE --continue RECORDING.",
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
            run.handle().set_auto_prune(o.prune);
            if let Some(p) = continued {
                println!("{}", tr!("Recording continued: {}", p.display()));
            }
            run
        }
        Err(e) => {
            eprintln!("{}", tr!("Error: {}", e.localized()));
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
                "step {} · t = {} · {} bodies · {} steps/s · {} snapshots · {}",
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
    println!("{}", crate::i18n::tr("Stopping: closing the recording and saving…"));
    match run.finish(&service) {
        Ok(out) => {
            println!("{}", tr!("Finished at step {}.", format::count(out.step)));
            println!("{}", tr!("Save: {}", out.save.display()));
            if let Some(r) = out.recording {
                println!("{}", tr!("Recording: {}", r.display()));
            }
            0
        }
        Err(e) => {
            eprintln!("{}", tr!("Failure: {}", e.localized()));
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
            "--bodies 2000 --disc --velocity rest --steps 500 --dust all --name test",
        ))
        .unwrap();
        assert_eq!(o.start, Start::New);
        assert_eq!(o.initial.count, 2000);
        assert!(matches!(o.initial.distribution, Distribution::Disk { .. }));
        assert_eq!(o.initial.velocity, VelocityMode::Rest);
        assert_eq!(o.target, RunTarget::Steps(500));
        assert_eq!(o.record.dust_budget, None);
        assert_eq!(o.name.as_deref(), Some("test"));
        assert!(!o.prune);
        assert!(parse(&args("--prune")).unwrap().prune);
        assert!(parse(&args("--load a.stellar --prune")).unwrap().prune);
        let o = parse(&args("--duration 1,5 --spread 0,1 --factor 0,8")).unwrap();
        assert_eq!(o.target, RunTarget::WallTime(Duration::from_secs(90)));
        assert_eq!(
            o.initial.velocity,
            VelocityMode::CircularOrbital { spread: 0.1, factor: 0.8 }
        );
    }

    #[test]
    fn parses_mass_options() {
        let o = parse(&args(
            "--max-mass 50 --mass-exponent -1,5 --sun 60 --planets 3 --planet-mass 2",
        ))
        .unwrap();
        assert_eq!(o.initial.dust.max, 50);
        assert_eq!(o.initial.dust.exponent, -1.5);
        assert_eq!(o.initial.sun, Some(0.6));
        assert_eq!(o.initial.planets.count, 3);
        assert!((o.initial.planets.mass_fraction - 0.02).abs() < 1e-12);
        let o = parse(&args("--steps 10")).unwrap();
        assert_eq!(o.initial.sun, None);
        assert_eq!(o.initial.planets.count, 0);
        assert!(parse(&args("--sun 95 --planets 10 --planet-mass 1")).is_err());
        assert!(parse(&args("--sun 0")).is_err());
        assert!(parse(&args("--max-mass 0")).is_err());
        assert!(parse(&args("--planets 1000")).is_err());
        let e = parse(&args("--load a --sun 50")).unwrap_err();
        assert!(e.contains("--sun"), "{e}");
    }

    #[test]
    fn parses_resume_options() {
        let o = parse(&args("--load saves/a.stellar --steps 100")).unwrap();
        assert_eq!(o.start, Start::Save("saves/a.stellar".into(), None));
        let o = parse(&args("--load a.stellar --continue r.stellarec")).unwrap();
        assert_eq!(
            o.start,
            Start::Save("a.stellar".into(), Some("r.stellarec".into()))
        );
        let o = parse(&args("--resume r.stellarec --duration 10")).unwrap();
        assert_eq!(o.start, Start::Recording("r.stellarec".into()));
        assert!(parse(&args("--continue r.stellarec")).is_err());
        assert!(parse(&args("--load a --resume b")).is_err());
        let e = parse(&args("--load a --bodies 10")).unwrap_err();
        assert!(e.contains("--bodies"), "{e}");
    }

    #[test]
    fn help_and_unknown_arguments() {
        assert_eq!(parse(&args("--help")).err().as_deref(), Some(""));
        assert_eq!(parse(&args("--steps 10 -h")).err().as_deref(), Some(""));
        let e = parse(&args("--bogus")).unwrap_err();
        assert!(e.contains("--bogus"), "{e}");
        let e = parse(&args("--steps 10 file.txt")).unwrap_err();
        assert!(e.contains("file.txt"), "{e}");
        assert!(parse(&args("--bodies")).is_err());
        assert!(parse(&args("--steps 10")).is_ok());
        // The former French names are refused.
        assert!(parse(&args("--corps 10")).is_err());
    }
}
