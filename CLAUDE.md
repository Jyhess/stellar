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
  commentaires, documentation, messages de commit. Nombres à la française
  (`format.rs` : virgule décimale, espace insécable comme séparateur de milliers).
- Mettre à jour `README.md` (table des commandes, sections concernées) et
  `SPEC.md` à chaque fonctionnalité.

## Commandes

```powershell
cargo build --release                      # exécutable : target\release\stellar.exe
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
`STELLAR_REPLAY=<enregistrement>`. Les dossiers `saves\` et `recordings\` sont
relatifs au répertoire courant.

## Architecture (clean architecture)

| Crate | Rôle |
|---|---|
| `stellar-domain` | Corps (`BodySet`, structure de tableaux), état, paramètres, règles physiques, traits `PhysicsEngine` / `EngineFactory`. Aucune dépendance d'infrastructure. |
| `stellar-application` | Génération des conditions initiales, thread de simulation (`SimulationHandle`, instantanés publiés), registre des moteurs, enregistrement (`Recorder`), relecture, ports, façade `SimulatorService`. |
| `stellar-engine-cpu` | Barnes-Hut sur octree de Morton, Leapfrog KDK à pas fixe, contacts balayés (grille hiérarchique + union-find), fusions, nettoyage. |
| `stellar-engine-gpu` | Même physique en shaders WGSL, simple précision : tri radix, LBVH de Karras, parcours de Barnes-Hut, paires de contact sur GPU, union-find des fusions sur CPU. |
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
- **Limites GPU** : `MAX_BODIES = 65 535 × 256`, `MAX_PAIRS = 2¹⁸` paires de
  contact par pas. Les tampons d'instances du rendu sont découpés en tronçons
  (4 M d'instances au plus) pour respecter `max_buffer_size` (plantage passé à
  10 M de corps).
- **Précision** : positions en `f64` côté CPU ; le rendu travaille relativement
  à une ancre (`anchor`) pour rester précis en `f32`. Le moteur GPU est en
  simple précision (écart d'énergie avec le CPU ~10⁻⁵ sur les essais).
- **Polices egui** : certains caractères manquent (●, ✦, U+202F) ; vérifier
  qu'un symbole s'affiche avant de l'utiliser.
- **Caméra** : orientation par quaternion (repère local : X avant, −Y droite,
  Z haut), rotations dans le repère de l'écran, roulis libre. Modes `Free`,
  `Orbit { target, distance }`, `Barycenter { distance }`. La vitesse en vol
  libre est proportionnelle à la distance à la sélection, sinon au barycentre.

## Formats de fichiers

- **`.stellar`** (version 3 ; versions 1 et 2 toujours lues) : signature,
  version, en-tête `SaveSummary` lisible sans décompression, puis `SaveData`
  en bincode compressé zstd. bincode n'est pas auto-descriptif : **tout ajout
  de champ impose une nouvelle version** et un module de relecture des
  anciennes formes (voir `legacy_view`, `v1`, `v2` dans
  `stellar-persistence/src/lib.rs`), avec un test de relecture.
  - Version 2 : `RecordingLink` (reprise exacte d'un enregistrement suspendu),
    masse initiale.
  - Version 3 : roulis de la caméra, suivi du barycentre (`ViewState`),
    `continue_recording` (enregistrement en cours lors d'une sauvegarde F5).
- **`.stellarec`** : instantanés tous les N pas (positions 21 bits, vitesses
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

- Mesurer les performances réelles du moteur GPU sur la RTX 4060 Ti (banc
  d'essai), et vérifier l'affichage direct sous DirectX 12 / Vulkan.
- Essayer la poursuite d'un enregistrement sur les fichiers réels :
  `saves\sim_20261027_191427.stellar` (format 1) avec
  `recordings\rec_20261027_001645.stellarec` (≈ 27 Go, peut-être non clos).
- Avertissements clippy préexistants (`is_multiple_of`, `is_none_or`…) : la
  version minimale de Rust (1.89) permet désormais d'adopter ces suggestions.
- Pistes de performance du moteur GPU, après l'affichage direct (le banc
  occupe la carte à 100 %, l'application doit s'en approcher) :
  - `step_inner` relit le compteur de paires de contact au milieu de chaque
    pas (`read_u32`), ce qui vide la file : appliquer les fusions avec un pas
    de décalage permettrait d'enchaîner les pas sans attente ;
  - chaque lancement crée son bind group, et `pass_uniform` un tampon
    uniforme (≈ 150 lancements par pas) : bind groups précalculés (deux jeux,
    un par tampon de corps), décalages dynamiques, une seule passe de calcul
    par pas ;
  - `positions_of` traite les identifiants par paquets de 15, chacun relançant
    un passage sur tous les corps puis une relecture bloquante : coûteux à
    chaque pas quand des traces sont actives ; un seul lancement pour tous ;
  - `forces.wgsl` relit l'enregistrement complet de 64 octets d'un corps aux
    feuilles alors que position et masse suffiraient (tableau compact `vec4`) ;
  - le coût du dessin des points (plusieurs millions à chaque image) partage
    la carte avec le calcul.
