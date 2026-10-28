//! Règle de nettoyage des éjectés : amas liés conservés.

use stellar_domain::ejection::{friends_of_friends, sort_out, Candidate, EjectionRules};
use stellar_domain::Vec3;

fn rules() -> EjectionRules {
    EjectionRules {
        g: 1.0,
        softening: 0.01,
        link_length: 1.0,
        min_bodies: 10,
        min_mass: 0.0,
    }
}

fn body(id: u64, position: Vec3, velocity: Vec3, mass: f64) -> Candidate {
    Candidate {
        id,
        position,
        velocity,
        mass,
        // Non lié au système entier : seule l'appartenance à un amas sauve.
        potential: 0.0,
    }
}

/// Amas compact de `n` corps de masse 1 autour de `center`, au repos dans
/// son référentiel, animé de la vitesse `velocity`.
fn cluster(first: u64, n: u64, center: Vec3, radius: f64, velocity: Vec3) -> Vec<Candidate> {
    (0..n)
        .map(|k| {
            let a = k as f64 * 2.399_963;
            let z = 1.0 - 2.0 * (k as f64 + 0.5) / n as f64;
            let s = (1.0 - z * z).sqrt();
            let r = radius * ((k as f64 + 0.5) / n as f64).cbrt();
            body(
                first + k,
                center + Vec3::new(a.cos() * s, a.sin() * s, z) * r,
                velocity,
                1.0,
            )
        })
        .collect()
}

#[test]
fn friends_of_friends_follows_chains() {
    let v = Vec3::ZERO;
    let c = vec![
        body(0, Vec3::new(0.0, 0.0, 0.0), v, 1.0),
        body(1, Vec3::new(0.9, 0.0, 0.0), v, 1.0),
        body(2, Vec3::new(1.8, 0.0, 0.0), v, 1.0),
        body(3, Vec3::new(5.0, 0.0, 0.0), v, 1.0),
        // De part et d'autre d'une frontière de cellule.
        body(4, Vec3::new(-0.1, 7.0, 0.0), v, 1.0),
        body(5, Vec3::new(0.1, 7.0, 0.0), v, 1.0),
    ];
    let mut groups = friends_of_friends(&c, 1.0);
    for g in &mut groups {
        g.sort_unstable();
    }
    groups.sort();
    assert_eq!(groups, vec![vec![0, 1, 2], vec![3], vec![4, 5]]);
}

#[test]
fn bound_cluster_is_kept_and_described() {
    let mut c = cluster(100, 20, Vec3::new(0.0, 30.0, 0.0), 0.2, Vec3::new(0.0, 50.0, 0.0));
    c.push(body(1, Vec3::new(-30.0, 0.0, 0.0), Vec3::new(-50.0, 0.0, 0.0), 1.0));
    let e = sort_out(&c, Vec3::ZERO, &rules());
    assert_eq!(e.removed, vec![20]);
    assert_eq!(e.clusters.len(), 1);
    let k = &e.clusters[0];
    assert_eq!(k.bodies, 20);
    assert!((k.mass - 20.0).abs() < 1e-12);
    assert!((k.velocity - Vec3::new(0.0, 50.0, 0.0)).length() < 1e-9);
    assert!(k.half_mass_radius > 0.0 && k.half_mass_radius < 0.2);
}

#[test]
fn unbound_member_is_stripped_from_cluster() {
    let mut c = cluster(0, 20, Vec3::ZERO, 0.2, Vec3::new(50.0, 0.0, 0.0));
    // Un membre beaucoup trop rapide pour le puits de l'amas.
    c[3].velocity += Vec3::new(1_000.0, 0.0, 0.0);
    let e = sort_out(&c, Vec3::ZERO, &rules());
    assert_eq!(e.removed, vec![3]);
    assert_eq!(e.clusters[0].bodies, 19);
}

#[test]
fn small_groups_are_removed_unless_massive() {
    let small = cluster(0, 5, Vec3::ZERO, 0.2, Vec3::new(50.0, 0.0, 0.0));
    let e = sort_out(&small, Vec3::ZERO, &rules());
    assert_eq!(e.removed, vec![0, 1, 2, 3, 4]);
    assert!(e.clusters.is_empty());

    // Critère de masse : un corps isolé assez massif est conservé.
    let mut r = rules();
    r.min_mass = 10.0;
    let lone = vec![
        body(7, Vec3::ZERO, Vec3::ZERO, 12.0),
        body(8, Vec3::new(50.0, 0.0, 0.0), Vec3::new(0.0, 50.0, 0.0), 1.0),
    ];
    let e = sort_out(&lone, Vec3::ZERO, &r);
    assert_eq!(e.removed, vec![1]);
    assert_eq!(e.clusters.len(), 1);
    assert_eq!(e.clusters[0].anchor, 7);
}

#[test]
fn bodies_bound_to_the_system_are_never_removed() {
    let mut c = vec![body(0, Vec3::new(40.0, 0.0, 0.0), Vec3::ZERO, 1.0)];
    c[0].potential = -1.0;
    let e = sort_out(&c, Vec3::ZERO, &rules());
    assert!(e.removed.is_empty());
}

#[test]
fn large_clusters_use_the_spherical_approximation() {
    // Au-delà de 2 048 membres, le potentiel propre est approché : un amas
    // lié reste reconnu, une vitesse d'évasion nette l'en détache.
    let mut c = cluster(0, 3_000, Vec3::ZERO, 2.0, Vec3::new(5.0, 0.0, 0.0));
    let escape = (2.0 * 3_000.0 / 2.0f64).sqrt();
    c[0].velocity += Vec3::new(0.0, 3.0 * escape, 0.0);
    let mut r = rules();
    r.softening = 0.05;
    let e = sort_out(&c, Vec3::ZERO, &r);
    assert_eq!(e.removed, vec![0]);
    assert_eq!(e.clusters.len(), 1);
    assert_eq!(e.clusters[0].bodies, 2_999);
}
