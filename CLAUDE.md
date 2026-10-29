# Stellar — consignes pour Claude

Simulateur gravitationnel N-corps en Rust (wgpu + egui) pour Windows. Poste de
développement : Windows, NVIDIA RTX 4060 Ti (8 Go dédiés), 16 Go de mémoire.
La spécification validée est dans `SPEC.md`, l'usage et l'architecture dans
`README.md` : les lire avant toute modification d'ampleur.

## Communication

- Répondre en français soutenu, au vocabulaire riche.
- Ne pas chercher à prolonger la discussion ; ne poser une question que si
  elle aide à répondre à la demande. Poser en revanche toutes les questions de
  clarification nécessaires **avant** d'implémenter une fonctionnalité.
- Tout le texte du projet est en français : interface, messages d'erreur,
  commentaires, documentation, messages de commit. L'interface est aussi
  traduite en anglais (`stellar-app/src/i18n.rs`) : tout texte affiché passe
  par `tr("…")` ou `tr!("… {} …", x)` (emplacements `{}` seulement) et reçoit
  sa traduction dans la table `EN` (un test vérifie doublons et
  emplacements). Nombres selon la langue (`format.rs` : `count`, `real`,
  `decimal_sep()` — jamais `.replace('.', ",")` en dur).
- Mettre à jour `README.md` (table des commandes, sections concernées) et
  `SPEC.md` à chaque fonctionnalité.

## Commandes

```powershell
cargo build --release                      # target\release\stellar.exe et stellar-cli.exe
cargo test --workspace                     # tests (les tests GPU s'ignorent sans carte)
cargo clippy --workspace --all-targets
cargo run --release --example bench -p stellar-engine-gpu   # banc d'essai CPU / GPU
```

Chaîne d'outils : Rust **1.89** au minimum (`rust-version` du `Cargo.toml`,
imposé par les dépendances verrouillées : `uuid`, `icu_*`). Les tests GPU
tournent aussi sans carte graphique, en rendu logiciel :
`WGPU_BACKEND=gl` (sous Linux : `xvfb-run` en plus).

Démarrage direct pour les essais (variables d'environnement, voir `app.rs`) :
`STELLAR_AUTOSTART=<n>` (simulation de n corps), `STELLAR_AUTODISK=1` (disque
en rotation), `STELLAR_ENGINE=<id>` (`cpu-barnes-hut` ou `gpu-barnes-hut`),
`STELLAR_AUTORECORD=<intervalle>`, `STELLAR_LOAD=<sauvegarde>`,
`STELLAR_REPLAY=<enregistrement>`, `STELLAR_LANG=fr|en`. Mode console :
`stellar-cli.exe --corps 2000 --pas 300`, `--charger`, `--reprendre` (voir
`console.rs`, `--help`). Deux exécutables sur une même bibliothèque
(`stellar_app`, `lib.rs`) : `stellar.exe` (fenêtré, sans argument ; avec
arguments, il renvoie vers stellar-cli) et `stellar-cli.exe` (console :
l'invite attend sa fin). Argument inconnu : code de sortie 2. Les dossiers `saves\` et `recordings\` sont
relatifs au répertoire courant. Mesures : `STELLAR_STATS=1` (une ligne par
seconde sur la sortie d'erreur : pas/s, durée du pas, images/s ; lancer
depuis un shell qui redirige `2>` pour la lire), `STELLAR_PRESENT=fifo|mailbox|immediate`
(mode de présentation imposé). Le banc d'essai accepte `disque` (disque en
rotation) et `STELLAR_GPU_PROFILE=1` (temps par noyau).

## Architecture (clean architecture)

| Crate | Rôle |
|---|---|
| `stellar-domain` | Corps (`BodySet`, structure de tableaux), état, paramètres, règles physiques (dont le nettoyage des éjectés et la détection des amas, `ejection.rs`), traits `PhysicsEngine` / `EngineFactory`. Aucune dépendance d'infrastructure. |
| `stellar-application` | Génération des conditions initiales, thread de simulation (`SimulationHandle`, instantanés publiés), moteur composite des sous-simulations (`MultiSystem`, `systems.rs`), registre des moteurs, enregistrement (`Recorder`), relecture, ports, façade `SimulatorService`. |
| `stellar-engine-cpu` | Barnes-Hut sur octree de Morton (parcours par groupes de 32 corps, évaluation AVX2), Leapfrog KDK à pas fixe, contacts balayés (grille hiérarchique + union-find), fusions, nettoyage. |
| `stellar-engine-gpu` | Même physique en shaders WGSL, simple précision : tri radix, LBVH de Karras (topologie refaite tous les 4 pas, réajustée entre-temps), parcours de Barnes-Hut par groupes de 128 corps, paires de contact sur GPU, union-find des fusions sur CPU. |
| `stellar-persistence` | Fichiers `.stellar` (sauvegardes) et `.stellarec` (enregistrements). |
| `stellar-app` | Écrans egui (`home.rs`, `sim.rs`), caméra (`camera.rs`), sélection (`picking.rs`), rendu des corps (`render/`). Racine de composition (`main.rs`). |

Règles :

- Le reste de l'application ne connaît les moteurs que par les traits du
  domaine ; un nouveau moteur s'ajoute par une fabrique enregistrée dans
  `main.rs`. `EngineFactory::create` renvoie une erreur lisible plutôt que de
  paniquer (carte absente, mémoire insuffisante).
- Les dépendances vont vers le domaine : pas d'egui ni de wgpu dans
  `stellar-domain` / `stellar-application`.
- Moteur GPU par défaut si la sonde réussit, repli sur le CPU au chargement
  d'une sauvegarde sinon.

## Points délicats

- **Affichage direct (moteur GPU)** : le moteur calcule sur la carte de
  l'interface et le rendu lit ses tampons sans copie (`PhysicsEngine::live_view`).
  L'état complet (`Snapshot::state`, partagé en `Arc`) n'est relu que toutes les
  une à deux secondes : il peut être **en retard** sur `Snapshot::step`. Toute
  donnée tirée de `snapshot.state` doit rester cohérente avec `state.step`,
  pas avec `snapshot.step`. Sous OpenGL, le moteur ouvre sa propre carte et
  l'affichage repasse par des copies.
- **Présentation et verrou de la carte** : dans wgpu 24,
  `Surface::get_current_texture` garde le verrou `device.fence` pendant
  l'attente de l'écran, et `queue.submit` le réclame. En mode Fifo, chaque
  soumission du moteur GPU attendait donc jusqu'à une image (~25 ms par pas
  quelle que soit la taille). D'où le mode Mailbox (`present_mode` dans
  `main.rs`, `STELLAR_PRESENT` pour l'imposer). Sous DirectX 12, la
  compilation des shaders par FXC prend ~100 s : rester sous Vulkan.
