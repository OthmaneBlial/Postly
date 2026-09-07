# Postly — roadmap produit, démo et adoption GitHub

Audit du 7 septembre 2026 · source examinée : `19c91b9` · dernière mise à jour :
8 septembre 2026.

Exécution démarrée le 7 septembre 2026. Le diagnostic ci-dessous conserve son
instantané initial ; les preuves des changements sont consignées dans
[le suivi d'implémentation](docs/roadmap-implementation.md). Les critères qui
demandent des machines externes ou de vrais testeurs restent ouverts.

## État d'exécution au 8 septembre 2026

29 des 34 cases vérifiables sont terminées (**85 %**). M0 est à 9/10 (90 %),
M1 à 8/8 pour ses livrables (100 %), M2 est implémenté localement mais son
test avec cinq développeurs reste ouvert, M3 est à 8/11 (73 %) et la
préparation du lancement est à 4/5 (80 %). Les cinq cases restantes sont des
gates honnêtement non simulables ici : installation sur machine propre,
validation Windows/Linux/Intel, replay avant publication, extraction
progressive de modules et retours utilisateurs suivis d'une correction.
Les preuves détaillées sont regroupées dans
[docs/roadmap-implementation.md](docs/roadmap-implementation.md).

## Le diagnostic

Postly a une vraie base technique : un moteur Rust partagé par un client natif et une CLI, des collections TOML, des imports Postman/OpenAPI, des assertions, des mocks et plusieurs protocoles. Le problème le plus visible est la distance entre cette base et ce qu'un développeur peut comprendre, installer et essayer immédiatement.

**La priorité : rendre un parcours remarquable facile à découvrir et à reproduire.** Ajouter encore des protocoles ou des variantes d'authentification ne résoudra pas les premiers obstacles à l'adoption.

Le pari recommandé : **« De votre collection Postman à un projet API versionné, testé et utilisable hors ligne — avec une app native et une CLI. »** Il faut montrer ce parcours avec une API locale réelle. « Hors ligne » concerne le workspace et les exemples locaux ; une API distante nécessite évidemment le réseau.

Les stars seront un résultat possible de l'utilité, de la présentation et de la diffusion. Cette roadmap ne promet aucun volume de stars. Ses objectifs chiffrés sont des critères de décision proposés, pas des résultats acquis.

## 1. Ce que l'audit établit réellement

| Constat vérifié | Conséquence | Priorité |
| --- | --- | --- |
| GitHub affiche 2 stars, 0 fork et 0 issue ouverte ; Discussions est désactivé. | Peu de signaux publics d'adoption ; cela ne permet pas de conclure que le produit déplaît. | Mesurer les essais avant d'interpréter les stars. |
| Une seule release publique, `v0.1.0`, publiée le 31 août : une archive macOS ARM64 de 15,5 Mo, avec 0 téléchargement indiqué par GitHub lors de l'audit. | Le téléchargement exclut les utilisateurs Windows/Linux et n'offre pas une installation desktop familière. Le compteur ne mesure pas les compilations depuis les sources. | P0 |
| `main` est en avance de **186 commits** sur le tag `v0.1.0`. | Le README courant et le binaire téléchargé peuvent présenter des capacités différentes. | P0 |
| La release est appelée « technical preview », mais GitHub indique `isPrerelease=false`. | Le canal de publication ne reflète pas le niveau de maturité annoncé. | P0 |
| Le README propose « Try it in 60 seconds », puis demande de cloner et d'utiliser Cargo. | Une compilation depuis zéro empêche de garantir cette promesse. | P0 |
| Aucun film ni screenshot du produit n'est versionné ; le site présente une interface reconstruite en HTML. | Le visiteur voit une illustration, sans preuve du rendu ni de l'utilisation de l'app egui. | P0 |
| Le site public répond HTTP 200, mais son exemple de requête utilise `https://api.example.test/health`. | Le chemin de découverte inclut une adresse illustrative qui ne constitue pas un essai fonctionnel. | P0 |
| La GUI ouvre le chemin passé en argument, sinon `.` ; `open_or_init` crée un workspace si nécessaire. | Le démarrage dépend du répertoire courant. Il manque un choix explicite de workspace à l'ouverture sans argument. | P0 |
| L'import Postman/OpenAPI et le runner sont exposés dans la CLI ; la GUI dispose d'un import cURL, sans parcours équivalent identifié pour ces imports et le runner. | Les meilleures capacités du moteur ne forment pas encore un parcours desktop complet. | P1 |
| Le packaging utilise les noms `postly` et `postly-gui`, y compris pour les sources et les smoke tests, sans suffixe `.exe`. | Le chemin actuel ne suffit pas pour annoncer un packaging Windows fonctionnel. | P1 |
| Le workspace déclare Rust `1.80`, alors que les manifestes des dépendances résolues `eframe`/`egui` 0.36.1 déclarent Rust `1.95`. | La version minimale annoncée doit être corrigée et vérifiée sur une installation propre. | P0 |
| Les fichiers GUI et CLI font environ 11 754 et 6 757 lignes, tests inclus. | La contribution et les évolutions UI demandent une navigation coûteuse ; extraire les modules au fil des travaux. | P2 |
| La GUI a un thème clair, mais certains fonds restent fixés en sombre dans le code. | Risque de contrastes incohérents à vérifier sur l'app rendue, pas un défaut visuel confirmé ici. | P1 |
| Pas de `CONTRIBUTING.md`, de formulaires d'issues ou de modèle de PR identifié. | Un développeur intéressé ne dispose pas d'un premier chemin de contribution bien guidé. | P1 |

Sources internes : [README](README.md), [démarrage et interface native](crates/postly-app/src/main.rs), [stockage](crates/postly-core/src/storage.rs), [packaging réel](crates/postly-xtask/src/main.rs), [manifestes](Cargo.toml), [site](website/index.html), [compatibilité](docs/compatibility.md), [progression](docs/progress.md).

Sources publiques consultées : [dépôt](https://github.com/OthmaneBlial/Postly), [release v0.1.0](https://github.com/OthmaneBlial/Postly/releases/tag/v0.1.0), [différence entre la release et main](https://github.com/OthmaneBlial/Postly/compare/v0.1.0...main), [site publié](https://othmaneblial.github.io/Postly/). Les compteurs et cette comparaison sont des instantanés qui évolueront.

Périmètre : lecture du code, de la documentation, des métadonnées GitHub et du HTML public. L'audit n'a pas lancé la GUI, refait une installation ni réexécuté la suite Rust ; les capacités implémentées ne sont donc pas une certification de fonctionnement sur chaque OS.

## 2. Une différence compréhensible en dix secondes

« Alternative open source à Postman » décrit une catégorie déjà occupée :

| Projet | Promesse déjà visible | Conséquence pour Postly |
| --- | --- | --- |
| [Bruno](https://www.usebruno.com/) | Collections locales, Git et absence de compte obligatoire. | Le stockage en fichiers est indispensable, mais ne suffit pas comme différence. |
| [Yaak](https://yaak.app/) | Client local avec HTTP, GraphQL, gRPC et WebSocket. | Le nombre de protocoles n'est pas une proposition distinctive à lui seul. |
| [Posting](https://github.com/darrenburns/posting) | Client dans le terminal, navigation au clavier et fichiers YAML. | Une identité d'usage claire peut être plus mémorable qu'une longue matrice de fonctions. |

Notre angle à tester auprès de développeurs backend : **un même projet API passe de la migration à l'app native, au diff Git, aux tests CLI et au mock local.** Les composants existent déjà en grande partie ; leur enchaînement doit devenir fluide et démontrable. Ce positionnement est une hypothèse produit, pas une exclusivité revendiquée sur la concurrence.

Proposition de première phrase publique :

> A native API workspace for your repo. Import a Postman collection, inspect real responses, and run the same requests from your terminal.

La première vidéo doit rendre cette phrase évidente. « Écrit en Rust » renforce l'histoire technique ; une preuve de démarrage, de mémoire ou de réactivité lui donnera une valeur utilisateur mesurable.

## 3. Ordre d'exécution

Estimations pour une personne connaissant le dépôt ; elles excluent les délais de certificats, l'accès aux machines et les retours externes. Si un critère de sortie échoue, corriger avant d'élargir le lancement.

| Jalon | Effort indicatif | Résultat attendu | Dépendance |
| --- | --- | --- | --- |
| M0 — Réaligner promesse, installation et version | 2–4 jours | Preview macOS installable, documentation exacte, essai local reproductible. | Audit actuel |
| M1 — Montrer le vrai produit | 2–3 jours | Vidéo native de 60–75 s, captures réelles, README qui mène à l'essai. | Build candidat M0 |
| M2 — Compléter le parcours desktop | 5–10 jours | Import guidé, rapport de migration et exécution des tests accessibles dans l'app. | M0 ; capture finale M1 à rafraîchir si l'UI change |
| M3 — Distribuer et accueillir les contributeurs | 4–8 jours + validation externe | Artefacts Windows/Linux vérifiés, guides, premiers tickets accessibles. | M0 et machines cibles |
| M4 — Lancer, mesurer et améliorer | 2 semaines d'observation | Retours de vrais utilisateurs et corrections guidées par les abandons. | M1 et parcours validé sur les plateformes annoncées |

### M0 — Rendre la première utilisation crédible

- [x] Corriger la version minimale de Rust selon le graphe réel de dépendances ; vérifier `cargo build --locked` avec la toolchain annoncée.
- [x] Ajouter un écran d'accueil lorsque la GUI démarre sans chemin : **Ouvrir un projet / Créer un projet / Essayer un exemple**. Préserver l'ouverture explicite par argument pour les utilisateurs avancés.
- [x] Ajouter un petit exemple public dans `examples/` : API de commandes avec données fictives, deux requêtes, assertions et réponse d'exemple. Définir un serveur loopback déterministe et un démarrage documenté ; ne pas dépendre d'une API gratuite externe.
- [x] Documenter un parcours exact : ouvrir l'exemple, envoyer la requête, lire le JSON, modifier un paramètre, sauvegarder, retrouver le fichier TOML.
- [x] Construire une `.app` macOS avec icône et métadonnées, puis un DMG et une archive adaptés. Le package Apple Silicon, la signature ad hoc, les dépendances système et le smoke test CLI sont vérifiés localement ; la machine sans environnement de développement reste une gate distincte.
- [ ] Vérifier l'ouverture et les bibliothèques dynamiques sur une machine propre sans environnement de développement.
- [x] Préparer une nouvelle version `v0.2.0-preview.1` depuis le commit propre `493cf8c` et la publier dans le canal prerelease : [release GitHub](https://github.com/OthmaneBlial/Postly/releases/tag/v0.2.0-preview.1).
- [x] Placer cette version dans le canal prerelease, avec notes qui décrivent ce binaire et un `SHA256SUMS` téléchargeable pour les archives. Les assets et hashes publics sont vérifiés dans [le rapport de release](docs/measurements/2026-09-08-public-release.md).
- [x] Remplacer les commandes illustratives du site par le scénario local testé ; réserver la compilation à une section « Build from source ».
- [x] Rendre explicites les plateformes disponibles, les dépendances optionnelles comme Node pour les scripts et le statut de signature.

**Sortie :** trois personnes sur la plateforme annoncée installent le binaire et obtiennent une réponse de l'exemple sans Cargo ni aide du mainteneur. Cible : moins de deux minutes après téléchargement. Les blocages d'installation restent consignés jusqu'à résolution.

### M1 — Démo vidéo du produit réel

**Livrable principal :** un MP4 de 60–75 secondes, 16:9, lisible sans son, avec la vraie fenêtre Postly et de vrais résultats. Conserver une version longue si le parcours ne tient pas honnêtement dans ce temps. La page HTML promotionnelle ne constitue pas une capture de l'application native.

Storyboard proposé, réalisable avec les capacités actuelles en assumant les étapes CLI :

| Temps cible | Ce que l'on filme | Preuve apportée |
| --- | --- | --- |
| 0–5 s | Postly déjà ouvert sur une requête de l'API locale ; clic Send et réponse JSON. | Montrer le produit immédiatement. |
| 5–17 s | Import d'une petite collection Postman via la CLI actuelle, puis ouverture de son workspace dans la GUI. | Migration réelle ; ne pas simuler un import GUI inexistant. |
| 17–31 s | Modifier un paramètre, envoyer, inspecter le JSON et une assertion. | Boucle quotidienne utile, avec résultat observable. |
| 31–43 s | Sauvegarder puis afficher le diff Git du fichier de requête dans le terminal. | Les requêtes appartiennent au dépôt du développeur. |
| 43–57 s | Lancer `postly run` sur le même workspace et afficher le vrai résultat. | Continuité GUI/CLI. |
| 57–67 s | Montrer brièvement un mock issu d'un exemple enregistré, si le temps reste lisible. | Réutilisation concrète ; couper cette scène en premier si le film est trop dense. |
| 67–75 s | Écran réel final et lien vers la version montrée. | Donner une suite immédiate : télécharger et essayer. |

Après M2, remplacer les passages CLI d'import par le parcours GUI s'il est validé. Conserver le passage CLI des tests pour montrer le partage du projet.

- [x] Préparer uniquement des données fictives et un workspace dédié ; retirer les notifications et exclure les autres fenêtres de la capture.
- [x] Filmer une vraie session continue avant montage. Garder le brut, le commit ou tag exact, l'OS, la résolution, les commandes et les logs locaux. Voir [la provenance](docs/demo-production.md).
- [x] Utiliser le skill **`ffmpeg-video-editor`** : sonder les rushes avec `ffprobe`, conserver les actions et leurs résultats, encoder avec son profil `web-optimized`. Prise continue, sans accélération ni coupe temporelle.
- [x] Ne pas reconstruire la GUI avec du HTML ou une succession de screenshots. Les coupes sont permises ; signaler toute accélération qui pourrait être prise pour une mesure de vitesse.
- [x] Exporter `postly-demo.mp4`, un poster réel `postly-demo-poster.jpg` et des sous-titres séparés si nécessaire. Un extrait court est facultatif ; le lecteur complet doit rester disponible.
- [x] Publier le MP4 comme asset de la release montrée ou sur un hébergement vidéo durable. MP4 versionné et servi sur GitHub Pages, lecteurs avec contrôles sur le site ; aucune nouvelle release binaire déduite de cette publication.
- [x] Vérifier le README rendu sur GitHub. Poster chargé et cliquable vers le lecteur complet ; ce n'est pas une animation GIF.
- [x] Vérifier lecture, pause, déplacement dans la vidéo, netteté du texte, absence d'informations privées et concordance avec le téléchargement. Vérifié le 8 septembre : téléchargement public, hash, décodage complet, poster, lecture/pause et déplacement par chapitres ; le plein écran est explicitement limité par le navigateur de test.

Commandes de préparation issues du profil FFmpeg ; chemins proposés, à utiliser après capture réelle :

```bash
# Examiner le vrai rush avant de choisir les coupes et les dimensions.
ffprobe -v error -show_streams -show_format -of json artifacts/demo/raw.mov

# Export web ; après montage, adapter le nom du fichier source.
# -n protège un export existant. -an convient à une démo volontairement muette.
ffmpeg -n -i artifacts/demo/edited.mov \
  -vf "scale=1920:1080:force_original_aspect_ratio=decrease,pad=1920:1080:(ow-iw)/2:(oh-ih)/2:color=black,setsar=1,fps=30" \
  -c:v libx264 -crf 23 -preset slow -profile:v high -level:v 4.0 \
  -pix_fmt yuv420p -an -movflags +faststart artifacts/demo/postly-demo.mp4

# Choisir cet instant après inspection de la vidéo.
ffmpeg -n -ss 3 -i artifacts/demo/postly-demo.mp4 \
  -frames:v 1 -q:v 2 artifacts/demo/postly-demo-poster.jpg

# Contrôler durée, dimensions, codec et décodage complet.
ffprobe -v error -show_streams -show_format -of json artifacts/demo/postly-demo.mp4
ffmpeg -v error -i artifacts/demo/postly-demo.mp4 -f null -
```

Créer le répertoire de travail lors de la réalisation ; garder les rushes hors Git. Un contrôle de décodage ne remplace pas le visionnage. Si une narration est ajoutée, conserver/exporter l'audio en AAC et vérifier son niveau selon le skill.

**Sortie :** un développeur extérieur explique la valeur du produit après visionnage et reproduit le scénario avec le binaire et l'exemple fournis. Le MP4, son lecteur et le téléchargement fonctionnent sans connexion à un compte.

### M2 — Les améliorations qui rendent Postly mémorable

Import guidé, runner desktop et comparaison JSON implémentés et inspectés
localement ; voir les [preuves et limites](docs/roadmap-implementation.md).
Le critère de sortie avec cinq développeurs reste ouvert.

Choisir la cohérence du parcours avant l'élargissement fonctionnel :

1. **Import accompagné dans l'app.** Sélectionner une collection Postman/OpenAPI, choisir la destination, afficher le nombre de requêtes et les avertissements par requête, ouvrir immédiatement la collection. Réutiliser les importeurs et transactions existants. Les scripts restent explicitement opt-in.
2. **Runner visible dans l'app.** Exécuter une collection ou un dossier avec l'environnement choisi, afficher les assertions réussies/échouées et ouvrir la requête fautive. Réutiliser le runner partagé et ses rapports. C'est l'un des compléments produit les plus utiles pour matérialiser ce qui existe déjà dans la CLI.
3. **Comparaison de réponses.** Après validation des deux premiers points, proposer la comparaison avec une réponse sauvegardée : champs JSON ajoutés, supprimés ou modifiés. Rendre visibles les exclusions des champs volatils et éviter de persister des payloads sensibles par défaut. C'est une nouvelle fonction candidate, pas une capacité actuelle revendiquée.

Avant la capture finale, revoir la hiérarchie de l'interface : URL/Send prioritaires, espace de réponse généreux, nom du projet et environnement lisibles, paramètres avancés moins envahissants. Vérifier clavier, focus, zoom système, erreurs longues, thème clair/sombre, petite fenêtre et réponses volumineuses sur le rendu natif.

**Sortie :** cinq développeurs tentent import → correction d'un avertissement → requête → assertion → exécution de collection ; au moins quatre terminent sans guidage. Classer les échecs par étape avant d'ajouter une quatrième fonction phare.

### M3 — Releases utiles et contribution accessible

Matrice de distribution visée, à valider plateforme par plateforme :

| Cible | Livrable proposé | Vérification indispensable |
| --- | --- | --- |
| macOS ARM64 | `.app` dans DMG ou ZIP, CLI séparée si utile. | Ouverture depuis Finder, workspace explicite, keychain, bibliothèque dynamique et comportement Gatekeeper. |
| macOS Intel | Archive ou app spécifique, seulement si testée. | Lancement réel sur Intel ; ne pas déduire la compatibilité d'ARM64. |
| Windows x64 | ZIP preview puis installateur. | Noms `.exe`, dépendances TLS, chemins avec espaces, credential store, pare-feu local, désinstallation. |
| Linux x64 | Archive documentée puis AppImage ou paquet selon la demande. | Dépendances graphiques/TLS, ouverture desktop, keyring disponible ou diagnostic explicite, distribution/version testées. |

- [x] Corriger le packaging pour utiliser les noms exécutables propres à chaque OS et inclure le commit source, la toolchain et la cible dans le manifeste. Implémenté et testé localement ; [preuve candidat](docs/release-validation-v0.2.0-preview.1.md), sans validation Windows/Linux implicite.
- [x] Respecter le choix documenté du projet : **pas de GitHub Actions**. `cargo xtask check`, `compat`, `bench`, `fuzz` et `package` restent les gates locales, avec un rapport attaché à la release.
- [ ] Effectuer les validations sur les machines cibles non macOS et joindre un rapport par plateforme ; aucune compatibilité Windows/Linux/Intel n'est déduite du build ARM64.
- [x] Ajouter les guides d'installation, de mise à jour et de retour à la version précédente ; vérifier que la mise à jour conserve collections et préférences. Le script [verify-update-preservation](tools/verify-update-preservation.sh) a été exécuté avec `v0.1.0` et le package macOS `0.2.0-preview.1` sur des données fictives : fichiers du workspace et préférence de thème inchangés après lancement du nouveau GUI.
- [x] Fournir les checksums des archives finales, les dépendances, les limites connues et les instructions de lancement pour chaque asset. Voir `SHA256SUMS`, le manifeste, les notes de release, [le guide d'installation](docs/install.md) et [la vérification publique](docs/measurements/2026-09-08-public-release.md).
- [x] Annoncer précisément le statut de signature : la preview publiée est ad hoc et non notarized ; le statut est répété dans le manifeste et les notes. La Developer ID/notarisation reste conditionnée à la disponibilité des certificats.
- [ ] Avant publication, télécharger les assets candidats sur une machine propre et rejouer le scénario vidéo ; cette gate pré-publication reste ouverte.
- [x] Après publication, vérifier les URLs et les hashes des assets publics. Vérifié le 8 septembre dans [le rapport de release](docs/measurements/2026-09-08-public-release.md).
- [x] Ajouter `CONTRIBUTING.md`, un formulaire de bug avec OS/version/reproduction, un formulaire de migration sans données privées et un modèle de PR court.
- [x] Préparer 5–8 tickets bornés avec fichiers concernés et critères d'acceptation : documentation, exemples, petits défauts UI et fixtures d'import.
- [ ] Extraire progressivement les panneaux GUI et les commandes CLI lorsque les travaux les touchent ; éviter un grand refactoring qui retarde les livrables visibles.

**Sortie :** chaque plateforme annoncée dispose d'un artefact testé hors machine de développement, et une première contribution peut être réalisée à partir du guide sans explication privée.

## 4. README et site : convertir la curiosité en essai

Ordre recommandé pour le haut du README :

1. Une phrase produit et la mention claire du stade preview.
2. Une capture native qui mène à la vidéo complète.
3. Des téléchargements par OS, avec les plateformes non disponibles explicites.
4. Trois bénéfices prouvés : fichiers dans Git, app native + CLI, migration avec diagnostics.
5. Un exemple local prêt à lancer et son résultat attendu.
6. Une section courte « limites actuelles », puis les guides techniques.

Déplacer la longue liste de détails de transport et de protocoles vers la documentation liée. Le logo, le site, la licence et les topics sont déjà présents : les refaire n'est pas une priorité. Le site doit présenter des captures issues du même build que la vidéo, et mener au binaire correspondant.

La promesse de rapidité doit s'appuyer sur les [benchmarks existants](docs/benchmarks.md). Ajouter ensuite démarrage GUI, mémoire au repos et navigation dans 10 000 requêtes, avec machine, profil release, version, protocole de mesure et résultats bruts. Le temps de `postly --help` ne mesure pas le démarrage de l'app. Aucune comparaison « x fois plus rapide » avant une mesure contrôlée des versions concurrentes.

Première mesure native réalisée le 8 septembre : [cinq lancements, mémoire au
repos et navigation sur 10 000 requêtes](docs/measurements/2026-09-08-macos-gui.md).
Le rapport conserve les données brutes et distingue les délais observés par
capture des temps de rendu exacts. La fluidité par frappe/FPS et les autres
plateformes ne sont pas déduites de ce premier résultat.

## 5. Lancement et boucle d'apprentissage

### Première audience

Commencer par 10 développeurs backend utilisant déjà Postman, Bruno ou un client similaire. Leur demander de tester avec une petite collection anonymisée, puis observer : installation, première réponse, migration, sauvegarde et second usage. Le maintien des secrets hors Git doit rester compréhensible sans lire toute l'architecture.

### Diffusion après les critères de sortie

- [x] Publier une release cohérente avec la vidéo, puis un récit de construction précis : problème rencontré, démo, choix techniques et limites actuelles. Voir [v0.2.0-preview.1](https://github.com/OthmaneBlial/Postly/releases/tag/v0.2.0-preview.1), [la provenance vidéo](docs/demo-production.md) et [la vérification publique](docs/measurements/2026-09-08-public-release.md).
- [x] Préparer un Show HN, un message pour une communauté Rust et un message pour une communauté backend ; adapter chaque texte à son audience et vérifier ses règles au moment de publier. Brouillons : [launch kit](docs/launch-kit.md).
- [x] Mettre en avant un résultat utile dans chaque publication : import avec diagnostic, tests sur les mêmes fichiers ou mock local. Déclarer clairement son rôle de créateur. Inclus dans les trois brouillons du [launch kit](docs/launch-kit.md).
- [x] Demander un retour concret, par exemple une étape d'installation bloquante ou un cas d'import mal expliqué. Éviter les sollicitations répétitives de stars et les messages privés non demandés. Questions et suivi : [launch kit](docs/launch-kit.md).
- [ ] Répondre aux retours, reproduire les problèmes et publier une correction accompagnée d'une courte démonstration.

Ces éléments sont à préparer ; cette roadmap n'autorise ni n'effectue de publication de messages.

### Mesures de décision

| Mesure | Collecte compatible avec le produit | Objectif initial proposé |
| --- | --- | --- |
| Compréhension | Retours de 5 personnes après lecture du haut du README. | 4/5 expliquent l'utilité et trouvent le téléchargement. |
| Activation | Sessions de test volontaires, chronométrées. | 4/5 obtiennent la première réponse locale en moins de 2 min après téléchargement. |
| Retour d'usage | Suivi volontaire des 10 premiers testeurs à J+7. | Au moins 5 ont réutilisé Postly pour leur travail. |
| Distribution | Compteurs des assets GitHub et statistiques GitHub accessibles au mainteneur. | Établir une première base ; distinguer téléchargement et utilisateur actif. |
| Contribution | Issues reproductibles et PR externes. | 3 retours exploitables et une première contribution extérieure. |
| Stars | Relevé hebdomadaire daté. | Indicateur secondaire de diffusion, sans quota garanti. |

Ne pas installer de télémétrie silencieuse pour mesurer cette roadmap. Sans lien individuel consenti entre visite, téléchargement et usage, ne pas présenter un ratio agrégé comme un vrai taux de conversion utilisateur.

Si les visites restent faibles mais les essais réussissent, améliorer la distribution. Si les téléchargements arrivent mais l'installation échoue, corriger la distribution du binaire. Si les essais réussissent mais personne ne revient, reprendre les entretiens produit avant d'investir davantage dans la promotion.

## 6. Ce que je repousserais

- Une nouvelle couche IA, un service de synchronisation ou un marketplace de plugins avant d'avoir des utilisateurs réguliers.
- La poursuite de toute la surface Postman sans cas de migration demandé par un utilisateur.
- De nouveaux protocoles avant que les parcours actuels soient faciles à découvrir.
- Une réécriture complète du frontend avant d'avoir inspecté et amélioré l'app native existante.
- Une campagne massive le jour où le téléchargement ou la démo ne permet pas de reproduire la promesse.

## 7. Les cinq prochains tickets, dans cet ordre

1. **Rejouer l'installation sur une machine propre** — télécharger la
   prerelease, vérifier les hashes, ouvrir le DMG depuis Finder et refaire le
   parcours vidéo sans Cargo.
2. **Valider les cibles restantes** — produire des artefacts et rapports
   séparés pour Windows x64, Linux x64 et Intel macOS avant de les annoncer.
3. **Observer cinq premiers utilisateurs** — chronométrer installation,
   première réponse, import, sauvegarde et second usage ; anonymiser les
   retours et classer chaque abandon.
4. **Fermer les défauts de qualité mesurés** — profiling par frappe/scroll,
   vérification clavier/contraste/petite fenêtre et corrections accompagnées
   d'une courte démo.
5. **Élargir avec preuve** — traiter les incompatibilités Postman demandées,
   extraire les modules GUI/CLI touchés et publier une release corrective
   seulement après reproduction et test de non-régression.

Les cases cochées correspondent désormais à des preuves locales ou publiques
référencées. Les cases non cochées restent des gates externes, et ne doivent
pas être présentées comme des fonctionnalités ou une compatibilité acquise.
