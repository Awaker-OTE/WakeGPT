<p align="center">
  <img src="../apps/desktop/src-tauri/icons/wakegpt-icon.svg" width="112" alt="Icône WakeGPT">
</p>

<h1 align="center">WakeGPT</h1>

<p align="center"><strong>Capturez maintenant. Faites grandir vos notes dans votre propre espace de travail.</strong></p>

<p align="center">Un bloc-notes de bureau local-first qui transforme textes, Markdown, liens et images en notes durables liées à vos projets.</p>

<p align="center">
  <a href="../README.md">English</a> ·
  <a href="README.zh-CN.md">简体中文</a> ·
  <a href="README.ja.md">日本語</a> ·
  Français ·
  <a href="README.ru.md">Русский</a>
</p>

---

## Transformez une idée passagère en note durable

Écrivez une entrée, choisissez un espace de travail et un carnet : WakeGPT peut ensuite l'ajouter à votre fichier Markdown. Inutile d'organiser des dossiers, d'ouvrir un éditeur ou d'interrompre votre tâche. Capturez d'abord, organisez plus tard.

WakeGPT est utile pour :

- noter rapidement des tâches, indices d'erreur et idées d'implémentation pendant le développement ;
- recueillir conclusions, liens et captures d'écran pendant un échange produit ou une conversation avec une IA ;
- enrichir un même fichier Markdown à partir de nombreuses petites notes ;
- réutiliser certaines notes dans le champ de saisie de ChatGPT sans les retaper.

## Ce que fait WakeGPT

- **Capture rapide** — Texte brut, Markdown, liens et images, y compris le collage direct d'une image du presse-papiers.
- **Ajout continu dans Markdown** — Créez un carnet ou associez un fichier `.md` existant de l'espace de travail, puis ajoutez automatiquement les nouvelles entrées.
- **Plusieurs espaces et carnets** — Séparez les projets et passez de la boîte de réception aux différents carnets par onglets.
- **Styles d'entrée flexibles** — Numéros séquentiels, puces, listes de tâches ou préfixes horaires, avec réorganisation automatique après modification.
- **Gestion des entrées** — Consultez, modifiez, copiez, déplacez, supprimez et restaurez vos notes. Les images disposent d'une miniature et d'un aperçu agrandi.
- **Envoi vers ChatGPT** — Placez le texte sélectionné et les véritables pièces jointes image dans le champ ChatGPT actif, puis vérifiez avant l'envoi.
- **Local-first** — Les notes, images et fichiers de l'espace de travail restent par défaut sur votre ordinateur. Les fonctions principales ne dépendent pas de la télémétrie.

## Trois points de capture

| Surface | Usage principal |
|---|---|
| **Application WakeGPT** | Gérer les espaces de travail, carnets, historiques et réglages. |
| **Barre des menus macOS** | Ouvrir un panneau léger sans quitter la tâche en cours. |
| **Carte latérale ChatGPT** | Prendre des notes à côté d'une conversation, changer de carnet et placer une entrée dans le champ ChatGPT. |

L'application et les panneaux rapides conservent chacun leur carnet actif et leur brouillon. Un changement dans l'une des surfaces ne déplace pas l'autre de façon inattendue.

## Parcours d'une entrée

1. Saisissez du texte, du Markdown ou un lien, ou collez une image.
2. Choisissez la boîte de réception de l'espace de travail ou un carnet Markdown.
3. Validez pour enregistrer localement ; si un carnet est associé, WakeGPT ajoute aussi l'entrée à son fichier Markdown.
4. Revenez ensuite pour modifier, déplacer, supprimer, restaurer ou réutiliser l'entrée dans ChatGPT.

WakeGPT ne gère que les zones de notes rapides clairement balisées dans un fichier Markdown. Le reste du document peut toujours être modifié avec l'outil de votre choix.

## État du projet

