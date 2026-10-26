//! Inspection d'un enregistrement ou d'une sauvegarde :
//! `cargo run --release -p stellar-persistence --example inspect -- <fichier>`
use std::path::Path;

use stellar_application::{RecordingRepository, SimulationRepository};
use stellar_persistence::{FileRecordingRepository, FileRepository};

fn main() {
    let arg = std::env::args().nth(1).expect("chemin attendu");
    let path = Path::new(&arg);
    let dir = path.parent().unwrap_or(Path::new("."));
    if path.extension().and_then(|e| e.to_str()) == Some("stellarec") {
        let repo = FileRecordingRepository::new(dir);
        let summary = repo
            .list()
            .unwrap()
            .into_iter()
            .find(|s| s.path == path)
            .unwrap();
        let r = repo.open(path).unwrap();
        let steps: Vec<u64> = r.info().frames.iter().map(|f| f.step).collect();
        println!(
            "instantanés : {} · clos : {}",
            steps.len(),
            summary.complete
        );
        println!("premiers pas : {:?}", &steps[..steps.len().min(5)]);
        println!(
            "derniers pas : {:?}",
            &steps[steps.len().saturating_sub(5)..]
        );
        let gaps: Vec<u64> = steps.windows(2).map(|w| w[1] - w[0]).collect();
        println!(
            "écarts min/max : {:?} / {:?}",
            gaps.iter().min(),
            gaps.iter().max()
        );
    } else {
        let repo = FileRepository::new(dir);
        let data = repo.load(path).unwrap();
        println!(
            "pas : {} · corps : {}",
            data.state.step,
            data.state.bodies.len()
        );
        println!(
            "lien : {:?}",
            data.recording
                .map(|l| (l.path, l.last_frame_step, l.next_step, l.pending.len()))
        );
    }
}