- **Pas du moteur GPU** : une soumission principale, une attente (tampon de
  relecture : compteurs, moments, positions suivies, premières paires), puis
  les fusions sans attente. `positions_of` suit jusqu'à 4 listes
  d'identifiants (table identifiant → indice sur le GPU) ; une liste nouvelle
  est relue tout de suite puis à chaque pas. Les accesseurs ne doivent pas
  relire la carte. Profilage : `STELLAR_GPU_PROFILE=1` (banc d'essai).
- **Limites GPU** : `MAX_BODIES = 65 535 × 256`, `MAX_PAIRS = 2¹⁸` paires de
  contact par pas. Les tampons d'instances du rendu sont découpés en tronçons
  (4 M d'instances au plus) pour respecter `max_buffer_size` (plantage passé à
  10 M de corps). Corps GPU de 48 octets (`GpuBody`, `common.wgsl` : position
  de début de pas = `pos − vel·dt`). La pile et la liste du parcours de
  Barnes-Hut sont dimensionnées selon `max_compute_workgroup_storage_size`
  (préfixe du module `forces`).
- **Précision** : positions en `f64` côté CPU ; le rendu travaille relativement
  à une ancre (`anchor`) pour rester précis en `f32`. Le moteur GPU est en
  simple précision (erreur médiane sur l'accélération ~10⁻³ à θ = 0,5).
- **Sous-simulations** : le thread de simulation enveloppe toujours le
  moteur dans `MultiSystem` (un seul sous-système le plus souvent, origine
  nulle). Chaque sous-système a son moteur et son repère ; `state()` renvoie
  les corps en coordonnées globales, rangés par sous-système
  (`SimulationState::systems`). Affichage direct : `SystemsView` (un
  `BodiesView` et une origine par sous-système). Traits du domaine :
  `set_external_field` (champ de la fin du pas ; le premier demi-kick
  reprend celui du pas précédent), `insert`, `moments`, `systems`,
  `StepReport::removed_bodies`. Les opérations de structure (séparation,
  réunion, recentrage) reconstruisent les moteurs depuis leur état.
- **Fichiers de l'utilisateur** : les dépôts de l'application suppriment
  vers la corbeille (`with_trash`, `stellar-persistence/src/files.rs`) ; les
  tests suppriment définitivement dans un dossier temporaire. Le nom d'une
  sauvegarde est celui de son fichier (l'en-tête garde le nom d'origine).
  Nom d'une simulation : `SimulationHandle::name`, tiré du fichier au
  chargement (`name_from_file`).
- **Polices egui** : certains caractères manquent (●, ✦, U+202F) ; vérifier
  qu'un symbole s'affiche avant de l'utiliser.
- **Caméra** : orientation par quaternion (repère local : X avant, −Y droite,
  Z haut), rotations dans le repère de l'écran, roulis libre. Modes `Free`,
  `Orbit { target, distance }`, `Barycenter { distance }`. La vitesse en vol
  libre est proportionnelle à la distance à la sélection, sinon au barycentre.

## Formats de fichiers

- **`.stellar`** (version 6 ; versions 5, 4 et 3 relues) : signature, version, en-tête
  `SaveSummary` lisible sans décompression, puis `SaveData` en bincode
  compressé zstd. bincode n'est pas auto-descriptif : **tout ajout de champ
  impose une nouvelle version** et une copie de l'ancienne forme dans
  `stellar-persistence/src/legacy.rs`, avec un test de relecture.
  `PhysicsParams` figure aussi dans l'en-tête des enregistrements : le
  modifier change les deux formats.
  - Version 4 : seuils des amas éjectés (`cluster_link_length`,
    `cluster_min_bodies`, `cluster_min_mass`).
  - Version 5 : sous-systèmes dans l'état (`SystemSpan` : plage, taille).
  - Version 6 : facteur de la vitesse orbitale
    (`VelocityMode::CircularOrbital { spread, factor }`) ; les conditions
    initiales figurent aussi dans l'en-tête des enregistrements (version 4).
- **`.stellarec`** (version 4 ; versions 3, 2 et 1 relues — en-têtes des
  versions 1 et 2-3, instantanés sans sous-systèmes avant la version 3 ; un
  enregistrement repris garde la version de son fichier) : instantanés tous
  les N pas, une boîte de quantification par sous-système (positions 21 bits, vitesses
  16 bits, masses, devenir des corps disparus), zstd, index en fin de fichier
  reconstructible si le fichier n'a pas été clos. Budget de poussière
  optionnel (sélection stable par identifiant). Relecture par interpolation
  d'Hermite.
- **Reprise d'enregistrement** : suspension automatique à la sortie (sauvegarde
  `<enregistrement>_reprise` liée) ; poursuite depuis une sauvegarde F5 faite
  pendant l'enregistrement ; poursuite manuelle depuis n'importe quelle
  sauvegarde compatible (bouton de l'accueil). Les instantanés postérieurs au
  pas de la sauvegarde sont supprimés.

## Tests et vérification

- Ajouter des tests unitaires ou d'intégration pour toute logique nouvelle
  (`crates/*/tests/`), notamment les formats de fichiers et la physique.
- Vérifier visuellement les changements d'interface en lançant l'application.
- Les fichiers de l'utilisateur dans `saves\` et `recordings\` peuvent peser
  plusieurs dizaines de gigaoctets : ne jamais les modifier pour un essai.

## Git

- Messages de commit en français : un titre, puis une liste des changements.
- Fins de ligne : LF dans le dépôt (`.gitattributes`, `eol=lf`). Les fichiers
  peuvent être en CRLF dans la copie de travail sous Windows, Git les
  normalise ; ne jamais valider une conversion de fins de ligne mêlée à un
  vrai changement.
- Ne valider ou pousser que sur demande.
- `*.exe`, `*.stellar`, `*.stellarec` sont ignorés.

## En attente

- Pistes de performance restantes (RTX 4060 Ti, 1 M de corps : ~14 ms par
  pas, dont ~9 ms pour les forces) :
  - tri radix (8 passes de 4 bits, ~1,5 ms à 1 M) : chiffres plus larges ;
  - un pas = une soumission longue (60 ms à 4 M) : les images de l'interface
    attendent derrière ; découper le pas en plusieurs soumissions rendrait
    la caméra plus fluide aux très grands nombres de corps ;
  - effondrement massif (des centaines de milliers de fusions en quelques
    pas) : jusqu'à ~150 ms par pas pendant quelques pas (paires de contact
    quadratiques dans le cœur dense, plafonnées à 2¹⁸ ; planification des
    fusions sur le CPU, relecture des paires) ;
  - préparer l'encodage du pas suivant pendant le calcul (~0,3 à 0,5 ms par
    pas, sensible aux petits nombres de corps) ;
  - sous-simulations : chaque petite sous-simulation sur GPU coûte ~0,6 ms
    par pas (soumission et attente propres) ; les calculer sur le CPU en
    deçà de quelques milliers de corps, ou regrouper leurs soumissions ;
    les évadés d'une sous-simulation sont insérés sans accélération propre
    pour leur premier demi-kick.