WakeGPT est actuellement un **prototype macOS** open source. La [préversion macOS `v0.1.0`](https://github.com/Awaker-OTE/WakeGPT/releases/tag/v0.1.0) est disponible au téléchargement.

- Le fonctionnement a été vérifié sur macOS avec Apple Silicon.
- `v0.1.0` est une préversion réservée à macOS.
- Son App Bundle Universal contient du code Apple Silicon et Intel ; l'exécution sur un véritable Mac Intel reste à vérifier.
- Cette version utilise un scellement de code ad-hoc complet avec hardened runtime, mais elle n'est **ni** signée avec un Apple Developer ID **ni** notarisée par Apple. macOS peut donc bloquer le premier lancement.
- La carte latérale ChatGPT est expérimentale et n'a été vérifiée qu'avec les versions exactes de l'hôte consignées dans le dépôt. Elle se désactive de manière sûre si le contrat de l'hôte change.
- La prise en charge de Windows x64 est en cours de développement ; elle n'est ni publiée ni vérifiée sur une machine Windows réelle.

Téléchargez WakeGPT uniquement depuis la [GitHub Release officielle `v0.1.0`](https://github.com/Awaker-OTE/WakeGPT/releases/tag/v0.1.0) et comparez sa valeur SHA-256 au fichier `SHA256SUMS.txt` inclus. Après une première tentative d'ouverture, macOS peut demander d'aller dans **Réglages Système → Confidentialité et sécurité → Ouvrir quand même**. Consultez aussi [les instructions Apple](https://support.apple.com/fr-fr/guide/mac-help/mh40616/mac). WakeGPT ne demande pas de désactiver Gatekeeper ni de supprimer la quarantaine avec `xattr`.

## Exécuter depuis les sources

Node.js, npm, Rustup et les [prérequis système de Tauri 2](https://v2.tauri.app/start/prerequisites/) sont nécessaires.

```bash
git clone https://github.com/Awaker-OTE/WakeGPT.git
cd WakeGPT/apps/desktop
npm ci
npm test
npm run tauri build
```

Pour un build macOS Universal :

```bash
rustup target add aarch64-apple-darwin x86_64-apple-darwin
npm run release:macos-universal
```

Un build depuis les sources reste un artefact de développement ou de vérification locale. Le paquet `v0.1.0` téléchargeable n'est pas non plus notarisé par Apple, mais il a passé les contrôles de publication de WakeGPT et est accompagné de sommes de contrôle et de documents de chaîne d'approvisionnement.

## Confidentialité et sécurité

- Les notes, pièces jointes et fichiers Markdown restent locaux par défaut ; la télémétrie et l'envoi des rapports de plantage sont désactivés par défaut.
- L'intégration ChatGPT prépare le contenu du champ de saisie, mais ne clique jamais sur Envoyer à votre place.
- WakeGPT ne lit ni ne conserve les mots de passe, cookies ou jetons de connexion ChatGPT.
- La suppression privilégie la corbeille récupérable de WakeGPT ou la Corbeille de macOS. En cas de conflit, l'opération s'arrête sans écraser les modifications.

Signalez les problèmes de sécurité en privé selon [SECURITY.md](../SECURITY.md). Ne publiez pas les détails d'une faille, des informations de compte ou des données réelles dans une Issue publique.

## Contribuer

Les rapports de bug, propositions de fonction, améliorations de documentation et contributions de code sont bienvenus. Consultez [CONTRIBUTING.md](../CONTRIBUTING.md) avant de commencer.

WakeGPT est distribué sous [Apache License 2.0](../LICENSE), avec la mention `Copyright 2026 WakeGPT Contributors`. Les composants tiers conservent leurs propres licences ; leur provenance et un résumé des licences figurent dans [NOTICE.md](../NOTICE.md).

> WakeGPT est un projet open source conçu et implémenté de manière indépendante. Ce n'est pas un produit officiel d'OpenAI, ChatGPT ou Codex, et aucune approbation ni garantie de compatibilité n'est sous-entendue.
