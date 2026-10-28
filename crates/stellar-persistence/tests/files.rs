//! Suppression et renommage des sauvegardes et des enregistrements liés.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use stellar_application::{
    EngineRegistry, Keyframe, RecordingHeader, RecordingLink, RecordingRepository, SaveData,
    SimulationRepository, SimulatorService,
};
use stellar_domain::{BodySet, SimulationConfig, SimulationState, Vec3};
use stellar_persistence::{FileRecordingRepository, FileRepository};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("stellar-files-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

struct Fixture {
    service: SimulatorService,
    saves: Arc<FileRepository>,
    recordings: Arc<FileRecordingRepository>,
}

fn fixture(dir: &Path) -> Fixture {
    let saves = Arc::new(FileRepository::new(dir.join("saves")));
    let recordings = Arc::new(FileRecordingRepository::new(dir.join("recordings")));
    let service = SimulatorService::new(EngineRegistry::new(), saves.clone(), recordings.clone());
    Fixture {
        service,
        saves,
        recordings,
    }
}

fn state() -> SimulationState {
    let mut b = BodySet::default();
    b.push(1, Vec3::ZERO, Vec3::ZERO, 1.0);
    b.push(2, Vec3::X, Vec3::ZERO, 1.0);
    SimulationState {
        bodies: b,
        ..Default::default()
    }
}

/// Un enregistrement « rec_a », sa sauvegarde de reprise liée, et une
/// sauvegarde sans lien.
fn populate(f: &Fixture) -> PathBuf {
    let header = RecordingHeader {
        config: SimulationConfig::default(),
        interval: 1,
        start_step: 0,
        created_at_unix: 0,
        dust_budget: None,
    };
    let mut w = f.recordings.create("rec_a", &header).unwrap();
    w.write_frame(&Keyframe {
        bodies: state().bodies,
        dust_weight: 1.0,
        ..Default::default()
    })
    .unwrap();
    let rec = w.finish().unwrap();
    let data = |link: Option<RecordingLink>| SaveData {
        config: SimulationConfig::default(),
        state: state(),
        view: None,
        recording: link,
        continue_recording: None,
    };
    let link = RecordingLink {
        path: rec.clone(),
        interval: 1,
        dust_budget: None,
        last_frame_step: 0,
        next_step: 1,
        pending: Vec::new(),
    };
    f.saves.save("rec_a_reprise", &data(Some(link))).unwrap();
    f.saves.save("autre", &data(None)).unwrap();
    rec
}

fn names(f: &Fixture) -> (Vec<String>, Vec<String>) {
    let mut s: Vec<String> = f.saves.list().unwrap().into_iter().map(|s| s.name).collect();
    let mut r: Vec<String> = f.recordings.list().unwrap().into_iter().map(|r| r.name).collect();
    s.sort();
    r.sort();
    (s, r)
}

#[test]
fn renaming_a_recording_carries_its_saves_along() {
    let dir = temp_dir("rename-rec");
    let f = fixture(&dir);
    populate(&f);
    let rec = f.recordings.list().unwrap().remove(0);
    f.service.rename_recording(&rec, "orbites").unwrap();
    assert_eq!(
        names(&f),
        (vec!["autre".into(), "orbites_reprise".into()], vec!["orbites".into()])
    );
    let save = f
        .saves
        .list()
        .unwrap()
        .into_iter()
        .find(|s| s.name == "orbites_reprise")
        .unwrap();
    assert_eq!(save.recording.as_deref(), Some("orbites.stellarec"));
    let link = f.saves.load(&save.path).unwrap().recording.unwrap();
    assert_eq!(link.path.file_name().unwrap(), "orbites.stellarec");

    // Renommer la sauvegarde renomme aussi l'enregistrement lié.
    f.service.rename_save(&save, "final_reprise").unwrap();
    assert_eq!(
        names(&f),
        (vec!["autre".into(), "final_reprise".into()], vec!["final".into()])
    );
    // Un nom déjà pris est refusé.
    let autre = f.saves.list().unwrap().into_iter().find(|s| s.name == "autre").unwrap();
    assert!(f.service.rename_save(&autre, "final_reprise").is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deleting_with_or_without_the_linked_file() {
    let dir = temp_dir("delete");
    let f = fixture(&dir);
    populate(&f);
    let save = |name: &str| f.saves.list().unwrap().into_iter().find(|s| s.name == name).unwrap();
    // Sans le fichier lié : l'enregistrement reste.
    f.service.delete_save(&save("autre"), true).unwrap();
    assert_eq!(names(&f), (vec!["rec_a_reprise".into()], vec!["rec_a".into()]));
    // Avec : la sauvegarde liée part aussi.
    let rec = f.recordings.list().unwrap().remove(0);
    f.service.delete_recording(&rec, true).unwrap();
    assert_eq!(names(&f), (Vec::new(), Vec::new()));

    populate(&f);
    f.service.delete_save(&save("rec_a_reprise"), false).unwrap();
    assert_eq!(names(&f), (vec!["autre".into()], vec!["rec_a".into()]));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn headless_run_stops_at_target_and_saves() {
    use stellar_application::{HeadlessRun, RecordSettings, RunTarget};
    use stellar_domain::{InitialConditions, PhysicsParams, VelocityMode};
    let dir = temp_dir("headless");
    let saves = Arc::new(FileRepository::new(dir.join("saves")));
    let recordings = Arc::new(FileRecordingRepository::new(dir.join("recordings")));
    let mut registry = EngineRegistry::new();
    registry.register(Arc::new(stellar_engine_cpu::CpuEngineFactory));
    let service = SimulatorService::new(registry, saves.clone(), recordings.clone());
    let initial = InitialConditions {
        count: 300,
        radius: 10.0,
        velocity: VelocityMode::Rest,
        ..Default::default()
    };
    let config = SimulationConfig {
        initial,
        physics: PhysicsParams::suggested(&initial, 1.0),
        engine: "cpu-barnes-hut".into(),
    };
    let run = HeadlessRun::start(
        &service,
        config,
        "essai_sans_affichage",
        RecordSettings {
            interval: 5,
            dust_budget: None,
        },
        RunTarget::Steps(40),
    )
    .unwrap();
    let t0 = std::time::Instant::now();
    while !run.done() {
        assert!(t0.elapsed().as_secs() < 60, "cible jamais atteinte");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let outcome = run.finish(&service).unwrap();
    assert_eq!(outcome.step, 40);
    let data = saves.load(&outcome.save).unwrap();
    assert_eq!(data.state.step, 40);
    let mut r = recordings.open(&outcome.recording.unwrap()).unwrap();
    let steps: Vec<u64> = r.info().frames.iter().map(|f| f.step).collect();
    assert_eq!(steps.first(), Some(&0));
    assert_eq!(steps.last(), Some(&40), "{steps:?}");
    assert!(r.read_frame(steps.len() - 1).is_ok());
    assert_eq!(f_names(&dir), vec!["essai_sans_affichage".to_owned()]);
    let _ = std::fs::remove_dir_all(&dir);
}

fn f_names(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir.join("saves"))
        .unwrap()
        .flatten()
        .map(|e| e.path().file_stem().unwrap().to_string_lossy().into_owned())
        .collect();
    v.sort();
    v
}

