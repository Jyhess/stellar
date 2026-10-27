//! Poursuite d'un enregistrement depuis une sauvegarde qui n'y est pas liée.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use stellar_application::{
    BodyFate, EngineRegistry, Phase, RecordingRepository, SimulationHandle, SimulatorService,
};
use stellar_domain::{Distribution, InitialConditions, PhysicsParams, SimulationConfig, VelocityMode};
use stellar_engine_cpu::CpuEngineFactory;
use stellar_persistence::{FileRecordingRepository, FileRepository};

fn temp_dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("stellar-cont-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn service(dir: &std::path::Path) -> (SimulatorService, Arc<FileRecordingRepository>) {
    let mut registry = EngineRegistry::new();
    registry.register(Arc::new(CpuEngineFactory));
    let recordings = Arc::new(FileRecordingRepository::new(dir.join("recordings")));
    let s = SimulatorService::new(
        registry,
        Arc::new(FileRepository::new(dir.join("saves"))),
        recordings.clone(),
    );
    (s, recordings)
}

fn config(seed: u64) -> SimulationConfig {
    let initial = InitialConditions {
        count: 400,
        radius: 10.0,
        seed,
        distribution: Distribution::Sphere,
        velocity: VelocityMode::Rest,
    };
    let mut physics = PhysicsParams::suggested(&initial, 1.0);
    physics.base_radius *= 25.0; // fusions fréquentes
    SimulationConfig {
        initial,
        physics,
        engine: "cpu-barnes-hut".into(),
    }
}

fn wait_ready(h: &SimulationHandle) {
    for _ in 0..1000 {
        if h.latest().phase == Phase::Ready {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("simulation jamais prête");
}

fn step_to(h: &SimulationHandle, step: u64) {
    while h.latest().state.step < step {
        h.step_once();
        let target = h.latest().state.step + 1;
        for _ in 0..1000 {
            if h.latest().state.step >= target {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

#[test]
fn continues_a_recording_from_an_unlinked_save() {
    let dir = temp_dir("unlinked");
    let (service, recordings) = service(&dir);
    let h = service.new_simulation(config(1)).unwrap();
    h.pause();
    wait_ready(&h);
    let start = h.latest().state.step;
    service.start_recording("rec", &h, 5, None).unwrap();
    step_to(&h, start + 23);
    let snap = h.latest();
    let save_step = snap.state.step;
    let save = service
        .save("milieu", h.config(), &snap, None, None)
        .unwrap();
    step_to(&h, save_step + 20);
    let n_before = h.latest().state.bodies.len();
    h.stop_recording();
    h.shutdown();
    let rec_path = recordings.list().unwrap()[0].path.clone();

    // Au chargement : l'enregistrement est proposé, avec ses instantanés à supprimer.
    let data = service.read_save(&save).unwrap();
    let options = service.continuable_recordings(&data);
    assert_eq!(options.len(), 1);
    assert!(options[0].dropped_frames > 0);
    assert!(options[0].resume_step <= save_step);
    let resume_step = options[0].resume_step;

    let loaded = service.start_loaded(data, Some(&rec_path)).unwrap();
    assert_eq!(loaded.recording.as_ref().unwrap().as_ref().unwrap(), &rec_path);
    let h = loaded.handle;
    wait_ready(&h);
    step_to(&h, save_step + 12);
    h.stop_recording();
    h.shutdown();

    let mut r = recordings.open(&rec_path).unwrap();
    let steps: Vec<u64> = r.info().frames.iter().map(|f| f.step).collect();
    assert!(steps.windows(2).all(|w| w[0] < w[1]), "{steps:?}");
    // Rien entre le dernier instantané conservé et la sauvegarde, puis un
    // instantané au pas de la sauvegarde, puis l'intervalle d'origine.
    let k = steps.iter().position(|&s| s == resume_step).unwrap();
    assert_eq!(steps[k + 1], save_step, "{steps:?}");
    assert_eq!(steps[k + 2], save_step + 5, "{steps:?}");
    // Les corps disparus entre les deux reçoivent un devenir (fusion connue
    // par l'instantané suivant de l'ancien enregistrement).
    let before = r.read_frame(k).unwrap();
    let at_save = r.read_frame(k + 1).unwrap();
    let gone = before.bodies.len() - at_save.bodies.len();
    assert_eq!(at_save.fates.len(), gone);
    assert!(n_before < 400);
    if gone > 0 {
        assert!(at_save
            .fates
            .iter()
            .any(|(_, f)| matches!(f, BodyFate::MergedInto(_))));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rejects_recordings_of_another_simulation() {
    let dir = temp_dir("other");
    let (service, recordings) = service(&dir);
    let h = service.new_simulation(config(1)).unwrap();
    h.pause();
    wait_ready(&h);
    service.start_recording("rec", &h, 5, None).unwrap();
    step_to(&h, h.latest().state.step + 6);
    h.stop_recording();
    h.shutdown();
    let rec_path = recordings.list().unwrap()[0].path.clone();

    let other = service.new_simulation(config(2)).unwrap();
    other.pause();
    wait_ready(&other);
    step_to(&other, other.latest().state.step + 6);
    let save = service
        .save("autre", other.config(), &other.latest(), None, None)
        .unwrap();
    other.shutdown();

    let data = service.read_save(&save).unwrap();
    assert!(service.continuable_recordings(&data).is_empty());
    let loaded = service.start_loaded(data, Some(&rec_path)).unwrap();
    assert!(loaded.recording.unwrap().is_err());
    loaded.handle.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn f5_save_during_recording_continues_it_on_load() {
    let dir = temp_dir("f5");
    let (service, recordings) = service(&dir);
    let h = service.new_simulation(config(1)).unwrap();
    h.pause();
    wait_ready(&h);
    service.start_recording("rec", &h, 4, None).unwrap();
    step_to(&h, h.latest().state.step + 10);
    let current = h.recording_status().current_path.lock().unwrap().clone();
    assert!(current.is_some());
    let snap = h.latest();
    let save = service
        .save("f5", h.config(), &snap, None, current.clone())
        .unwrap();
    step_to(&h, snap.state.step + 8);
    h.stop_recording();
    h.shutdown();
    assert_eq!(
        service.list_saves().unwrap()[0].recording.as_deref(),
        current.as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str())
    );

    let loaded = service.load_simulation(&save).unwrap();
    assert!(loaded.recording.unwrap().is_ok());
    loaded.handle.shutdown();
    let r = recordings.open(&current.unwrap()).unwrap();
    assert!(r.info().frames.iter().any(|f| f.step == snap.state.step));
    assert!(r.info().frames.iter().all(|f| f.step <= snap.state.step));
    let _ = std::fs::remove_dir_all(&dir);
}
