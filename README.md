# Stellar

Simulateur gravitationnel N-corps en 3D. Un nuage de particules de masse 1 s'effondre sous l'effet de la gravité, et deux corps qui se touchent fusionnent. Le calcul se fait **sur la carte graphique** (moteur GPU par défaut : shaders de calcul wgpu) ou **sur le CPU** (Barnes-Hut multithread) ; l'affichage passe par wgpu dans les deux cas.

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

La première compilation prend quelques minutes, le temps de télécharger et de compiler les dépendances. Les suivantes sont rapides. Les exécutables produits se trouvent dans `target\release\` : `stellar.exe` (l'interface) et `stellar-cli.exe` (la ligne de commande, voir « Simulation sans affichage »).

> Lancez toujours en `--release` : en mode debug, le calcul est plusieurs dizaines de fois plus lent.

L'affichage est plafonné à 120 images par seconde. Sur les cartes NVIDIA et AMD (Vulkan) ou sous DirectX 12, il est synchronisé en mode « Mailbox » : sans déchirement, et sans que l'attente de l'écran ne bloque le moteur GPU, qui partage la carte (voir « Moteur GPU »).

Comptez environ 420 octets de mémoire vive par particule : à peu près 4,2 Go pour 10 millions.

En cas d'erreur fatale, une boîte de dialogue s'affiche et le détail est consigné dans `stellar-crash.log`, dans le répertoire de lancement.

Les sauvegardes sont écrites dans le sous-dossier `saves\` du répertoire de lancement.

---

## 2. Utilisation

### Écran d'accueil

- **Nouvelle simulation** : nom, nombre de particules, rayon du nuage, graine aléatoire, répartition (sphère ou disque, avec épaisseur et profil de densité), vitesse initiale (repos, aléatoire, circulaire imposée, circulaire orbitale × facteur ± dispersion — choix par défaut ; facteur de 0,5 à 1,5 : 1 pour des orbites circulaires, moins pour un effondrement, plus pour une expansion —, radiale). Chaque paramètre porte une pastille « ? » qui explique son rôle.
  - **Nom** : celui des sauvegardes (F5) et des enregistrements de la simulation. Par défaut, date, nombre de particules abrégé, répartition et vitesse initiale (`2026-10-28_1M_sph_orb`), avec l'heure si ce nom existe déjà ; il suit les paramètres tant qu'il n'est pas saisi (bouton « Auto » pour y revenir). Une simulation chargée reprend le nom de sa sauvegarde (sans « _reprise » ni numéro).
  - **Nombre de particules** : curseur à crans 1-2-5 (1 000, 2 000, 5 000, 10 000…) jusqu'au maximum de la machine (limite du moteur, 80 % de la mémoire centrale, un milliard au plus), et champ de saisie exacte.
  - Sous le formulaire : mémoire estimée et **durée estimée du premier pas** (d'après le banc d'essai).
- **Paramètres avancés** : G, dt, adoucissement ε, rayon r₀ d'une particule de masse 1, précision θ, période et distance du nettoyage, seuils des amas éjectés (distance de liaison, nombre de corps, part de la masse), moteur de calcul. Par défaut, dt, ε, r₀ et la liaison sont déduits des conditions initiales :
  - dt = temps de chute libre / 500 ;
  - ε = 0,3 × distance moyenne entre voisins ;
  - r₀ = 1 % de cette distance ;
  - liaison des amas = distance moyenne entre voisins ; un amas est conservé à partir de 10 corps ou de 0,1 % de la masse initiale.
- **Simulations sauvegardées** : la liste du dossier `saves\`, avec un bouton « Parcourir… » pour ouvrir un fichier situé ailleurs. Une simulation chargée redémarre en pause.
- **Renommer, supprimer** (sauvegardes et enregistrements) : la suppression demande confirmation et envoie le fichier à la corbeille de Windows, avec en option le fichier lié (l'enregistrement d'une sauvegarde, les sauvegardes d'un enregistrement). Le renommage change aussi le fichier ; une sauvegarde et son enregistrement liés sont renommés ensemble et leurs liens mis à jour.
- **Langue** : français ou anglais (liste en bas de l'accueil, ou menu Échap) ; par défaut celle de Windows, le choix est mémorisé dans `%APPDATA%\Stellar\langue`. Les nombres suivent la langue (virgule décimale et espace des milliers en français, point et virgule en anglais).

### Simulation sans affichage

Pour les longs calculs : aucun rendu des corps, toute la carte graphique pour le calcul, enregistrement automatique (intervalle et budget de poussière réglables).

- **Depuis l'accueil** : case « Sans affichage », puis condition d'arrêt — nombre de pas, temps simulé, durée réelle, ou manuel. Un écran de suivi montre l'avancement (pas, pas/s, temps restant estimé, enregistrement) ; « Arrêter et sauvegarder » clôt l'enregistrement et sauvegarde l'état final sous le nom de la simulation, « Afficher la simulation » bascule vers l'affichage habituel (le calcul et l'enregistrement continuent). Fermer la fenêtre arrête proprement.
- **En ligne de commande** : `stellar-cli.exe`, application console installée à côté de `stellar.exe` (l'invite attend sa fin, Ctrl+C l'arrête proprement). `stellar-cli --help` liste les options ; un argument inconnu est signalé avec un code de sortie 2. `stellar.exe` lancé avec des arguments renvoie vers `stellar-cli`.

  ```powershell
  .\stellar-cli.exe --corps 4000000 --disque --pas 200000 --intervalle 20 --nom galaxie
  .\stellar-cli.exe --charger saves\galaxie.stellar --pas 100000
  .\stellar-cli.exe --reprendre recordings\galaxie.stellarec --duree 120
  ```

  - **Nouvelle simulation** : `--corps`, `--rayon`, `--graine`, `--disque`, `--vitesse repos|alea|circ|orb|rad`, `--dispersion`, `--facteur` (multiple de la vitesse orbitale), `--vmin`/`--vmax`, `--moteur gpu|cpu`.
  - **Reprise d'une sauvegarde** : `--charger FICHIER` ; l'enregistrement lié à la sauvegarde est poursuivi, sinon un nouvel enregistrement commence ; `--poursuivre FICHIER` prolonge à la place un enregistrement compatible à partir du pas de la sauvegarde (instantanés postérieurs supprimés).
  - **Reprise d'un enregistrement** : `--reprendre FICHIER` charge la dernière sauvegarde liée à l'enregistrement (sauvegarde de reprise ou F5 faite pendant l'enregistrement) et le poursuit.
  - **Dans tous les cas** : `--nom`, `--intervalle`, `--poussiere N|toutes`, et une cible `--pas N`, `--temps T` (comptés à partir du pas de la sauvegarde en reprise) ou `--duree minutes` ; sans cible, jusqu'à Ctrl+C. L'avancement s'affiche toutes les cinq secondes ; à l'arrêt, l'enregistrement est clos et l'état final sauvegardé.

### Commandes

| Touche | Action |
|---|---|
| Z / S | avancer / reculer (en suivi : rapprocher / éloigner de la cible) |
| Q / D | gauche / droite (sans effet en suivi) |
| A / E | basculer la vue autour de l'axe de visée |
| H | redresser progressivement l'horizon : le haut de l'écran revient sur la normale au disque |
| Maj / Ctrl | déplacement ×5 / ×0,2 |
| + / − | multiplicateur de vitesse |
| Souris | orientation de la vue, dans le repère de l'écran (en suivi : tourner autour de la cible) |
| Molette | avancer / reculer comme Z / S (en suivi : distance à la cible ; Maj accélère) |
| Ctrl + molette | zoom optique |
| F | suivre la sélection (corps ou barycentre) / cesser le suivi |
| G | grille du plan du disque |
| Clic gauche | suivre le corps visé (réticule, ou pointeur quand la souris est libre) |
| Origine | aller au corps le plus massif |
| PgDn / PgUp | corps suivant / précédent dans l'ordre des masses |
| B | sélectionner le barycentre et le regarder de face au disque (selon l'axe du moment cinétique), en conservant la distance |
| C | viser le centre de gravité (vol libre) |
| T | mode trace : trajectoires des 10 corps les plus massifs visibles à l'écran à cet instant / effacer |
| R | traces : référentiel suivant — corps sélectionné, barycentre de la simulation, ou barycentre de l’amas lié du corps sélectionné |
| V | vue d'ensemble : recul jusqu'à voir 90 % de la masse, dans la direction de visée actuelle |
| Espace | pause / reprise |
| P | pas à pas |
| Tab | libérer / capturer la souris |
| Clic droit | reprendre la visée quand la souris est libre |
| F5 | sauvegarder |
| Échap | menu (reprendre, sauvegarder, accueil, quitter) |

Quand un corps (ou le barycentre) est suivi, il reste au centre de l'écran : la souris fait tourner la caméra autour de lui, Z / S règlent la distance et A / E basculent la vue. Si ce corps fusionne, le suivi passe au corps qui en résulte. F rend la main en vol libre sans rien changer à la vue. L'orientation est libre dans toutes les directions (on peut passer par-dessus un objet) ; H ramène l'horizon sur le plan du disque. La vitesse est proportionnelle à la distance au corps sélectionné, ou au barycentre si rien n'est sélectionné : lente près de la cible, rapide au loin. À l'écran, un trièdre (coin inférieur gauche) montre les axes X, Y, Z et la normale au disque N, et une flèche au bord de l'écran indique la direction et la distance du barycentre et du corps sélectionné quand ils sont hors champ. En mode trace, chaque trace a sa couleur, rappelée dans le panneau « Corps ». Elle reçoit un point à chaque pas de calcul et se poursuit sur le corps issu d'une fusion. Le panneau propose deux affichages :

- l'**historique** complet depuis l'activation (décimé au-delà de 4 096 points) ;
- une **queue** des N derniers pas, de 10 à 5 000, qui s'estompe vers le passé.

Les traces sont toujours vues dans un **référentiel** (touche R pour passer au suivant) : chaque point devient p(t) − r(t) + r(maintenant), où r est la trajectoire du corps sélectionné, du barycentre de la simulation, ou du barycentre de l'**amas local** du corps sélectionné (groupe lié qui le contient, même règle que le nettoyage, redésigné toutes les une à deux secondes ; ses 512 membres les plus massifs sont suivis à chaque pas). Sans corps sélectionné, le barycentre sert de référentiel. La trajectoire du barycentre est enregistrée dès l'activation des traces, ce qui permet de relire tout l'historique dans son référentiel. La légende donne, pour chaque trace, la masse du corps et sa part de la masse présente. Le classement des plus massifs ajoute une ligne pour le corps sélectionné quand il n'en fait pas partie. La référence reste ainsi fixe à l'écran et les orbites autour d'elle apparaissent. Sa trajectoire vient de sa propre trace si elle fait partie des corps tracés, sinon d'une piste enregistrée depuis sa sélection. Dans ce second cas, la partie plus ancienne des traces n'est pas affichée.

Les traces ne sont pas sauvegardées.

Le panneau « Simulation » indique la masse présente et la masse éjectée, c'est-à-dire sortie de la simulation par le nettoyage, avec sa part de la masse initiale. Le classement des corps les plus massifs donne la part de chacun dans la masse présente.

Le panneau « Corps » liste aussi les **sous-simulations** et les **amas éjectés** que le nettoyage a conservés (voir plus bas) : corps repère (le plus massif), nombre de corps, masse et distance au barycentre. Un clic sur une sous-simulation sélectionne son **barycentre** et y centre la caméra, à une distance qui la montre en entier (B la montre de face ; le panneau donne ses corps, sa masse, sa vitesse, sa taille). Si elle se réunit à une autre, la caméra suit l'ensemble réuni ; réunie à la simulation principale sans autre sous-simulation, elle revient au barycentre global. Un clic sur un amas éjecté sélectionne son corps repère. La liste des amas est mise à jour à chaque nettoyage ; ces listes n'existent pas en relecture.

En passant d'un corps à un autre (clic, Origine, PgDn/PgUp, classement), la direction de visée, le zoom et la distance à la cible sont conservés.

---

### Enregistrement et relecture

Une simulation lourde peut être enregistrée puis rejouée de façon fluide, à 60 images par seconde, avec une navigation entièrement libre.

- **Enregistrer :**
  - **F9**, ou le panneau « Simulation », démarre ou arrête l'enregistrement à tout moment ;
  - à l'accueil, la case « Enregistrer dès le départ » lance l'enregistrement avec la simulation ;
  - un instantané (positions, vitesses, masses, fusions) est écrit tous les N pas, réglable, dans le dossier `recordings\` (fichiers `.stellarec`) ;
  - l'écriture se fait dans un thread dédié, ce qui ne ralentit presque pas le calcul.
- **Budget de poussière (optionnel) :**
  - au plus B particules de masse 1 par instantané (1 million par défaut), choisies d'après leur identifiant : ce sont toujours les mêmes, sans clignotement ;
  - les corps plus massifs sont toujours tous enregistrés ;
  - chaque particule gardée est éclairée en proportion de celles qu'elle représente, et l'exclusion disparaît d'elle-même quand le nombre de corps passe sous le budget ;
  - un instantané pèse environ 12 octets par corps enregistré, soit environ 12 Mo pour un million.
- **Rejouer :**
  - depuis la liste « Enregistrements » de l'accueil ;
  - entre deux instantanés, chaque corps suit une interpolation cubique d'Hermite sur ses positions et vitesses, et un corps absorbé rejoint celui qui l'absorbe ;
  - commandes : Espace (lecture / pause), ← / → (instantané précédent / suivant), ↑ / ↓ (vitesse), ⌫ (sens de lecture), barre de temps ;
  - la caméra, la sélection, les traces (T, R) et la vue d'ensemble fonctionnent comme en direct.
- **Suspendre et reprendre :**
  - si l'on quitte le programme ou revient à l'accueil pendant un enregistrement, celui-ci est clos proprement ;
  - la simulation est alors sauvegardée automatiquement (« <enregistrement>_reprise ») et liée à l'enregistrement ;
  - recharger cette sauvegarde, ou cliquer « Reprendre » dans la liste des enregistrements, poursuit l'enregistrement dans le même fichier dès que le calcul est relancé, sans trou ni doublon ;
  - recharger une sauvegarde de reprise plus ancienne tronque l'enregistrement à ce point : les instantanés postérieurs sont supprimés.
- **Sauvegarde F5 pendant un enregistrement :** la sauvegarde mémorise l'enregistrement en cours ; la recharger le poursuit au pas de la sauvegarde, en supprimant les instantanés postérieurs.
- **Poursuivre un enregistrement depuis n'importe quelle sauvegarde :**
  - le bouton « Poursuivre un enregistrement… » d'une sauvegarde liste les enregistrements compatibles (mêmes conditions initiales, au moins un instantané antérieur à la sauvegarde) et le nombre d'instantanés qui seront supprimés ;
  - un instantané est écrit aussitôt au pas de la sauvegarde, puis l'enregistrement continue à son intervalle d'origine ;
  - les corps disparus entre le dernier instantané conservé et la sauvegarde reçoivent leur devenir réel s'il figure dans l'instantané suivant de l'ancien enregistrement, sinon ils sont marqués « éliminés » ;
  - un enregistrement d'une autre simulation est refusé (corps massif de la sauvegarde absent de l'enregistrement).
- **Après un plantage :** un enregistrement interrompu reste lisible, car son index est reconstruit à l'ouverture.

### Moteur GPU

Si une carte graphique compatible (DirectX 12 ou Vulkan) est détectée, le moteur **GPU** devient le moteur par défaut. Le moteur CPU reste disponible dans « Paramètres avancés → Moteur de calcul », où la carte détectée est indiquée.

- **Même algorithme que le moteur CPU :** Barnes-Hut, Leapfrog, fusions balayées, nettoyage.
- **Simple précision :** l'erreur relative médiane sur l'accélération, comparée à la somme directe, est d'environ 10⁻³ (θ = 0,5) ; après 50 pas, l'écart avec le moteur CPU reste inférieur à 10⁻³ en position.
- **Mémoire vidéo :** environ 240 octets par corps, soit environ 2,4 Go pour 10 millions. Le maximum est de 16,7 millions de corps.
- **Performances** (RTX 4060 Ti, nuage sphérique au repos) : environ 14 ms par pas pour 1 million de corps, 62 ms pour 4 millions.
- **Repli automatique :** une sauvegarde faite avec le moteur GPU se recharge avec le moteur CPU si la carte est absente.
- **Affichage direct :** le moteur calcule sur la carte graphique de l'interface, qui dessine les corps directement depuis ses tampons, sans copie. L'état complet n'est relu qu'une fois toutes les une à deux secondes (pointage, classement, barycentre, sauvegarde) ; le pas, les énergies, le centre de masse et la position du corps suivi sont publiés à jour à chaque instantané. Si la carte de l'interface ne convient pas au calcul (OpenGL), le moteur ouvre sa propre carte et l'affichage repasse par les copies.
- **Banc d'essai** (nombre de corps, nombre de pas ; `gpu` ou `cpu` pour n'essayer qu'un moteur, `disque` pour un disque en rotation) :

```powershell
cargo run --release -p stellar-engine-gpu --example bench -- 1000000 10
```

Avec `$env:STELLAR_GPU_PROFILE=1`, le banc affiche le temps passé dans chaque noyau GPU (horodatage des passes).

Ce qui rend le moteur GPU rapide :

1. **Parcours de Barnes-Hut par groupes** : les 128 corps consécutifs d'un groupe de travail, voisins dans l'ordre de Morton, descendent l'arbre ensemble. Un nœud est assimilé à un point s'il est assez loin de la boîte englobante du groupe entier, critère plus strict que pour chaque corps isolément. Nœuds acceptés et corps des feuilles proches s'accumulent dans une liste en mémoire partagée, que chaque fil évalue ensuite pour son corps, en boucle déroulée.
2. **Arbre réajusté** : la topologie (codes de Morton, tri radix, construction de Karras) n'est refaite que tous les quatre pas, ou après une compaction. Entre-temps, seuls les centres de masse, boîtes et rayons d'ouverture sont recalculés : le critère reste exact.
3. **Une soumission et une attente par pas** : paires de contact, corps éliminés, moments globaux et positions des corps suivis (traces, caméra) sont recopiés ensemble dans un tampon de relecture. Les fusions, regroupées sur le CPU, sont appliquées dans une seconde soumission, sans attente.
4. **Lancements regroupés** : une seule passe de calcul par soumission, groupes de liaisons et paramètres de passe créés une fois pour toutes.
5. **Corps de 48 octets** : la position de début de pas, utile aux contacts balayés, se déduit de la vitesse (`pos − vel·dt`).

## 3. Architecture

Le projet suit la **clean architecture**. Les dépendances pointent toutes vers le domaine :

```
stellar-app (présentation + racine de composition : egui, wgpu)
   │
   ├──► stellar-persistence (adaptateur : fichiers .stellar, .stellarec)
   ├──► stellar-engine-cpu  (adaptateur : moteur Barnes-Hut CPU)
   ├──► stellar-engine-gpu  (adaptateur : moteur Barnes-Hut GPU, wgpu)
   │
   └──► stellar-application (cas d'usage, ports)
              │
              └──► stellar-domain (entités, règles, trait PhysicsEngine)
```

| Crate | Rôle |
|---|---|
| `stellar-domain` | Corps (`BodySet`, en structure de tableaux), état, paramètres, règles physiques (rayon, fusion, distance balayée), classement par masse, **traits `PhysicsEngine` / `EngineFactory`**. Aucune infrastructure. |
| `stellar-application` | Génération des conditions initiales, thread de simulation découplé (`SimulationHandle`, instantanés publiés à ≤ 60 Hz ; copie complète de l'état espacée quand le moteur est affiché directement), registre des moteurs, ports `SimulationRepository`, façade `SimulatorService`. |
| `stellar-engine-cpu` | Moteur de calcul CPU. Optimisé librement, comme prévu par la spécification (voir ci-dessous). |
| `stellar-engine-gpu` | Moteur de calcul GPU (shaders de calcul wgpu) : tri radix, arbre LBVH de Karras agrégé niveau par niveau et réajusté entre deux constructions, parcours de Barnes-Hut par groupes de 128 corps, contacts balayés ; les fusions sont regroupées sur le CPU puis appliquées sur le GPU. Sur la carte de l'interface, expose ses tampons de corps (`BodiesView`) pour l'affichage. |
| `stellar-persistence` | Format `.stellar` : signature, version, en-tête lisible sans décompression, puis données bincode compressées zstd. Versions lues : 6 (courante, facteur de la vitesse orbitale), 5 (sous-systèmes), 4 et 3 ; enregistrements `.stellarec` : 4 (facteur dans l'en-tête), 3 (une boîte de quantification par sous-système), 2 et 1 (`legacy.rs`). Un enregistrement repris garde la version de son fichier. |
| `stellar-app` | Écrans egui, caméra, sélection, rendu GPU des corps (billboards additifs, positions relatives à une ancre pour garder la précision en `f32`), lus directement dans les tampons du moteur GPU ou téléversés depuis les instantanés. |

### Moteur CPU

À chaque pas :

1. **Kick ½ dt** : demi-mise à jour des vitesses.
2. **Drift dt** : mise à jour des positions ; les positions de départ sont conservées.
3. **Contacts balayés** :
   - grille hiérarchique sur les boîtes englobantes des trajectoires ; les corps très rapides ou très gros sont rangés aux niveaux supérieurs ;
   - cellules voisines trouvées sans table de hachage : les clés de cellule, triées, sont parcourues avec un curseur par direction ;
   - distance minimale exacte entre trajectoires rectilignes ;
   - fusions en chaîne regroupées par union-find.
4. **Fusion** : la masse et la quantité de mouvement sont conservées et la position passe au barycentre. L'identifiant du membre le plus massif est conservé.
5. **Forces** :
   - réordonnancement des corps selon leur code de Morton ;
   - octree construit en parallèle, stocké en ordre préfixe et parcouru sans pile ;
   - critère d'ouverture `d > s/θ + δ` et adoucissement de Plummer ;
   - **parcours par groupes** de 32 corps voisins au plus : l'arbre est descendu une fois par groupe, avec la boîte du groupe pour le critère, puis la liste d'interactions est évaluée pour chaque corps (AVX2 et FMA quand le processeur les a : quatre corps par instruction, inverse de la racine par estimation affinée de Newton).
6. **Kick ½ dt**.
7. **Nettoyage, tous les N pas** : suppression des corps **non liés** (énergie spécifique > 0 relativement au barycentre) **et** situés au-delà de k × R, **sauf les amas éjectés**. Règle commune aux deux moteurs (`stellar-domain/src/ejection.rs`) :
   - les corps lointains sont regroupés par « amis d'amis » (distance de liaison) ;
   - chaque groupe est épuré : les membres non liés au groupe (énergie dans son référentiel, sous son seul potentiel, exact jusqu'à 2 048 membres, à symétrie sphérique au-delà) en sont retirés, les plus énergiques d'abord ;
   - un groupe lié d'au moins n corps, ou d'au moins une part donnée de la masse initiale (un corps isolé très massif compte), est conservé en entier, même s'il s'éloigne indéfiniment.

   Le moteur GPU repère les corps lointains sur la carte et ne relit qu'eux, aux seuls pas de nettoyage.

### Sous-simulations

Un amas éjecté et détaché devient une **sous-simulation** : son propre moteur (même type que la simulation), son propre repère (origine à son barycentre, d'où une précision intacte même très loin), son propre arbre. Le moteur composite (`stellar-application/src/systems.rs`) les fait avancer au même pas ; le reste de l'application ne voit qu'un moteur.

- **Couplage** : chaque sous-système ressent les autres par un champ linéaire a₀ + T·(x − x_c) (accélération au barycentre et tenseur de marée de masses ponctuelles), calculé sur les barycentres extrapolés à la fin du pas ; le premier demi-kick reprend le champ du pas précédent (Leapfrog). Les marées étirent et arrachent donc bien les membres d'un amas ; deux amas s'attirent et peuvent orbiter l'un autour de l'autre.
- **Séparation** : au nettoyage, un amas éjecté (voir ci-dessus) devient un sous-système si (taille de l'amas + taille du système) < 0,3 × distance ; taille d'un amas : deux fois son rayon de demi-masse.
- **Réunion** : deux sous-systèmes sont réunis quand (somme des tailles) > 0,6 × distance (hystérésis) ; collisions et échanges de corps redeviennent alors possibles.
- **Évadés** : un sous-système secondaire nettoie son voisinage comme la simulation principale (k × sa taille) ; les corps qu'il élimine rejoignent le système principal (le plus massif), dont le nettoyage décide ensuite.
- **Recentrage** : un sous-système dont le barycentre s'écarte de son origine de plus de la moitié de sa taille est reconstruit autour de lui.
- **Affichage** : chaque sous-système est dessiné depuis son propre tampon, l'ancre de la caméra étant rapportée à son origine.
- **Fichiers** : l'état sauvegardé range les corps par sous-système (plages et tailles) ; chaque instantané d'un enregistrement a une boîte de quantification par sous-système.

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

Pour lancer directement une simulation sans passer par l'accueil : `$env:STELLAR_AUTOSTART=200000; cargo run --release`. Variables utiles aux essais :

| Variable | Effet |
|---|---|
| `STELLAR_AUTOSTART=<n>` | simulation de n corps dès le lancement |
| `STELLAR_AUTODISK=1` | disque en rotation plutôt que sphère au repos |
| `STELLAR_ENGINE=<id>` | moteur imposé (`cpu-barnes-hut`, `gpu-barnes-hut`) |
| `STELLAR_AUTORECORD=<intervalle>` | enregistrement immédiat |
| `STELLAR_LOAD=<sauvegarde>` / `STELLAR_REPLAY=<enregistrement>` | chargement ou relecture directs |
| `STELLAR_STATS=1` | une ligne par seconde sur la sortie d'erreur : pas, pas/s, durée du pas, images/s, corps |
| `STELLAR_PRESENT=fifo\|mailbox\|immediate` | mode de présentation de l'interface imposé |
| `STELLAR_LANG=fr\|en` | langue de l'interface imposée |

Les tests physiques (`crates/stellar-engine-cpu/tests/physics.rs`) vérifient notamment :

- la précision de Barnes-Hut par rapport à la somme directe ;
- la conservation de l'énergie sur une orbite à deux corps ;
- la conservation de la masse et de la quantité de mouvement lors des fusions ;
- la détection des croisements rapides ;
- les fusions en chaîne ;
- le critère d'éjection, la conservation des amas liés éjectés et l'élimination des petits groupes ;
- la relecture des sauvegardes version 3 et des enregistrements version 1.
