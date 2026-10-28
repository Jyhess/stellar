# Stellar — Spécification v1 (issue de la séance de questions)

## Cadre
- Simulateur N-corps **3D**, particules initiales de masse 1, fusion au contact.
- **Rust + wgpu + egui**, Windows uniquement (exécutable natif).
- Calcul sur le **CPU** (rayon) à l'origine ; un moteur **GPU** (shaders de calcul wgpu) est venu s'y ajouter et devient le moteur par défaut quand une carte compatible est présente (voir « Évolutions »).
- Cible : **1 million de particules et plus**.

## Architecture
- **Clean architecture** (domaine / cas d'usage / adaptateurs / infrastructure), sauf à l'intérieur du moteur de calcul, optimisé librement (SoA, SIMD, rayon).
- Moteur derrière une interface (`trait PhysicsEngine`) : un autre moteur (ex. GPU) pourra être ajouté et **sélectionné dans les paramètres**.
- Format de sauvegarde indépendant du moteur.
- Simulation dans un **thread dédié, découplée** de l'affichage (60 i/s sur le dernier état publié).

## Physique (moteur CPU v1)
- Forces : **Barnes-Hut** (octree), paramètre θ, adoucissement ε.
- Intégrateur : **Leapfrog (KDK), dt fixe**.
- Corps sphériques, **r = r₀ · m^(1/3)** ; r₀ réglable (défaut déduit de la densité du nuage).
- Contact : **détection balayée** (distance minimale sur le pas, grille spatiale) ; fusions en chaîne via union-find.
- Fusion : masses additionnées, position au barycentre, quantité de mouvement conservée ; le corps résultant hérite d'un identifiant stable (le suivi caméra s'y reporte).
- Espace ouvert. **Nettoyage périodique** (tous les N pas) : suppression des corps **non liés (énergie > 0) ET au-delà de k × rayon initial** du barycentre, sauf s'ils appartiennent à un amas éjecté lié et significatif (voir v1.3).

## Création d'une simulation
- Répartition : **sphère** (défaut) ou **disque** (épaisseur, profil de densité uniforme/exponentiel).
- Vitesse initiale, **modes exclusifs** : repos (défaut) / aléatoire (isotrope, plage) / circulaire (vitesse fixe ± plage, ou orbitale képlérienne sur la masse intérieure ± plage) / radiale vers le centre (plage).
- Paramètres : nombre de particules, taille du nuage, graine aléatoire ; panneau avancé : G, dt, ε, θ, r₀, facteur k et période du nettoyage, moteur de calcul.
- En cours de route : seulement **pause / reprise / pas-à-pas / multiplicateur de vitesse**.

