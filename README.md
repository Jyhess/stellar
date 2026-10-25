# Stellar

Simulateur gravitationnel N-corps en 3D. Un nuage de particules de masse 1 s'effondre sous l'effet de la gravité, et deux corps qui se touchent fusionnent. Le calcul se fait **sur le CPU** (Barnes-Hut multithread) ; la carte graphique sert **uniquement à l'affichage** (wgpu).

La spécification fonctionnelle se trouve dans [`SPEC.md`](SPEC.md).

---

## 1. Installation sous Windows

### Rust

1. Téléchargez et lancez **rustup-init.exe** depuis <https://rustup.rs>.
2. Si l'installeur le propose, acceptez l'installation des **Visual Studio Build Tools** (charge de travail « Développement Desktop en C++ », avec le SDK Windows). Rust en a besoin pour l'édition de liens.
3. Ouvrez un **nouveau** terminal (PowerShell) et vérifiez :

```powershell
rustc --version
cargo --version
```

### Compiler et lancer

Depuis le dossier `stellar` :

```powershell
cargo run --release
```

La première compilation prend quelques minutes, le temps de télécharger et de compiler les dépendances. Les suivantes sont rapides. L'exécutable produit se trouve dans `target\release\stellar.exe`.

> Lancez toujours en `--release` : en mode debug, le calcul est plusieurs dizaines de fois plus lent.

L'affichage est plafonné à 120 images par seconde.