## Interface
- Accueil : nouvelle simulation (formulaire) ; **liste des sauvegardes** du dossier `saves/` (date, nb de corps, pas) + « Parcourir… ».
- Navigation : souris **capturée en permanence** (style FPS) ; Z/S avancer/reculer, Q/D gauche/droite, molette = zoom ; Tab libère le curseur (Échap ouvre le menu).
- Sélection : **réticule + clic** en mode capturé, **clic libre** en mode curseur.
- Corps sélectionné (ou barycentre) : **orbite + suivi** — la cible reste au centre, la souris tourne autour d'elle, Z/S règlent la distance, Q/D sont inactives, A/E font basculer la vue sur l'axe de visée ; F cesse ou reprend le suivi sans changer la vue.
- Orientation libre (quaternion) : rotations dans le repère de l'écran, sans limite aux pôles ; H redresse l'horizon vers la normale au disque.
- Vol libre : vitesse proportionnelle à la distance au corps sélectionné (sinon au barycentre) ; repères à l'écran : trièdre (X, Y, Z, normale N), flèches vers les cibles hors champ, grille du plan du disque (G).
- Raccourcis : **Origine** = plus gros corps, **PgDn** = suivant (masse inférieure), **PgUp** = précédent.
- Rendu : billboards lumineux, taille réelle (min. 1 px), couleur selon la masse, mélange additif.
- HUD : statistiques globales (corps, pas, temps simulé, pas/s, i/s, dérive d'énergie), corps sélectionné, top 10 cliquable.
- Sauvegarde **manuelle (F5)** : état complet + paramètres + caméra (bincode + zstd).

## Qualité
- Tests physiques (masse, quantité de mouvement, dérive d'énergie, Barnes-Hut vs force brute), benchmarks (criterion), dépôt git, README, guide d'installation de Rust sous Windows.

## Décisions complémentaires (v1)
- E / A : monter / descendre ; Maj / Ctrl : déplacement ×5 / ×0,2 ; + / − : vitesse de déplacement.
- Molette : zoom optique (champ de vision) dans tous les modes ; la sensibilité de la souris suit le zoom.
- Rotation initiale (modes circulaires) autour de l'axe vertical Oz, sens trigonométrique.
- Espace : pause / reprise ; P : pas à pas ; « multiplicateur de vitesse » réalisé par une limite de cadence (pas/s), le calcul allant sinon au plus vite.
- Échap ouvre un menu (reprendre, sauvegarder, accueil, quitter, aide des touches) ; Tab bascule la capture de la souris.
- Une simulation chargée redémarre en pause.

## Évolutions (v1.1)
- Passage d'un corps à un autre : direction de visée, zoom et distance à la cible conservés (seule exception : ne jamais se retrouver à l'intérieur du nouveau corps).
- Souris libre : un clic droit reprend la visée.
- C : viser le centre de gravité ; V : vue d'ensemble (recul, dans la direction de visée, jusqu'à voir la sphère centrée sur le barycentre contenant 90 % de la masse).
- Affichage plafonné à 120 images par seconde.
- Mode trace (T) : trajectoires des 10 corps les plus massifs visibles à l'écran (dans le champ de la caméra) à l'activation, un point par pas (enregistré par le thread de calcul) ; historique complet décimé ou queue glissante de N pas, au choix ; une trace se poursuit sur le corps issu d'une fusion ; traces non sauvegardées.
- Traces en référentiel relatif (R) : affichage p(t) − r(t) + r(maintenant) par rapport au corps sélectionné (orbites autour de ce corps) ; trajectoire de référence issue de sa trace, ou d'une piste dédiée enregistrée depuis sa sélection.
- Enregistrement (F9 ou case à la création) : instantané tous les N pas (positions 21 bits, vitesses 16 bits, masses, fusions ; zstd), écrit par un thread dédié ; budget de poussière optionnel (au plus B particules de masse 1 par instantané, sélection stable par identifiant, luminosité compensée) ; fichier lisible même s'il n'a pas été clos. Une sauvegarde F5 faite pendant un enregistrement le mémorise et le poursuit au chargement ; depuis l'accueil, on peut poursuivre n'importe quel enregistrement compatible à partir d'une sauvegarde (instantanés postérieurs supprimés, corps disparus entre-temps marqués « éliminés » faute de mieux).
- Relecture : interpolation cubique d'Hermite entre instantanés, chargement anticipé en arrière-plan ; lecture / pause, vitesse, sens, barre de temps ; navigation, sélection et traces identiques au direct ; pas de reprise du calcul depuis un enregistrement (utiliser les sauvegardes F5).
- Suspension / reprise d'enregistrement : à la sortie (ou au retour à l'accueil) pendant un enregistrement, sauvegarde automatique liée ; son rechargement poursuit l'enregistrement dans le même fichier (reprise automatique dès la relance du calcul) ; une sauvegarde plus ancienne tronque l'enregistrement à son point de reprise. Format de sauvegarde v2, puis v3 (roulis de la caméra, suivi du barycentre, poursuite d'enregistrement).
- Panneaux : masse présente et masse éjectée (part de la masse initiale) ; part de la masse présente pour chacun des corps les plus massifs.
- B : sélection du barycentre (réticule doré à la place du repère du corps), vue perpendiculaire au disque selon l'axe du moment cinétique total (Oz à défaut de rotation), distance conservée, orbite autour du barycentre ; panneau dédié ; référentiel des traces « Barycentre » (trajectoire du barycentre enregistrée dès l'activation des traces).
- Moteur GPU (wgpu, shaders de calcul) : Barnes-Hut sur LBVH, Leapfrog, fusions balayées et nettoyage sur la carte, simple précision ; moteur par défaut si une carte compatible est présente ; repli sur le CPU au chargement sinon ; banc d'essai CPU/GPU.
- Affichage direct (moteur GPU) : le moteur calcule sur la carte graphique de l'interface, qui dessine les corps depuis ses tampons, sans copie ; l'état complet n'est relu que toutes les une à deux secondes (pointage, classement, barycentre, sauvegarde), le pas, les énergies, le centre de masse et la position du corps suivi restant publiés à jour ; repli sur une carte propre au moteur, avec affichage par copies, si la carte de l'interface ne convient pas au calcul (OpenGL).