Les sauvegardes sont écrites dans le sous-dossier `saves\` du répertoire de lancement.

---

## 2. Utilisation

### Écran d'accueil

- **Nouvelle simulation** : nombre de particules, rayon du nuage, graine aléatoire, répartition (sphère ou disque, avec épaisseur et profil de densité), vitesse initiale (repos, aléatoire, circulaire imposée, circulaire orbitale ± dispersion, radiale).
- **Paramètres avancés** : G, dt, adoucissement ε, rayon r₀ d'une particule de masse 1, précision θ, période et distance du nettoyage, moteur de calcul. Par défaut, dt, ε et r₀ sont déduits des conditions initiales :
  - dt = temps de chute libre / 500 ;
  - ε = 0,3 × distance moyenne entre voisins ;
  - r₀ = 1 % de cette distance.
- **Simulations sauvegardées** : la liste du dossier `saves\`, avec un bouton « Parcourir… » pour ouvrir un fichier situé ailleurs. Une simulation chargée redémarre en pause.

### Commandes

| Touche | Action |
|---|---|
| Z / S | avancer / reculer |
| Q / D | gauche / droite |
| E / A | monter / descendre |
| Maj / Ctrl | déplacement ×5 / ×0,2 |
| + / − | vitesse de déplacement |
| Souris | orientation de la vue (ou orbite autour du corps suivi) |
| Molette | zoom optique (ou distance au corps suivi) |
| Clic gauche | suivre le corps visé (réticule, ou pointeur quand la souris est libre) |
| Origine | aller au corps le plus massif |
| PgDn / PgUp | corps suivant / précédent dans l'ordre des masses |
| C | viser le centre de gravité (vol libre) |
| T | mode trace : trajectoires des 10 corps les plus massifs visibles à l'écran à cet instant / effacer |
| R | traces : référentiel absolu / relatif au corps sélectionné |
| V | vue d'ensemble : recul jusqu'à voir 90 % de la masse, dans la direction de visée actuelle |
| Espace | pause / reprise |
| P | pas à pas |
| Tab | libérer / capturer la souris |
| Clic droit | reprendre la visée quand la souris est libre |
| F5 | sauvegarder |
| Échap | menu (reprendre, sauvegarder, accueil, quitter) |

Quand un corps est suivi, la caméra l'accompagne et tourne autour de lui. Si ce corps fusionne, le suivi passe au corps qui en résulte. Une touche de déplacement rend la main en vol libre. En mode trace, chaque trace a sa couleur, rappelée dans le panneau « Corps ». Elle reçoit un point à chaque pas de calcul et se poursuit sur le corps issu d'une fusion. Le panneau propose deux affichages :

- l'**historique** complet depuis l'activation (décimé au-delà de 4 096 points) ;
- une **queue** des N derniers pas, de 10 à 5 000, qui s'estompe vers le passé.

En **référentiel relatif** (touche R), chaque point devient p(t) − r(t) + r(maintenant), où r est la trajectoire du corps sélectionné. La référence reste ainsi fixe à l'écran et les orbites autour d'elle apparaissent. Sa trajectoire vient de sa propre trace si elle fait partie des corps tracés, sinon d'une piste enregistrée depuis sa sélection. Dans ce second cas, la partie plus ancienne des traces n'est pas affichée.

Les traces ne sont pas sauvegardées.

En passant d'un corps à un autre (clic, Origine, PgDn/PgUp, classement), la direction de visée, le zoom et la distance à la cible sont conservés.

---

## 3. Architecture

Le projet suit la **clean architecture**. Les dépendances pointent toutes vers le domaine :

```
stellar-app (présentation + racine de composition : egui, wgpu)
   │
   ├──► stellar-persistence (adaptateur : fichiers .stellar)
   ├──► stellar-engine-cpu  (adaptateur : moteur Barnes-Hut CPU)
   │
   └──► stellar-application (cas d'usage, ports)
              │
              └──► stellar-domain (entités, règles, trait PhysicsEngine)
```

| Crate | Rôle |
|---|---|
| `stellar-domain` | Corps (`BodySet`, en structure de tableaux), état, paramètres, règles physiques (rayon, fusion, distance balayée), classement par masse, **traits `PhysicsEngine` / `EngineFactory`**. Aucune infrastructure. |
| `stellar-application` | Génération des conditions initiales, thread de simulation découplé (`SimulationHandle`, instantanés publiés à ≤ 60 Hz), registre des moteurs, ports `SimulationRepository`, façade `SimulatorService`. |
| `stellar-engine-cpu` | Moteur de calcul. Optimisé librement, comme prévu par la spécification (voir ci-dessous). |
| `stellar-persistence` | Format `.stellar` : signature, version, en-tête lisible sans décompression, puis données bincode compressées zstd. |
| `stellar-app` | Écrans egui, caméra, sélection, rendu GPU des corps (billboards additifs, positions relatives à une ancre pour garder la précision en `f32`). |

### Moteur CPU

À chaque pas :

1. **Kick ½ dt** : demi-mise à jour des vitesses.
2. **Drift dt** : mise à jour des positions ; les positions de départ sont conservées.
3. **Contacts balayés** :
   - grille uniforme sur les boîtes englobantes des trajectoires ; les corps très rapides ou très gros sont traités à part ;
   - distance minimale exacte entre trajectoires rectilignes ;
   - fusions en chaîne regroupées par union-find.
4. **Fusion** : la masse et la quantité de mouvement sont conservées et la position passe au barycentre. L'identifiant du membre le plus massif est conservé.
5. **Forces** :
   - réordonnancement des corps selon leur code de Morton ;
   - octree construit en parallèle, stocké en ordre préfixe et parcouru sans pile ;
   - critère d'ouverture `d > s/θ + δ` et adoucissement de Plummer.
6. **Kick ½ dt**.
7. **Nettoyage, tous les N pas** : suppression des corps **non liés** (énergie spécifique > 0 relativement au barycentre) **et** situés au-delà de k × R.

Le calcul utilise tous les cœurs moins un, laissé libre pour l'interface.

### Ajouter un moteur de calcul

1. Créer une crate qui implémente `stellar_domain::PhysicsEngine` et `stellar_domain::EngineFactory`, avec un `id` unique.
2. L'enregistrer dans `crates/stellar-app/src/main.rs` :

```rust
registry.register(Arc::new(MonMoteurGpuFactory));
```

Le moteur apparaît alors dans « Paramètres avancés → Moteur de calcul ». Son identifiant est enregistré dans les sauvegardes.

---

## 4. Tests et benchmarks

```powershell
cargo test --release            # tests unitaires et physiques
cargo bench -p stellar-engine-cpu   # temps par pas pour 10k / 100k / 1M corps
```

Mesure rapide du moteur, avec affichage de l'évolution (corps restants, fusions, dérive d'énergie) :

```powershell
cargo run --release -p stellar-engine-cpu --example timing -- 1000000 5
```

Pour lancer directement une simulation sans passer par l'accueil : `$env:STELLAR_AUTOSTART=200000; cargo run --release`.

Les tests physiques (`crates/stellar-engine-cpu/tests/physics.rs`) vérifient notamment :

- la précision de Barnes-Hut par rapport à la somme directe ;
- la conservation de l'énergie sur une orbite à deux corps ;
- la conservation de la masse et de la quantité de mouvement lors des fusions ;
- la détection des croisements rapides ;
- les fusions en chaîne ;
- le critère d'éjection.