## Évolutions (v1.2) — performances
- Moteur GPU :
  - parcours de Barnes-Hut par groupes de 128 corps voisins (boîte du groupe pour le critère d'ouverture, liste d'interactions en mémoire partagée), plus précis à θ égal que le parcours par corps ;
  - topologie de l'arbre refaite tous les 4 pas ou après une compaction, arbre seulement réajusté entre-temps (centres de masse, boîtes, rayons d'ouverture) ;
  - une soumission et une attente par pas : paires de contact, éliminés, moments globaux et positions des corps suivis relus ensemble ; positions, centre de masse et énergies servis ensuite sans relecture ;
  - fusions appliquées après le second demi-kick (quantité de mouvement et barycentre inchangés par rapport à l'ordre précédent) ;
  - corps de 48 octets (position de début de pas déduite de la vitesse), une passe de calcul par soumission, groupes de liaisons créés une fois ;
  - profilage par noyau dans le banc d'essai (`STELLAR_GPU_PROFILE=1`) ; banc sur disque en rotation (`disque`).
- Moteur CPU : parcours par groupes de 32 corps voisins au plus, évaluation AVX2/FMA si disponible ; contacts sans table de hachage (curseurs sur les clés de cellule triées).
- Interface : présentation en mode Mailbox quand il est garanti (NVIDIA, AMD sous Vulkan ; DirectX 12) — l'attente de l'écran en mode Fifo bloquait chaque soumission du moteur GPU d'une image entière ; `STELLAR_STATS=1` journalise pas/s et images/s.
- Formats : seule la version courante de `.stellar` (v3) est lue ; la relecture des versions 1 et 2 est retirée.

## Évolutions (v1.3) — amas éjectés
- Nettoyage : les corps lointains (au-delà de k × R) sont regroupés par « amis d'amis » (distance de liaison, par défaut la distance moyenne initiale entre voisins) ; chaque groupe est épuré de ses membres non liés (énergie dans le référentiel du groupe, sous son seul potentiel ; les plus énergiques retirés d'abord) ; un groupe lié d'au moins n corps (10 par défaut) **ou** d'au moins une part de la masse initiale (0,1 % par défaut, un corps isolé pouvant suffire) est conservé en entier. Les autres corps lointains non liés au système sont éliminés comme avant.
- Règle du domaine commune aux moteurs CPU et GPU ; le GPU repère les corps lointains et ne relit qu'eux aux pas de nettoyage.
- Panneau « Corps » : liste des amas éjectés conservés au dernier nettoyage (repère, corps, masse, distance au barycentre), sélection du repère par clic ; absente en relecture.
- Paramètres avancés : distance de liaison (déduite automatiquement), nombre de corps et part de masse minimaux.
- Formats : sauvegardes `.stellar` version 4 (version 3 relue, seuils complétés par les valeurs suggérées), enregistrements `.stellarec` version 2 (version 1 relue : seul l'en-tête change).

## Évolutions (v1.4) — sous-simulations
- Un amas éjecté devient une sous-simulation quand (taille de l'amas + taille du système) < 0,3 × distance (taille d'un amas : deux fois son rayon de demi-masse) : moteur propre (même type), repère propre (origine au barycentre), même pas de temps.
- Couplage par champ linéaire (accélération au barycentre et tenseur de marée des autres sous-systèmes, sources ponctuelles), sur les barycentres extrapolés à la fin du pas ; énergie totale = énergies propres + énergies d'interaction.
- Réunion quand (somme des tailles) > 0,6 × distance ; recentrage quand le barycentre s'écarte de l'origine de plus d'une demi-taille.
- Corps éliminés par une sous-simulation secondaire : confiés au système principal (le plus massif).
- Panneau « Corps » : liste des sous-simulations (repère, corps, masse, distance), sélection du repère par clic.
- Formats : sauvegardes `.stellar` version 5 (plages de corps par sous-système ; versions 4 et 3 relues), enregistrements `.stellarec` version 3 (une boîte de quantification par sous-système ; versions 2 et 1 relues, un enregistrement repris garde sa version).

